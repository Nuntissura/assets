//! Simple-font encodings: which text a one-byte code draws (ISO 32000-2 9.6.5 "Character encoding",
//! 9.6.5.2 encoding dictionaries with `/Differences`, 9.6.5.5 / 9.8 Type 3 encodings, Annex D).
//!
//! A simple font (Type 1, MMType1, TrueType, Type 3) draws a glyph per byte code. What text that is
//! follows from the font's `/Encoding`:
//! - a base encoding (`StandardEncoding`, `WinAnsiEncoding`, `MacRomanEncoding`, `PDFDocEncoding`),
//!   named in the encoding dictionary's `/BaseEncoding` or as the `/Encoding` name itself;
//! - when no base is named, an implicit one: `StandardEncoding` for a non-embedded, non-symbolic font,
//!   the font program's built-in encoding for an embedded or symbolic font (that table lives in the font
//!   program and is NOT read, so those codes are unknown), nothing for Type 3 (its glyph names come only
//!   from `/Differences`);
//! - `/Differences [code /name /name ... code /name ...]` on top: each name replaces the code's glyph.
//!
//! A glyph name becomes text through the Adobe Glyph List and its naming conventions (suffix after the
//! first period dropped, underscore-joined components, `uniXXXX...` and `uXXXX[XX]` forms; AGL
//! specification, <https://github.com/adobe-type-tools/agl-specification>). The list is embedded in
//! `encoding_data` (BSD-3-Clause, attribution there).
//!
//! Fail closed: a code the encoding cannot turn into text (a glyph name outside the list and the
//! conventions, a code left to an unread built-in encoding, a malformed `/Differences`) is
//! [`Slot::Unknown`]. Text shown with such a code is opaque unless the font's `/ToUnicode` map reads
//! the code (the same rule as a composite font without a map, `NotProven::OpaqueText`).
//! Not covered (stated in the DESCRIPTOR): a font program whose glyph SHAPE differs from its glyph name
//! (including the shapes of Type 3 glyph procedures), and the built-in `cmap` of symbolic TrueType fonts.
//!
//! Tests: unit tests below (naming conventions, base tables, `/Differences` parsing, implicit bases) and
//! `differences_encoded_text_is_found_proven_and_redacted`, `unmappable_glyph_names_and_encodingless_fonts_are_opaque`,
//! `differences_names_are_read_as_pdf_syntax` in `tests/pdf_delivery.rs`.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::Cmap;
use crate::encoding_data::{AGL, MAC_ROMAN, STANDARD, WIN_ANSI};
use crate::error::{PressError, Result};
use crate::redaction::res;
use crate::show_text::Objs;
use crate::syntax::{Dict, Object};
use crate::text_codec::pdfdoc_char;

/// Most elements of a `/Differences` array that are followed (a legitimate array holds at most 256
/// codes and 256 names plus a few repeated assignments); a longer array is unreadable.
const MAX_DIFFERENCES: usize = 2048;
/// Longest glyph name that is looked up.
const MAX_GLYPH_NAME: usize = 128;
/// Decoded-byte budget one `/Differences` element costs (plus 16 elements per font): a document that
/// reads thousands of fonts with the longest legal array runs out of `max_decoded_total` instead of
/// spending seconds per font (test `hostile_differences_are_bounded`).
const ELEMENT_COST: usize = 64;

/// What one code of a simple font draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// The code draws no text (unassigned in the base encoding, `.notdef`, a glyph that is blank
    /// or a zero-width character).
    NoGlyph,
    /// One character.
    Char(char),
    /// A glyph that stands for several characters (a ligature name such as `f_i`).
    Text(String),
    /// The code draws something this reading cannot turn into text.
    Unknown,
}

/// The font's base encoding before `/Differences`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Standard,
    WinAnsi,
    MacRoman,
    PdfDoc,
    /// No text at any code: Type 3 before `/Differences`, and the symbols of a non-embedded
    /// ZapfDingbats (dingbats cannot spell a needle).
    Empty,
    /// Lives in the font program, or a name this reading does not know (`MacExpertEncoding`, ...).
    BuiltIn,
}

/// Subtypes whose codes are one byte.
fn simple_subtype(subtype: Option<&[u8]>) -> bool {
    matches!(subtype, Some(b"Type1" | b"MMType1" | b"TrueType" | b"Type3"))
}

