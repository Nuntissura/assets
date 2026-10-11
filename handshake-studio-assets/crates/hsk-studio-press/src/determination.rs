//! Positive determination of a composite font's encoding CMap (lead decisions 2026-10-11T13:10:26Z,
//! R9-F1, and 2026-10-11T15:00:20Z, R10-F2/F4): its writing mode, its codespace (how many bytes each
//! code of a string takes) and its code -> CID map. Shared by the content walk of the proof
//! (`show_text`) and the redaction selector (`redaction`) through `font.rs`, so both answer the same
//! question the same way.
//!
//! - A simple font (Type 1, MMType1, TrueType, Type 3) writes horizontally: its glyphs have one set of
//!   metrics, so only a composite font's CMap can select writing mode 1 (ISO 32000-2 9.2.4 "Glyph
//!   positioning and metrics"; wording read through a secondary source, the Apryse PDFNet `Font`
//!   reference that paraphrases the PDF Reference font chapter, not the ISO text itself).
//! - A composite (Type 0) font writes in the mode of its CMap, the font's `/Encoding` (ISO 32000-2
//!   9.7.6.1 Table 119): a predefined CMap name is horizontal or vertical only when it is one of the
//!   names of ISO 32000-2 9.7.5.2 Table 116 ([`HORIZONTAL`], [`VERTICAL`]); its codespace is the one of
//!   that Adobe CMap resource ([`PREDEFINED_CODESPACES`]); `Identity-H`/`Identity-V` map every 2-byte
//!   code to the same CID, the other predefined CMaps map codes to CIDs this crate does not hold.
//! - An embedded CMap stream is read the way viewers read it (`cmap::parse_program`; pdf.js `cmap.js`
//!   `parseCMap`/`parseWMode` at da95557, MuPDF `pdf-cmap-parse.c` at 200426a): EVERY `/WMode` name
//!   followed by a number up to `endcmap`, whatever comes after it (`/WMode 1 def`, `/WMode 1 pop`,
//!   `<< /WMode 1 >>`, `[/WMode 1]`), counts; a non-zero integer (also `1.0`, which pdf.js reads as 1)
//!   is vertical, a number with a fraction part is unresolved; the dictionary's `/WMode` (Table 118: 0
//!   or 1) counts too, any other value is unresolved; a CMap it uses (`/UseCMap`, or the name before
//!   `usecmap`) counts. pdf.js replaces an embedded CMap NAMED `/Identity-V` or `/Identity-H` whose map
//!   is the identity by the built-in one (`CMapFactory.create`, `isIdentityCMap`), so such a name counts
//!   as that mode (an `/Identity-V` name with horizontal content reads vertical, R10-F2), and its
//!   codespace must then be the built-in 2-byte one or its codes are unknown. Vertical wins over
//!   unresolved, unresolved over horizontal: a CMap reads horizontal only when every interpretation
//!   does. Its codespace is its own `begincodespacerange` ranges, else those of the CMap it uses (pdf.js
//!   `extendCMap`); its CIDs are its `cidrange`/`cidchar` entries, then the used CMap's.
//! - Anything else (another name, a program that cannot be decoded, a stray delimiter in the program,
//!   a font without `/Encoding`, `/UseCMap` nesting over [`MAX_USECMAP_DEPTH`]) is
//!   [`WMode::Unresolved`] with an unknown codespace: never assumed horizontal, never split by guess.
//!
//! Sources for the name lists: ISO 32000-2:2020 9.7.5.2 Table 116 as encoded in the PDF Association
//! Arlington PDF Model (`tsv/latest/FontType0.tsv`, key `Encoding`, PossibleValues; 61 names, read
//! 2026-10-11); the horizontal/vertical split cross-checked against the `IS_VERTICAL` flag of the CMap
//! data bundled with pdfminer.six 20260107 for the 59 names other than Identity-H/-V (0 differences).
//! Codespace ranges: the `begincodespacerange` sections of the 59 CMap resources in
//! adobe-type-tools/cmap-resources at f5cf3bc (BSD-3-Clause, Copyright 1990-2023 Adobe; ranges of a
//! CMap that has none come from the CMap it `usecmap`s; every -V CMap has its -H CMap's ranges and
//! /WMode 1, every -H CMap /WMode 0, read 2026-10-11).
//! Tests: `writing_mode_is_read_from_the_cmap`, `unknown_cmap_names_are_unresolved_writing_modes`,
//! `wmode_spellings_and_identity_names_are_read_as_viewers_do`,
//! `composite_codes_follow_the_encoding_codespace`, `type3_and_vertical_text_never_read_as_cleared` and
//! the unit test below.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Space, WModeTok, parse_program};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::redaction::res;
use crate::show_text::Objs;
use crate::syntax::Object;

