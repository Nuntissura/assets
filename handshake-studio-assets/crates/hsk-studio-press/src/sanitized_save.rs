//! Sanitized full save and the independent "full reachable payload" proof.
//!
//! `verify_payload_absent` re-opens the OUTPUT bytes with the bounded reader (it shares no state
//! with the redaction code) and looks for the removed strings in: every raw byte of the file, every
//! decoded stream (Flate/LZW/ASCIIHex/ASCII85/RunLength, object streams included), every PDF
//! string/name object (hex and escaped spellings compare by decoded value), every string operand of
//! decoded content streams, plus the UTF-16BE spelling of PDFDocEncoding-readable needles, and the
//! text that only a font's `/ToUnicode` map reveals. It also reports incremental sections, objects
//! present in the cross-reference table but unreachable from the trailer, and `endobj` markers with
//! no cross-reference entry (orphans an exporter left behind).
//! A stream whose filter is not implemented is scanned as raw bytes only and listed in
//! `streams_not_decodable`, which withholds `is_clean`/`is_proven` (test:
//! `undecodable_stream_never_reads_as_proven`). The one exception is the narrow raster class (see
//! `raster`): image XObjects and inline images whose chain ends in a raster codec are listed in
//! `raster_streams`, scanned after their decodable prefix, and do not block (test:
//! `raster_class_is_listed_and_region_images_are_removed`).
#![deny(clippy::arithmetic_side_effects)]
use crate::content::parse_content;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::raster::{InlineClass, RASTER_CLASS, classify_inline, decode_prefix, raster_tail};
use crate::reader::{check_cancel, find, parse_document};
use crate::redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
use crate::show_text::{FontCache, content_resources, show_text_ops};
use crate::syntax::{Dict, Object};
use crate::text_codec::{Enc, decode_text, pdfdoc_byte, utf16be_bytes};
use crate::text_fields::{TextFieldPolicy, TextFieldReport, appearance_stream_nums};
use crate::writer::{reachable, trailer_roots, write_document};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