/// A font dictionary whose codes are NOT one byte: Type 0 and (when referenced directly) CIDFonts.
pub(crate) fn is_composite(fd: &Dict) -> bool {
    matches!(fd.get("Subtype").and_then(Object::as_name), Some(b"Type0" | b"CIDFontType0" | b"CIDFontType2"))
}

/// The text of every code of one simple font.
pub(crate) struct SimpleEncoding {
    slots: Vec<Slot>,
}

/// Text of one shown string.
pub(crate) struct Reading {
    /// The encoding's text (plus `/ToUnicode` text for codes the encoding leaves open).
    pub text: Vec<char>,
    /// Some code could not be turned into text by the encoding or by `/ToUnicode`.
    pub opaque: bool,
}

static UNKNOWN: Slot = Slot::Unknown;

impl SimpleEncoding {
    /// Every code unknown (a malformed encoding).
    fn unreadable() -> Self {
        Self { slots: vec![Slot::Unknown; 256] }
    }

    /// Reads the encoding of a simple font dictionary (`Subtype` Type1, MMType1, TrueType or Type3;
    /// any other subtype has an unknown base). The work is charged to `budget`
    /// (`InputLimit("decoded_total")` when it is spent).
    pub(crate) fn of_font(objs: &Objs, fd: &Dict, budget: &mut usize) -> Result<Self> {
        let subtype = fd.get("Subtype").map(|s| res(objs, s)).and_then(Object::as_name);
        let (explicit, differences) = match fd.get("Encoding").map(|e| res(objs, e)) {
            None | Some(Object::Null) => (None, None),
            Some(Object::Name(n)) => (Some(named_base(n)), None),
            Some(Object::Dict(d)) => {
                let base = match d.get("BaseEncoding").map(|b| res(objs, b)) {
                    None | Some(Object::Null) => None,
                    Some(Object::Name(n)) => Some(named_base(n)),
                    Some(_) => Some(Base::BuiltIn),
                };
                (base, d.get("Differences").map(|x| res(objs, x)))
            }
            Some(_) => return Ok(Self::unreadable()),
        };
        let base = explicit.unwrap_or_else(|| implicit_base(objs, fd, subtype));
        let mut slots = base_slots(base);
        match differences {
            None | Some(Object::Null) => {}
            Some(Object::Array(items)) => {
                if items.len() > MAX_DIFFERENCES {
                    return Ok(Self::unreadable());
                }
                let cost = items.len().saturating_add(16).saturating_mul(ELEMENT_COST);
                *budget = budget.checked_sub(cost).ok_or(PressError::InputLimit("decoded_total"))?;
                if !apply_differences(objs, items, &mut slots) {
                    return Ok(Self::unreadable());
                }
            }
            Some(_) => return Ok(Self::unreadable()),
        }
        Ok(Self { slots })
    }

    /// Text of `s` code by code. A code the encoding leaves unread (or without a glyph) falls back to
    /// the font's `/ToUnicode` map when that has the code; a code that stays unknown makes the reading
    /// opaque.
    pub(crate) fn read(&self, s: &[u8], cmap: Option<&Cmap>) -> Reading {
        let mut text = Vec::with_capacity(s.len());
        let mut opaque = false;
        for b in s {
            let slot = self.slots.get(usize::from(*b)).unwrap_or(&UNKNOWN);
            match slot {
                Slot::Char(c) => text.push(*c),
                Slot::Text(t) => text.extend(t.chars()),
                Slot::NoGlyph | Slot::Unknown => {
                    let mapped = cmap.map(|c| c.decode(std::slice::from_ref(b))).unwrap_or_default();
                    if mapped.is_empty() {
                        opaque |= *slot == Slot::Unknown;
                    } else {
                        text.extend(mapped);
                    }
                }
            }
        }
        Reading { text, opaque }
    }

    /// The codes that draw `text`, one per character (the lowest code when several draw the same
    /// character), or `None` when a character has no single-character code.
    pub(crate) fn encode(&self, text: &[char]) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len());
        for c in text {
            let code = self.slots.iter().position(|s| matches!(s, Slot::Char(x) if x == c))?;
            out.push(u8::try_from(code).ok()?);
        }
        Some(out)
    }

    #[cfg(test)]
    fn slot(&self, code: u8) -> &Slot {
        self.slots.get(usize::from(code)).unwrap_or(&UNKNOWN)
    }
}

