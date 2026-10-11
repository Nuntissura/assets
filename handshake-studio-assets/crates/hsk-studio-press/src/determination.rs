//! Positive determination of a font's writing mode (lead decision 2026-10-11T13:10:26Z, R9-F1),
//! shared by the content walk of the proof (`show_text`) and the redaction selector (`redaction`), so
//! both answer the same question the same way: is this font written horizontally, vertically, or is
//! that unknown?
//!
//! - A simple font (Type 1, MMType1, TrueType, Type 3) writes horizontally: its glyphs have one set of
//!   metrics, so only a composite font's CMap can select writing mode 1 (ISO 32000-2 9.2.4 "Glyph
//!   positioning and metrics"; wording read through a secondary source, the Apryse PDFNet `Font`
//!   reference that paraphrases the PDF Reference font chapter, not the ISO text itself).
//! - A composite (Type 0) font writes in the mode of its CMap, the font's `/Encoding` (ISO 32000-2
//!   9.7.6.1 Table 119): a predefined CMap name is horizontal or vertical only when it is one of the
//!   names of ISO 32000-2 9.7.5.2 Table 116 ([`HORIZONTAL`], [`VERTICAL`]); an embedded CMap stream
//!   (9.7.5, Table 118 "Additional entries in a CMap stream dictionary": `/WMode` 0 horizontal, 1
//!   vertical, default 0; `/UseCMap`) is vertical when its dictionary or its program sets `/WMode 1` or it uses
//!   (`/UseCMap`, `usecmap`) a vertical CMap, horizontal only when every `/WMode` it sets is 0 and
//!   every CMap it uses is horizontal. Anything else (another name, a `/WMode` other than 0 or 1, a
//!   program that cannot be decoded or tokenized, a font without `/Encoding`, `/UseCMap` nesting over
//!   [`MAX_USECMAP_DEPTH`]) is [`WMode::Unresolved`]: never assumed horizontal.
//!
//! Sources for the name lists: ISO 32000-2:2020 9.7.5.2 Table 116 as encoded in the PDF Association
//! Arlington PDF Model (`tsv/latest/FontType0.tsv`, key `Encoding`, PossibleValues; 61 names, read
//! 2026-10-11); the horizontal/vertical split cross-checked against the `IS_VERTICAL` flag of the CMap
//! data bundled with pdfminer.six 20260107 for the 59 names other than Identity-H/-V (0 differences).
//! Tests: `writing_mode_is_read_from_the_cmap`, `unknown_cmap_names_are_unresolved_writing_modes`,
//! `type3_and_vertical_text_never_read_as_cleared` and the
//! unit test below.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::parse_content;
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

fn wmode_value(v: &Object) -> WMode {
    match v.as_int() {
        Some(0) => WMode::Horizontal,
        Some(1) => WMode::Vertical,
        _ => WMode::Unresolved,
    }
}

/// Writing mode of the CMap a composite font names in `/Encoding` (`None`: the font has no
/// `/Encoding`). Decoding an embedded CMap is charged to `budget`; a budget overrun or cancellation is
/// returned, never read as a mode.
pub(crate) fn writing_mode(objs: &Objs, encoding: Option<&Object>, limits: &Limits, budget: &mut usize, depth: usize) -> Result<WMode> {
    let Some(encoding) = encoding else { return Ok(WMode::Unresolved) };
    match res(objs, encoding) {
        Object::Name(n) => Ok(predefined(n)),
        Object::Stream(s) => {
            if depth >= MAX_USECMAP_DEPTH {
                return Ok(WMode::Unresolved);
            }
            let mut modes = Vec::new();
            if let Some(w) = s.dict.get("WMode") {
                modes.push(wmode_value(res(objs, w)));
            }
            if let Some(used) = s.dict.get("UseCMap") {
                modes.push(writing_mode(objs, Some(used), limits, budget, depth.saturating_add(1))?);
            }
            if !filters::is_decodable(&s.dict) {
                return Ok(WMode::Unresolved);
            }
            let data = match filters::decode_stream(s, limits, budget) {
                Ok(d) => d,
                Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
                Err(_) => return Ok(WMode::Unresolved),
            };
            // A CMap program is PostScript-like syntax the content tokenizer reads: `/WMode 1 def`
            // is a `def` with operands [/WMode 1]; `/UniJIS-UCS2-H usecmap` names a predefined CMap.
            let Ok(ops) = parse_content(&data, limits) else { return Ok(WMode::Unresolved) };
            for op in &ops {
                match (op.name.as_slice(), op.operands.as_slice()) {
                    (b"def", [Object::Name(k), v]) if k == b"WMode" => modes.push(wmode_value(v)),
                    (b"usecmap", [Object::Name(n)]) => modes.push(predefined(n)),
                    (b"usecmap", _) => modes.push(WMode::Unresolved),
                    _ => {}
                }
            }
            Ok(combine(&modes))
        }
        _ => Ok(WMode::Unresolved),
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
    }
}
