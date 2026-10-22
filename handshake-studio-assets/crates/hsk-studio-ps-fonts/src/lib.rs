// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure-Rust parsers for Type 1, CFF / Type 2, and TrueType fonts, plus
//! the shared geometry primitives they produce.
//!
//! This crate has **no stet-specific dependencies**, so it is usable on
//! its own for font-parsing workflows that don't need a PostScript
//! interpreter or a PDF renderer. The rest of the stet workspace uses it
//! as the foundational font/geometry layer.
//!
//! # What's here
//!
//! - [`geometry`] — `Matrix` (affine transforms), `PathSegment`, `PsPath`
//! - [`type1_parser`] / [`charstring`] — Adobe Type 1 parsing and
//!   charstring interpretation (eexec decryption included)
//! - [`cff_parser`] / [`type2_charstring`] — Compact Font Format parser
//!   and Type 2 charstring interpreter
//! - [`cff_writer`] — CID-keyed CFF writer, with a reader and subsetting
//!   for round trips
//! - [`type1_to_type2`] — Type 1 → Type 2 charstring conversion that
//!   keeps hints, hint replacement and flex
//! - [`cid_type0`] — the glyph data of CIDFontType 0 fonts with Type 1
//!   charstrings (CID map, subroutine map)
//! - [`truetype`] — TrueType table accessors, simple + composite glyph
//!   resolution, `glyf` → [`PsPath`] conversion
//! - [`encoding`] — StandardEncoding, ISOLatin1Encoding, SymbolEncoding,
//!   MacRoman, etc.
//! - [`agl`] — Adobe Glyph List (glyph name ↔ Unicode)
//! - [`to_unicode`] — PDF `/ToUnicode` CMap parser for text extraction
//!   (full UTF-16 destinations, multi-character strings)
//! - [`cid_unicode`] — Adobe CJK registry orderings (Japan1, CNS1, GB1,
//!   Korea1) CID ↔ Unicode
//! - [`system_fonts`] — platform font directory discovery
//! - [`symbolic`] — Symbol / ZapfDingbats substitutes (fork addition)
//! - [`FONT_SUBSTITUTIONS`] — 35-standard PostScript → free font substitution
//!   (fork: TeX Gyre and the symbolic substitutes; no URW++ fonts)
//!
//! # Quick example
//!
//! Parse a Type 1 font and inspect its glyph dictionary:
//!
//! ```no_run
//! use stet_fonts::type1_parser::parse_type1;
//!
//! let data = std::fs::read("qtmr.pfa")?;
//! let font = parse_type1(&data)?;
//! println!("font: {} ({} glyphs)", font.font_name, font.charstrings.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Scope and non-goals
//!
//! The surface is shaped around what stet needs to render PDFs and run a
//! PostScript Level 3 interpreter — whole-font parsing with glyph
//! outlines as `PsPath`. There is no text layout, no shaping, no colour
//! fonts, no variable-font interpolation, and no OpenType
//! GSUB/GPOS/BASE/GDEF tables. For a general-purpose font-layout
//! library, consider `ttf-parser`, `rustybuzz`, or `skrifa`.

pub mod agl;
pub mod cff_parser;
pub mod cff_writer;
pub mod charstring;
pub mod cid_type0;
pub mod cid_unicode;
pub mod encoding;
pub mod geometry;
pub mod symbolic;
pub mod system_fonts;
pub mod to_unicode;
pub mod truetype;
pub mod type1_parser;
pub mod type1_to_type2;
pub mod type2_charstring;

// Re-export core geometry types at crate root for convenience.
pub use geometry::{Matrix, PathSegment, PsPath};