/// Both texts hold the same letters and digits in the same order (punctuation, spaces and quote
/// shapes ignored): the encoding reading tells nothing the bytes did not.
pub(crate) fn same_words(a: &[char], b: &[char]) -> bool {
    a.iter().filter(|c| c.is_alphanumeric()).eq(b.iter().filter(|c| c.is_alphanumeric()))
}

fn named_base(name: &[u8]) -> Base {
    match name {
        b"StandardEncoding" => Base::Standard,
        b"WinAnsiEncoding" => Base::WinAnsi,
        b"MacRomanEncoding" => Base::MacRoman,
        b"PDFDocEncoding" => Base::PdfDoc,
        _ => Base::BuiltIn,
    }
}

/// ISO 32000-2 9.6.5.2 (`/BaseEncoding` absent): the font program's built-in encoding for an embedded
/// or symbolic font, `StandardEncoding` for a non-embedded non-symbolic one; Type 3 has none.
fn implicit_base(objs: &Objs, fd: &Dict, subtype: Option<&[u8]>) -> Base {
    if subtype == Some(b"Type3".as_slice()) {
        return Base::Empty;
    }
    if !simple_subtype(subtype) {
        return Base::BuiltIn;
    }
    let desc = fd.get("FontDescriptor").map(|d| res(objs, d)).and_then(Object::as_dict);
    let embedded = desc.is_some_and(|d| ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.get(k).is_some()));
    let flags = desc.and_then(|d| d.get("Flags")).map(|f| res(objs, f)).and_then(Object::as_int).unwrap_or(0);
    let symbolic = flags & 4 != 0;
    let name = fd.get("BaseFont").map(|b| res(objs, b)).and_then(Object::as_name).unwrap_or(&[]);
    // A subset tag is six capital letters and a plus sign.
    let name = match name.get(6) {
        Some(b'+') if name.get(..6).is_some_and(|t| t.iter().all(u8::is_ascii_uppercase)) => name.get(7..).unwrap_or(&[]),
        _ => name,
    };
    if !embedded && name == b"ZapfDingbats" {
        return Base::Empty;
    }
    if embedded || symbolic || name == b"Symbol" {
        Base::BuiltIn
    } else {
        Base::Standard
    }
}

fn table_slots(table: &[u16; 256]) -> Vec<Slot> {
    table.iter().map(|v| char::from_u32(u32::from(*v)).filter(|_| *v != 0).map_or(Slot::NoGlyph, Slot::Char)).collect()
}

fn base_slots(base: Base) -> Vec<Slot> {
    match base {
        Base::Standard => table_slots(&STANDARD),
        Base::WinAnsi => table_slots(&WIN_ANSI),
        Base::MacRoman => table_slots(&MAC_ROMAN),
        Base::PdfDoc => (0..=255u8)
            .map(|b| match b {
                0x18..=0x7E | 0x80..=0x9E | 0xA0..=0xAC | 0xAE..=0xFF => Slot::Char(pdfdoc_char(b)),
                _ => Slot::NoGlyph,
            })
            .collect(),
        Base::Empty => vec![Slot::NoGlyph; 256],
        Base::BuiltIn => vec![Slot::Unknown; 256],
    }
}

/// Applies `[code /name /name ... code /name ...]`; `false` = malformed (a name before any code, a
/// code outside 0..=255 that names follow, a non-name non-integer element).
fn apply_differences(objs: &Objs, items: &[Object], slots: &mut [Slot]) -> bool {
    let mut code: Option<usize> = None;
    for it in items {
        match res(objs, it) {
            Object::Int(n) => code = usize::try_from(*n).ok().filter(|c| *c < 256),
            Object::Name(name) => {
                let Some(c) = code else { return false };
                let Some(slot) = slots.get_mut(c) else { return false };
                *slot = glyph_slot(name);
                code = c.checked_add(1).filter(|c| *c < 256);
            }
            _ => return false,
        }
    }
    true
}

enum Glyph {
    Text(String),
    /// Draws nothing as text (`.notdef`).
    Blank,
    Unknown,
}

fn glyph_slot(name: &[u8]) -> Slot {
    match glyph_text(name) {
        Glyph::Text(t) => finish(&t),
        Glyph::Blank => Slot::NoGlyph,
        Glyph::Unknown => Slot::Unknown,
    }
}

