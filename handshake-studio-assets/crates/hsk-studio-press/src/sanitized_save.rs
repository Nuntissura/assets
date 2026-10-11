//! Sanitized full save and the independent "full reachable payload" proof.
//!
//! `verify_payload_absent` re-opens the OUTPUT bytes with the bounded reader (it shares no state
//! with the redaction code) and looks for the removed strings. Sound by construction: every place
//! is scanned and only positive evidence exempts a match.
//! - Raw backstop: every byte of the output file, in every spelling (below). A raw match is dropped
//!   only when it lies entirely inside the bytes the first filter of a stream CONSUMED and the decoded
//!   analysis of that same stream found the same needle; dictionaries, comments, keywords, bytes
//!   after a filter's end-of-data marker and bytes between objects always count (test
//!   `bytes_after_end_of_data_and_dictionary_comments_are_hits`).
//! - Every decoded stream (Flate/LZW/ASCIIHex/ASCII85/RunLength, object streams included) and every
//!   inline image's decoded data (test `inline_image_data_is_decoded_and_scanned`); non-whitespace
//!   input left after a filter's end-of-data marker is `NotProven::TrailingData`.
//! - Every PDF string, name and dictionary key (hex and escaped spellings compare by decoded value),
//!   and every string operand of content that tokenizes.
//! - Text that only a font reading reveals, read by the content walk (`show_text`): fonts
//!   follow the graphics state (`q`/`Q`, `gs`, `Tf`, across a page's content streams and into forms
//!   drawn by `Do`, with inherited resources); content the walk cannot follow is
//!   `NotProven::UnreadableContent` (tests `font_state_follows_q_q_gs_and_do`,
//!   `unreadable_or_stray_content_never_reads_clean`). The walk covers the object table, not only
//!   the page tree: page objects outside the tree (template pages, pages reached only through a
//!   destination) are read with their resources, and every other stream that tokenizes as content is
//!   read with none (an unresolvable font makes its text opaque); a kerned `TJ` is matched per string
//!   and as one joined run (R8 F1/F2; tests `partial_tounicode_and_unreachable_pages_are_read_or_opaque`,
//!   `kerned_tj_runs_are_read_whole`).
//! - Spellings: the given bytes, PDFDocEncoding, UTF-8, UTF-16BE, UTF-16LE, upper- and lower-case hex
//!   of the byte and UTF-16BE spellings, and the codes each font's `/ToUnicode` map gives the text
//!   (scanned in the content streams that can select that font).
//!
//! It also reports incremental sections, objects present in the cross-reference table but
//! unreachable from the trailer, and `endobj` markers with no cross-reference entry.
//! A stream whose filter is not implemented is scanned as raw bytes only and listed in
//! `streams_not_decodable`, which withholds `is_clean`/`is_proven` (test:
//! `undecodable_stream_never_reads_as_proven`). The one exception is the narrow raster class (see
//! `raster`): image XObjects and inline images whose chain ends in a raster codec are listed in
//! `raster_streams`, scanned after their decodable prefix, and do not block (test:
//! `raster_class_is_listed_and_region_images_are_removed`).
//! Out of scope (no claim): application-level encodings inside a decoded payload (zip or base64 in an
//! embedded file, compressed data at an arbitrary offset of a stream that is not content) and pixels.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::Op;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::raster::{InlineClass, RASTER_CLASS, classify_inline, codec_signature, decode_prefix, hidden_zlib, raster_tail};
use crate::reader::{check_cancel, find, parse_document};
use crate::redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
use crate::show_text::{FontCache, FontText, Role, Walker, content_role_streams, fonts_in, is_show, pages, pages_outside_tree, show_strings};
use crate::syntax::{Dict, Object, Stream};
use crate::text_codec::{Enc, decode_text, pdfdoc_byte, utf16be_bytes};
use crate::text_fields::{TextFieldPolicy, TextFieldReport, appearance_roots};
use crate::writer::{reachable, trailer_roots, write_document};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const MAX_HITS: usize = 1000;
/// Font-code spellings (ToUnicode inverse) one content stream is scanned for.
const MAX_CODE_VARIANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadLocation {
    RawBytes { offset: usize },
    DecodedStream { object: u32, offset: usize },
    StringObject { object: u32 },
    ContentString { object: u32 },
    /// Text drawn in an annotation appearance or stray content (including page objects outside the
    /// page tree) that only a font reading (encoding, `/ToUnicode`) or the joined run of a kerned
    /// `TJ` reveals.
    DecodedText { object: u32 },
    /// The decoded data of an inline image in content stream `object`.
    InlineImage { object: u32, offset: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadHit {
    /// Index into the needle list passed to the verifier.
    pub needle: usize,
    pub location: PayloadLocation,
}

/// A needle found as (part of) a text-showing operand of a decoded content stream: the same text
/// legitimately occurs in kept content. Reported separately; not counted as a leak.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextCollision {
    pub needle: usize,
    pub object: u32,
    pub count: u32,
}

/// A stream (or inline image) of the `raster_unsearchable` class: listed, not blocking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterStream {
    /// The stream object, or for an inline image the content stream that holds it.
    pub object: u32,
    /// [`RASTER_CLASS`].
    pub class: &'static str,
    /// The final raster codec (`DCTDecode`, `JPXDecode`, `CCITTFaxDecode`, `JBIG2Decode`).
    pub filter: String,
    pub inline: bool,
}

/// Why a result does not read as proven. Typed so a consumer can tell "a needle was found" from
/// "some text could not be checked".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotProven {
    /// A needle was found outside kept text (`hits`).
    Leak,
    /// More than one xref section, unreachable objects or orphan object bodies.
    Structure,
    /// A stream with an unimplemented filter outside the raster class (`streams_not_decodable`).
    UndecodableStream,
    /// A text-needle search was requested and some content draws text this proof cannot read
    /// (`opaque_text_streams`: a composite font without a usable `/ToUnicode` or with a code it does
    /// not name, a simple-font code nothing reads, a font name page or stray content cannot resolve):
    /// that text was not checked.
    OpaqueText,
    /// A redaction region still holds a mark that was not removed (an image without
    /// `RemoveImages`, a vector, a deep form).
    RegionIncomplete,
    /// A fresh redaction pass over the saved bytes still selects something in a region.
    ResidualInRegion,
    /// The text-field report kept matches by policy or could not verify an appearance.
    TextFieldsKept,
    /// A stream (or inline image) holds non-whitespace bytes after a filter's end-of-data marker
    /// (`trailing_data_streams`): no decoder reads them, so they can hold a removed string in any
    /// encoding.
    TrailingData,
    /// A text-needle search was requested and some content could not be followed by the content
    /// walk (`unreadable_content_streams`: no tokenization, operands split across a page's streams,
    /// graphics-state nesting, form cycle or depth, undecodable form): its text was not checked.
    UnreadableContent,
    /// A distinctive removed string has no complete reading (its font is not in the resources, is a
    /// composite font without a usable `/ToUnicode`, or has a code neither its encoding nor its
    /// `/ToUnicode` reads): the words it drew are unknown, so the proof never searched for them
    /// elsewhere (`RedactedOutput::unreadable_removed`; R8 F3/D3; test
    /// `unreadable_removed_text_is_never_proven`). Withholds `is_proven`, not `region_cleared`.
    UnreadableRemovedText,
}

