// hsk-studio-ps-fonts: fork addition (lane SMB-EPS-FREE-FONTS, Studio, 2026-10-22).
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Symbolic substitutes for the two non-Latin base 35 fonts, Symbol and
//! ZapfDingbats.
//!
//! The fork ships no URW++ font programs. The 33 Latin base 35 fonts are
//! replaced by TeX Gyre Type 1 fonts whose built-in StandardEncoding works
//! unchanged; Symbol and ZapfDingbats have no free Type 1 equivalent with
//! their built-in encodings, so they are replaced by TrueType fonts (STIX
//! Two Math and DejaVu Sans) loaded with an encoding built here: every code
//! of the PLRM `SymbolEncoding` / ZapfDingbats built-in encoding is mapped
//! to a Unicode scalar (Adobe Glyph List for Symbol names, the ITC Zapf
//! Dingbats Glyph List for `a1`..`a191`), then through the substitute's own
//! `cmap` to a glyph. The glyph is named `uniXXXX` in the font's Encoding and
//! CharStrings, so a PDF reader resolves it through the same Unicode `cmap`.
//!
//! Symbol glyph names that the AGL assigns to the Private Use Area (bracket
//! and brace pieces, extenders, serif/sans copyright marks) fall back to the
//! standard Unicode characters for those pieces ([`symbol_pua_fallback`]).

use crate::agl::{glyph_name_to_unicode, zapf_dingbats_glyph_name_to_text};
use crate::encoding::{SYMBOL_ENCODING, ZAPFDINGBATS_ENCODING};

/// The built-in encoding a symbolic substitute is loaded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolicEncoding {
    /// PLRM `SymbolEncoding` (the built-in encoding of the Symbol font).
    Symbol,
    /// The built-in encoding of the ZapfDingbats font.
    ZapfDingbats,
}

impl SymbolicEncoding {
    /// The 256 glyph names of the encoding.
    pub fn names(self) -> &'static [&'static str; 256] {
        match self {
            Self::Symbol => &SYMBOL_ENCODING,
            Self::ZapfDingbats => &ZAPFDINGBATS_ENCODING,
        }
    }
}

/// Internal font name, embedded resource stem and encoding of each symbolic
/// substitute. The internal names are not the substitutes' own PostScript
/// names, so a document that asks for `DejaVuSans` or `STIXTwoMath-Regular`
/// by name never receives a re-encoded copy.
pub const SYMBOLIC_SUBSTITUTES: &[(&str, SymbolicEncoding)] = &[
    ("HskStudioPs-Symbol", SymbolicEncoding::Symbol),
    ("HskStudioPs-ZapfDingbats", SymbolicEncoding::ZapfDingbats),
];

/// The encoding of a symbolic substitute, by its internal font name.
pub fn symbolic_substitute(font_name: &str) -> Option<SymbolicEncoding> {
    SYMBOLIC_SUBSTITUTES
        .iter()
        .find(|(name, _)| *name == font_name)
        .map(|&(_, encoding)| encoding)
}

/// Standard Unicode characters for the Symbol glyph names that the Adobe
/// Glyph List maps into the Private Use Area (U+F6D9..U+F6DB, U+F8E5..U+F8FE).
/// The pieces are the Unicode 3.2 Miscellaneous Technical bracket pieces
/// (U+239B..U+23AE), U+23AF / U+23D0 for the arrow extenders and U+203E for
/// the radical extender; the serif and sans copyright, registered and
/// trademark marks map to the plain characters.
pub fn symbol_pua_fallback(name: &str) -> Option<u32> {
    Some(match name {
        "radicalex" => 0x203E,
        "arrowvertex" => 0x23D0,
        "arrowhorizex" => 0x23AF,
        "registerserif" | "registersans" => 0x00AE,
        "copyrightserif" | "copyrightsans" => 0x00A9,
        "trademarkserif" | "trademarksans" => 0x2122,
        "parenlefttp" => 0x239B,
        "parenleftex" => 0x239C,
        "parenleftbt" => 0x239D,
        "parenrighttp" => 0x239E,
        "parenrightex" => 0x239F,
        "parenrightbt" => 0x23A0,
        "bracketlefttp" => 0x23A1,
        "bracketleftex" => 0x23A2,
        "bracketleftbt" => 0x23A3,
        "bracketrighttp" => 0x23A4,
        "bracketrightex" => 0x23A5,
        "bracketrightbt" => 0x23A6,
        "bracelefttp" => 0x23A7,
        "braceleftmid" => 0x23A8,
        "braceleftbt" => 0x23A9,
        "braceex" => 0x23AA,
        "bracerighttp" => 0x23AB,
        "bracerightmid" => 0x23AC,
        "bracerightbt" => 0x23AD,
        "integralex" => 0x23AE,
        _ => return None,
    })
}

