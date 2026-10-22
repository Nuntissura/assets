// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Embedded resource files for the stet facade crate.
//!
//! All PostScript resources (init scripts, encodings, fonts, etc.) are compiled
//! into the binary via `include_bytes!()`, eliminating the need for a
//! `resources/` directory at runtime.
//!
//! Fork change (lane SMB-EPS-FREE-FONTS): the 35 URW++ font programs and the
//! NimbusRoman CFF FontSet are removed; the fonts are the free sets below.

use std::sync::OnceLock;

use stet_core::file_store::FileStore;

/// Embedded CC0-licensed CMYK ICC profile for CMYK→sRGB conversion.
pub const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("../resources/default_cmyk.icc");

/// All embedded resource entries: (relative_path, data).
const ENTRIES: &[(&str, &[u8])] = &[
    // Init scripts
    (
        "Init/sysdict.ps",
        include_bytes!("../resources/Init/sysdict.ps"),
    ),
    (
        "Init/resourcecategories.ps",
        include_bytes!("../resources/Init/resourcecategories.ps"),
    ),
    (
        "Init/fontcategory.ps",
        include_bytes!("../resources/Init/fontcategory.ps"),
    ),
    (
        "Init/fontmapping.ps",
        include_bytes!("../resources/Init/fontmapping.ps"),
    ),
    (
        "Init/pdfextensions.ps",
        include_bytes!("../resources/Init/pdfextensions.ps"),
    ),
    // Encodings
    (
        "Encoding/StandardEncoding.ps",
        include_bytes!("../resources/Encoding/StandardEncoding.ps"),
    ),
    (
        "Encoding/ISOLatin1Encoding.ps",
        include_bytes!("../resources/Encoding/ISOLatin1Encoding.ps"),
    ),
    (
        "Encoding/SymbolEncoding.ps",
        include_bytes!("../resources/Encoding/SymbolEncoding.ps"),
    ),
    // ProcSet
    (
        "ProcSet/CIDInit.ps",
        include_bytes!("../resources/ProcSet/CIDInit.ps"),
    ),
    (
        "ProcSet/FontSetInit.ps",
        include_bytes!("../resources/ProcSet/FontSetInit.ps"),
    ),
    // CMap
    (
        "CMap/Identity-H.ps",
        include_bytes!("../resources/CMap/Identity-H.ps"),
    ),
    (
        "CMap/Identity-V.ps",
        include_bytes!("../resources/CMap/Identity-V.ps"),
    ),
    // ColorSpace
    (
        "ColorSpace/DefaultCMYK.ps",
        include_bytes!("../resources/ColorSpace/DefaultCMYK.ps"),
    ),
    (
        "ColorSpace/DefaultGray.ps",
        include_bytes!("../resources/ColorSpace/DefaultGray.ps"),
    ),
    (
        "ColorSpace/DefaultRGB.ps",
        include_bytes!("../resources/ColorSpace/DefaultRGB.ps"),
    ),
    // OutputDevice
    (
        "OutputDevice/png.ps",
        include_bytes!("../resources/OutputDevice/png.ps"),
    ),
    (
        "OutputDevice/null.ps",
        include_bytes!("../resources/OutputDevice/null.ps"),
    ),
    (
        "OutputDevice/pdf.ps",
        include_bytes!("../resources/OutputDevice/pdf.ps"),
    ),
    (
        "OutputDevice/viewer.ps",
        include_bytes!("../resources/OutputDevice/viewer.ps"),
    ),
];

/// Free Type 1 fonts replacing the 33 Latin URW++ base 35 fonts (fork change, lane
/// SMB-EPS-FREE-FONTS): TeX Gyre 2.004-2.501 by B. Jackowski and J. M. Nowacki (GUST
/// e-foundry), GUST Font License (an instance of the LPPL 1.3c), built from the URW
/// fonts URW++ released to GUST under the GFL. The .pfb files are byte-identical to the
/// TeX Gyre distribution (resources/Font/tex-gyre, licence and manifests beside them);
/// each is registered as `Font/<FontName>.t1` with its PFB segment headers removed
/// ([`unwrap_pfb`]), the form the interpreter runs.
pub const FREE_TYPE1_FONTS: &[(&str, &[u8])] = &[
    ("TeXGyreTermes-Regular", include_bytes!("../resources/Font/tex-gyre/qtmr.pfb")),
    ("TeXGyreTermes-Bold", include_bytes!("../resources/Font/tex-gyre/qtmb.pfb")),
    ("TeXGyreTermes-Italic", include_bytes!("../resources/Font/tex-gyre/qtmri.pfb")),
    ("TeXGyreTermes-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qtmbi.pfb")),
    ("TeXGyreHeros-Regular", include_bytes!("../resources/Font/tex-gyre/qhvr.pfb")),
    ("TeXGyreHeros-Bold", include_bytes!("../resources/Font/tex-gyre/qhvb.pfb")),
    ("TeXGyreHeros-Italic", include_bytes!("../resources/Font/tex-gyre/qhvri.pfb")),
    ("TeXGyreHeros-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qhvbi.pfb")),
    ("TeXGyreHerosCondensed-Regular", include_bytes!("../resources/Font/tex-gyre/qhvcr.pfb")),
    ("TeXGyreHerosCondensed-Bold", include_bytes!("../resources/Font/tex-gyre/qhvcb.pfb")),
    ("TeXGyreHerosCondensed-Italic", include_bytes!("../resources/Font/tex-gyre/qhvcri.pfb")),
    ("TeXGyreHerosCondensed-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qhvcbi.pfb")),
    ("TeXGyreCursor-Regular", include_bytes!("../resources/Font/tex-gyre/qcrr.pfb")),
    ("TeXGyreCursor-Bold", include_bytes!("../resources/Font/tex-gyre/qcrb.pfb")),
    ("TeXGyreCursor-Italic", include_bytes!("../resources/Font/tex-gyre/qcrri.pfb")),
    ("TeXGyreCursor-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qcrbi.pfb")),
    ("TeXGyrePagella-Regular", include_bytes!("../resources/Font/tex-gyre/qplr.pfb")),
    ("TeXGyrePagella-Bold", include_bytes!("../resources/Font/tex-gyre/qplb.pfb")),
    ("TeXGyrePagella-Italic", include_bytes!("../resources/Font/tex-gyre/qplri.pfb")),
    ("TeXGyrePagella-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qplbi.pfb")),
    ("TeXGyreSchola-Regular", include_bytes!("../resources/Font/tex-gyre/qcsr.pfb")),
    ("TeXGyreSchola-Bold", include_bytes!("../resources/Font/tex-gyre/qcsb.pfb")),
    ("TeXGyreSchola-Italic", include_bytes!("../resources/Font/tex-gyre/qcsri.pfb")),
    ("TeXGyreSchola-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qcsbi.pfb")),
    ("TeXGyreBonum-Regular", include_bytes!("../resources/Font/tex-gyre/qbkr.pfb")),
    ("TeXGyreBonum-Bold", include_bytes!("../resources/Font/tex-gyre/qbkb.pfb")),
    ("TeXGyreBonum-Italic", include_bytes!("../resources/Font/tex-gyre/qbkri.pfb")),
    ("TeXGyreBonum-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qbkbi.pfb")),
    ("TeXGyreAdventor-Regular", include_bytes!("../resources/Font/tex-gyre/qagr.pfb")),
    ("TeXGyreAdventor-Bold", include_bytes!("../resources/Font/tex-gyre/qagb.pfb")),
    ("TeXGyreAdventor-Italic", include_bytes!("../resources/Font/tex-gyre/qagri.pfb")),
    ("TeXGyreAdventor-BoldItalic", include_bytes!("../resources/Font/tex-gyre/qagbi.pfb")),
    ("TeXGyreChorus-MediumItalic", include_bytes!("../resources/Font/tex-gyre/qzcmi.pfb")),
];