impl NotProven {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Leak => "leak",
            Self::Structure => "structure",
            Self::UndecodableStream => "undecodable_stream",
            Self::OpaqueText => "opaque_text",
            Self::RegionIncomplete => "region_incomplete",
            Self::ResidualInRegion => "residual_in_region",
            Self::TextFieldsKept => "text_fields_kept",
            Self::TrailingData => "trailing_data",
            Self::UnreadableContent => "unreadable_content",
            Self::UnreadableRemovedText => "unreadable_removed_text",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadProof {
    pub raw_bytes_scanned: usize,
    pub objects_checked: u32,
    pub streams_decoded: u32,
    /// Streams scanned raw only: `(object number, filter name or reason)`. Blocks `is_clean`.
    pub streams_not_decodable: Vec<(u32, String)>,
    /// Raster image streams excluded from the text-needle proof (listed, not blocking; test raster_class_is_listed_and_region_images_are_removed).
    pub raster_streams: Vec<RasterStream>,
    /// Content streams that draw text this proof cannot read: a composite font without a usable
    /// `/ToUnicode` or with a code the map does not name, a simple-font code neither its encoding nor
    /// its `/ToUnicode` reads, or page and stray content naming a font its resources do not hold (tests
    /// `opaque_composite_text_blocks_the_proof`, `partial_tounicode_and_unreachable_pages_are_read_or_opaque`).
    /// Blocks `is_clean` when `needles_searched > 0` (reason `NotProven::OpaqueText`).
    pub opaque_text_streams: Vec<u32>,
    /// `(object, first filter)` of streams with non-whitespace input after a filter's end-of-data
    /// marker (`inline:<filter>` for inline images, `hidden_zlib` behind a raster codec name).
    /// Blocks `is_clean` (reason `NotProven::TrailingData`).
    pub trailing_data_streams: Vec<(u32, String)>,
    /// `(object, reason)` of content the walk could not follow. Blocks `is_clean` when
    /// `needles_searched > 0` (reason `NotProven::UnreadableContent`).
    pub unreadable_content_streams: Vec<(u32, String)>,
    /// Distinct needles the proof searched for (0 = no text-needle search was requested).
    pub needles_searched: u32,
    /// Cross-reference sections in the output (1 = fresh full save, no incremental tail).
    pub xref_sections: u32,
    /// Objects in the cross-reference table that are unreachable from the trailer.
    pub unreachable_objects: Vec<u32>,
    /// `endobj` markers beyond the objects the cross-reference table lists (orphan bodies).
    pub unlisted_object_markers: u32,
    /// Leaks: needle found outside kept text-showing operands (see `verify_payload_absent`).
    pub hits: Vec<PayloadHit>,
    /// Needle also present as kept text-showing operand text (not a leak).
    pub kept_text_collisions: Vec<TextCollision>,
}

impl PayloadProof {
    /// No needle found anywhere scanned, a single xref section, no unreachable or unlisted
    /// objects, AND every stream decoded and scanned. A stream the verifier cannot decode (a filter
    /// this crate does not implement, or a decode error) is only scanned as raw bytes, which says
    /// nothing about a needle stored in its encoded form, so a document holding one is not reported
    /// clean: see `streams_not_decodable` for which streams block the claim. Raster-class streams
    /// (`raster_streams`) do not block. Bytes after an end-of-data marker block. Unreadable
    /// composite text and content the walk cannot follow block when a needle search ran.
    pub fn is_clean(&self) -> bool {
        self.not_proven_reasons().is_empty()
    }
    /// Text drawn with an unreadable composite font was present while a needle search ran.
    pub fn opaque_text_blocks(&self) -> bool {
        self.needles_searched > 0 && !self.opaque_text_streams.is_empty()
    }
    /// Every reason this proof is not clean (empty = clean). Test: `opaque_composite_text_blocks_the_proof`.
    pub fn not_proven_reasons(&self) -> Vec<NotProven> {
        let mut v = self.structural_reasons();
        if !self.hits.is_empty() {
            v.insert(0, NotProven::Leak);
        }
        v
    }
    /// The reasons that do not depend on what was found: structure, undecodable, trailing data,
    /// opaque text and unreadable content.
    fn structural_reasons(&self) -> Vec<NotProven> {
        let mut v = Vec::new();
        if self.xref_sections > 1 || !self.unreachable_objects.is_empty() || self.unlisted_object_markers != 0 {
            v.push(NotProven::Structure);
        }
        if !self.fully_decoded() {
            v.push(NotProven::UndecodableStream);
        }
        if !self.trailing_data_streams.is_empty() {
            v.push(NotProven::TrailingData);
        }
        if self.opaque_text_blocks() {
            v.push(NotProven::OpaqueText);
        }
        if self.needles_searched > 0 && !self.unreadable_content_streams.is_empty() {
            v.push(NotProven::UnreadableContent);
        }
        v
    }
    /// Every stream was decoded and scanned (otherwise see `streams_not_decodable`).
    pub fn fully_decoded(&self) -> bool {
        self.streams_not_decodable.is_empty()
    }
}

/// A search pattern: the needle bytes (or a spelling of them), whether the whole-token boundary
/// rule applies, and (on one variant per needle) the needle as text for matching ToUnicode text.
pub(crate) struct Variant {
    pub(crate) needle_index: usize,
    pub(crate) bytes: Vec<u8>,
    pub(crate) token: bool,
    pub(crate) text: Option<String>,
    /// A hexadecimal spelling (never compared with decoded string values).
    pub(crate) hex: bool,
}

fn utf16le_bytes(chars: &[char]) -> Vec<u8> {
    let be = utf16be_bytes(chars);
    be.chunks(2).flat_map(|c| c.iter().rev().copied()).collect()
}

fn hex_of(bytes: &[u8], upper: bool) -> Vec<u8> {
    let digits: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    bytes.iter().flat_map(|b| [digits[usize::from(b >> 4)], digits[usize::from(b & 15)]]).collect()
}

/// Adds every spelling of needle `i` (see the module docs). `token` applies to the byte spellings
/// that start or end with ASCII letters or digits; UTF-16 and hex spellings match strictly.
fn push_spellings(out: &mut Vec<Variant>, i: usize, bytes: &[u8], chars: &[char], text: Option<String>) {
    let push = |out: &mut Vec<Variant>, bytes: Vec<u8>, token: bool, text: Option<String>, hex: bool| {
        if !bytes.is_empty() && !out.iter().any(|v| v.needle_index == i && v.bytes == bytes) {
            out.push(Variant { needle_index: i, bytes, token, text, hex });
        } else if let Some(t) = text
            && let Some(v) = out.iter_mut().find(|v| v.needle_index == i && v.bytes == bytes)
            && v.text.is_none()
        {
            v.text = Some(t);
        }
    };
    push(out, bytes.to_vec(), true, text, false);
    if !chars.is_empty() {
        if let Some(b) = chars.iter().map(|c| pdfdoc_byte(*c)).collect::<Option<Vec<u8>>>() {
            push(out, b, true, None, false);
        }
        push(out, chars.iter().collect::<String>().into_bytes(), true, None, false);
        push(out, utf16be_bytes(chars), false, None, false);
        push(out, utf16le_bytes(chars), false, None, false);
    }
    let plain: Vec<Vec<u8>> = out.iter().filter(|v| v.needle_index == i && !v.hex).map(|v| v.bytes.clone()).collect();
    for b in plain {
        push(out, hex_of(&b, true), false, None, true);
        push(out, hex_of(&b, false), false, None, true);
    }
}

/// Raw-byte needles: the bytes as given, plus their spellings (module docs). `unicode[i]`, when
/// present, is the text of needle `i` as its font's `/ToUnicode` reads it.
fn variants_unicode(needles: &[Vec<u8>], unicode: &[Option<String>]) -> Vec<Variant> {
    let mut out = Vec::new();
    for (i, n) in needles.iter().enumerate() {
        if n.is_empty() {
            continue;
        }
        let (chars, enc) = decode_text(n);
        let mapped = unicode.get(i).cloned().flatten().filter(|t| !t.trim().is_empty());
        let text = mapped.clone().or_else(|| (enc == Enc::PdfDoc).then(|| chars.iter().collect::<String>()));
        let spelled: Vec<char> = match &mapped {
            Some(t) => t.chars().collect(),
            None if enc == Enc::PdfDoc => chars,
            None => Vec::new(),
        };
        push_spellings(&mut out, i, n, &spelled, text);
    }
    out
}

fn variants(needles: &[Vec<u8>]) -> Vec<Variant> {
    variants_unicode(needles, &[])
}

/// Unicode needles: PDFDocEncoding (when every char has a code), UTF-8, UTF-16BE/LE and hex
/// spellings. Hit indices refer to `needles`.
pub(crate) fn variants_text(needles: &[String]) -> Vec<Variant> {
    let mut out = Vec::new();
    for (i, s) in needles.iter().enumerate() {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        let chars: Vec<char> = t.chars().collect();
        let first = chars.iter().map(|c| pdfdoc_byte(*c)).collect::<Option<Vec<u8>>>().unwrap_or_else(|| t.as_bytes().to_vec());
        push_spellings(&mut out, i, &first, &chars, Some(t.to_owned()));
    }
    out
}

/// Whole-token rule: a needle that starts (ends) with an ASCII letter or digit only matches when
/// the byte before (after) is not an ASCII letter or digit. "memory" inside the identifier
/// `cite.kang2025memoryos` is a different token; "the budget plan memo" contains the token
/// "budget plan". UTF-16 and hex spellings match strictly.
fn at_boundary(hay: &[u8], at: usize, v: &Variant) -> bool {
    if !v.token {
        return true;
    }
    let (Some(first), Some(last)) = (v.bytes.first(), v.bytes.last()) else { return false };
    let before = at.checked_sub(1).and_then(|p| hay.get(p)).is_none_or(|b| !b.is_ascii_alphanumeric()) || !first.is_ascii_alphanumeric();
    let after = hay.get(at.saturating_add(v.bytes.len())).is_none_or(|b| !b.is_ascii_alphanumeric()) || !last.is_ascii_alphanumeric();
    before && after
}

/// The same whole-token rule over characters (ToUnicode text).
fn text_found(hay: &[char], needle: &[char], token: bool) -> bool {
    let n = needle.len();
    let (Some(first), Some(last)) = (needle.first(), needle.last()) else { return false };
    if hay.len() < n {
        return false;
    }
    (0..=hay.len().saturating_sub(n)).any(|i| {
        let end = i.saturating_add(n);
        hay.get(i..end) == Some(needle)
            && (!token || {
                let before = i.checked_sub(1).and_then(|p| hay.get(p)).is_none_or(|c| !c.is_ascii_alphanumeric()) || !first.is_ascii_alphanumeric();
                let after = hay.get(end).is_none_or(|c| !c.is_ascii_alphanumeric()) || !last.is_ascii_alphanumeric();
                before && after
            })
    })
}

/// Window of one search step: cancellation is polled between windows (a 64 MiB file is 64 polls).
const SCAN_CHUNK: usize = 1 << 20;

/// A match: `(needle index, position, matched length)`.
type Match = (usize, usize, usize);

/// Every occurrence of every variant (whole-token rule applied), in variant order then position
/// order. The haystack is searched in windows that overlap by the needle length minus one, so a hit
/// is found exactly once whatever the window edges are; the token is polled between windows.
fn scan(hay: &[u8], vars: &[Variant], cancel: &CancellationToken) -> Result<Vec<Match>> {
    let mut hits = Vec::new();
    for v in vars {
        let overlap = v.bytes.len().saturating_sub(1);
        let mut start = 0usize;
        while start < hay.len() {
            check_cancel(cancel)?;
            let window_end = start.saturating_add(SCAN_CHUNK).min(hay.len());
            let slice = hay.get(start..window_end.saturating_add(overlap).min(hay.len())).unwrap_or(&[]);
            let mut from = 0usize;
            while let Some(at) = find(slice.get(from..).unwrap_or(&[]), &v.bytes) {
                let rel = from.saturating_add(at);
                let pos = start.saturating_add(rel);
                if pos >= window_end {
                    break; // starts in the next window
                }
                if at_boundary(hay, pos, v) {
                    hits.push((v.needle_index, pos, v.bytes.len()));
                    if hits.len() >= MAX_HITS {
                        return Ok(hits);
                    }
                }
                from = rel.saturating_add(1);
            }
            start = window_end;
        }
    }
    Ok(hits)
}

/// Number of non-overlapping-start occurrences of `marker`, polling the token between windows.
fn count_marker(hay: &[u8], marker: &[u8], cancel: &CancellationToken) -> Result<usize> {
    let mut n = 0usize;
    let mut start = 0usize;
    while start < hay.len() {
        check_cancel(cancel)?;
        let window_end = start.saturating_add(SCAN_CHUNK).min(hay.len());
        let slice = hay.get(start..window_end.saturating_add(marker.len().saturating_sub(1)).min(hay.len())).unwrap_or(&[]);
        let width = window_end.saturating_sub(start);
        n = n.saturating_add(slice.windows(marker.len().max(1)).take(width).filter(|w| *w == marker).count());
        start = window_end;
    }
    Ok(n)
}

fn contains(s: &[u8], vars: &[Variant]) -> Option<usize> {
    vars.iter().find(|v| scan_one(s, v)).map(|v| v.needle_index)
}

fn scan_one(hay: &[u8], v: &Variant) -> bool {
    let mut from = 0usize;
    while let Some(at) = find(hay.get(from..).unwrap_or(&[]), &v.bytes) {
        let pos = from.saturating_add(at);
        if at_boundary(hay, pos, v) {
            return true;
        }
        from = pos.saturating_add(1);
    }
    false
}

/// Streams a best-effort tokenization is tried on when no page, appearance or role draws them (their
/// string operands and inline images are then checked too).
fn looks_like_content(d: &Dict) -> bool {
    let sub_ok = match d.get("Subtype") {
        None => true,
        Some(s) => s.is_name("Form"),
    };
    let type_ok = !matches!(d.get("Type").and_then(Object::as_name), Some(b"XRef" | b"ObjStm" | b"Metadata" | b"EmbeddedFile"));
    sub_ok && type_ok
}

/// What the kept contexts of one text-showing operator draw: every font reading (per string spaced,
/// and joined over a kerned `TJ`), and per string whether its bytes are themselves drawn text.
#[derive(Default)]
struct KeptOp {
    readings: Vec<Vec<char>>,
    raw_drawn: Vec<bool>,
}

/// Needles a kept text-showing operator itself DRAWS: a non-hex spelling inside one of its string
/// values whose bytes are the drawn text (a simple font that reads the same letters as the bytes; no
/// token rule), or a font reading of the operator (encoding, `/ToUnicode`, kerned run joined). Only
/// these may exempt a match inside the operator's bytes, so a comment or a name between operand and
/// operator never rides along, and bytes a font draws as other glyphs (`/Differences` remapping,
/// composite codes, an unresolved or unset font) are no kept text (R8 P1; test
/// `kept_text_needs_a_drawn_reading`).
fn shown_needles(op: &Op, kept: Option<&KeptOp>, vars: &[Variant], texts: &[(usize, Vec<char>, bool)]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let Some(k) = kept else { return out };
    for (idx, s) in show_strings(op).into_iter().enumerate() {
        if !k.raw_drawn.get(idx).copied().unwrap_or(false) {
            continue;
        }
        for v in vars.iter().filter(|v| !v.hex) {
            if find(s, &v.bytes).is_some() {
                out.insert(v.needle_index);
            }
        }
    }
    for m in &k.readings {
        for (needle, text, _) in texts {
            if text_found(m, text, false) {
                out.insert(*needle);
            }
        }
    }
    out
}

/// Inline-image dictionary with the abbreviated filter keys spelled out (ISO 32000-2 8.9.7).
fn inline_dict(operands: &[Object]) -> Dict {
    let mut d = Dict::default();
    for pair in operands.chunks(2) {
        if let [Object::Name(k), v] = pair {
            let key: &[u8] = match k.as_slice() {
                b"F" => b"Filter",
                b"DP" => b"DecodeParms",
                b"W" => b"Width",
                b"H" => b"Height",
                other => other,
            };
            d.set_key(key, v.clone());
        }
    }
    d
}

/// Scans needles in `bytes`, pushing hits at `loc(offset)`.
fn push_hits(proof: &mut PayloadProof, bytes: &[u8], vars: &[Variant], cancel: &CancellationToken, loc: impl Fn(usize) -> PayloadLocation) -> Result<()> {
    for (needle, offset, _) in scan(bytes, vars, cancel)? {
        proof.hits.push(PayloadHit { needle, location: loc(offset) });
    }
    Ok(())
}

/// Decodes and scans every inline image of a parsed content stream (test
/// `inline_image_data_is_decoded_and_scanned`).
#[allow(clippy::too_many_arguments)]
fn inline_images(
    proof: &mut PayloadProof,
    num: u32,
    data: &[u8],
    ops: &[Op],
    vars: &[Variant],
    limits: &Limits,
    budget: &mut usize,
    cancel: &CancellationToken,
) -> Result<()> {
    for op in ops.iter().filter(|op| op.name == b"BI") {
        let Some((a, b)) = op.inline_data else { continue };
        let image = Stream { dict: inline_dict(&op.operands), data: data.get(a..b).unwrap_or(&[]).to_vec() };
        let at = |offset| PayloadLocation::InlineImage { object: num, offset };
        match classify_inline(&op.operands) {
            InlineClass::Searchable => {
                if image.dict.get("Filter").is_none_or(|f| matches!(f, Object::Array(x) if x.is_empty())) {
                    continue; // unfiltered: these bytes are content-stream bytes, scanned already
                }
                match filters::decode_chain(&image, limits, budget) {
                    Ok(d) => {
                        if let Some(f) = d.trailing {
                            proof.trailing_data_streams.push((num, format!("inline:{f}")));
                        }
                        push_hits(proof, &d.data, vars, cancel, at)?;
                    }
                    Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
                    Err(_) => proof.streams_not_decodable.push((num, "inline:decode_error".into())),
                }
            }
            InlineClass::Raster(filter) => {
                match decode_prefix(&image, limits, budget) {
                    Ok(d) => {
                        if let Some(f) = d.trailing {
                            proof.trailing_data_streams.push((num, format!("inline:{f}")));
                        }
                        push_hits(proof, &d.data, vars, cancel, at)?;
                        if let Some((inner, trailing)) = hidden_zlib(&d.data, limits, budget)? {
                            if trailing {
                                proof.trailing_data_streams.push((num, "inline:hidden_zlib".into()));
                            }
                            push_hits(proof, &inner, vars, cancel, at)?;
                        }
                        // PLR-040: only data with the codec's own structure is raster.
                        match codec_signature(&filter, &d.data, &image.dict) {
                            Ok(()) => proof.raster_streams.push(RasterStream { object: num, class: RASTER_CLASS, filter, inline: true }),
                            Err(why) => proof.streams_not_decodable.push((num, format!("inline:{filter}:{why}"))),
                        }
                    }
                    Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
                    Err(_) => proof.streams_not_decodable.push((num, "inline:decode_error".into())),
                }
            }
            InlineClass::Blocking(f) => proof.streams_not_decodable.push((num, format!("inline:{f}"))),
        }
    }
    Ok(())
}

/// Scans the saved bytes for `needles` (see module docs). Needles are raw string bytes; empty
/// needles are ignored.
///
/// Context rule (what makes the proof usable for natural-language documents without weakening
/// it): a needle inside a TEXT-SHOWING operator (Tj, TJ, ', ") of a stream that is drawn ONLY as
/// kept page text (page content, and forms a page draws by `Do`; test
/// `listed_but_undrawn_forms_are_not_kept_text`), where a reading that operator DRAWS carries the
/// needle (its font reading: encoding, `/ToUnicode`, the joined run of a kerned `TJ`; its bytes only
/// where its simple font reads them as the same letters, test `kept_text_needs_a_drawn_reading`), is a
/// kept-text collision, reported in `kept_text_collisions`, because the same word legitimately occurs
/// in kept text (the in-region instance is proven gone by the residual pass in [`redact_and_save`]). A
/// needle anywhere else is a leak and lands in `hits`: raw bytes (headers, dictionaries, comments,
/// object text, xref, orphan bodies, bytes after an end-of-data marker), string/name objects and
/// dictionary keys (metadata, annotation contents, outlines, ActualText), non-show content operands
/// (marked-content properties), inline-image data, every decoded non-content stream (XMP, embedded
/// files, thumbnails, private data, object streams), stray content that no page draws (page objects
/// outside the page tree included), and all text of annotation appearance streams.
pub fn verify_payload_absent(
    output: &[u8],
    needles: &[Vec<u8>],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<PayloadProof> {
    verify_with_variants(output, variants(needles), limits, cancel)
}

/// [`verify_payload_absent`] with the text of each needle as its font's `/ToUnicode` reads it
/// (`unicode[i]` belongs to `needles[i]`), so the same words drawn with other font codes are found.
pub fn verify_payload_absent_unicode(
    output: &[u8],
    needles: &[Vec<u8>],
    unicode: &[Option<String>],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<PayloadProof> {
    verify_with_variants(output, variants_unicode(needles, unicode), limits, cancel)
}

/// The codes under which `font` draws `text`: through its `/ToUnicode` map (composite and simple
/// fonts) and through the encoding of a simple font (base encoding, `/Differences`).
fn font_codes(font: &FontText, text: &[char]) -> Vec<Vec<u8>> {
    match font {
        FontText::Composite { cmap } => cmap.encode(text).into_iter().collect(),
        FontText::Simple { enc, cmap } => enc.encode(text).into_iter().chain(cmap.as_ref().and_then(|c| c.encode(text))).collect(),
        FontText::Opaque => Vec::new(),
    }
}

/// Font-code spellings (raw and hex) of the needle texts under each of `fonts` that can encode
/// them (test `differences_encoded_text_is_found_proven_and_redacted` for the encoding side).
fn code_variants(
    fonts: &[Rc<FontText>],
    texts: &[(usize, Vec<char>, bool)],
    vars: &[Variant],
) -> Vec<Variant> {
    let mut out: Vec<Variant> = Vec::new();
    for f in fonts {
        for (needle, text, _) in texts {
            for codes in font_codes(f, text).into_iter().filter(|c| !c.is_empty()) {
                if out.len() >= MAX_CODE_VARIANTS {
                    return out;
                }
                // Content streams usually write codes as hex strings: the hex spellings too.
                for (bytes, hex) in [(hex_of(&codes, true), true), (hex_of(&codes, false), true), (codes.clone(), false)] {
                    if !vars.iter().chain(out.iter()).any(|v| v.needle_index == *needle && v.bytes == bytes) {
                        out.push(Variant { needle_index: *needle, bytes, token: false, text: None, hex });
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn verify_with_variants(output: &[u8], vars: Vec<Variant>, limits: &Limits, cancel: &CancellationToken) -> Result<PayloadProof> {
    let parsed = parse_document(output, limits, cancel)?;
    let objs = &parsed.objects;
    let mut proof = PayloadProof {
        raw_bytes_scanned: output.len(),
        objects_checked: 0,
        streams_decoded: 0,
        streams_not_decodable: Vec::new(),
        raster_streams: Vec::new(),
        opaque_text_streams: Vec::new(),
        trailing_data_streams: Vec::new(),
        unreadable_content_streams: Vec::new(),
        needles_searched: u32::try_from(
            vars.iter().filter(|v| !v.bytes.is_empty() || v.text.is_some()).map(|v| v.needle_index).collect::<BTreeSet<_>>().len(),
        )
        .unwrap_or(u32::MAX),
        xref_sections: u32::try_from(parsed.xref_sections).unwrap_or(u32::MAX),
        unreachable_objects: Vec::new(),
        unlisted_object_markers: 0,
        hits: Vec::new(),
        kept_text_collisions: Vec::new(),
    };
    let raw_hits = scan(output, &vars, cancel)?;
    let live = reachable(objs, &trailer_roots(&parsed.trailer));
    proof.unreachable_objects = objs.keys().copied().filter(|n| !live.contains(n)).collect();
    let markers = count_marker(output, b"endobj", cancel)?;
    let listed = objs.values().filter(|e| e.span.is_some()).count();
    proof.unlisted_object_markers = u32::try_from(markers.saturating_sub(listed)).unwrap_or(u32::MAX);
    // Text needles for ToUnicode-read text.
    let texts: Vec<(usize, Vec<char>, bool)> =
        vars.iter().filter_map(|v| v.text.as_ref().map(|t| (v.needle_index, t.chars().collect(), v.token))).collect();

    // 1. Object model: every string, name and dictionary key.
    for (i, (num, entry)) in objs.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        proof.objects_checked = proof.objects_checked.saturating_add(1);
        let mut stack: Vec<&Object> = vec![&entry.obj];
        let hit = |s: &[u8], proof: &mut PayloadProof| {
            if let Some(needle) = contains(s, &vars) {
                proof.hits.push(PayloadHit { needle, location: PayloadLocation::StringObject { object: *num } });
            }
        };
        while let Some(o) = stack.pop() {
            let dict = match o {
                Object::Str(s) | Object::Name(s) => {
                    hit(s, &mut proof);
                    continue;
                }
                Object::Array(a) => {
                    stack.extend(a.iter());
                    continue;
                }
                Object::Dict(d) => d,
                Object::Stream(s) => &s.dict,
                _ => continue,
            };
            for (k, v) in &dict.entries {
                hit(k, &mut proof);
                stack.push(v);
            }
        }
    }

    // 2. Decode every stream once: bytes to scan, and the raw range its first filter consumed.
    let mut budget = limits.max_decoded_total;
    let mut decoded: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut consumed: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let mut raster_objects: BTreeSet<u32> = BTreeSet::new();
    for (i, (num, entry)) in objs.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        let Object::Stream(s) = &entry.obj else { continue };
        let mut raster: Option<String> = None;
        if !filters::is_decodable(&s.dict) {
            raster = raster_tail(&s.dict);
            if raster.is_none() {
                let name = s
                    .dict
                    .get("Filter")
                    .map(|f| match f {
                        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
                        Object::Array(a) => a.iter().filter_map(|x| x.as_name().map(|n| String::from_utf8_lossy(n).into_owned())).collect::<Vec<_>>().join("+"),
                        _ => "unknown".into(),
                    })
                    .unwrap_or_default();
                proof.streams_not_decodable.push((*num, name));
                continue;
            }
        }
        let result = if raster.is_some() { decode_prefix(s, limits, &mut budget) } else { filters::decode_chain(s, limits, &mut budget) };
        match result {
            Ok(d) => {
                if let Some(f) = &d.trailing {
                    proof.trailing_data_streams.push((*num, f.clone()));
                }
                if let Some(at) = entry.data_at {
                    consumed.insert(*num, (at, at.saturating_add(d.consumed)));
                }
                match raster {
                    Some(filter) => {
                        // A "raster" stream whose codec input is itself a zlib stream is not pixel
                        // data: the inflated bytes are scanned like any decoded stream (test
                        // stray_streams_and_hidden_payloads_are_not_kept_text).
                        if let Some((inner, trailing)) = hidden_zlib(&d.data, limits, &mut budget)? {
                            if trailing {
                                proof.trailing_data_streams.push((*num, "hidden_zlib".into()));
                            }
                            push_hits(&mut proof, &inner, &vars, cancel, |offset| PayloadLocation::DecodedStream { object: *num, offset })?;
                        }
                        // PLR-040 (fail closed): the codec name alone does not make pixels; data
                        // without the codec's structure blocks as undecodable (test
                        // raster_names_need_codec_structure).
                        match codec_signature(&filter, &d.data, &s.dict) {
                            Ok(()) => proof.raster_streams.push(RasterStream { object: *num, class: RASTER_CLASS, filter, inline: false }),
                            Err(why) => proof.streams_not_decodable.push((*num, format!("{filter}:{why}"))),
                        }
                        raster_objects.insert(*num);
                    }
                    None => proof.streams_decoded = proof.streams_decoded.saturating_add(1),
                }
                decoded.insert(*num, d.data);
            }
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => proof.streams_not_decodable.push((*num, "decode_error".into())),
        }
    }

    // 3. Content walk over the object table (R8 F1): pages of the page tree (kept text), page
    // objects outside it, appearances, content-role streams, then every other stream that tokenizes
    // as content (stray, no resources).
    let mut walker = Walker::new(objs, limits, budget, FontCache::default()).with_decoded(&decoded);
    let tree = pages(objs, &parsed.trailer);
    for (_, segments, resources) in &tree {
        check_cancel(cancel)?;
        walker.run_page(segments, *resources, Role::Kept)?;
    }
    let tree_pages: BTreeSet<u32> = tree.iter().map(|(n, _, _)| *n).collect();
    for (segments, resources) in pages_outside_tree(objs, &tree_pages) {
        check_cancel(cancel)?;
        walker.run_page(&segments, resources, Role::Stray)?;
    }
    for n in appearance_roots(objs) {
        let resources = match objs.get(&n).map(|e| &e.obj) {
            Some(Object::Stream(s)) => s.dict.get("Resources").map(|r| crate::redaction::res(objs, r)).and_then(Object::as_dict),
            _ => None,
        };
        walker.run_stream(n, resources, Role::Appearance)?;
    }
    for (n, resources) in content_role_streams(objs) {
        if !walker.roles.contains_key(&n) && decoded.contains_key(&n) {
            walker.run_stream(n, resources, Role::Stray)?;
        }
    }
    for (i, num) in decoded.keys().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        if walker.roles.contains_key(num) || raster_objects.contains(num) {
            continue;
        }
        if let Some(Object::Stream(s)) = objs.get(num).map(|e| &e.obj)
            && looks_like_content(&s.dict)
        {
            walker.run_unreached(*num)?;
        }
    }
    check_cancel(cancel)?;

    // Records by (stream, op): font readings in their drawing contexts. Kept text is a collision;
    // anything else a hit. A kerned TJ is matched spaced (per string) and joined (one run): its
    // readings joined and its string bytes joined, in every byte spelling (R8 F2/D2).
    let mut collisions: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    let mut kept_ops: BTreeMap<(u32, usize), KeptOp> = BTreeMap::new();
    let mut opaque: BTreeSet<u32> = BTreeSet::new();
    for rec in &walker.records {
        let Some(stream) = rec.stream else { continue };
        if rec.opaque {
            opaque.insert(stream);
        }
        let raw_joined: Vec<char> = rec.bytes.iter().map(|b| char::from(*b)).collect();
        if rec.role == Role::Kept {
            let k = kept_ops.entry((stream, rec.op)).or_default();
            if !rec.mapped.is_empty() {
                k.readings.push(rec.mapped.clone());
            }
            k.readings.extend(rec.joined.iter().cloned());
            if !raw_joined.is_empty() && rec.raw_drawn.iter().all(|d| *d) {
                k.readings.push(raw_joined.clone());
            }
            if k.raw_drawn.len() < rec.raw_drawn.len() {
                k.raw_drawn.resize(rec.raw_drawn.len(), false);
            }
            for (slot, d) in k.raw_drawn.iter_mut().zip(&rec.raw_drawn) {
                *slot |= *d;
            }
        }
        let mut matched: BTreeSet<usize> = BTreeSet::new();
        for (needle, text, token) in &texts {
            let readings = std::iter::once(&rec.mapped).chain(rec.joined.iter()).chain(std::iter::once(&raw_joined));
            if readings.filter(|r| !r.is_empty()).any(|r| text_found(r, text, *token)) {
                matched.insert(*needle);
            }
        }
        if !rec.bytes.is_empty() {
            matched.extend(vars.iter().filter(|v| !v.hex && scan_one(&rec.bytes, v)).map(|v| v.needle_index));
        }
        for needle in matched {
            if rec.role == Role::Kept {
                let c = collisions.entry((needle, stream)).or_default();
                *c = c.saturating_add(1);
            } else {
                proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedText { object: stream } });
            }
        }
    }
    proof.opaque_text_streams = opaque.into_iter().collect();
    proof.unreadable_content_streams = walker.unreadable.iter().map(|(n, why)| (*n, (*why).to_owned())).collect();

    // 4. Every decoded stream: bytes, content operands, inline images.
    let mut found: BTreeSet<(usize, u32)> = BTreeSet::new();
    let mut fonts = FontCache::default();
    for (i, (num, bytes)) in decoded.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        if !matches!(objs.get(num).map(|e| &e.obj), Some(Object::Stream(_))) {
            continue;
        }
        let roles = walker.roles.get(num);
        // Byte exemptions only where every context that draws this stream is kept page text.
        let exempt = roles.is_some_and(|r| !r.is_empty() && r.iter().all(|x| *x == Role::Kept));
        // Every stream that tokenizes as content was walked above (it has a role); any other stream
        // is not content and its bytes are scanned in every spelling.
        let ops: Option<Rc<(Vec<Op>, bool)>> = if roles.is_some() { walker.ops_of(*num)? } else { None };
        let ops_list: &[Op] = ops.as_ref().map(|p| p.0.as_slice()).unwrap_or(&[]);
        // Needles each kept show operator draws (only those exempt a match in its bytes or operands).
        let drawn: BTreeMap<usize, BTreeSet<usize>> = if exempt {
            ops_list
                .iter()
                .enumerate()
                .filter(|(_, op)| is_show(&op.name))
                .map(|(k, op)| (k, shown_needles(op, kept_ops.get(&(*num, k)), &vars, &texts)))
                .filter(|(_, ns)| !ns.is_empty())
                .collect()
        } else {
            BTreeMap::new()
        };
        let spans: Vec<(usize, usize, &BTreeSet<usize>)> =
            drawn.iter().filter_map(|(k, ns)| ops_list.get(*k).map(|op| (op.start, op.end, ns))).collect();
        // Font-code spellings for the fonts this stream can select (backstop for the font model).
        let mut stream_vars: Vec<Variant> = Vec::new();
        if let Some(used) = walker.resources_used.get(num) {
            let fs = fonts_in(objs, used, limits, &mut walker.budget, &mut fonts)?;
            stream_vars = code_variants(&fs, &texts, &vars);
        }
        for list in [&vars, &stream_vars] {
            for (needle, offset, _) in scan(bytes, list, cancel)? {
                found.insert((needle, *num));
                if spans.iter().any(|(a, b, ns)| offset >= *a && offset < *b && ns.contains(&needle)) {
                    continue;
                }
                proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedStream { object: *num, offset } });
            }
        }
        for (k, op) in ops_list.iter().enumerate() {
            let kept_needles = drawn.get(&k);
            let mut st: Vec<&Object> = op.operands.iter().collect();
            while let Some(o) = st.pop() {
                match o {
                    Object::Str(x) => {
                        if let Some(needle) = contains(x, &vars) {
                            if kept_needles.is_some_and(|ns| ns.contains(&needle)) {
                                let c = collisions.entry((needle, *num)).or_default();
                                *c = c.saturating_add(1);
                            } else {
                                proof.hits.push(PayloadHit { needle, location: PayloadLocation::ContentString { object: *num } });
                            }
                        }
                    }
                    Object::Name(x) => {
                        if let Some(needle) = contains(x, &vars) {
                            proof.hits.push(PayloadHit { needle, location: PayloadLocation::ContentString { object: *num } });
                        }
                    }
                    Object::Array(a) => st.extend(a.iter()),
                    Object::Dict(d) => {
                        for (k, v) in &d.entries {
                            if let Some(needle) = contains(k, &vars) {
                                proof.hits.push(PayloadHit { needle, location: PayloadLocation::ContentString { object: *num } });
                            }
                            st.push(v);
                        }
                    }
                    _ => {}
                }
            }
        }
        inline_images(&mut proof, *num, bytes, ops_list, &vars, limits, &mut walker.budget, cancel)?;
        if proof.hits.len() > MAX_HITS {
            proof.hits.truncate(MAX_HITS);
            break;
        }
    }

    // 5. Raw backstop: a raw match counts unless it sits wholly inside bytes a stream's first filter
    // consumed and that stream's decoded scan found the same needle.
    for (needle, offset, len) in raw_hits {
        let end = offset.saturating_add(len);
        let explained = consumed.iter().any(|(num, (a, b))| offset >= *a && end <= *b && found.contains(&(needle, *num)));
        if !explained {
            proof.hits.push(PayloadHit { needle, location: PayloadLocation::RawBytes { offset } });
        }
    }
    proof.hits.truncate(MAX_HITS);
    proof.kept_text_collisions = collisions.into_iter().map(|((needle, object), count)| TextCollision { needle, object, count }).collect();
    Ok(proof)
}

/// Output of [`redact_and_save`]: the saved bytes, what each region removed, and the
/// independent proof over the saved bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactedOutput {
    pub bytes: Vec<u8>,
    pub reports: Vec<RedactionReport>,
    /// Payload proof over the saved bytes for the distinctive removed strings.
    pub proof: PayloadProof,
    /// Removed strings shorter than [`MIN_DISTINCTIVE_NEEDLE`] bytes (typical for kerned words
    /// and subset-font glyph codes). A byte scan is no evidence for those because they legitimately
    /// occur elsewhere, so they are NOT part of `proof`; `residual_in_region` covers them.
    pub short_needles: u32,
    /// Text strings, annotations and unsupported marks a fresh redaction pass over the SAVED
    /// bytes still finds in the regions (0 = the saved bytes hold nothing the region test selects).
    pub residual_in_region: u32,
    /// Outline/annotation text redaction of the removed strings (`redact_and_save_with` with a
    /// text policy); `None` when it was not requested.
    pub text_fields: Option<TextFieldReport>,
    /// Distinctive removed strings (at least [`MIN_DISTINCTIVE_NEEDLE`] bytes) without a complete
    /// reading (`RedactionReport::removed_unreadable`): the words they drew are unknown, so the proof
    /// searched only their codes. Non-zero withholds `is_proven` with
    /// `NotProven::UnreadableRemovedText` (R8 F3/D3; test `unreadable_removed_text_is_never_proven`).
    pub unreadable_removed: u32,
}