/// Writing mode of a font as far as it is positively known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WMode {
    Horizontal,
    Vertical,
    Unresolved,
}

/// `/UseCMap` chains followed before the mode reads as unresolved.
pub(crate) const MAX_USECMAP_DEPTH: usize = 4;

/// ISO 32000-2 9.7.5.2 Table 116: the predefined CMaps that write horizontally.
pub(crate) const HORIZONTAL: [&[u8]; 32] = [
    b"GB-EUC-H", b"GBpc-EUC-H", b"GBK-EUC-H", b"GBKp-EUC-H", b"GBK2K-H", b"UniGB-UCS2-H", b"UniGB-UTF16-H", b"B5pc-H", b"HKscs-B5-H",
    b"ETen-B5-H", b"ETenms-B5-H", b"CNS-EUC-H", b"UniCNS-UCS2-H", b"UniCNS-UTF16-H", b"83pv-RKSJ-H", b"90ms-RKSJ-H", b"90msp-RKSJ-H",
    b"90pv-RKSJ-H", b"Add-RKSJ-H", b"EUC-H", b"Ext-RKSJ-H", b"H", b"UniJIS-UCS2-H", b"UniJIS-UCS2-HW-H", b"UniJIS-UTF16-H", b"KSC-EUC-H",
    b"KSCms-UHC-H", b"KSCms-UHC-HW-H", b"KSCpc-EUC-H", b"UniKS-UCS2-H", b"UniKS-UTF16-H", b"Identity-H",
];

/// ISO 32000-2 9.7.5.2 Table 116: the predefined CMaps that write vertically.
pub(crate) const VERTICAL: [&[u8]; 29] = [
    b"GB-EUC-V", b"GBpc-EUC-V", b"GBK-EUC-V", b"GBKp-EUC-V", b"GBK2K-V", b"UniGB-UCS2-V", b"UniGB-UTF16-V", b"B5pc-V", b"HKscs-B5-V",
    b"ETen-B5-V", b"ETenms-B5-V", b"CNS-EUC-V", b"UniCNS-UCS2-V", b"UniCNS-UTF16-V", b"90ms-RKSJ-V", b"90msp-RKSJ-V", b"Add-RKSJ-V",
    b"EUC-V", b"Ext-RKSJ-V", b"V", b"UniJIS-UCS2-V", b"UniJIS-UCS2-HW-V", b"UniJIS-UTF16-V", b"KSC-EUC-V", b"KSCms-UHC-V",
    b"KSCms-UHC-HW-V", b"UniKS-UCS2-V", b"UniKS-UTF16-V", b"Identity-V",
];