/// Text as matching sees it: no-break and other fixed spaces read as a space, zero-width characters
/// and the soft hyphen read as nothing.
fn finish(text: &str) -> Slot {
    let norm: String = text
        .chars()
        .filter_map(|c| match c {
            '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => Some(' '),
            '\u{AD}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' => None,
            other => Some(other),
        })
        .collect();
    let mut it = norm.chars();
    match (it.next(), it.next()) {
        (None, _) => Slot::NoGlyph,
        (Some(c), None) => Slot::Char(c),
        _ => Slot::Text(norm),
    }
}

fn is_pua(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// Adobe Glyph List specification, "Mapping glyph names to Unicode": drop everything from the first
/// period, split at underscores, map each component through the list, `uniXXXX...` or `uXXXX[XX]`.
/// A component that maps to nothing (or only to a Private Use Area character) makes the glyph unknown.
/// Only the exact name `.notdef` draws no text (ISO 32000-2 9.6.5); any other name whose stem has no
/// component (`/.s1`, `/.A`, `/_`, `/__`, the empty name) names a glyph the font program draws, which
/// this reading cannot turn into text: unknown, so the text is opaque (R8 D1; unit test
/// `glyph_names_follow_the_agl_specification`, test `empty_stem_glyph_names_are_opaque`; R9-D1 citation).
fn glyph_text(name: &[u8]) -> Glyph {
    let Ok(name) = std::str::from_utf8(name) else { return Glyph::Unknown };
    if name.len() > MAX_GLYPH_NAME {
        return Glyph::Unknown;
    }
    if name == ".notdef" {
        return Glyph::Blank;
    }
    let stem = name.split('.').next().unwrap_or_default();
    let mut text = String::new();
    for component in stem.split('_').filter(|c| !c.is_empty()) {
        match component_text(component) {
            Some(t) if !t.chars().any(is_pua) => text.push_str(&t),
            _ => return Glyph::Unknown,
        }
    }
    if text.is_empty() { Glyph::Unknown } else { Glyph::Text(text) }
}

fn component_text(c: &str) -> Option<String> {
    if let Ok(i) = AGL.binary_search_by(|(n, _)| n.as_bytes().cmp(c.as_bytes())) {
        return AGL.get(i).map(|(_, t)| (*t).to_owned());
    }
    // Hexadecimal digits are accepted in either case (the specification asks for upper case; viewers
    // are lenient, and a lenient reading only finds more).
    if let Some(hex) = c.strip_prefix("uni")
        && !hex.is_empty()
        && hex.len().checked_rem(4) == Some(0)
        && hex.len() <= 64
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return hex.as_bytes().chunks(4).map(|g| u32::from_str_radix(std::str::from_utf8(g).ok()?, 16).ok().and_then(char::from_u32)).collect();
    }
    if let Some(hex) = c.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from);
    }
    None
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::reader::Entry;
    use crate::syntax::Lexer;
    use std::collections::BTreeMap;

    fn objs(entries: &[(u32, &str)]) -> Objs {
        let mut m = BTreeMap::new();
        for (n, text) in entries {
            let obj = Lexer::new(text.as_bytes(), 0, 32).parse_object(0, true).unwrap();
            m.insert(*n, Entry { generation: 0, obj, span: None, data_at: None });
        }
        m
    }

    fn font(extra: &[(u32, &str)], dict: &str) -> SimpleEncoding {
        let o = objs(extra);
        let fd = Lexer::new(dict.as_bytes(), 0, 32).parse_object(0, true).unwrap();
        let mut budget = usize::MAX;
        SimpleEncoding::of_font(&o, fd.as_dict().unwrap(), &mut budget).unwrap()
    }

    fn text(e: &SimpleEncoding, s: &[u8]) -> String {
        let r = e.read(s, None);
        assert!(!r.opaque, "unexpected opaque reading of {s:?}");
        r.text.into_iter().collect()
    }

    #[test]
    fn glyph_list_is_sorted_attributed_and_complete() {
        assert!(AGL.windows(2).all(|w| w[0].0.as_bytes() < w[1].0.as_bytes()), "binary search needs strictly sorted names");
        assert_eq!(AGL.len(), 4089, "4281 Adobe Glyph List 2.0 entries minus 192 Private Use Area entries");
        let get = |n: &str| component_text(n);
        assert_eq!(get("A").as_deref(), Some("A"));
        assert_eq!(get("fi").as_deref(), Some("\u{FB01}"));
        assert_eq!(get("afii10017").as_deref(), Some("\u{410}"));
        assert_eq!(get("Euro").as_deref(), Some("\u{20AC}"));
        assert_eq!(get("dalethatafpatah").as_deref(), Some("\u{5D3}\u{5B2}"), "multi-character entries survive");
        assert_eq!(get("zukatakana").as_deref(), Some("\u{30BA}"), "last entry of the list");
        assert_eq!(get("AEsmall"), None, "Private Use Area names are left out on purpose");
        assert!(include_str!("encoding_data.rs").contains("Redistribution and use in source and binary forms"), "the licence notice travels with the data");
    }

    #[test]
    fn glyph_names_follow_the_agl_specification() {
        let t = |n: &str| match glyph_text(n.as_bytes()) {
            Glyph::Text(t) => Some(t),
            _ => None,
        };
        assert_eq!(t("uni0041").as_deref(), Some("A"));
        assert_eq!(t("uni00410042").as_deref(), Some("AB"));
        assert_eq!(t("u1F600").as_deref(), Some("\u{1F600}"));
        assert_eq!(t("u0041").as_deref(), Some("A"));
        assert_eq!(t("A.sc").as_deref(), Some("A"), "suffix after the first period is dropped");
        assert_eq!(t("f_i").as_deref(), Some("fi"), "components joined");
        assert_eq!(t("uni00e9").as_deref(), Some("\u{E9}"), "lower-case hex is read leniently");
        assert_eq!(t("uniD800"), None, "a surrogate is no character");
        assert_eq!(t("uniE000"), None, "a Private Use Area character is not text");
        assert_eq!(t("Asmall"), None);
        for unknown in ["g123", "G12", "uni004", "uni0041zzzz", "Uni0041", "u41", "u0000041", "cid12", ""] {
            assert!(matches!(glyph_text(unknown.as_bytes()), Glyph::Unknown), "{unknown}");
            assert!(t(unknown).is_none(), "{unknown}");
        }
        // R8 D1: only `.notdef` draws nothing; an empty stem is a glyph this reading cannot name.
        assert!(matches!(glyph_text(b".notdef"), Glyph::Blank));
        for empty_stem in [".s1", ".A", "_", "__", ".notdef.alt", "._"] {
            assert!(matches!(glyph_text(empty_stem.as_bytes()), Glyph::Unknown), "{empty_stem}");
            assert_eq!(glyph_slot(empty_stem.as_bytes()), Slot::Unknown, "{empty_stem}");
        }
        assert!(matches!(glyph_text(&[0xFF, 0xFE]), Glyph::Unknown));
        assert!(matches!(glyph_text(&[b'A'; 200]), Glyph::Unknown), "name length bound");
        assert_eq!(glyph_slot(b"nbspace"), Slot::Char(' '));
        assert_eq!(glyph_slot(b"sfthyphen"), Slot::NoGlyph);
        assert_eq!(glyph_slot(b"f_f_i"), Slot::Text("ffi".into()));
    }

    #[test]
    fn base_encodings_follow_annex_d() {
        let base = |name: &str| font(&[], &format!("<< /Subtype /TrueType /Encoding /{name} >>"));
        let std = font(&[], "<< /Subtype /Type1 /BaseFont /Helvetica >>");
        assert_eq!(std.slot(0x27), &Slot::Char('\u{2019}'), "StandardEncoding 0x27 is quoteright");
        assert_eq!(std.slot(0xAE), &Slot::Char('\u{FB01}'));
        assert_eq!(std.slot(0x00), &Slot::NoGlyph);
        assert_eq!(text(&std, b"Hello"), "Hello");
        let win = base("WinAnsiEncoding");
        assert_eq!((win.slot(0x80), win.slot(0x27), win.slot(0xA0), win.slot(0xAD)), (&Slot::Char('\u{20AC}'), &Slot::Char('\''), &Slot::Char(' '), &Slot::Char('-')));
        assert_eq!(win.slot(0x7F), &Slot::NoGlyph);
        let mac = base("MacRomanEncoding");
        assert_eq!((mac.slot(0x8A), mac.slot(0xCA), mac.slot(0xAD)), (&Slot::Char('\u{E4}'), &Slot::Char(' '), &Slot::Char('\u{2260}')));
        assert_eq!(base("PDFDocEncoding").slot(0xA0), &Slot::Char('\u{20AC}'));
        // Not read: unknown names and MacExpertEncoding leave every code unknown.
        assert_eq!(base("MacExpertEncoding").slot(b'A'), &Slot::Unknown);
        assert_eq!(base("Identity-H").slot(b'A'), &Slot::Unknown);
    }

    #[test]
    fn differences_are_applied_over_the_base() {
        let e = font(&[], "<< /Subtype /Type1 /BaseFont /Test /Encoding << /Type /Encoding /Differences [1 /S /E /C /R /E /T 40 /uni0041 /f_i 200 /space] >> >>");
        assert_eq!(text(&e, &[1, 2, 3, 4, 5, 6]), "SECRET");
        assert_eq!((e.slot(40), e.slot(41), e.slot(200)), (&Slot::Char('A'), &Slot::Text("fi".into()), &Slot::Char(' ')));
        assert_eq!(text(&e, b"abc"), "abc", "codes outside the array keep the implicit StandardEncoding");
        // An explicit base is honoured and /Differences sits on top of it.
        let e = font(&[], "<< /Subtype /TrueType /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [65 /Euro] >> >>");
        assert_eq!((e.slot(65), e.slot(66)), (&Slot::Char('\u{20AC}'), &Slot::Char('B')));
        // Indirect encoding, array and elements.
        let e = font(&[(7, "<< /Differences 8 0 R >>"), (8, "[1 9 0 R /B]"), (9, "5")], "<< /Subtype /Type1 /BaseFont /Test /Encoding 7 0 R >>");
        // `1 9 0 R /B`: the reference resolves to the integer 5, so /B lands on code 5.
        assert_eq!((e.slot(1), e.slot(5), e.slot(6)), (&Slot::NoGlyph, &Slot::Char('B'), &Slot::NoGlyph));
    }

    #[test]
    fn implicit_base_follows_embedding_symbolic_flag_and_type3() {
        // Non-embedded, non-symbolic: StandardEncoding. Embedded, symbolic flag, Symbol: built-in, unknown.
        let std = |dict: &str| font(&[(5, "<< /Flags 32 >>"), (6, "<< /Flags 4 >>"), (7, "<< /Flags 32 /FontFile2 9 0 R >>")], dict);
        assert_eq!(std("<< /Subtype /TrueType /BaseFont /Arial /FontDescriptor 5 0 R >>").slot(b'A'), &Slot::Char('A'));
        assert_eq!(std("<< /Subtype /TrueType /BaseFont /Arial /FontDescriptor 6 0 R >>").slot(b'A'), &Slot::Unknown);
        assert_eq!(std("<< /Subtype /Type1 /BaseFont /ABCDEF+Arial /FontDescriptor 7 0 R >>").slot(b'A'), &Slot::Unknown);
        assert_eq!(std("<< /Subtype /Type1 /BaseFont /ABCDEF+Symbol >>").slot(b'A'), &Slot::Unknown);
        // The dingbats of a non-embedded ZapfDingbats (checkbox marks) are symbols, never text; an
        // embedded font of that name is read like any embedded font.
        assert_eq!(std("<< /Subtype /Type1 /BaseFont /ZapfDingbats /FontDescriptor 6 0 R >>").slot(b'4'), &Slot::NoGlyph);
        assert_eq!(std("<< /Subtype /Type1 /BaseFont /ZapfDingbats /FontDescriptor 7 0 R >>").slot(b'4'), &Slot::Unknown);
        assert_eq!(std("<< /Subtype /Type1 /BaseFont /abcdef+Arial >>").slot(b'A'), &Slot::Char('A'), "a subset tag is capital letters only");
        assert_eq!(std("<< /BaseFont /Arial >>").slot(b'A'), &Slot::Unknown, "no subtype: not read");
        // Type 3: nothing but /Differences assigns; the rest draws nothing.
        let t3 = font(&[], "<< /Subtype /Type3 /Encoding << /Differences [65 /A /B /g1] >> >>");
        assert_eq!((t3.slot(65), t3.slot(66), t3.slot(67), t3.slot(68)), (&Slot::Char('A'), &Slot::Char('B'), &Slot::Unknown, &Slot::NoGlyph));
    }

    #[test]
    fn malformed_differences_leave_every_code_unknown() {
        // dims_u32_max / i64 edge: codes far outside a byte never wrap into the table.
        for diff in ["[/A]", "[300 /A]", "[1 2.5 /A]", "[1 (A)]", "[255 /A /B]", "[1 /A -1 /B]", "[4294967295 /A]", "[4294967296 /A]", "[9223372036854775807 /A]"] {
            let e = font(&[], &format!("<< /Subtype /Type1 /BaseFont /Test /Encoding << /Differences {diff} >> >>"));
            assert_eq!(e.slot(b'x'), &Slot::Unknown, "{diff}");
            assert_eq!(e.slot(1), &Slot::Unknown, "{diff}");
        }
        for enc in ["(WinAnsiEncoding)", "[1 2]", "5"] {
            assert_eq!(font(&[], &format!("<< /Subtype /Type1 /BaseFont /Test /Encoding {enc} >>")).slot(b'A'), &Slot::Unknown, "{enc}");
        }
        let long: String = (0..2100).map(|_| "/A ").collect();
        assert_eq!(font(&[], &format!("<< /Subtype /Type1 /Encoding << /Differences [0 {long}] >> >>")).slot(0), &Slot::Unknown, "element cap");
    }

    #[test]
    fn reading_an_encoding_is_charged_to_the_budget() {
        let o = objs(&[]);
        let names: String = (0..1000).map(|_| "/A ").collect();
        let fd = Lexer::new(format!("<< /Subtype /Type1 /Encoding << /Differences [0 {names}] >> >>").as_bytes(), 0, 32).parse_object(0, true).unwrap();
        let cost = (1001 + 16) * ELEMENT_COST;
        let mut budget = cost * 2 + 5;
        assert!(SimpleEncoding::of_font(&o, fd.as_dict().unwrap(), &mut budget).is_ok());
        assert_eq!(budget, cost + 5);
        assert!(SimpleEncoding::of_font(&o, fd.as_dict().unwrap(), &mut budget).is_ok());
        assert!(matches!(SimpleEncoding::of_font(&o, fd.as_dict().unwrap(), &mut budget), Err(PressError::InputLimit("decoded_total"))));
    }

    #[test]
    fn unknown_codes_fall_back_to_tounicode_and_otherwise_are_opaque() {
        let e = font(&[], "<< /Subtype /Type1 /BaseFont /Test /Encoding << /Differences [1 /g1 /A /g3] >> >>");
        let r = e.read(&[2], None);
        assert_eq!((r.text, r.opaque), (vec!['A'], false));
        let r = e.read(&[1, 2, 3], None);
        assert!(r.opaque && r.text == vec!['A'], "g1 and g3 are unknown");
        let cmap = crate::cmap::parse_to_unicode(b"1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <01> <0053> <03> <0054> endbfchar").unwrap();
        let r = e.read(&[1, 2, 3], Some(&cmap));
        assert_eq!((r.text, r.opaque), (vec!['S', 'A', 'T'], false), "/ToUnicode reads what the names do not");
        // A blank code the map names is read through the map too.
        let t3 = font(&[], "<< /Subtype /Type3 /Encoding << /Differences [1 /A] >> >>");
        assert_eq!(t3.read(&[1, 3], Some(&cmap)).text, vec!['A', 'T']);
        assert!(!t3.read(&[2], None).opaque, "a Type 3 code outside /Differences has no glyph");
        // Out-of-table codes cannot happen for u8, encode is the inverse for single characters.
        let e = font(&[], "<< /Subtype /Type1 /BaseFont /Test /Encoding << /Differences [1 /S /E /C] >> >>");
        assert_eq!(e.encode(&['S', 'E', 'C']), Some(vec![1, 2, 3]));
        assert_eq!(e.encode(&['\u{1F600}']), None);
    }

    #[test]
    fn helpers_expand_ligatures_and_compare_words() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        // R9: ligatures are spelled out by the matching normalisation (norm.rs, NFKD table), which
        // replaced expand_ligatures.
        let norm = |s: &str| crate::norm::normalized(&chars(s), crate::norm::Escapes::Chars);
        assert_eq!(norm("o\u{FB03}ce"), chars("office"));
        assert_eq!(norm("office"), chars("office"));
        assert!(same_words(&chars("it\u{2019}s"), &chars("it's")));
        assert!(!same_words(&chars("SECRET"), &chars("\u{1}\u{2}")));
    }
}