/// TrueType substitutes for Symbol and ZapfDingbats, registered as `Font/<name>.ttf`
/// under the internal names of `stet_fonts::symbolic` (fork change): STIX Two Math
/// (SIL Open Font License 1.1, The STIX Fonts Project Authors) and DejaVu Sans 2.37
/// (Bitstream Vera and Arev font licences; DejaVu changes public domain). Both files
/// are byte-identical to their distributions (resources/Font/stix-two, resources/Font/dejavu).
pub const FREE_SYMBOLIC_FONTS: &[(&str, &[u8])] = &[
    ("HskStudioPs-Symbol", include_bytes!("../resources/Font/stix-two/STIXTwoMath-Regular.ttf")),
    ("HskStudioPs-ZapfDingbats", include_bytes!("../resources/Font/dejavu/DejaVuSans.ttf")),
];

/// The licence files of the embedded fonts (resource path, text), for audits.
pub const FONT_LICENCES: &[(&str, &str)] = &[
    ("Font/tex-gyre/GUST-FONT-LICENSE.txt", include_str!("../resources/Font/tex-gyre/GUST-FONT-LICENSE.txt")),
    ("Font/stix-two/OFL.txt", include_str!("../resources/Font/stix-two/OFL.txt")),
    ("Font/dejavu/LICENSE", include_str!("../resources/Font/dejavu/LICENSE")),
];

/// The Type 1 program of a PFB file: the payloads of its ASCII (1) and binary (2)
/// segments in order, up to the EOF segment (3). `None` for a malformed file.
pub fn unwrap_pfb(pfb: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(pfb.len());
    let mut rest = pfb;
    loop {
        match rest {
            [0x80, 3, ..] => return Some(out),
            // ASCII (1) and binary (2) payloads are copied as they are: the ASCII part ends
            // in `eexec`, which reads the binary part that follows (hex or binary).
            [0x80, 1 | 2, a, b, c, d, tail @ ..] => {
                let len = usize::try_from(u32::from_le_bytes([*a, *b, *c, *d])).ok()?;
                let (payload, next) = (tail.get(..len)?, tail.get(len..)?);
                out.extend_from_slice(payload);
                rest = next;
            }
            [] => return Some(out),
            _ => return None,
        }
    }
}

/// The unwrapped Type 1 programs, built once per process.
fn unwrapped_type1() -> &'static [(String, Vec<u8>)] {
    static UNWRAPPED: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    UNWRAPPED.get_or_init(|| {
        FREE_TYPE1_FONTS
            .iter()
            .filter_map(|&(name, pfb)| unwrap_pfb(pfb).map(|t1| (format!("Font/{name}.t1"), t1)))
            .collect()
    })
}

/// Register all embedded resource files into the FileStore's virtual filesystem.
///
/// Maps paths like "Init/sysdict.ps" and "resources/Init/sysdict.ps"
/// (both with and without the "resources/" prefix) so that `run` and
/// `.loadfont` can find them.
pub fn register_all(files: &mut FileStore) {
    let fonts = unwrapped_type1().iter().map(|(path, data)| (path.clone(), data.as_slice()));
    let symbolic = FREE_SYMBOLIC_FONTS.iter().map(|&(name, data)| (format!("Font/{name}.ttf"), data));
    let resources = ENTRIES.iter().map(|&(path, data)| (path.to_string(), data));
    for (path, data) in resources.chain(fonts).chain(symbolic) {
        files.add_embedded_file(&path, data);
        let full_path = format!("resources/{}", path);
        files.add_embedded_file(&full_path, data);
    }
}