const CS0: &[Space] = &[(1, 0x00, 0x80), (2, 0xA1A1, 0xFEFE)];
const CS1: &[Space] = &[(1, 0x00, 0x80), (2, 0xA1A1, 0xFCFE), (1, 0xFD, 0xFF)];
const CS2: &[Space] = &[(1, 0x00, 0x80), (2, 0x8140, 0xFEFE)];
const CS3: &[Space] = &[(1, 0x00, 0x7F), (4, 0x8130_8130, 0xFE39_FE39), (2, 0x8140, 0xFEFE)];
const CS4: &[Space] = &[(2, 0x0000, 0xD7FF), (2, 0xE000, 0xFFFF)];
const CS5: &[Space] = &[(2, 0x0000, 0xD7FF), (4, 0xD800_DC00, 0xDBFF_DFFF), (2, 0xE000, 0xFFFF)];
const CS6: &[Space] = &[(1, 0x00, 0x80), (2, 0xA140, 0xFCFE), (1, 0xFD, 0xFF)];
const CS7: &[Space] = &[(1, 0x00, 0x80), (2, 0x8740, 0xFEFE)];
const CS8: &[Space] = &[(1, 0x00, 0x80), (2, 0xA140, 0xFEFE)];
const CS9: &[Space] = &[(1, 0x00, 0x80), (4, 0x8EA1_A1A1, 0x8EA1_FEFE), (4, 0x8EA2_A1A1, 0x8EA2_FEFE), (4, 0x8EA3_A1A1, 0x8EA3_FEFE), (2, 0xA1A1, 0xFEFE)];
const CS10: &[Space] = &[(1, 0x00, 0x80), (2, 0x8140, 0x9FFC), (1, 0xA0, 0xDF), (2, 0xE040, 0xFCFC), (1, 0xFD, 0xFF)];
const CS11: &[Space] = &[(1, 0x00, 0x80), (2, 0x8140, 0x9FFC), (1, 0xA0, 0xDF), (2, 0xE040, 0xFCFC)];
const CS12: &[Space] = &[(1, 0x00, 0x80), (2, 0x8EA0, 0x8EDF), (2, 0xA1A1, 0xFEFE)];
const CS13: &[Space] = &[(2, 0x2121, 0x7E7E)];
const CS14: &[Space] = &[(1, 0x00, 0x80), (2, 0x8141, 0xFEFE)];
const CS15: &[Space] = &[(1, 0x00, 0x84), (2, 0xA141, 0xFDFE), (1, 0xFE, 0xFF)];
/// The built-in `Identity-H` / `Identity-V` codespace.
pub(crate) const IDENTITY_SPACE: &[Space] = &[(2, 0x0000, 0xFFFF)];

/// Codespace ranges of the 59 Table 116 CMaps other than Identity-H/-V (module docs), sorted by name.
pub(crate) static PREDEFINED_CODESPACES: [(&[u8], &[Space]); 59] = [
    (b"83pv-RKSJ-H", CS10), (b"90ms-RKSJ-H", CS11), (b"90ms-RKSJ-V", CS11), (b"90msp-RKSJ-H", CS11),
    (b"90msp-RKSJ-V", CS11), (b"90pv-RKSJ-H", CS10), (b"Add-RKSJ-H", CS11), (b"Add-RKSJ-V", CS11), (b"B5pc-H", CS6),
    (b"B5pc-V", CS6), (b"CNS-EUC-H", CS9), (b"CNS-EUC-V", CS9), (b"ETen-B5-H", CS8), (b"ETen-B5-V", CS8),
    (b"ETenms-B5-H", CS8), (b"ETenms-B5-V", CS8), (b"EUC-H", CS12), (b"EUC-V", CS12), (b"Ext-RKSJ-H", CS11),
    (b"Ext-RKSJ-V", CS11), (b"GB-EUC-H", CS0), (b"GB-EUC-V", CS0), (b"GBK-EUC-H", CS2), (b"GBK-EUC-V", CS2),
    (b"GBK2K-H", CS3), (b"GBK2K-V", CS3), (b"GBKp-EUC-H", CS2), (b"GBKp-EUC-V", CS2), (b"GBpc-EUC-H", CS1),
    (b"GBpc-EUC-V", CS1), (b"H", CS13), (b"HKscs-B5-H", CS7), (b"HKscs-B5-V", CS7), (b"KSC-EUC-H", CS0),
    (b"KSC-EUC-V", CS0), (b"KSCms-UHC-H", CS14), (b"KSCms-UHC-HW-H", CS14), (b"KSCms-UHC-HW-V", CS14),
    (b"KSCms-UHC-V", CS14), (b"KSCpc-EUC-H", CS15), (b"UniCNS-UCS2-H", CS4), (b"UniCNS-UCS2-V", CS4),
    (b"UniCNS-UTF16-H", CS5), (b"UniCNS-UTF16-V", CS5), (b"UniGB-UCS2-H", CS4), (b"UniGB-UCS2-V", CS4),
    (b"UniGB-UTF16-H", CS5), (b"UniGB-UTF16-V", CS5), (b"UniJIS-UCS2-H", CS4), (b"UniJIS-UCS2-HW-H", CS4),
    (b"UniJIS-UCS2-HW-V", CS4), (b"UniJIS-UCS2-V", CS4), (b"UniJIS-UTF16-H", CS5), (b"UniJIS-UTF16-V", CS5),
    (b"UniKS-UCS2-H", CS4), (b"UniKS-UCS2-V", CS4), (b"UniKS-UTF16-H", CS5), (b"UniKS-UTF16-V", CS5), (b"V", CS13),
];