impl RedactedOutput {
    /// Every region fully removed (nothing left in `unredacted`), the saved bytes pass the payload
    /// proof for every distinctive removed string, every distinctive removed string has a complete
    /// reading, and a fresh pass over the saved bytes finds nothing left in the regions.
    pub fn is_proven(&self) -> bool {
        self.not_proven_reasons().is_empty()
    }

    /// Why this result does not read as proven (empty = proven). Tests:
    /// `opaque_composite_text_blocks_the_proof`, `unreadable_removed_text_is_never_proven`.
    pub fn not_proven_reasons(&self) -> Vec<NotProven> {
        let mut v = self.region_reasons();
        if !self.proof.hits.is_empty() {
            v.push(NotProven::Leak);
        }
        if self.unreadable_removed != 0 {
            v.push(NotProven::UnreadableRemovedText);
        }
        v
    }

    fn region_reasons(&self) -> Vec<NotProven> {
        let mut v = Vec::new();
        if !self.reports.iter().all(|r| r.complete) {
            v.push(NotProven::RegionIncomplete);
        }
        if self.residual_in_region != 0 {
            v.push(NotProven::ResidualInRegion);
        }
        v.extend(self.proof.structural_reasons());
        v
    }

    /// The regions themselves are proven clear: every mark removed (an image under a region is
    /// removed only with `UnsupportedPolicy::RemoveImages`, otherwise the region is incomplete), a
    /// fresh pass over the saved bytes selects nothing, one xref section, no unreachable or
    /// unlisted objects, and every stream of the saved file was decoded (an undecodable stream can
    /// still hold a removed string in its encoded form, so the structural claim is withheld:
    /// `proof.streams_not_decodable` names the blockers; raster-class streams do not block; bytes after
    /// an end-of-data marker block, `NotProven::TrailingData`; unreadable composite text and content
    /// the walk cannot follow block when a needle search ran, `NotProven::OpaqueText` and
    /// `NotProven::UnreadableContent`). Says
    /// nothing about the same words legitimately living elsewhere (outline titles, metadata);
    /// `is_proven` also requires `proof.hits` to be empty.
    pub fn region_cleared(&self) -> bool {
        self.region_reasons().is_empty()
    }
}

