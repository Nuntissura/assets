//! Lane SMB-EPS-FREE-FONTS (Operator 2026-10-22): the vendored `stet` fork ships free fonts only,
//! the 35 standard PostScript font names resolve to them, and text set in them reaches the PDF with
//! the substitutes' own metrics and outlines.
use hsk_studio_ps::Interpreter;
use hsk_studio_ps::embedded_resources::{FONT_LICENCES, FREE_SYMBOLIC_FONTS, FREE_TYPE1_FONTS, register_all, unwrap_pfb};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use stet_core::file_store::FileStore;
use stet_fonts::charstring::execute_charstring;
use stet_fonts::system_fonts::extract_ps_name_from_name_table;
use stet_fonts::truetype::{get_advance_width, get_units_per_em, parse_cmap};
use stet_fonts::type1_parser::parse_type1;

/// The 35 standard names (PLRM base 35) and the free font each resolves to.
const BASE35: [(&str, &str); 35] = [
    ("Times-Roman", "TeXGyreTermes-Regular"),
    ("Times-Bold", "TeXGyreTermes-Bold"),
    ("Times-Italic", "TeXGyreTermes-Italic"),
    ("Times-BoldItalic", "TeXGyreTermes-BoldItalic"),
    ("Helvetica", "TeXGyreHeros-Regular"),
    ("Helvetica-Bold", "TeXGyreHeros-Bold"),
    ("Helvetica-Oblique", "TeXGyreHeros-Italic"),
    ("Helvetica-BoldOblique", "TeXGyreHeros-BoldItalic"),
    ("Helvetica-Narrow", "TeXGyreHerosCondensed-Regular"),
    ("Helvetica-Narrow-Bold", "TeXGyreHerosCondensed-Bold"),
    ("Helvetica-Narrow-Oblique", "TeXGyreHerosCondensed-Italic"),
    ("Helvetica-Narrow-BoldOblique", "TeXGyreHerosCondensed-BoldItalic"),
    ("Courier", "TeXGyreCursor-Regular"),
    ("Courier-Bold", "TeXGyreCursor-Bold"),
    ("Courier-Oblique", "TeXGyreCursor-Italic"),
    ("Courier-BoldOblique", "TeXGyreCursor-BoldItalic"),
    ("Symbol", "HskStudioPs-Symbol"),
    ("ZapfDingbats", "HskStudioPs-ZapfDingbats"),
    ("Palatino-Roman", "TeXGyrePagella-Regular"),
    ("Palatino-Bold", "TeXGyrePagella-Bold"),
    ("Palatino-Italic", "TeXGyrePagella-Italic"),
    ("Palatino-BoldItalic", "TeXGyrePagella-BoldItalic"),
    ("NewCenturySchlbk-Roman", "TeXGyreSchola-Regular"),
    ("NewCenturySchlbk-Bold", "TeXGyreSchola-Bold"),
    ("NewCenturySchlbk-Italic", "TeXGyreSchola-Italic"),
    ("NewCenturySchlbk-BoldItalic", "TeXGyreSchola-BoldItalic"),
    ("Bookman-Light", "TeXGyreBonum-Regular"),
    ("Bookman-LightItalic", "TeXGyreBonum-Italic"),
    ("Bookman-Demi", "TeXGyreBonum-Bold"),
    ("Bookman-DemiItalic", "TeXGyreBonum-BoldItalic"),
    ("AvantGarde-Book", "TeXGyreAdventor-Regular"),
    ("AvantGarde-BookOblique", "TeXGyreAdventor-Italic"),
    ("AvantGarde-Demi", "TeXGyreAdventor-Bold"),
    ("AvantGarde-DemiOblique", "TeXGyreAdventor-BoldItalic"),
    ("ZapfChancery-MediumItalic", "TeXGyreChorus-MediumItalic"),
];