/// Mode of a predefined CMap name (closed lists; any other name is unresolved).
pub(crate) fn predefined(name: &[u8]) -> WMode {
    if HORIZONTAL.contains(&name) {
        WMode::Horizontal
    } else if VERTICAL.contains(&name) {
        WMode::Vertical
    } else {
        WMode::Unresolved
    }
}

/// Codespace of a predefined CMap name.
fn predefined_space(name: &[u8]) -> Option<&'static [Space]> {
    if name == b"Identity-H" || name == b"Identity-V" {
        return Some(IDENTITY_SPACE);
    }
    PREDEFINED_CODESPACES.binary_search_by(|(n, _)| n.cmp(&name)).ok().and_then(|i| PREDEFINED_CODESPACES.get(i)).map(|(_, s)| *s)
}

/// Combines the modes one CMap sets: vertical wins, then unresolved; horizontal only if all are.
fn combine(modes: &[WMode]) -> WMode {
    if modes.contains(&WMode::Vertical) {
        WMode::Vertical
    } else if modes.contains(&WMode::Unresolved) {
        WMode::Unresolved
    } else {
        WMode::Horizontal
    }
}

fn dict_wmode(v: &Object) -> WMode {
    match v.as_int() {
        Some(0) => WMode::Horizontal,
        Some(1) => WMode::Vertical,
        _ => WMode::Unresolved,
    }
}

/// How a composite font's codes become CIDs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CidMap {
    /// CID = code (`Identity-H`/`-V`).
    Identity,
    /// `(low, high, first CID)` entries in program order (a later entry replaces an earlier one, as
    /// pdf.js's map does), then the used CMap's map.
    Ranges(Vec<(u32, u32, u32)>, Box<CidMap>),
    /// Not held by this crate (a predefined CMap other than Identity): CIDs are unknown.
    Unknown,
}

impl CidMap {
    /// The CID of `code`, when known.
    pub(crate) fn cid(&self, code: u32) -> Option<u32> {
        match self {
            Self::Identity => Some(code),
            Self::Ranges(r, used) => match r.iter().rev().find(|(lo, hi, _)| *lo <= code && code <= *hi) {
                Some((lo, _, first)) => code.checked_sub(*lo).and_then(|off| first.checked_add(off)),
                None => used.cid(code),
            },
            Self::Unknown => None,
        }
    }
}

/// What a composite font's `/Encoding` CMap determines.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EncodingCmap {
    pub wmode: WMode,
    /// Codespace ranges (`None`: not positively known, so no code of the font can be read).
    pub space: Option<Vec<Space>>,
    pub cid: CidMap,
}

impl EncodingCmap {
    fn unresolved() -> Self {
        Self { wmode: WMode::Unresolved, space: None, cid: CidMap::Unknown }
    }
}

