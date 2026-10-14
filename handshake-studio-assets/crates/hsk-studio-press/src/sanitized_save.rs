//! Sanitized full save and the independent "full reachable payload" proof.
//!
//! NARROWED CLAIM (Operator decision D27, "Flatten + narrow"): a redaction clears its regions by
//! flattening (`flatten.rs`: every page carrying a region becomes its raster); the payload proof below
//! is the SECOND check, verified against Studio's own strict PDF reading: it reads the saved file as
//! this crate's reader does and says nothing about a viewer that reads the same bytes differently.
//! Anything outside the supported profile (DESCRIPTOR `proof.supported_profile`) is NotProven by
//! default; known viewer-differential classes are listed in DESCRIPTOR `proof.limitations` with their
//! pre-review ids (test `descriptor_states_the_narrowed_claim_and_its_limitations`).
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
//! - Text that only a font reading reveals, read by the determination pass (`show_text`, lead
//!   decision 2026-10-11T13:10:26Z): fonts follow the graphics state (`q`/`Q`, `gs`, `Tf`, across a
//!   page's content streams and into forms drawn by `Do`, with inherited resources); content the walk
//!   cannot follow is `NotProven::UnreadableContent` (tests `font_state_follows_q_q_gs_and_do`,
//!   `unreadable_or_stray_content_never_reads_clean`). The walk covers the object table, not only
//!   the page tree: page objects outside the tree (template pages, pages reached only through a
//!   destination) are read with their resources, Type 3 glyph procedures of every Type 3 font
//!   (direct dictionaries included) with the font's, and every other stream that tokenizes as content
//!   with none, whatever its dictionary keys (R8 F1, R9-P1; tests
//!   `partial_tounicode_and_unreachable_pages_are_read_or_opaque`, `direct_type3_glyph_procedures_are_read`).
//!   Every text-showing operator is recorded as positively determined or not (font, writing mode,
//!   reading; `PayloadProof::undetermined_text`): an undetermined one is `NotProven::OpaqueText` or
//!   `NotProven::UnresolvedWritingMode`, and on a redaction's page an operator whose glyph geometry is
//!   not computed makes the region incomplete (`RedactedOutput::region_undetermined`).
//! - Needles are matched in the text each root draws in drawing order: the readings of all its
//!   strings (across operators, text objects and the forms it draws) joined, and spaced (a space
//!   between strings, so kerning used as the word space reads as one), every reading kind (font text,
//!   `/ToUnicode`, bytes); the whole-token rule exempts only a match inside ONE string glued to a letter
//!   of that string, never a match that spans strings (R9-F3; tests `split_runs_are_read_whole`,
//!   `kerning_word_spaces_are_read`). Every string and name, and the text of EVERY stream that is not
//!   only kept page text (XMP and plain ASCII streams included, R10-F7), is also matched as text. All
//!   text is normalised first (`norm.rs`: language escapes, characters that draw nothing, NFKC
//!   compatibility forms, right-to-left overrides; R9-P2, R10-P1/P2; tests
//!   `language_escapes_and_invisible_characters_never_hide_text`,
//!   `zero_width_and_compatibility_characters_never_hide_text`, `ascii_streams_are_matched_as_text`).
//! - Canonical content (lead decision 2026-10-11T15:00:20Z): every content stream a viewer may interpret
//!   (every stream the walk reaches by a content role) is checked to be exactly the canonical
//!   serialisation of its own strict parse (`canon::is_canonical`); `PayloadProof::non_canonical_streams`
//!   lists the others. A redaction (`redact_and_save`) rewrites every content stream canonically before
//!   it saves, and its result is proven only when the saved file holds no non-canonical content
//!   (`NotProven::NonCanonicalContent`), so the operators the proof read are the operators a viewer
//!   reads.
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
use crate::canon::{canonicalize, is_canonical};
use crate::content::{Op, inline_dict};
use crate::error::{Limits, PressError, RedactionClass, Result, TextCause};
use crate::filters;
use crate::flatten::{FlattenedPage, check_flattened, flatten_region_pages};
use crate::flatten_purge::{Stripped, check_document, drop_painted_sources, neutralize_structure, page_annotations, remove_annotations, strip_structure, strip_xfa};
use crate::font::Font;
use crate::norm::{Escapes, normalize, normalized, normalized_str, visual_order};
use crate::pdf_edit::PdfDoc;
use crate::raster::{InlineClass, RASTER_CLASS, classify_inline, codec_signature, decode_prefix, hidden_zlib, raster_tail};
use crate::reader::{check_cancel, find, parse_document};
use crate::redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
use crate::show_text::{FontCache, FontState, KINDS, Role, SEP, Walker, content_role_streams, fonts_in, is_show, pages, pages_outside_tree, show_strings};
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
    /// In a decoded stream: at `offset` of its bytes, or (offset 0) in the stream read as text.
    DecodedStream { object: u32, offset: usize },
    StringObject { object: u32 },
    ContentString { object: u32 },
    /// Text drawn in an annotation appearance or stray content (including page objects outside the
    /// page tree), read through its fonts in drawing order (`object`: the stream of the first string
    /// of the match).
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
/// "some text could not be checked". Every reason is judged by Studio's own strict PDF reading (module
/// docs, narrowed claim): proven means "verified against that reading", not "no viewer can show it"; a
/// redaction's regions are cleared by flattening, not by this check.
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
    /// The independent checks of the saved bytes found a mismatch (`RedactedOutput::residual_in_region`:
    /// `flatten::check_flattened`, `flatten_purge::check_document`), a structure tree or structure-element dictionary remaining in the saved document included.
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
    /// Some text is drawn with a composite font whose writing mode is not positively known
    /// (`PayloadProof::undetermined_text` with [`TextCause::UnresolvedWritingMode`]; lead decision
    /// 2026-10-11T13:10:26Z, R9-F1; test `unknown_cmap_names_are_unresolved_writing_modes`).
    UnresolvedWritingMode,
    /// A redaction's saved file holds a content stream that is not the canonical serialisation of its
    /// own parse (`PayloadProof::non_canonical_streams`): a viewer may read other operators from it
    /// than the proof did (lead decision 2026-10-11T15:00:20Z; test `canonical_rewrite_keeps_only_written_content`).
    NonCanonicalContent,
    /// An image, form, pattern or shading a flattened page reached is still in the saved file and is not silent
    /// (R18, the minimal draw set: silent only when it is an image XObject stream without an `/OC` that a kept page
    /// draws with `Do` directly in its own content stream(s), each named by exactly one reference of the file (R19),
    /// outside `BDC /OC` marked content, resolved through the page's own nearest resource entry with a dictionary, not a
    /// stream, at every step): some content's usage could not be determined, a kept object only references it or
    /// places it where a viewer draws nothing from it (a
    /// catalog `/AF`, a `/Names` or `/Dests` tree, an entry of a kept page's `/Resources` outside `/XObject`
    /// `/Pattern` `/Shading` `/ExtGState` `/Font`, a plain key of an object a kept page draws, an object used as a
    /// `/Resources` value or as a gated sub-dictionary, a name an operator uses for an object of another kind, an
    /// entry a nearer entry of the same name shadows, any other plain reference), or a viewer draws it through
    /// anything but that direct `Do` (a form, tiling pattern, Type 3 glyph procedure, soft-mask group, image mask,
    /// shading or annotation appearance, a page whose content streams are named elsewhere, R19; over-reporting is
    /// accepted); a font or graphics state is not a painted source and is not reported (R19);
    /// `RedactedOutput::painted_sources_kept`; R13-F6, R14-P1, R16, R17, R18, R19; tests
    /// `painted_sources_under_a_region_do_not_survive`,
    /// `painted_source_drawn_directly_by_a_kept_page_is_not_reported`,
    /// `painted_source_referenced_by_a_kept_catalog_structure_is_reported`,
    /// `painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported`,
    /// `painted_source_referenced_by_a_key_of_a_drawn_object_is_reported`,
    /// `painted_source_placed_as_a_resources_value_is_reported`,
    /// `painted_source_placed_as_a_gated_sub_dictionary_is_reported`,
    /// `painted_source_placed_as_an_appearance_state_is_reported`,
    /// `painted_source_named_where_it_is_not_drawn_is_reported`,
    /// `painted_source_shadowed_by_a_nearer_resource_entry_is_reported`,
    /// `painted_source_in_an_annotation_appearance_is_reported`,
    /// `painted_source_under_optional_content_is_reported`,
    /// `painted_source_named_through_a_stream_used_as_a_dictionary_is_reported`,
    /// `painted_source_drawn_by_an_image_or_shading_stream_read_as_content_is_reported`,
    /// `painted_source_selected_by_a_type3_font_pattern_or_soft_mask_is_reported`,
    /// `painted_source_drawn_by_anything_but_a_kept_pages_own_content_is_reported`,
    /// `painted_sources_rendered_with_a_drawn_image_are_reported`,
    /// `painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere`,
    /// `a_font_or_graphics_state_that_stays_is_not_a_painted_source`). Residual (PLR-084): the silence of an image
    /// that is not reported is syntactic: a kept page's own content stream draws it directly with `Do`, and the
    /// visibility of that draw (a zero-scale `cm`, an empty clip, an alpha of 0, a later object painted over it) is not
    /// checked (test `silent_painted_source_visibility_is_not_checked`).
    PaintedSourceKept,
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
            Self::UnresolvedWritingMode => "unresolved_writing_mode",
            Self::NonCanonicalContent => "non_canonical_content",
            Self::PaintedSourceKept => "painted_source_kept",
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
    /// Blocks `is_clean` when `needles_searched > 0` (reason `NotProven::OpaqueText`). Since the
    /// positive determination (lead decision 2026-10-11T13:10:26Z) it also lists text with no font
    /// selected and text whose font is not resolved in any role (an appearance included).
    pub opaque_text_streams: Vec<u32>,
    /// `(content stream, cause)` of every text-showing operator that was not positively determined
    /// (sorted, unique): no font, an unresolved font, an unresolved writing mode, an unreadable code.
    /// Font and code causes are `opaque_text_streams` (`NotProven::OpaqueText`), a writing-mode cause
    /// is `NotProven::UnresolvedWritingMode` (tests `undetermined_fonts_never_read_as_cleared`,
    /// `unknown_cmap_names_are_unresolved_writing_modes`).
    pub undetermined_text: Vec<(u32, TextCause)>,
    /// `(object, first filter)` of streams with non-whitespace input after a filter's end-of-data
    /// marker (`inline:<filter>` for inline images, `hidden_zlib` behind a raster codec name).
    /// Blocks `is_clean` (reason `NotProven::TrailingData`).
    pub trailing_data_streams: Vec<(u32, String)>,
    /// `(object, reason)` of content the walk could not follow (it does not parse completely with the
    /// strict parser: an unknown operator, an extra or missing operand, an inline image whose end is not
    /// certain, page content streams a byte-for-byte join would read differently, ...). Blocks
    /// `is_clean` when `needles_searched > 0` (reason `NotProven::UnreadableContent`).
    pub unreadable_content_streams: Vec<(u32, String)>,
    /// Content streams a viewer may interpret (reached by a content role) whose decoded bytes are not
    /// the canonical serialisation of their own parse (`canon.rs`; inline images included). A redaction
    /// result is proven only when this is empty (`NotProven::NonCanonicalContent`); a plain payload check
    /// of a file this crate did not write reports it without blocking.
    pub non_canonical_streams: Vec<u32>,
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
        let mut v = self.structural_reasons(self.needles_searched > 0);
        if !self.hits.is_empty() {
            v.insert(0, NotProven::Leak);
        }
        v
    }
    /// The reasons that do not depend on what was found: structure, undecodable, trailing data, and
    /// (when `determination` applies) opaque text, an unresolved writing mode and unreadable content.
    /// A plain payload check applies the determination when a needle search ran; a redaction always
    /// does (lead decision 2026-10-11T13:10:26Z: every text-showing operator of the saved file must be
    /// determined, whatever was removed; test `opaque_composite_text_blocks_the_proof`).
    fn structural_reasons(&self, determination: bool) -> Vec<NotProven> {
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
        if determination && !self.opaque_text_streams.is_empty() {
            v.push(NotProven::OpaqueText);
        }
        if determination && !self.unreadable_content_streams.is_empty() {
            v.push(NotProven::UnreadableContent);
        }
        if determination && self.undetermined_text.iter().any(|(_, c)| *c == TextCause::UnresolvedWritingMode) {
            v.push(NotProven::UnresolvedWritingMode);
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
/// present, is the text of needle `i` as its font draws it (`RedactionReport::removed_unicode`). Both
/// that text and the bytes read as PDFDocEncoding text are searched, in every spelling and as
/// normalised text: a `/Differences` or base-encoding remap of a quote, dash or space draws other
/// characters than the bytes spell (R10-D1; test `remapped_punctuation_is_searched_as_drawn`).
fn variants_unicode(needles: &[Vec<u8>], unicode: &[Option<String>]) -> Vec<Variant> {
    let mut out = Vec::new();
    for (i, n) in needles.iter().enumerate() {
        if n.is_empty() {
            continue;
        }
        let (chars, enc) = decode_text(n);
        let mapped = unicode.get(i).cloned().flatten().filter(|t| !t.trim().is_empty());
        let byte_text = (enc == Enc::PdfDoc).then(|| chars.iter().collect::<String>()).filter(|t| !t.trim().is_empty());
        match &mapped {
            Some(t) => {
                push_spellings(&mut out, i, n, &t.chars().collect::<Vec<_>>(), Some(t.clone()));
                if let Some(b) = byte_text.filter(|b| b != t) {
                    let pdfdoc: Vec<char> = b.chars().collect();
                    let spelled = pdfdoc.iter().map(|c| crate::text_codec::pdfdoc_byte(*c)).collect::<Option<Vec<u8>>>().unwrap_or_else(|| n.clone());
                    out.push(Variant { needle_index: i, bytes: Vec::new(), token: true, text: Some(b), hex: false });
                    push_spellings(&mut out, i, &spelled, &pdfdoc, None);
                }
            }
            None => {
                let spelled: Vec<char> = if enc == Enc::PdfDoc { chars } else { Vec::new() };
                push_spellings(&mut out, i, n, &spelled, byte_text);
            }
        }
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

/// A needle's text, normalised (`norm.rs`), for matching drawn text, text strings and text streams.
struct Needle {
    index: usize,
    chars: Vec<char>,
    token: bool,
}

/// The needles, indexed by their first character (a text is searched once for all of them).
struct Needles {
    list: Vec<Needle>,
    by_first: BTreeMap<char, Vec<usize>>,
}

impl Needles {
    fn new(list: Vec<Needle>) -> Self {
        let mut by_first: BTreeMap<char, Vec<usize>> = BTreeMap::new();
        for (i, n) in list.iter().enumerate() {
            if let Some(c) = n.chars.first() {
                by_first.entry(*c).or_default().push(i);
            }
        }
        Self { list, by_first }
    }

    fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Occurrences `(needle, start, end, exempt)` in `hay` (normalised), where `slot[i]` names the drawn
    /// string (or text string) character `i` belongs to ([`SEP`]: a separator); `needle` indexes
    /// `list`. Matching is by substring. The whole-token rule may only EXEMPT a match, and only where
    /// the drawn reading proves a different token: the whole match lies in one string, an ASCII letter
    /// or digit of that SAME string touches a needle edge that is one too, and that string's glyphs are
    /// drawn next to each other (`adjacent(slot)`: `show_text::StrRead::adjacent`, R10-P4; a glued
    /// letter a hostile width places far away proves nothing). A match that spans strings (a
    /// concatenated reading) is never exempt (lead decision 2026-10-11T13:10:26Z; tests
    /// `split_runs_are_read_whole`, `token_exemption_needs_drawn_adjacency`).
    fn matches(&self, hay: &[char], slot: &[u32], adjacent: &dyn Fn(u32) -> bool) -> Vec<(usize, usize, usize, bool)> {
        let mut out = Vec::new();
        for (i, c) in hay.iter().enumerate() {
            let Some(candidates) = self.by_first.get(c) else { continue };
            for k in candidates {
                let Some(n) = self.list.get(*k) else { continue };
                let (Some(first), Some(last)) = (n.chars.first(), n.chars.last()) else { continue };
                let end = i.saturating_add(n.chars.len());
                if hay.get(i..end) != Some(n.chars.as_slice()) {
                    continue;
                }
                let s0 = slot.get(i).copied().unwrap_or(SEP);
                let single = s0 != SEP && slot.get(i..end).is_some_and(|s| s.iter().all(|x| *x == s0));
                let glued = |at: Option<usize>, edge: &char| {
                    at.is_some_and(|p| slot.get(p) == Some(&s0) && hay.get(p).is_some_and(char::is_ascii_alphanumeric)) && edge.is_ascii_alphanumeric()
                };
                let exempt = n.token && single && adjacent(s0) && (glued(i.checked_sub(1), first) || glued(Some(end), last));
                out.push((*k, i, end, exempt));
            }
        }
        out
    }
}

/// Needles found (not exempt) in a text string or name read as text, both with its language escapes
/// dropped (as a viewer shows it) and with only the `ESC` characters dropped, and in the order a bidi
/// viewer shows it when it holds a right-to-left override (`norm::visual_order`, R10-P2).
fn string_text_hits(text: &[char], needles: &Needles, escape_forms: &[Escapes]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    if needles.is_empty() {
        return out;
    }
    let visual = visual_order(text);
    for form in std::iter::once(text).chain(visual.as_deref()) {
        for esc in escape_forms {
            let hay = normalized(form, *esc);
            let slot = vec![0u32; hay.len()];
            for (k, _, _, exempt) in needles.matches(&hay, &slot, &|_| true) {
                if !exempt && let Some(n) = needles.list.get(k) {
                    out.insert(n.index);
                }
            }
        }
    }
    out
}

/// Needles found (not exempt) in a decoded stream read as text (UTF-16 with a byte-order mark, else
/// UTF-8), normalised: EVERY stream, plain printable ASCII included (R10-F7: a needle whose text
/// differs from its normalised form, a ligature or a no-break space, is not found by byte spellings in
/// an ASCII XMP packet; test `ascii_streams_are_matched_as_text`), also in bidi visual order when the
/// text holds a right-to-left override (R10-P2).
fn stream_text_hits(bytes: &[u8], needles: &Needles) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    if needles.is_empty() {
        return out;
    }
    let text: String = if bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE]) {
        decode_text(bytes).0.into_iter().collect()
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).into_owned()
    };
    let visual: Option<String> = if text.contains('\u{202E}') { visual_order(&text.chars().collect::<Vec<_>>()).map(|v| v.into_iter().collect()) } else { None };
    let forms: &[Escapes] = if text.contains('\u{1B}') { &[Escapes::Chars, Escapes::Segments] } else { &[Escapes::Chars] };
    for (source, escapes) in std::iter::once(&text).chain(visual.as_ref()).flat_map(|t| forms.iter().map(move |e| (t, e))) {
        let hay = normalized_str(source, *escapes);
        for n in &needles.list {
            let pat: String = n.chars.iter().collect();
            let (first, last) = (n.chars.first().copied().unwrap_or(' '), n.chars.last().copied().unwrap_or(' '));
            let found = hay.match_indices(pat.as_str()).any(|(at, m)| {
                if !n.token {
                    return true;
                }
                let before = hay.get(..at).and_then(|h| h.chars().next_back()).is_some_and(|c| c.is_ascii_alphanumeric()) && first.is_ascii_alphanumeric();
                let after = hay.get(at.saturating_add(m.len())..).and_then(|h| h.chars().next()).is_some_and(|c| c.is_ascii_alphanumeric()) && last.is_ascii_alphanumeric();
                !(before || after)
            });
            if found {
                out.insert(n.index);
            }
        }
    }
    out
}