/// Unicode candidates for a glyph name of `encoding`, in preference order.
pub fn symbolic_candidates(encoding: SymbolicEncoding, name: &str) -> Vec<u32> {
    let mut out = Vec::with_capacity(2);
    match encoding {
        SymbolicEncoding::Symbol => {
            if let Some(cp) = glyph_name_to_unicode(name) {
                out.push(u32::from(cp));
            }
            if let Some(cp) = symbol_pua_fallback(name) {
                out.push(cp);
            }
        }
        SymbolicEncoding::ZapfDingbats => {
            if let Some(text) = zapf_dingbats_glyph_name_to_text(name)
                && let Some(c) = text.chars().next()
            {
                out.push(u32::from(c));
            }
        }
    }
    out
}

/// The glyph name used for a Unicode scalar in a substitute's Encoding and
/// CharStrings (`uniXXXX` in the BMP, `uXXXXX` beyond it).
pub fn unicode_glyph_name(cp: u32) -> String {
    if cp <= 0xFFFF { format!("uni{cp:04X}") } else { format!("u{cp:X}") }
}

/// The encoding of a symbolic substitute resolved against a font's Unicode
/// `cmap` (`lookup` returns the glyph id of a scalar). Each entry is the
/// glyph name for the code and its glyph id; a code whose name has no glyph
/// in the font keeps its original name with no glyph (`None`), so it shows
/// the font's `.notdef`.
pub fn resolve_symbolic_encoding(
    encoding: SymbolicEncoding,
    lookup: impl Fn(u32) -> Option<u16>,
) -> Vec<(String, Option<u16>)> {
    encoding
        .names()
        .iter()
        .map(|&name| {
            if name == ".notdef" {
                return (name.to_string(), None);
            }
            symbolic_candidates(encoding, name)
                .into_iter()
                .find_map(|cp| lookup(cp).filter(|&gid| gid != 0).map(|gid| (unicode_glyph_name(cp), Some(gid))))
                .unwrap_or_else(|| (name.to_string(), None))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_names_resolve_through_the_agl_and_the_fallback_table() {
        assert_eq!(symbolic_candidates(SymbolicEncoding::Symbol, "alpha"), vec![0x03B1]);
        assert!(symbolic_candidates(SymbolicEncoding::Symbol, "parenlefttp").contains(&0x239B));
        assert_eq!(symbolic_candidates(SymbolicEncoding::ZapfDingbats, "a20"), vec![0x2714]);
        assert_eq!(unicode_glyph_name(0x2714), "uni2714");
        assert_eq!(unicode_glyph_name(0x1F600), "u1F600");
        assert_eq!(symbolic_substitute("HskStudioPs-Symbol"), Some(SymbolicEncoding::Symbol));
        assert_eq!(symbolic_substitute("DejaVuSans"), None);
    }

    #[test]
    fn unresolved_codes_keep_their_names_without_a_glyph() {
        // A font with only U+03B1 (alpha, Symbol code 0x61).
        let resolved = resolve_symbolic_encoding(SymbolicEncoding::Symbol, |cp| (cp == 0x03B1).then_some(7));
        assert_eq!(resolved[0x61], ("uni03B1".to_string(), Some(7)));
        assert_eq!(resolved[0x62], ("beta".to_string(), None));
        assert_eq!(resolved[0], (".notdef".to_string(), None));
    }
}