/// File names of the URW++ base 35 programs the upstream crate embeds (and its CFF FontSet).
const URW_FILES: [&str; 36] = [
    "C059-BdIta", "C059-Bold", "C059-Italic", "C059-Roman", "D050000L", "NimbusMonoPS-Bold", "NimbusMonoPS-BoldItalic", "NimbusMonoPS-Italic",
    "NimbusMonoPS-Regular", "NimbusRoman-Bold", "NimbusRoman-BoldItalic", "NimbusRoman-Italic", "NimbusRoman-Regular", "NimbusSans-Bold",
    "NimbusSans-BoldItalic", "NimbusSans-Italic", "NimbusSans-Regular", "NimbusSansNarrow-Bold", "NimbusSansNarrow-BoldOblique",
    "NimbusSansNarrow-Oblique", "NimbusSansNarrow-Regular", "P052-Bold", "P052-BoldItalic", "P052-Italic", "P052-Roman", "StandardSymbolsPS",
    "URWBookman-Demi", "URWBookman-DemiItalic", "URWBookman-Light", "URWBookman-LightItalic", "URWGothic-Book", "URWGothic-BookOblique",
    "URWGothic-Demi", "URWGothic-DemiOblique", "Z003-MediumItalic", "NimbusRoman-Regular-CFF",
];

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable crate directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Type 1 advance width of `glyph` (font units, 1/1000 em) from the font program itself.
fn type1_width(program: &[u8], glyph: &str) -> f64 {
    let font = parse_type1(program).expect("type 1 font");
    let cs = font.charstrings.get(glyph).unwrap_or_else(|| panic!("{} has no glyph {glyph}", font.font_name));
    execute_charstring(cs, &font.subrs, font.len_iv, true).expect("charstring").width_x
}

/// TrueType advance width of the glyph for `cp`, in 1/1000 em, and that the font has the glyph.
fn truetype_width(font: &[u8], cp: u32) -> i64 {
    let gid = *parse_cmap(font).get(&cp).unwrap_or_else(|| panic!("no glyph for U+{cp:04X}"));
    assert_ne!(gid, 0, "U+{cp:04X} maps to .notdef");
    let advance = get_advance_width(font, gid).expect("hmtx advance");
    (f64::from(advance) * 1000.0 / f64::from(get_units_per_em(font))).round() as i64
}

/// Slice 1: no AGPL or GPL file anywhere in the vendored crates, no URW++ font program among the
/// embedded resources, and every embedded font is one of the free sets with its licence text: the
/// 33 TeX Gyre Type 1 programs (GUST Font License, an LPPL 1.3c instance; their notices name TeX
/// Gyre and the TeX Users Groups), STIX Two Math (SIL OFL 1.1) and DejaVu Sans (Bitstream Vera / Arev licences).
#[test]
fn embedded_fonts_are_free_and_their_licences_ship() {
    // Licence texts.
    let licences: BTreeMap<&str, &str> = FONT_LICENCES.iter().copied().collect();
    assert!(licences["Font/tex-gyre/GUST-FONT-LICENSE.txt"].contains("LaTeX Project Public License"));
    assert!(licences["Font/stix-two/OFL.txt"].contains("SIL OPEN FONT LICENSE Version 1.1"));
    assert!(licences["Font/dejavu/LICENSE"].contains("Bitstream Vera Fonts Copyright"));
    // The Type 1 programs: 33, each named as the substitution table says, each a TeX Gyre font.
    assert_eq!(FREE_TYPE1_FONTS.len(), 33);
    for &(name, pfb) in FREE_TYPE1_FONTS {
        let program = unwrap_pfb(pfb).unwrap_or_else(|| panic!("{name}: PFB segments"));
        let text = String::from_utf8_lossy(&program[..program.len().min(4096)]).into_owned();
        assert!(text.contains(&format!("/FontName /{name} def")), "{name}: FontName");
        // Every TeX Gyre notice names TeX Gyre and the TeX USERS GROUPS; 25 of 33 also name GUST
        // (Adventor and Pagella do not), so GUST is not asserted per font.
        assert!(text.contains("TeX Gyre") && text.contains("TeX USERS GROUPS"), "{name}: TeX Gyre notice");
        let font = parse_type1(&program).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(font.font_name, name);
        assert!(font.charstrings.len() > 300, "{name}: {} glyphs", font.charstrings.len());
        assert!(BASE35.iter().any(|&(_, sub)| sub == name), "{name} is a base 35 substitute");
    }
    let psnames: Vec<_> = FREE_SYMBOLIC_FONTS.iter().map(|&(_, ttf)| extract_ps_name_from_name_table(ttf)).collect();
    assert_eq!(psnames, [Some("STIXTwoMath-Regular".to_string()), Some("DejaVuSans".to_string())]);
    // Nothing of the URW++ set is registered.
    let mut store = FileStore::new();
    register_all(&mut store);
    for urw in URW_FILES {
        for path in [format!("Font/{urw}.t1"), format!("resources/Font/{urw}.t1"), format!("FontSet/{urw}.ps")] {
            assert!(store.get_embedded_file(&path).is_none(), "{path} is embedded");
        }
    }
    for &(_, sub) in &BASE35 {
        let ext = if sub.starts_with("HskStudioPs-") { "ttf" } else { "t1" };
        assert!(store.get_embedded_file(&format!("resources/Font/{sub}.{ext}")).is_some(), "{sub} is embedded");
    }
    // No copyleft licence file and no URW++ font file anywhere in the seven vendored crates.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for c in ["hsk-studio-ps", "hsk-studio-ps-core", "hsk-studio-ps-engine", "hsk-studio-ps-fonts", "hsk-studio-ps-graphics", "hsk-studio-ps-ops", "hsk-studio-ps-pdf"] {
        files_under(&crates.join(c), &mut files);
    }
    assert!(files.len() > 200, "{} files", files.len());
    for path in &files {
        let file = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        assert!(!file.starts_with("LICENSE-URW"), "{}", path.display());
        assert!(!URW_FILES.iter().any(|u| file.starts_with(u)), "{}", path.display());
        let bytes = std::fs::read(path).expect("readable");
        let upper = String::from_utf8_lossy(&bytes).to_uppercase();
        // Needles built at run time so this file does not match itself.
        for needle in [format!("GNU {}GENERAL PUBLIC LICENSE", "AFFERO "), format!("GNU {}GENERAL PUBLIC LICENSE", "")] {
            assert!(!upper.contains(&needle), "{needle} text in {}", path.display());
        }
    }
}