/// What the kept contexts of one text-showing operator draw: the needles a match lying wholly in this
/// operator's font readings carries, and per string whether its bytes are themselves drawn text.
#[derive(Default)]
struct KeptOp {
    drawn: BTreeSet<usize>,
    raw_drawn: Vec<bool>,
}

/// Needles a kept text-showing operator itself DRAWS: a non-hex spelling inside one of its string
/// values whose bytes are the drawn text (a simple font that reads the same letters as the bytes; no
/// token rule), or a font reading of the operator (encoding, `/ToUnicode`, its strings joined). Only
/// these may exempt a match inside the operator's bytes, so a comment or a name between operand and
/// operator never rides along, and bytes a font draws as other glyphs (`/Differences` remapping,
/// composite codes, an unresolved or unset font) are no kept text (R8 P1; test
/// `kept_text_needs_a_drawn_reading`).
fn shown_needles(op: &Op, kept: Option<&KeptOp>, vars: &[Variant]) -> BTreeSet<usize> {
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
    out.extend(k.drawn.iter().copied());
    out
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

/// Font-code spellings (raw and hex) of the needle texts under each of `fonts` that can encode
/// them (`font::Font::encode`: a simple font's encoding, a `/ToUnicode` code by value written in the
/// font's codespace; test `differences_encoded_text_is_found_proven_and_redacted` for the encoding side).
fn code_variants(
    fonts: &[Rc<Font>],
    texts: &[(usize, Vec<char>, bool)],
    vars: &[Variant],
) -> Vec<Variant> {
    let mut out: Vec<Variant> = Vec::new();
    for f in fonts {
        for (needle, text, _) in texts {
            for codes in f.encode(text).into_iter().filter(|c| !c.is_empty()) {
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
    verify_internal(output, vars, limits, cancel, &[]).map(|(proof, _)| proof)
}

/// Every text-showing operator drawn on one of `region_pages` (page objects of the saved file): the
/// page's content streams and every form they draw (any depth, by `Do`, as walked). The geometry
/// determination of those operators decides whether the region can be cleared.
fn page_classes(walker: &Walker<'_>, segments: &[u32]) -> BTreeSet<RedactionClass> {
    let mut closure: BTreeSet<u32> = BTreeSet::new();
    let mut work: Vec<u32> = segments.to_vec();
    while let Some(n) = work.pop() {
        if closure.insert(n)
            && let Some(forms) = walker.draws.get(&n)
        {
            work.extend(forms.iter().copied());
        }
    }
    walker
        .records
        .iter()
        .filter(|r| r.role == Role::Kept && r.stream.is_some_and(|s| closure.contains(&s)))
        .flat_map(|r| r.geometry())
        .collect()
}

/// [`verify_with_variants`] plus, for each page object in `region_pages`, the classes of text drawn
/// on it whose glyph geometry the redaction selector does not compute (`(page object, class)`).
fn verify_internal(
    output: &[u8],
    vars: Vec<Variant>,
    limits: &Limits,
    cancel: &CancellationToken,
    region_pages: &[u32],
) -> Result<(PayloadProof, Vec<(u32, RedactionClass)>)> {
    let parsed = parse_document(output, limits, cancel)?;
    let objs = &parsed.objects;
    let mut proof = PayloadProof {
        raw_bytes_scanned: output.len(),
        objects_checked: 0,
        streams_decoded: 0,
        streams_not_decodable: Vec::new(),
        raster_streams: Vec::new(),
        opaque_text_streams: Vec::new(),
        undetermined_text: Vec::new(),
        trailing_data_streams: Vec::new(),
        unreadable_content_streams: Vec::new(),
        non_canonical_streams: Vec::new(),
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
    // Text needles (normalised, norm.rs) for every text match: drawn text, text strings, text streams.
    let texts: Vec<(usize, Vec<char>, bool)> =
        vars.iter().filter_map(|v| v.text.as_ref().map(|t| (v.needle_index, t.chars().collect(), v.token))).collect();
    let needles = Needles::new(
        texts
            .iter()
            .map(|(index, chars, token)| Needle { index: *index, chars: normalized(chars, Escapes::Chars), token: *token })
            .filter(|n| !n.chars.is_empty())
            .collect(),
    );

    // 1. Object model: every string, name and dictionary key, by its spellings and as normalised
    // text (a text string with its language escapes dropped and with only ESC dropped; R9-P2).
    for (i, (num, entry)) in objs.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        proof.objects_checked = proof.objects_checked.saturating_add(1);
        let mut stack: Vec<&Object> = vec![&entry.obj];
        let hit = |s: &[u8], text: Vec<char>, escapes: &[Escapes], proof: &mut PayloadProof| {
            let mut found: BTreeSet<usize> = contains(s, &vars).into_iter().collect();
            found.extend(string_text_hits(&text, &needles, escapes));
            for needle in found {
                proof.hits.push(PayloadHit { needle, location: PayloadLocation::StringObject { object: *num } });
            }
        };
        while let Some(o) = stack.pop() {
            let dict = match o {
                Object::Str(s) => {
                    hit(s, decode_text(s).0, &[Escapes::Segments, Escapes::Chars], &mut proof);
                    continue;
                }
                Object::Name(s) => {
                    hit(s, String::from_utf8_lossy(s).chars().collect(), &[Escapes::Chars], &mut proof);
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
                hit(k, String::from_utf8_lossy(k).chars().collect(), &[Escapes::Chars], &mut proof);
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

    // 3. The determination pass over the object table (R8 F1, R9): pages of the page tree (kept
    // text), page objects outside it, appearances, content-role streams (Type 3 glyph procedures of
    // direct font dictionaries included, R9-P1), then EVERY other decoded stream that tokenizes as
    // content (stray, no resources; no dictionary key exempts a stream, R9-P1).
    let mut walker = Walker::new(objs, limits, budget, FontCache::default()).with_decoded(&decoded);
    let tree = pages(objs, &parsed.trailer);
    for (_, segments, resources) in &tree {
        check_cancel(cancel)?;
        walker.run_page(segments, *resources, Role::Kept)?;
    }
    let tree_pages: BTreeSet<u32> = tree.iter().map(|(n, _, _)| *n).collect();
    for (_, segments, resources) in pages_outside_tree(objs, &tree_pages) {
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
    for (n, role) in content_role_streams(objs) {
        if !walker.roles.contains_key(&n) && decoded.contains_key(&n) {
            walker.run_stream(n, role.resources, Role::Stray)?;
        }
    }
    for (i, num) in decoded.keys().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        if walker.roles.contains_key(num) || raster_objects.contains(num) {
            continue;
        }
        if matches!(objs.get(num).map(|e| &e.obj), Some(Object::Stream(_))) {
            walker.run_unreached(*num)?;
        }
    }
    check_cancel(cancel)?;

    // Determination record -> typed reasons: every operator not positively determined is listed
    // with its cause; unreadable text (no or unresolved font, unread code) is opaque.
    let mut opaque: BTreeSet<u32> = BTreeSet::new();
    let mut undetermined: BTreeSet<(u32, TextCause)> = BTreeSet::new();
    for rec in &walker.records {
        let Some(stream) = rec.stream else { continue };
        if rec.opaque() {
            opaque.insert(stream);
        }
        undetermined.extend(rec.causes().into_iter().map(|c| (stream, c)));
    }
    proof.opaque_text_streams = opaque.into_iter().collect();
    proof.undetermined_text = undetermined.into_iter().collect();
    proof.unreadable_content_streams = walker.unreadable.iter().map(|(n, why)| (*n, (*why).to_owned())).collect();
    // Canonical content: every stream a viewer may interpret (reached by a content role, not only by
    // the object-table scan) is the canonical serialisation of its own strict parse.
    for num in walker.roles.keys() {
        if walker.unreached.contains(num) || walker.unreadable.contains_key(num) {
            continue;
        }
        let canonical = match (decoded.get(num), walker.parsed_any(*num)) {
            (Some(data), Some(parsed)) => is_canonical(data, &parsed),
            _ => false,
        };
        if !canonical {
            proof.non_canonical_streams.push(*num);
        }
    }
    let mut region: Vec<(u32, RedactionClass)> = Vec::new();
    for page in region_pages {
        let segments: &[u32] = tree.iter().find(|(n, _, _)| n == page).map(|(_, s, _)| s.as_slice()).unwrap_or(&[]);
        region.extend(page_classes(&walker, segments).into_iter().map(|c| (*page, c)));
    }

    // Needles in the text each root draws (drawing order, joined and spaced, every reading kind;
    // walker.root_texts). Kept text is a collision; anything else a hit. In kept text only a font
    // reading of positively determined fonts draws text (never the bytes, never a guess for an
    // unresolved font), and only a match wholly inside one operator makes a needle that operator
    // draws (exemption of byte matches inside it). Collisions are counted once per operator.
    let mut collided: BTreeSet<(usize, u32, usize)> = BTreeSet::new();
    let mut kept_ops: BTreeMap<(u32, usize), KeptOp> = BTreeMap::new();
    let mut text_hits: BTreeSet<(usize, u32)> = BTreeSet::new();
    for rec in walker.records.iter().filter(|r| r.role == Role::Kept) {
        let Some(stream) = rec.stream else { continue };
        let k = kept_ops.entry((stream, rec.op)).or_default();
        if k.raw_drawn.len() < rec.strings.len() {
            k.raw_drawn.resize(rec.strings.len(), false);
        }
        for (slot, s) in k.raw_drawn.iter_mut().zip(&rec.strings) {
            *slot |= s.raw_drawn;
        }
    }
    let mut chars_left = limits.max_decoded_total;
    let roots = walker.root_texts(&mut chars_left)?;
    for rt in &roots {
        check_cancel(cancel)?;
        let mut matched: BTreeSet<(usize, usize)> = BTreeSet::new();
        // A reading with an ESC is matched with its ESC..ESC segments dropped too (as in a text string).
        let forms = rt.hays.iter().enumerate().flat_map(|(h, hay)| {
            let both: &[Escapes] = if hay.chars.contains(&'\u{1B}') { &[Escapes::Chars, Escapes::Segments] } else { &[Escapes::Chars] };
            both.iter().map(move |e| (h, hay, *e))
        });
        for (h, hay, escapes) in forms {
            if hay.chars.is_empty() {
                continue;
            }
            let (chars, src) = normalize(&hay.chars, escapes);
            let slot: Vec<u32> = src.iter().map(|i| hay.slot.get(*i).copied().unwrap_or(SEP)).collect();
            let font_reading = h < KINDS.saturating_sub(1).saturating_mul(2);
            let adjacent = |s: u32| rt.slots.get(s as usize).and_then(|(r, si)| walker.records.get(*r)?.strings.get(*si)).is_some_and(|x| x.adjacent);
            for (k, a, b, exempt) in needles.matches(&chars, &slot, &adjacent) {
                let Some(n) = needles.list.get(k) else { continue };
                {
                    let recs: BTreeSet<usize> =
                        slot.get(a..b).unwrap_or(&[]).iter().filter(|s| **s != SEP).filter_map(|s| rt.slots.get(*s as usize)).map(|(r, _)| *r).collect();
                    let Some(first) = recs.first().copied() else { continue };
                    if rt.role == Role::Kept {
                        let determined = recs.iter().all(|r| walker.records.get(*r).is_some_and(|x| x.font == FontState::Known));
                        if !font_reading || !determined {
                            continue;
                        }
                        if recs.len() == 1
                            && let Some(r) = walker.records.get(first)
                            && let Some(stream) = r.stream
                        {
                            kept_ops.entry((stream, r.op)).or_default().drawn.insert(n.index);
                        }
                    }
                    if !exempt {
                        matched.insert((n.index, first));
                    }
                }
            }
        }
        for (needle, first) in matched {
            let Some(r) = walker.records.get(first) else { continue };
            let Some(stream) = r.stream else { continue };
            if rt.role == Role::Kept {
                collided.insert((needle, stream, r.op));
            } else {
                text_hits.insert((needle, stream));
            }
        }
    }
    // The joined string bytes of one operator of two or more strings, in every byte spelling.
    for rec in &walker.records {
        let Some(stream) = rec.stream else { continue };
        if rec.strings.iter().filter(|s| !s.raw.is_empty()).count() < 2 {
            continue;
        }
        let bytes: Vec<u8> = rec.strings.iter().flat_map(|s| s.raw.iter().map(|c| u8::try_from(u32::from(*c)).unwrap_or(0))).collect();
        for needle in vars.iter().filter(|v| !v.hex && scan_one(&bytes, v)).map(|v| v.needle_index).collect::<BTreeSet<_>>() {
            if rec.role != Role::Kept {
                text_hits.insert((needle, stream));
            } else if rec.strings.iter().all(|s| s.raw_drawn) {
                // Kept bytes the font draws as the same letters: kept text.
                collided.insert((needle, stream, rec.op));
                kept_ops.entry((stream, rec.op)).or_default().drawn.insert(needle);
            }
        }
    }
    for (needle, stream) in text_hits {
        proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedText { object: stream } });
    }

    // 4. Every decoded stream: bytes, content operands, inline images, and a non-content stream's
    // text (XMP, embedded files) normalised.
    let mut found: BTreeSet<(usize, u32)> = BTreeSet::new();
    let mut fonts = FontCache::default();
    for (i, (num, bytes)) in decoded.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        if !matches!(objs.get(num).map(|e| &e.obj), Some(Object::Stream(_))) {
            continue;
        }
        let roles = walker.roles.get(num).cloned();
        // Byte exemptions only where every context that draws this stream is kept page text.
        let exempt = roles.as_ref().is_some_and(|r| !r.is_empty() && r.iter().all(|x| *x == Role::Kept));
        // Every stream that tokenizes as content was walked above (it has a role); any other stream
        // is not content: its bytes are scanned in every spelling and read as text.
        let ops = if roles.is_some() { walker.parsed_any(*num) } else { None };
        let ops_list: &[Op] = ops.as_ref().map(|p| p.ops.as_slice()).unwrap_or(&[]);
        // Read as text: every stream that is not only kept page text (XMP, embedded files, stray
        // and appearance content alike); kept text is matched through its font readings above.
        if !exempt && !raster_objects.contains(num) {
            for needle in stream_text_hits(bytes, &needles) {
                found.insert((needle, *num));
                proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedStream { object: *num, offset: 0 } });
            }
        }
        // Needles each kept show operator draws (only those exempt a match in its bytes or operands).
        let drawn: BTreeMap<usize, BTreeSet<usize>> = if exempt {
            ops_list
                .iter()
                .enumerate()
                .filter(|(_, op)| is_show(&op.name))
                .map(|(k, op)| (k, shown_needles(op, kept_ops.get(&(*num, k)), &vars)))
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
            let show = is_show(&op.name);
            let mut st: Vec<&Object> = op.operands.iter().collect();
            while let Some(o) = st.pop() {
                match o {
                    Object::Str(x) => {
                        let mut found_here: BTreeSet<usize> = contains(x, &vars).into_iter().collect();
                        // A text string outside a show operator (ActualText, marked-content
                        // properties) is read as text too; show strings are read through their font.
                        if !show {
                            found_here.extend(string_text_hits(&decode_text(x).0, &needles, &[Escapes::Segments, Escapes::Chars]));
                        }
                        for needle in found_here {
                            if kept_needles.is_some_and(|ns| ns.contains(&needle)) {
                                collided.insert((needle, *num, k));
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
    let mut collisions: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    for (needle, object, _) in collided {
        let c = collisions.entry((needle, object)).or_default();
        *c = c.saturating_add(1);
    }
    proof.kept_text_collisions = collisions.into_iter().map(|((needle, object), count)| TextCollision { needle, object, count }).collect();
    Ok((proof, region))
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
    /// occur elsewhere, so they are NOT part of `proof`; `residual_in_region` covers them, and their
    /// readability still counts (`unreadable_removed`).
    pub short_needles: u32,
    /// Mismatches the independent checks of the SAVED bytes find (0 = none): for every flattened page
    /// `flatten::check_flattened` (page keys with `/Annots` empty, the `/Parent` chain holding nothing to
    /// inherit, the canonical raster content, the raster image dictionary and size, every region pixel
    /// the fill colour), and once for the document `flatten_purge::check_document` (no `/XFA`, no
    /// `/NeedsRendering`, no annotation whose `/P` and no structure item whose `/Pg` is a flattened page, and, when the save
    /// flattened at least one region (PLR-101), no structure tree: it adds 1, once, when the catalog holds a `/StructTreeRoot`,
    /// directly or through a chain of references, or any dictionary of the file holds a `/StructParents` or `/StructParent`
    /// entry, or any dictionary of the file, an object-stream member included, has the shape of a structure element or of an
    /// outline item that names one (PLR-102), however many elements there are and whatever the purge removed of them.
    /// Operator decision D28: the default [`StructureTreePolicy::Strip`] removes the tree before the save, so the redaction proof
    /// covers untagged documents and tagged documents saved with it (test `default_redaction_strips_the_structure_tree`; what the
    /// strip cannot reach is counted here, test `structure_strip_reports_what_it_cannot_remove`); with the opt-out
    /// [`StructureTreePolicy::KeepNotProven`] the tree is neutralized best effort and reported (R23 FIX_FIRST -> STRUCTURE TREE =
    /// NOT PROVEN, lead decision 2026-10-12T08:41:40Z); tests
    /// `tagged_documents_read_not_proven_whatever_the_tree_looks_like`,
    /// `untagged_documents_and_stripped_trees_still_read_flattened`, `structure_tree_keys_are_read_as_names_not_text`,
    /// `structure_parents_left_in_the_file_are_reported`, `detached_structure_elements_are_reported`,
    /// `structure_shape_is_read_from_the_parsed_dictionary`, `structure_rules_apply_only_when_a_region_is_flattened`,
    /// `kept_annotation_with_a_flattened_p_is_reported`). PLR-105: a `/Type` or `/S` past the reader's hop bound, object-stream
    /// bytes that are not members and the saved trailer are read by that check too (test
    /// `unreadable_structure_shapes_are_counted`). Also counted, by the purge before the save (PLR-104, `flatten_purge.rs`
    /// marked-content property lists, lead diagnosis DIAG-PRESS-R27): a `/Properties` dictionary the old page reached whose usage
    /// could not be determined, one the purge cannot edit with an unnamed entry holding `/Alt` `/ActualText` `/E` `/T` or `/Lang`,
    /// an unnamed inline list with such text in a dictionary a kept page reaches (not purged per entry), and such a property-list
    /// object of the old page left in the file that no kept content names (tests `property_lists_the_purge_cannot_clear_are_counted`,
    /// `property_lists_a_kept_page_reaches_are_counted`).
    pub residual_in_region: u32,
    /// Outline/annotation text redaction of the removed strings (`redact_and_save_with` with a
    /// text policy); `None` when it was not requested.
    pub text_fields: Option<TextFieldReport>,
    /// Removed strings and runs (`RedactionReport::removed_unreadable`), of ANY length, without a
    /// complete reading: the words they drew are unknown, so the proof could not search for them.
    /// Non-zero withholds `is_proven` with `NotProven::UnreadableRemovedText` (R8 F3/D3; lead decision
    /// 2026-10-11T13:10:26Z removed the 6-byte minimum: one glyph per operator counts; tests
    /// `unreadable_removed_text_is_never_proven`, `glyph_by_glyph_unreadable_text_is_never_proven`).
    pub unreadable_removed: u32,
    /// Every page carrying a region, as flattened (Operator decision D27, `flatten.rs`): the page's only
    /// content is a raster with the regions painted opaque; `notes` lists what the renderer did not
    /// draw (absent from the raster). Tests `flattened_pages_hold_only_the_raster`,
    /// `flattened_region_pixels_are_opaque_and_the_rest_is_rendered`.
    pub flattened: Vec<FlattenedPage>,
    /// Annotations a viewer drew on a flattened page (widgets included) that were removed from the file
    /// with their AcroForm field entries (`flatten_purge.rs`; R13-F1, R13-P1).
    pub flattened_annotations_removed: u32,
    /// Structure elements that pointed at a flattened page and lost their text, attributes and the
    /// content items on that page (`flatten_purge.rs`; R13-P1).
    pub structure_elements_neutralized: u32,
    /// `/AcroForm /XFA` or `/NeedsRendering` was removed (R13-F2; every redaction output is a PDF-page
    /// document; test `xfa_is_stripped_from_redaction_outputs`).
    pub xfa_removed: bool,
    /// What the save removed from the document beyond the regions, as typed losses (sorted, unique; Operator decision D28:
    /// [`RedactionLoss::AccessibilityTagsRemoved`] when the default [`StructureTreePolicy::Strip`] removed a structure tree or a
    /// structure entry; `/MarkInfo` alone is not one, R26-S4; tests `default_redaction_strips_the_structure_tree`,
    /// `structure_strip_reports_no_loss_without_structure`).
    pub losses: Vec<RedactionLoss>,
    /// Images, forms, patterns and shadings (object numbers in `bytes`) a flattened page reached that
    /// stay in the saved file and are not silent (R18, the minimal draw set: silent only when the source is an image
    /// XObject stream without an `/OC` that a kept page draws with `Do` directly in its own content stream(s), each
    /// named by exactly one reference of the file (R19), outside `BDC /OC` marked content, resolved through the page's
    /// own nearest resource entry with a dictionary, not a stream, at every step): some content's usage could not be
    /// determined, a kept object only references them or
    /// places them where a viewer draws nothing from them (a catalog structure, a kept page's `/Resources` entry it
    /// does not draw, a plain key of an object a kept page draws, an object used as a `/Resources` value or as a
    /// gated sub-dictionary, a name an operator uses for an object of another kind, an entry a nearer entry of the
    /// same name shadows, any other plain reference), or a viewer draws them through anything but that direct `Do` (a
    /// form, tiling pattern, Type 3 glyph procedure, soft-mask group, image mask, shading or annotation appearance, a
    /// page whose content streams are named elsewhere, R19; over-reporting is accepted); not pruned, the kept
    /// structure is not edited; only painted sources are listed: a font or graphics state that stays is not one and is
    /// not reported (R19); non-empty is `NotProven::PaintedSourceKept` (R13-F6, R18, R19; tests
    /// `painted_sources_under_a_region_do_not_survive`,
    /// `painted_source_drawn_directly_by_a_kept_page_is_not_reported`,
    /// `painted_source_referenced_by_a_kept_catalog_structure_is_reported`,
    /// `painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported`,
    /// `painted_source_referenced_by_a_key_of_a_drawn_object_is_reported`,
    /// `painted_source_placed_as_a_resources_value_is_reported`,
    /// `painted_source_placed_as_a_gated_sub_dictionary_is_reported`,
    /// `painted_source_placed_as_an_appearance_state_is_reported`,
    /// `painted_source_named_where_it_is_not_drawn_is_reported`,
    /// `painted_source_shadowed_by_a_nearer_resource_entry_is_reported`,
    /// `painted_source_in_an_annotation_appearance_is_reported`,
    /// `painted_source_under_optional_content_is_reported`,
    /// `painted_source_named_through_a_stream_used_as_a_dictionary_is_reported`,
    /// `painted_source_drawn_by_an_image_or_shading_stream_read_as_content_is_reported`,
    /// `painted_source_selected_by_a_type3_font_pattern_or_soft_mask_is_reported`,
    /// `painted_source_drawn_by_anything_but_a_kept_pages_own_content_is_reported`,
    /// `painted_sources_rendered_with_a_drawn_image_are_reported`,
    /// `painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere`,
    /// `a_font_or_graphics_state_that_stays_is_not_a_painted_source`). Residual (PLR-084): an image that is not listed is
    /// silent by syntax only: a kept page's own content stream draws it directly with `Do`, and the visibility of that
    /// draw (a zero-scale `cm`, an empty clip, an alpha of 0, a later object painted over it) is not checked (test
    /// `silent_painted_source_visibility_is_not_checked`).
    pub painted_sources_kept: Vec<u32>,
    /// `(region page index, class)`: text drawn on a region's page in the SAVED file whose glyph
    /// geometry the selector does not compute (Type 3, vertical, unresolved writing mode, unresolved
    /// font), from the determination pass of the proof (sorted, unique). Non-empty makes the region
    /// incomplete (`NotProven::RegionIncomplete`; lead decision 2026-10-11T13:10:26Z; tests
    /// `fonts_inherited_into_forms_are_resolved`, `writing_mode_is_read_from_the_cmap`,
    /// `undetermined_fonts_never_read_as_cleared`). A flattened page holds no text, so for a redaction
    /// output (every region page flattened) this is empty; the selector's own marks on the INPUT still
    /// make a region incomplete (`RedactionReport::unredacted`).
    pub region_undetermined: Vec<(u32, RedactionClass)>,
}

impl RedactedOutput {
    /// [`RedactedOutput::region_cleared`], the saved bytes pass the payload proof for every distinctive
    /// removed string (no hit), and every removed string has a complete reading.
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
        if !self.reports.iter().all(|r| r.complete) || !self.region_undetermined.is_empty() {
            v.push(NotProven::RegionIncomplete);
        }
        if self.residual_in_region != 0 {
            v.push(NotProven::ResidualInRegion);
        }
        if !self.painted_sources_kept.is_empty() {
            v.push(NotProven::PaintedSourceKept);
        }
        // Positive determination: every text-showing operator of the saved file, whatever was removed.
        v.extend(self.proof.structural_reasons(true));
        // Canonical rewrite: the saved file holds only content this crate wrote from its own parse.
        if !self.proof.non_canonical_streams.is_empty() {
            v.push(NotProven::NonCanonicalContent);
        }
        v
    }

    /// Every page carrying a region was flattened (Operator decision D27) and the SAVED bytes hold, for
    /// each, exactly the raster page with every pixel of every region the fill colour, nothing to inherit
    /// on its `/Parent` chain, no annotation or structure item pointing at it, no structure tree and no structure-element
    /// shaped dictionary (a saved document that still has one reads false, so this claim covers untagged documents and tagged
    /// documents saved with the default strip, Operator decision D28; with the opt-out `StructureTreePolicy::KeepNotProven` a
    /// tagged document reads false, STRUCTURE TREE = NOT PROVEN; a save with no region does not apply this rule, PLR-101) and no XFA
    /// (`residual_in_region == 0`: `flatten::check_flattened`, `flatten_purge::check_document`), no
    /// painted source of the old page kept that is not silent (`painted_sources_kept` empty), in a file with one
    /// xref section and no unreachable or unlisted objects. This is the by-construction claim; it does
    /// not depend on the needle search (tests `prereview_region_pages_are_flattened`,
    /// `flattened_pages_hold_only_the_raster`, `flattened_region_pixels_are_opaque_and_the_rest_is_rendered`).
    pub fn region_flattened(&self) -> bool {
        let pages: BTreeSet<u32> = self.reports.iter().map(|r| r.page_index).collect();
        let flat: BTreeSet<u32> = self.flattened.iter().map(|f| f.page_index).collect();
        pages == flat
            && self.residual_in_region == 0
            && self.painted_sources_kept.is_empty()
            && !self.proof.structural_reasons(false).contains(&NotProven::Structure)
    }

    /// The regions themselves are proven clear: every region page flattened and verified on the saved
    /// bytes (`region_flattened`: `residual_in_region == 0`, `painted_sources_kept` empty), every text
    /// the selector found in the regions placed (painted classes do not count,
    /// `RedactionReport::painted_over`), no text whose geometry is not computed on a region's page
    /// (`region_undetermined`), one xref section, no unreachable or unlisted objects, every
    /// stream of the saved file decoded (`proof.streams_not_decodable` names the blockers; raster-class
    /// streams do not block), no bytes after an end-of-data marker (`NotProven::TrailingData`), and
    /// every text-showing operator of the saved file positively determined: font resolved, writing
    /// mode known, every code read, content followed (`NotProven::OpaqueText`,
    /// `NotProven::UnresolvedWritingMode`, `NotProven::UnreadableContent`), whatever was removed (lead
    /// decision 2026-10-11T13:10:26Z), and every content stream of the saved file in the canonical form
    /// this crate writes (`NotProven::NonCanonicalContent`, lead decision 2026-10-11T15:00:20Z). Says nothing about the same words legitimately living elsewhere
    /// (outline titles, metadata); `is_proven` also requires `proof.hits` to be empty.
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
/// as leaks there. `RedactedOutput::text_fields` carries the per-object outcomes. The structure tree
/// follows the default [`StructureTreePolicy::Strip`] (Operator decision D28).
pub fn redact_and_save_with(
    doc: &mut PdfDoc,
    regions: &[RedactionRegion],
    policy: UnsupportedPolicy,
    text: Option<&TextFieldPolicy>,
    cancel: &CancellationToken,
) -> Result<RedactedOutput> {
    redact_and_save_opts(doc, regions, policy, &RedactOptions { text, structure_tree: StructureTreePolicy::default() }, cancel)
}

/// What a redaction save does with the document's structure tree (tagged PDF; Operator decision D28, 2026-10-14).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StructureTreePolicy {
    /// Default. When the save flattens at least one region, the structure tree is unlinked (`flatten_purge.rs`
    /// `strip_structure`, lead diagnosis DIAG-PRESS-R27: the catalog's `/StructTreeRoot` and `/MarkInfo`, every `/StructParents`,
    /// `/StructParent` and `/SE` entry and the `/SD` of every `GoTo` action are removed; no object is deleted) and the save, which
    /// writes only the objects reachable from the trailer, no longer writes what only the tree reached, `/ParentTree`, `/IDTree`,
    /// `/RoleMap` and `/ClassMap` included; an object anything kept still reaches stays (and structure that stays is reported);
    /// marked-content operators in content streams (`BDC` with `/MCID`) are kept. The save reports
    /// [`RedactionLoss::AccessibilityTagsRemoved`] and a tagged document can read `region_flattened` (tests
    /// `default_redaction_strips_the_structure_tree`, `structure_strip_unlinks_and_keeps_what_anything_kept_reaches`).
    #[default]
    Strip,
    /// Opt-out: the structure tree is kept and neutralized best effort, and a save that flattens a region of a document
    /// that still has one reads `region_flattened` false with `NotProven::ResidualInRegion` (STRUCTURE TREE = NOT PROVEN,
    /// lead decision 2026-10-12T08:41:40Z; test `tagged_documents_read_not_proven_whatever_the_tree_looks_like`).
    KeepNotProven,
}

/// A typed loss of a redaction save: something the save removed from the document beyond the regions
/// (`RedactedOutput::losses`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RedactionLoss {
    /// The structure tree (accessibility tags) was removed ([`StructureTreePolicy::Strip`], Operator decision D28).
    AccessibilityTagsRemoved,
}

impl RedactionLoss {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AccessibilityTagsRemoved => "accessibility_tags_removed",
        }
    }
}

/// Options of [`redact_and_save_opts`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RedactOptions<'a> {
    /// Also redact the removed strings from outline titles and annotation text ([`redact_and_save_with`]).
    pub text: Option<&'a TextFieldPolicy>,
    /// The structure tree (Operator decision D28); default [`StructureTreePolicy::Strip`].
    pub structure_tree: StructureTreePolicy,
}

/// [`redact_and_save_with`] with every option explicit ([`RedactOptions`]).
pub fn redact_and_save_opts(
    doc: &mut PdfDoc,
    regions: &[RedactionRegion],
    policy: UnsupportedPolicy,
    options: &RedactOptions<'_>,
    cancel: &CancellationToken,
) -> Result<RedactedOutput> {
    let text = options.text;
    let backup = doc.objects.clone();
    let result = (|| -> Result<RedactedOutput> {
        let mut reports = Vec::with_capacity(regions.len());
        for region in regions {
            reports.push(doc.select_region(region, policy, true, cancel)?);
        }
        let all: Vec<(Vec<u8>, Option<String>)> = reports
            .iter()
            .flat_map(|r| r.removed_text.iter().cloned().zip(r.removed_unicode.iter().cloned()))
            .collect();
        let (needles, unicode): (Vec<Vec<u8>>, Vec<Option<String>>) =
            all.iter().filter(|(n, _)| n.len() >= MIN_DISTINCTIVE_NEEDLE).cloned().unzip();
        let short_needles = u32::try_from(all.len().saturating_sub(needles.len())).unwrap_or(u32::MAX);
        // R8 F3/D3, R9-F3 (d): a removed string or run whose words are unknown cannot be searched for,
        // whatever its length.
        let unreadable = reports.iter().flat_map(|r| r.removed_unreadable.iter()).filter(|u| **u).count();
        let unreadable_removed = u32::try_from(unreadable).unwrap_or(u32::MAX);
        let text_fields = match text {
            Some(tp) => {
                // The drawn text and, where a remap makes it differ, the bytes' text (R10-D1).
                let mut strings: Vec<String> = Vec::new();
                for (n, u) in needles.iter().zip(&unicode) {
                    let bytes_text: String = decode_text(n).0.into_iter().collect();
                    if let Some(t) = u
                        && !strings.contains(t)
                    {
                        strings.push(t.clone());
                    }
                    if (u.is_none() || u.as_ref() != Some(&bytes_text)) && !strings.contains(&bytes_text) {
                        strings.push(bytes_text);
                    }
                }
                Some(doc.redact_text_fields(&strings, tp, cancel)?)
            }
            None => None,
        };
        // FLATTEN (Operator decision D27): every page carrying a region becomes a raster of itself with
        // the regions painted opaque, detached from inheritance; nothing else of it survives (flatten.rs).
        let flats = flatten_region_pages(doc, regions, cancel)?;
        // What described or drew the old pages (R13 lead decision 2026-10-11T20:26:55Z, flatten_purge.rs):
        // XFA, their annotations and widgets, the structure elements pointing at them.
        let xfa_removed = strip_xfa(doc)?;
        let annotations = page_annotations(doc, &flats);
        // Operator decision D28: by default a save that flattens a region removes the whole structure tree (typed loss);
        // the opt-out keeps it and neutralizes it best effort (then check_document reports it).
        // DIAG-PRESS-R28/R29: the strip also returns the closed set of the tree's objects (every key but /Pg /Obj /Stm /StmOwn, no depth
        // cap, object budget); check_document counts any the save still writes, and a set the budget left incomplete.
        let stripped = if options.structure_tree == StructureTreePolicy::Strip && !flats.is_empty() { strip_structure(doc, cancel)? } else { Stripped::default() };
        let tags_removed = stripped.removed;
        let structure_elements_neutralized = neutralize_structure(doc, &flats, &annotations, cancel)?;
        let flattened_annotations_removed = remove_annotations(doc, &annotations, cancel)?;
        // CANONICAL REWRITE (lead decision 2026-10-11T15:00:20Z): every other content stream of the
        // document is re-serialised from this crate's parse before the save (canon.rs).
        canonicalize(&mut doc.objects, &doc.trailer, &doc.limits, cancel)?;
        // R13-F6: the old pages' images, forms, patterns and shadings that no kept content names and no kept object
        // references are dropped; a painted source that stays and is not drawn directly by a kept page is returned (reported).
        // PLR-104: the same pass purges the marked-content property lists (`/Properties`) of the old pages that no kept
        // `BDC` or `DP` names; what it cannot purge or determine is counted in `residual_in_region` below.
        let purged = drop_painted_sources(doc, &flats, cancel)?;
        let painted_sources_kept = purged.painted_kept;
        let flattened: Vec<FlattenedPage> = flats.into_iter().map(|f| f.page).collect();
        let bytes = doc.save_full(cancel)?;
        // Fresh re-open of the saved bytes for the independent checks below.
        let saved = PdfDoc::open(&bytes, &doc.limits, cancel)?;
        let pages: Vec<(u32, u32)> =
            regions.iter().filter_map(|r| saved.pages.get(r.page_index as usize).map(|p| (r.page_index, *p))).collect();
        let page_objects: Vec<u32> = pages.iter().map(|(_, p)| *p).collect();
        let (proof, classes) = verify_internal(&bytes, variants_unicode(&needles, &unicode), &doc.limits, cancel, &page_objects)?;
        let mut region_undetermined: Vec<(u32, RedactionClass)> = classes
            .into_iter()
            .flat_map(|(object, class)| pages.iter().filter(move |(_, p)| *p == object).map(move |(index, _)| (*index, class)))
            .collect();
        region_undetermined.sort();
        region_undetermined.dedup();
        // Every region page is a raster now: the saved page must hold exactly the raster, and every pixel
        // of every region on it the fill colour (re-read from the saved bytes, flatten::check_flattened).
        let mut residual = purged.property_residual;
        for flat in &flattened {
            let rects: Vec<[f64; 4]> = regions.iter().filter(|r| r.page_index == flat.page_index).map(|r| r.rect).collect();
            residual = residual.saturating_add(check_flattened(&saved, flat, &rects, cancel)?);
        }
        let flat_objects: BTreeSet<u32> =
            flattened.iter().filter_map(|f| saved.pages.get(usize::try_from(f.page_index).unwrap_or(usize::MAX)).copied()).collect();
        // The structure rules of check_document apply only when a region was flattened (PLR-101).
        residual = residual.saturating_add(check_document(&saved, &flat_objects, !flattened.is_empty(), &stripped));
        Ok(RedactedOutput {
            bytes,
            reports,
            proof,
            short_needles,
            residual_in_region: residual,
            text_fields,
            unreadable_removed,
            flattened,
            flattened_annotations_removed,
            structure_elements_neutralized,
            xfa_removed,
            losses: if tags_removed { vec![RedactionLoss::AccessibilityTagsRemoved] } else { Vec::new() },
            painted_sources_kept,
            region_undetermined,
        })
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
    // Indirect name objects, so an indirect /S or /Subtype is read as what it names (R10-F3).
    let named = |name: &str| -> BTreeSet<u32> { staged.iter().filter(|(_, e)| e.obj.is_name(name)).map(|(n, _)| *n).collect() };
    let (js_names, attachment_names) = (named("JavaScript"), named("FileAttachment"));
    let is_name = |o: Option<&Object>, name: &str, refs: &BTreeSet<u32>| o.is_some_and(|s| s.is_name(name) || matches!(s, Object::Ref(r) if refs.contains(&r.num)));
    let js_nums: BTreeSet<u32> =
        staged.iter().filter(|(_, e)| is_name(e.obj.as_dict().and_then(|d| d.get("S")), "JavaScript", &js_names)).map(|(n, _)| *n).collect();
    let is_js = |o: &Object| match o {
        Object::Ref(r) => js_nums.contains(&r.num),
        Object::Dict(d) => is_name(d.get("S"), "JavaScript", &js_names),
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
                    if is_name(d.get("Subtype"), "FileAttachment", &attachment_names) {
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