/// Removed strings at least this long (bytes) are verified by byte scan.
pub const MIN_DISTINCTIVE_NEEDLE: usize = 6;

/// Redacts every region, full-saves, and verifies the saved bytes against the removed strings.
/// On error the document is restored to its state before the call.
pub fn redact_and_save(
    doc: &mut PdfDoc,
    regions: &[RedactionRegion],
    policy: UnsupportedPolicy,
    cancel: &CancellationToken,
) -> Result<RedactedOutput> {
    redact_and_save_with(doc, regions, policy, None, cancel)
}

/// [`redact_and_save`], and with `text` also redacts the distinctive removed strings from outline
/// titles and annotation text (slice 3) before the full save, so the same words no longer survive
/// as leaks there. `RedactedOutput::text_fields` carries the per-object outcomes.
pub fn redact_and_save_with(
    doc: &mut PdfDoc,
    regions: &[RedactionRegion],
    policy: UnsupportedPolicy,
    text: Option<&TextFieldPolicy>,
    cancel: &CancellationToken,
) -> Result<RedactedOutput> {
    let backup = doc.objects.clone();
    let result = (|| -> Result<RedactedOutput> {
        let mut reports = Vec::with_capacity(regions.len());
        for region in regions {
            reports.push(doc.redact_region(region, policy, cancel)?);
        }
        let all: Vec<(Vec<u8>, Option<String>)> = reports
            .iter()
            .flat_map(|r| r.removed_text.iter().cloned().zip(r.removed_unicode.iter().cloned()))
            .collect();
        let (needles, unicode): (Vec<Vec<u8>>, Vec<Option<String>>) =
            all.iter().filter(|(n, _)| n.len() >= MIN_DISTINCTIVE_NEEDLE).cloned().unzip();
        let short_needles = u32::try_from(all.len().saturating_sub(needles.len())).unwrap_or(u32::MAX);
        // R8 F3/D3: a distinctive removed string whose words are unknown cannot be searched for.
        let unreadable = reports
            .iter()
            .flat_map(|r| r.removed_text.iter().zip(r.removed_unreadable.iter()))
            .filter(|(n, unreadable)| n.len() >= MIN_DISTINCTIVE_NEEDLE && **unreadable)
            .count();
        let unreadable_removed = u32::try_from(unreadable).unwrap_or(u32::MAX);
        let text_fields = match text {
            Some(tp) => {
                let strings: Vec<String> = needles
                    .iter()
                    .zip(&unicode)
                    .map(|(n, u)| u.clone().unwrap_or_else(|| decode_text(n).0.into_iter().collect()))
                    .collect();
                Some(doc.redact_text_fields(&strings, tp, cancel)?)
            }
            None => None,
        };
        let bytes = doc.save_full(cancel)?;
        let proof = verify_payload_absent_unicode(&bytes, &needles, &unicode, &doc.limits, cancel)?;
        // Fresh re-open of the saved bytes: the region test must now select nothing.
        let mut saved = PdfDoc::open(&bytes, &doc.limits, cancel)?;
        let mut residual = 0u32;
        for region in regions {
            let again = saved.redact_region(region, UnsupportedPolicy::ReportAndContinue, cancel)?;
            residual = residual
                .saturating_add(again.text_strings_removed)
                .saturating_add(u32::try_from(again.annotations_removed.len()).unwrap_or(u32::MAX))
                .saturating_add(u32::try_from(again.unredacted.len()).unwrap_or(u32::MAX));
        }
        Ok(RedactedOutput { bytes, reports, proof, short_needles, residual_in_region: residual, text_fields, unreadable_removed })
    })();
    if result.is_err() {
        doc.objects = backup;
    }
    result
}