/// Standard PostScript font name → free replacement font name (fork change, lane
/// SMB-EPS-FREE-FONTS: TeX Gyre Type 1 fonts under the GUST Font License for the 33 Latin
/// base 35 fonts, internal symbolic substitutes for Symbol and ZapfDingbats; upstream named
/// the URW++ base 35 fonts, which this fork does not ship).
pub const FONT_SUBSTITUTIONS: &[(&str, &str)] = &[
    // Times family → TeX Gyre Termes
    ("Times-Roman", "TeXGyreTermes-Regular"),
    ("Times-Bold", "TeXGyreTermes-Bold"),
    ("Times-Italic", "TeXGyreTermes-Italic"),
    ("Times-BoldItalic", "TeXGyreTermes-BoldItalic"),
    // Helvetica family → TeX Gyre Heros
    ("Helvetica", "TeXGyreHeros-Regular"),
    ("Helvetica-Bold", "TeXGyreHeros-Bold"),
    ("Helvetica-Oblique", "TeXGyreHeros-Italic"),
    ("Helvetica-BoldOblique", "TeXGyreHeros-BoldItalic"),
    // Helvetica Narrow → TeX Gyre Heros Condensed
    ("Helvetica-Narrow", "TeXGyreHerosCondensed-Regular"),
    ("Helvetica-Narrow-Bold", "TeXGyreHerosCondensed-Bold"),
    ("Helvetica-Narrow-Oblique", "TeXGyreHerosCondensed-Italic"),
    ("Helvetica-Narrow-BoldOblique", "TeXGyreHerosCondensed-BoldItalic"),
    // Courier family → TeX Gyre Cursor
    ("Courier", "TeXGyreCursor-Regular"),
    ("Courier-Bold", "TeXGyreCursor-Bold"),
    ("Courier-Oblique", "TeXGyreCursor-Italic"),
    ("Courier-BoldOblique", "TeXGyreCursor-BoldItalic"),
    // Arial family → TeX Gyre Heros (Helvetica metrics)
    ("ArialMT", "TeXGyreHeros-Regular"),
    ("Arial-BoldMT", "TeXGyreHeros-Bold"),
    ("Arial-ItalicMT", "TeXGyreHeros-Italic"),
    ("Arial-BoldItalicMT", "TeXGyreHeros-BoldItalic"),
    ("Arial", "TeXGyreHeros-Regular"),
    ("Arial-Bold", "TeXGyreHeros-Bold"),
    ("Arial-Italic", "TeXGyreHeros-Italic"),
    ("Arial-BoldItalic", "TeXGyreHeros-BoldItalic"),
    // Symbol fonts → the symbolic substitutes (TrueType, see `symbolic`)
    ("Symbol", "HskStudioPs-Symbol"),
    ("ZapfDingbats", "HskStudioPs-ZapfDingbats"),
    // Palatino → TeX Gyre Pagella
    ("Palatino-Roman", "TeXGyrePagella-Regular"),
    ("Palatino-Bold", "TeXGyrePagella-Bold"),
    ("Palatino-Italic", "TeXGyrePagella-Italic"),
    ("Palatino-BoldItalic", "TeXGyrePagella-BoldItalic"),
    // New Century Schoolbook → TeX Gyre Schola
    ("NewCenturySchlbk-Roman", "TeXGyreSchola-Regular"),
    ("NewCenturySchlbk-Bold", "TeXGyreSchola-Bold"),
    ("NewCenturySchlbk-Italic", "TeXGyreSchola-Italic"),
    ("NewCenturySchlbk-BoldItalic", "TeXGyreSchola-BoldItalic"),
    // Bookman → TeX Gyre Bonum
    ("Bookman-Light", "TeXGyreBonum-Regular"),
    ("Bookman-LightItalic", "TeXGyreBonum-Italic"),
    ("Bookman-Demi", "TeXGyreBonum-Bold"),
    ("Bookman-DemiItalic", "TeXGyreBonum-BoldItalic"),
    // AvantGarde → TeX Gyre Adventor
    ("AvantGarde-Book", "TeXGyreAdventor-Regular"),
    ("AvantGarde-BookOblique", "TeXGyreAdventor-Italic"),
    ("AvantGarde-Demi", "TeXGyreAdventor-Bold"),
    ("AvantGarde-DemiOblique", "TeXGyreAdventor-BoldItalic"),
    // TimesNewRoman variants (common in Windows-generated PDFs) → TeX Gyre Termes
    ("TimesNewRoman", "TeXGyreTermes-Regular"),
    ("TimesNewRomanPS", "TeXGyreTermes-Regular"),
    ("TimesNewRomanPSMT", "TeXGyreTermes-Regular"),
    ("TimesNewRoman-Bold", "TeXGyreTermes-Bold"),
    ("TimesNewRoman,Bold", "TeXGyreTermes-Bold"),
    ("TimesNewRomanPS-Bold", "TeXGyreTermes-Bold"),
    ("TimesNewRomanPS-BoldMT", "TeXGyreTermes-Bold"),
    ("TimesNewRoman-Italic", "TeXGyreTermes-Italic"),
    ("TimesNewRoman,Italic", "TeXGyreTermes-Italic"),
    ("TimesNewRomanPS-Italic", "TeXGyreTermes-Italic"),
    ("TimesNewRomanPS-ItalicMT", "TeXGyreTermes-Italic"),
    ("TimesNewRoman-BoldItalic", "TeXGyreTermes-BoldItalic"),
    ("TimesNewRomanPS-BoldItalic", "TeXGyreTermes-BoldItalic"),
    ("TimesNewRomanPS-BoldItalicMT", "TeXGyreTermes-BoldItalic"),
    // ZapfChancery → TeX Gyre Chorus
    ("ZapfChancery-MediumItalic", "TeXGyreChorus-MediumItalic"),
];

/// Look up a font substitution: "Helvetica" → "TeXGyreHeros-Regular".
pub fn find_substitution(name: &str) -> Option<&'static str> {
    for &(ps_name, free_name) in FONT_SUBSTITUTIONS {
        if ps_name == name {
            return Some(free_name);
        }
    }
    None
}

/// Test support shared by this crate's unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// Directory of the bundled URW fonts, for tests that need a real font.
    ///
    /// Fork change (lane SMB-EPS-FREE-FONTS): the URW++ fonts are not shipped
    /// (licence), so the upstream tests that read them cannot run here; they
    /// print `NOT_PROVEN` (Studio rule V4-03) and return early. The free
    /// substitutes are tested in `hsk-studio-ps` (`tests/free_fonts.rs`).
    pub(crate) fn font_dir() -> Option<PathBuf> {
        let _ = Path::new(env!("CARGO_MANIFEST_DIR"));
        println!("NOT_PROVEN: upstream URW font fixture removed for licensing (fork hsk-studio-ps-fonts)");
        None
    }
}