/// The CMap a composite font names in `/Encoding` (module docs). Decoding an embedded CMap is charged to
/// `budget`; a budget overrun or cancellation is returned, never read as a mode.
pub(crate) fn encoding_cmap(objs: &Objs, encoding: Option<&Object>, limits: &Limits, budget: &mut usize, depth: usize) -> Result<EncodingCmap> {
    let Some(encoding) = encoding else { return Ok(EncodingCmap::unresolved()) };
    match res(objs, encoding) {
        Object::Name(n) => {
            let cid = if n == b"Identity-H" || n == b"Identity-V" { CidMap::Identity } else { CidMap::Unknown };
            Ok(EncodingCmap { wmode: predefined(n), space: predefined_space(n).map(<[Space]>::to_vec), cid })
        }
        Object::Stream(s) => {
            if depth >= MAX_USECMAP_DEPTH || !filters::is_decodable(&s.dict) {
                return Ok(EncodingCmap::unresolved());
            }
            let mut modes = Vec::new();
            if let Some(w) = s.dict.get("WMode") {
                modes.push(dict_wmode(res(objs, w)));
            }
            let data = match filters::decode_stream(s, limits, budget) {
                Ok(d) => d,
                Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
                Err(_) => return Ok(EncodingCmap::unresolved()),
            };
            let p = parse_program(&data);
            for w in &p.wmodes {
                modes.push(match w {
                    WModeTok::Int(0) => WMode::Horizontal,
                    WModeTok::Int(_) => WMode::Vertical,
                    WModeTok::Other => WMode::Unresolved,
                });
            }
            let named_identity = p.name.as_deref().filter(|n| *n == b"Identity-H" || *n == b"Identity-V");
            if named_identity == Some(b"Identity-V") {
                modes.push(WMode::Vertical);
            }
            // The used CMap: the program's `usecmap` (pdf.js prefers it), else the dictionary's /UseCMap.
            let used = match &p.usecmap {
                Some(Ok(name)) => Some(encoding_cmap(objs, Some(&Object::Name(name.clone())), limits, budget, depth.saturating_add(1))?),
                Some(Err(())) => Some(EncodingCmap::unresolved()),
                None => match s.dict.get("UseCMap") {
                    Some(u) => Some(encoding_cmap(objs, Some(u), limits, budget, depth.saturating_add(1))?),
                    None => None,
                },
            };
            if let Some(u) = &used {
                modes.push(u.wmode);
            }
            if p.stray {
                modes.push(WMode::Unresolved);
            }
            let mut space = if !p.space.is_empty() { Some(p.space.clone()) } else { used.as_ref().and_then(|u| u.space.clone()) };
            if p.malformed || p.stray || (named_identity.is_some() && space.as_deref() != Some(IDENTITY_SPACE)) {
                space = None;
            }
            let fallback = used.map_or(CidMap::Unknown, |u| u.cid);
            let cid = if p.malformed { CidMap::Unknown } else { CidMap::Ranges(p.cid, Box::new(fallback)) };
            Ok(EncodingCmap { wmode: combine(&modes), space, cid })
        }
        _ => Ok(EncodingCmap::unresolved()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_names_are_closed_lists() {
        assert_eq!(HORIZONTAL.len() + VERTICAL.len(), 61, "ISO 32000-2 Table 116 lists 61 names");
        assert!(HORIZONTAL.iter().all(|n| !VERTICAL.contains(n)));
        assert_eq!(predefined(b"Identity-H"), WMode::Horizontal);
        assert_eq!(predefined(b"H"), WMode::Horizontal);
        assert_eq!(predefined(b"V"), WMode::Vertical, "R9-F1: V does not end in -V and is vertical");
        assert_eq!(predefined(b"Identity-V"), WMode::Vertical);
        // Names outside the table (a real Adobe CMap that ISO 32000-2 does not list, and junk) are
        // never assumed horizontal.
        assert_eq!(predefined(b"UniJIS2004-UTF16-H"), WMode::Unresolved);
        assert_eq!(predefined(b"Identity-h"), WMode::Unresolved);
        assert_eq!(predefined(b""), WMode::Unresolved);
        // Every Table 116 name has a codespace; the table is sorted for the binary search.
        assert!(PREDEFINED_CODESPACES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(HORIZONTAL.iter().chain(VERTICAL.iter()).all(|n| predefined_space(n).is_some()));
        assert_eq!(predefined_space(b"UniJIS2004-UTF16-H"), None);
    }
}