/// Selective sanitization classes applied by [`sanitize_full_rewrite`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SanitizePolicy {
    pub drop_info_metadata: bool,
    pub drop_xmp: bool,
    pub drop_embedded_files: bool,
    pub drop_javascript: bool,
    /// Page and form `/Thumb` thumbnail images.
    pub drop_thumbnails: bool,
    /// Application private data: `/PieceInfo` on the catalog, pages and forms.
    pub drop_private_data: bool,
}

impl SanitizePolicy {
    pub const ALL: Self = Self {
        drop_info_metadata: true,
        drop_xmp: true,
        drop_embedded_files: true,
        drop_javascript: true,
        drop_thumbnails: true,
        drop_private_data: true,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizeReport {
    /// `(class, count)` for every class that removed something.
    pub removed: Vec<(String, u32)>,
}

fn visit_dicts(o: &mut Object, f: &mut dyn FnMut(&mut Dict), depth: usize) {
    if depth > 64 {
        return;
    }
    let next = depth.saturating_add(1);
    match o {
        Object::Dict(d) => {
            f(d);
            for (_, v) in &mut d.entries {
                visit_dicts(v, f, next);
            }
        }
        Object::Stream(s) => {
            f(&mut s.dict);
            for (_, v) in &mut s.dict.entries {
                visit_dicts(v, f, next);
            }
        }
        Object::Array(a) => a.iter_mut().for_each(|v| visit_dicts(v, f, next)),
        _ => {}
    }
}

/// Full rewrite with the selected payload classes removed. The output holds only objects
/// reachable from the trailer after the removals (fresh file, no incremental tail, no orphans).
pub fn sanitize_full_rewrite(
    doc: &PdfDoc,
    policy: &SanitizePolicy,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, SanitizeReport)> {
    doc.guard_mutation(cancel)?;
    let mut staged = doc.objects.clone();
    let mut trailer = doc.trailer.clone();
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let bump = |counts: &mut BTreeMap<&'static str, u32>, class: &'static str| {
        let c = counts.entry(class).or_default();
        *c = c.saturating_add(1);
    };
    if policy.drop_info_metadata && trailer.remove("Info").is_some() {
        bump(&mut counts, "info_metadata");
    }
    let js_nums: BTreeSet<u32> = staged
        .iter()
        .filter(|(_, e)| e.obj.as_dict().and_then(|d| d.get("S")).is_some_and(|s| s.is_name("JavaScript")))
        .map(|(n, _)| *n)
        .collect();
    let is_js = |o: &Object| match o {
        Object::Ref(r) => js_nums.contains(&r.num),
        Object::Dict(d) => d.get("S").is_some_and(|s| s.is_name("JavaScript")),
        _ => false,
    };
    for entry in staged.values_mut() {
        let mut touched = false;
        visit_dicts(
            &mut entry.obj,
            &mut |d: &mut Dict| {
                let mut hit = |key: &str, class: &'static str, d: &mut Dict| {
                    if d.remove(key).is_some() {
                        bump(&mut counts, class);
                        touched = true;
                    }
                };
                if policy.drop_xmp {
                    hit("Metadata", "xmp", d);
                }
                if policy.drop_thumbnails {
                    hit("Thumb", "thumbnails", d);
                }
                if policy.drop_private_data {
                    hit("PieceInfo", "private_data", d);
                }
                if policy.drop_embedded_files {
                    hit("EmbeddedFiles", "embedded_files", d);
                    hit("AF", "embedded_files", d);
                    if d.get("Subtype").is_some_and(|s| s.is_name("FileAttachment")) {
                        hit("FS", "embedded_files", d);
                    }
                }
                if policy.drop_javascript {
                    hit("JavaScript", "javascript", d);
                    hit("AA", "javascript", d);
                    for key in ["A", "OpenAction"] {
                        if d.get(key).is_some_and(is_js) {
                            hit(key, "javascript", d);
                        }
                    }
                }
            },
            0,
        );
        if touched {
            entry.span = None;
        }
    }
    let bytes = write_document(&doc.version, &staged, &doc.source, &trailer, &doc.limits, cancel)?;
    Ok((bytes, SanitizeReport { removed: counts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() }))
}