/// Slice 2: each of the 35 standard names loads its free substitute (never the fallback font): the
/// substitute is in FontDirectory under its own name after `findfont`, with the font type of its
/// format (Type 1 for TeX Gyre, Type 42 for the TrueType symbolic substitutes), and the font the
/// program sees keeps the requested name.
#[test]
fn standard_postscript_names_resolve_to_the_free_fonts() {
    let mut interp = Interpreter::builder().suppress_output().build();
    for (requested, substitute) in BASE35 {
        let font_type = if substitute.starts_with("HskStudioPs-") { 42 } else { 1 };
        let ps = format!(
            "/{requested} findfont dup /FontName get /{requested} ne {{ /hsk_wrong_fontname cvx exec }} if \
             pop FontDirectory /{substitute} known not {{ /hsk_substitute_not_loaded cvx exec }} if \
             FontDirectory /{substitute} get /FontType get {font_type} ne {{ /hsk_wrong_fonttype cvx exec }} if"
        );
        interp.exec(ps.as_bytes()).unwrap_or_else(|e| panic!("{requested} -> {substitute}: {e}"));
    }
}

/// Slice 2: text in Times-Roman, Helvetica, Courier, Symbol and ZapfDingbats renders to a PDF whose
/// fonts are embedded (FontFile / FontFile2) and whose Widths are the substitutes' own advance
/// widths: the TeX Gyre AFM values (independent of the program) and the Type 1 charstrings for the
/// Latin fonts; the hmtx advances of the glyphs the Symbol / ZapfDingbats encodings map to (alpha,
/// beta, gamma in STIX Two Math; U+2701 and U+2714 in DejaVu Sans) for the symbolic ones, whose
/// Encoding names those glyphs `uniXXXX`.
#[test]
fn base_fonts_render_to_pdf_with_their_own_metrics() {
    let ps = b"%!PS\n\
/Times-Roman findfont 12 scalefont setfont 10 100 moveto (AWay) show\n\
/Helvetica findfont 12 scalefont setfont 10 80 moveto (AWay) show\n\
/Courier findfont 12 scalefont setfont 10 60 moveto (AWay) show\n\
/Symbol findfont 12 scalefont setfont 10 40 moveto (abg) show\n\
/ZapfDingbats findfont 12 scalefont setfont 10 20 moveto (!4) show\n\
showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pdf = interp.render_to_pdf(ps, 72.0).expect("pdf");
    assert!(pdf.starts_with(b"%PDF-"));
    let text = String::from_utf8_lossy(&pdf).into_owned();
    // Font dictionaries (written uncompressed) by BaseFont: (first char, widths, raw dict).
    let mut fonts: BTreeMap<String, (i64, Vec<i64>, String)> = BTreeMap::new();
    let mut objects: BTreeMap<String, String> = BTreeMap::new();
    for chunk in text.split("endobj") {
        let Some(at) = chunk.find(" 0 obj") else { continue };
        let id = chunk[..at].rsplit(|c: char| !c.is_ascii_digit()).next().unwrap_or_default().to_string();
        let body = chunk[at..].to_string();
        objects.insert(id, body.clone());
        if !body.contains("/Type /Font") || !body.contains("/Widths") {
            continue;
        }
        let name = body.split("/BaseFont /").nth(1).and_then(|s| s.split(|c: char| c.is_whitespace() || c == '/').next()).expect("BaseFont").to_string();
        let first: i64 = body.split("/FirstChar ").nth(1).and_then(|s| s.split_whitespace().next()).and_then(|n| n.parse().ok()).expect("FirstChar");
        let widths = body.split("/Widths [").nth(1).and_then(|s| s.split(']').next()).expect("Widths").split_whitespace().map(|w| w.parse().expect("width")).collect();
        fonts.insert(name, (first, widths, body));
    }
    let width = |font: &str, code: u8| -> i64 {
        let (first, widths, _) = fonts.get(font).unwrap_or_else(|| panic!("{font} not in the PDF: {:?}", fonts.keys().collect::<Vec<_>>()));
        widths[usize::try_from(i64::from(code) - first).expect("code in range")]
    };
    let embedded = |font: &str, key: &str| {
        let (_, _, body) = &fonts[font];
        let descriptor = body.split("/FontDescriptor ").nth(1).and_then(|s| s.split_whitespace().next()).expect("descriptor ref");
        assert!(objects[descriptor].contains(key), "{font}: {key} in its FontDescriptor");
    };
    let program = |name: &str| unwrap_pfb(FREE_TYPE1_FONTS.iter().find(|(n, _)| *n == name).expect("embedded").1).expect("pfb");
    // TeX Gyre AFM (qtmr.afm, qhvr.afm, qcrr.afm of the TeX Gyre distribution): A W a y.
    let afm = [("Times-Roman", "TeXGyreTermes-Regular", [722, 944, 444, 500]), ("Helvetica", "TeXGyreHeros-Regular", [667, 944, 556, 500]), ("Courier", "TeXGyreCursor-Regular", [600; 4])];
    for (font, sub, expected) in afm {
        let program = program(sub);
        for ((code, glyph), afm_width) in [(b'A', "A"), (b'W', "W"), (b'a', "a"), (b'y', "y")].into_iter().zip(expected) {
            assert_eq!(width(font, code), afm_width, "{font} {glyph}: PDF width vs TeX Gyre AFM");
            assert_eq!(type1_width(&program, glyph).round() as i64, afm_width, "{sub} {glyph}: charstring width vs AFM");
        }
        embedded(font, "/FontFile");
    }
    let stix = FREE_SYMBOLIC_FONTS[0].1;
    for (code, cp) in [(b'a', 0x03B1), (b'b', 0x03B2), (b'g', 0x03B3)] {
        assert_eq!(width("Symbol", code), truetype_width(stix, cp), "Symbol code {code:#x} vs STIX Two Math U+{cp:04X}");
    }
    let dejavu = FREE_SYMBOLIC_FONTS[1].1;
    for (code, cp) in [(b'!', 0x2701), (b'4', 0x2714)] {
        assert_eq!(width("ZapfDingbats", code), truetype_width(dejavu, cp), "ZapfDingbats code {code:#x} vs DejaVu Sans U+{cp:04X}");
    }
    for (font, names) in [("Symbol", ["uni03B1", "uni03B2", "uni03B3"].as_slice()), ("ZapfDingbats", ["uni2701", "uni2714"].as_slice())] {
        embedded(font, "/FontFile2");
        let (_, _, body) = &fonts[font];
        let encoding = body.split("/Encoding ").nth(1).expect("Encoding");
        let encoding = match encoding.split_whitespace().next() {
            Some(reference) if reference.chars().all(|c| c.is_ascii_digit()) => objects[reference].clone(),
            _ => encoding.to_string(),
        };
        for name in names {
            assert!(encoding.contains(&format!("/{name}")), "{font}: {name} in the Encoding Differences");
        }
    }
}