const MAX_HITS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadLocation {
    RawBytes { offset: usize },
    DecodedStream { object: u32, offset: usize },
    StringObject { object: u32 },
    ContentString { object: u32 },
    /// Text drawn in an annotation appearance that only the font's `/ToUnicode` map reveals.
    DecodedText { object: u32 },
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
    /// A text-needle search was requested and some content draws text with a composite font that
    /// has no usable `/ToUnicode` (`opaque_text_streams`): that text was not checked.
    OpaqueText,
    /// A redaction region still holds a mark that was not removed (an image without
    /// `RemoveImages`, a vector, a deep form).
    RegionIncomplete,
    /// A fresh redaction pass over the saved bytes still selects something in a region.
    ResidualInRegion,
    /// The text-field report kept matches by policy or could not verify an appearance.
    TextFieldsKept,
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
    /// Content streams that draw text with a composite font having no usable `/ToUnicode`: that
    /// text is invisible to this proof. Blocks `is_clean` when `needles_searched > 0` (reason
    /// `NotProven::OpaqueText`).
    pub opaque_text_streams: Vec<u32>,
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
    /// (`raster_streams`) do not block. Unreadable composite text blocks when a needle search ran.
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
    /// The reasons that do not depend on what was found: structure, undecodable and opaque text.
    fn structural_reasons(&self) -> Vec<NotProven> {
        let mut v = Vec::new();
        if self.xref_sections > 1 || !self.unreachable_objects.is_empty() || self.unlisted_object_markers != 0 {
            v.push(NotProven::Structure);
        }
        if !self.fully_decoded() {
            v.push(NotProven::UndecodableStream);
        }
        if self.opaque_text_blocks() {
            v.push(NotProven::OpaqueText);
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
}

/// Raw-byte needles: the bytes as given, plus (for PDFDocEncoding-readable needles) their
/// UTF-16BE spelling, which is how outline titles and annotation text are usually stored.
/// `unicode[i]`, when present, is the text of needle `i` as its font's `/ToUnicode` reads it.
fn variants_unicode(needles: &[Vec<u8>], unicode: &[Option<String>]) -> Vec<Variant> {
    let mut out = Vec::new();
    for (i, n) in needles.iter().enumerate() {
        if n.is_empty() {
            continue;
        }
        let (chars, enc) = decode_text(n);
        let text = unicode
            .get(i)
            .cloned()
            .flatten()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| (enc == Enc::PdfDoc).then(|| chars.iter().collect::<String>()));
        out.push(Variant { needle_index: i, bytes: n.clone(), token: true, text });
        if enc == Enc::PdfDoc {
            let u = utf16be_bytes(&chars);
            if u != *n {
                out.push(Variant { needle_index: i, bytes: u, token: false, text: None });
            }
        }
    }
    out
}

fn variants(needles: &[Vec<u8>]) -> Vec<Variant> {
    variants_unicode(needles, &[])
}

/// Unicode needles: ASCII needles by bytes and UTF-16BE; other needles by PDFDocEncoding (when
/// every char has a code), UTF-8 and UTF-16BE spellings. Hit indices refer to `needles`.
pub(crate) fn variants_text(needles: &[String]) -> Vec<Variant> {
    let mut out = Vec::new();
    for (i, s) in needles.iter().enumerate() {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        let chars: Vec<char> = t.chars().collect();
        let text = Some(t.to_owned());
        if t.is_ascii() {
            out.push(Variant { needle_index: i, bytes: t.as_bytes().to_vec(), token: true, text });
        } else {
            if let Some(b) = chars.iter().map(|c| pdfdoc_byte(*c)).collect::<Option<Vec<u8>>>() {
                out.push(Variant { needle_index: i, bytes: b, token: true, text: None });
            }
            out.push(Variant { needle_index: i, bytes: t.as_bytes().to_vec(), token: true, text });
        }
        out.push(Variant { needle_index: i, bytes: utf16be_bytes(&chars), token: false, text: None });
    }
    out
}

/// Whole-token rule: a needle that starts (ends) with an ASCII letter or digit only matches when
/// the byte before (after) is not an ASCII letter or digit. "memory" inside the identifier
/// `cite.kang2025memoryos` is a different token; "the budget plan memo" contains the token
/// "budget plan". UTF-16BE spellings match strictly.
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

fn scan(hay: &[u8], vars: &[Variant]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for v in vars {
        let mut from = 0usize;
        while let Some(at) = find(hay.get(from..).unwrap_or(&[]), &v.bytes) {
            let pos = from.saturating_add(at);
            if at_boundary(hay, pos, v) {
                hits.push((v.needle_index, pos));
            }
            from = pos.saturating_add(1);
            if hits.len() >= MAX_HITS {
                return hits;
            }
        }
    }
    hits
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

fn looks_like_content(d: &Dict) -> bool {
    let sub_ok = match d.get("Subtype") {
        None => true,
        Some(s) => s.is_name("Form"),
    };
    let type_ok = !matches!(d.get("Type").and_then(Object::as_name), Some(b"XRef" | b"ObjStm" | b"Metadata" | b"EmbeddedFile"));
    sub_ok && type_ok
}

/// Scans the saved bytes for `needles` (see module docs). Needles are raw string bytes; empty
/// needles are ignored.
///
/// Context rule (what makes the proof usable for natural-language documents without weakening
/// it): in decoded page/form content streams a needle that appears as (part of) a TEXT-SHOWING
/// operand (Tj, TJ, ', ") is a kept-text collision, reported in `kept_text_collisions`, because
/// the same word legitimately occurs in kept text (the in-region instance is proven gone by the
/// residual pass in [`redact_and_save`]). A needle anywhere else is a leak and lands in `hits`:
/// raw bytes outside decoded streams (headers, object text, xref, orphan bodies), string/name
/// objects (metadata, annotation contents, outlines, ActualText), non-show content operands
/// (marked-content properties), inline-image data, comments, every decoded non-content stream
/// (XMP, embedded files, thumbnails, private data, object streams), and all text of annotation
/// appearance streams.
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

pub(crate) fn verify_with_variants(output: &[u8], vars: Vec<Variant>, limits: &Limits, cancel: &CancellationToken) -> Result<PayloadProof> {
    let parsed = parse_document(output, limits, cancel)?;
    let mut proof = PayloadProof {
        raw_bytes_scanned: output.len(),
        objects_checked: 0,
        streams_decoded: 0,
        streams_not_decodable: Vec::new(),
        raster_streams: Vec::new(),
        opaque_text_streams: Vec::new(),
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
    let raw_hits = scan(output, &vars);
    let live = reachable(&parsed.objects, &trailer_roots(&parsed.trailer));
    proof.unreachable_objects = parsed.objects.keys().copied().filter(|n| !live.contains(n)).collect();
    let markers = output.windows(6).filter(|w| *w == b"endobj").count();
    let listed = parsed.objects.values().filter(|e| e.span.is_some()).count();
    proof.unlisted_object_markers = u32::try_from(markers.saturating_sub(listed)).unwrap_or(u32::MAX);

    // Annotation appearance streams hold annotation text, not kept page text: a needle in their
    // show operands is a leak (hit), not a kept-text collision.
    let ap_streams = appearance_stream_nums(&parsed.objects);
    // Text needles for ToUnicode-read text, and the resources that apply to each content stream.
    let texts: Vec<(usize, Vec<char>, bool)> =
        vars.iter().filter_map(|v| v.text.as_ref().map(|t| (v.needle_index, t.chars().collect(), v.token))).collect();
    let resources = content_resources(&parsed.objects);
    let mut fonts = FontCache::default();
    // Raw spans of streams that decode successfully: their bytes are judged in decoded context.
    let mut covered: Vec<(usize, usize)> = Vec::new();
    let mut collisions: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    let mut budget = limits.max_decoded_total;
    for (i, (num, entry)) in parsed.objects.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        proof.objects_checked = proof.objects_checked.saturating_add(1);
        let mut stack: Vec<&Object> = vec![&entry.obj];
        while let Some(o) = stack.pop() {
            match o {
                Object::Str(s) | Object::Name(s) => {
                    if let Some(needle) = contains(s, &vars) {
                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::StringObject { object: *num } });
                    }
                }
                Object::Array(a) => stack.extend(a.iter()),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| v)),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| v)),
                _ => {}
            }
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
        let decoded = if raster.is_some() { decode_prefix(s, limits, &mut budget) } else { filters::decode_stream(s, limits, &mut budget) };
        match decoded {
            Ok(bytes) => {
                match raster {
                    Some(filter) => proof.raster_streams.push(RasterStream { object: *num, class: RASTER_CLASS, filter, inline: false }),
                    None => proof.streams_decoded = proof.streams_decoded.saturating_add(1),
                }
                if let Some(span) = entry.span {
                    covered.push(span);
                }
                let ops = if looks_like_content(&s.dict) { parse_content(&bytes, limits).ok() } else { None };
                let is_show = |name: &[u8]| matches!(name, b"Tj" | b"TJ" | b"'" | b"\"");
                let is_ap = ap_streams.contains(num);
                if let Some(list) = &ops {
                    for op in list.iter().filter(|op| op.name == b"BI") {
                        match classify_inline(&op.operands) {
                            InlineClass::Searchable => {}
                            InlineClass::Raster(filter) => {
                                proof.raster_streams.push(RasterStream { object: *num, class: RASTER_CLASS, filter, inline: true })
                            }
                            InlineClass::Blocking(f) => proof.streams_not_decodable.push((*num, format!("inline:{f}"))),
                        }
                    }
                    let shown = show_text_ops(&parsed.objects, resources.get(num).copied(), list, limits, &mut budget, &mut fonts)?;
                    if shown.opaque {
                        proof.opaque_text_streams.push(*num);
                    }
                    if !shown.mapped.is_empty() {
                        for (needle, text, token) in &texts {
                            if !text_found(&shown.mapped, text, *token) {
                                continue;
                            }
                            if is_ap {
                                proof.hits.push(PayloadHit { needle: *needle, location: PayloadLocation::DecodedText { object: *num } });
                            } else {
                                let c = collisions.entry((*needle, *num)).or_default();
                                *c = c.saturating_add(1);
                            }
                        }
                    }
                }
                let show_spans: Vec<(usize, usize)> = if is_ap {
                    Vec::new()
                } else {
                    ops.iter().flatten().filter(|op| is_show(&op.name)).map(|op| (op.start, op.end)).collect()
                };
                for (needle, offset) in scan(&bytes, &vars) {
                    if !show_spans.iter().any(|(a, b)| offset >= *a && offset < *b) {
                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedStream { object: *num, offset } });
                    }
                }
                for op in ops.iter().flatten() {
                    let show = is_show(&op.name);
                    let mut st: Vec<&Object> = op.operands.iter().collect();
                    while let Some(o) = st.pop() {
                        match o {
                            Object::Str(x) => {
                                if let Some(needle) = contains(x, &vars) {
                                    if show && !is_ap {
                                        let c = collisions.entry((needle, *num)).or_default();
                                        *c = c.saturating_add(1);
                                    } else {
                                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::ContentString { object: *num } });
                                    }
                                }
                            }
                            Object::Array(a) => st.extend(a.iter()),
                            Object::Dict(d) => st.extend(d.entries.iter().map(|(_, v)| v)),
                            _ => {}
                        }
                    }
                }
            }
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => proof.streams_not_decodable.push((*num, "decode_error".into())),
        }
        if proof.hits.len() > MAX_HITS {
            proof.hits.truncate(MAX_HITS);
            break;
        }
    }
    for (needle, offset) in raw_hits {
        if !covered.iter().any(|(a, b)| offset >= *a && offset < *b) {
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
}

impl RedactedOutput {
    /// Every region fully removed (nothing left in `unredacted`), the saved bytes pass the payload
    /// proof for every distinctive removed string, and a fresh pass over the saved bytes finds
    /// nothing left in the regions.
    pub fn is_proven(&self) -> bool {
        self.not_proven_reasons().is_empty()
    }

    /// Why this result does not read as proven (empty = proven). Test:
    /// `opaque_composite_text_blocks_the_proof`.
    pub fn not_proven_reasons(&self) -> Vec<NotProven> {
        let mut v = self.region_reasons();
        if !self.proof.hits.is_empty() {
            v.push(NotProven::Leak);
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
    /// `proof.streams_not_decodable` names the blockers; raster-class streams do not block; unreadable
    /// composite text blocks when a needle search ran, `NotProven::OpaqueText`). Says
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
        Ok(RedactedOutput { bytes, reports, proof, short_needles, residual_in_region: residual, text_fields })
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
