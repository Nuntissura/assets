//! In-crate proofs of the export builder: an independent render through hayro-interpret 0.7.0 into
//! the crate's paint device (third-party interpreter, not the writer's code), and dictionary-level
//! read-back (TrimBox, output intent profile bytes, `/ToUnicode`, hostile title strings) through the
//! crate's strict reader `PdfDoc::open`.
use super::*;
use crate::cmap::parse_to_unicode;
use crate::flatten::RENDER_STACK_BYTES;
use crate::paint::Painter;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::{Context, InterpreterCache, InterpreterSettings, InterpreterWarning, interpret_page};
use kurbo::Affine;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Inter Regular 4.x (OFL, licence file beside it in the type crate's fixtures). Glyph ids and
/// advances read with fontTools 4.62.1: `H` = gid 161, advance 2084 / 2816 em = 740;
/// `i` = gid 679, advance 668 / 2816 em = 237.
const INTER: &[u8] = include_bytes!("../../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const SRGB: &[u8] = include_bytes!("../../../hsk-studio-develop/tests/profiles/sRGB-v4.icc");
const H: u16 = 161;
const I: u16 = 679;
const PAGE_W: u32 = 595;
const PAGE_H: u32 = 842;

fn square(x: f64, y: f64, s: f64) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(x, y),
        PathCommand::LineTo(x + s, y),
        PathCommand::LineTo(x + s, y + s),
        PathCommand::LineTo(x, y + s),
        PathCommand::Close,
    ]
}

/// Page 1 of the acceptance document: red square, 2x2 RGB image with alpha, "Hi" in Inter.
fn page_one(ex: &mut PdfExport) -> PageHandle {
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, f64::from(PAGE_W), f64::from(PAGE_H)], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.fill_path(p, &square(100.0, 100.0, 100.0), FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    // Rows top to bottom: red, green / blue, white. Alpha: opaque, opaque / half, clear.
    let samples = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    let alpha = [255, 255, 128, 0];
    let img = ImageSpec { width: 2, height: 2, bits: 8, colour: ImageColour::DeviceRgb, samples: &samples, alpha: Some(&alpha) };
    ex.draw_image(p, img, [50.0, 0.0, 0.0, 50.0, 300.0, 300.0]).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(H, 740), (I, 237)], &[(H, "H"), (I, "i")]).unwrap();
    ex.show_glyphs(p, font, 48.0, [100.0, 500.0], Colour::DeviceGray(0.0), &[(H, 0.0, 0.0), (I, 35.52, 0.0)]).unwrap();
    p
}

/// RGB raster of page `index` at 1 px per point; also the count of hayro font warnings.
fn render(bytes: Vec<u8>, index: usize) -> (Vec<u8>, u32) {
    let cancel = CancellationToken::default();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(RENDER_STACK_BYTES)
            .spawn_scoped(scope, || {
                let font_warnings = Arc::new(AtomicU32::new(0));
                let fw = font_warnings.clone();
                let pdf = Pdf::new(bytes).expect("hayro opens the export");
                let pages = pdf.pages();
                let page = &pages[index];
                let settings = InterpreterSettings {
                    render_annotations: false,
                    warning_sink: Arc::new(move |w| {
                        if matches!(w, InterpreterWarning::UnsupportedFont) {
                            fw.fetch_add(1, Ordering::Relaxed);
                        }
                    }),
                    ..InterpreterSettings::default()
                };
                let device = Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, f64::from(PAGE_H)]);
                let cache = InterpreterCache::new();
                let rect = kurbo::Rect::new(0.0, 0.0, f64::from(PAGE_W), f64::from(PAGE_H));
                let mut ctx = Context::new(device, rect, &cache, pdf.xref(), settings);
                let mut painter = Painter::new(PAGE_W as usize, PAGE_H as usize, 1 << 30, &cancel).unwrap();
                interpret_page(page, &mut ctx, &mut painter);
                (std::mem::take(&mut painter.px), font_warnings.load(Ordering::Relaxed))
            })
            .unwrap()
            .join()
            .unwrap()
    })
}

/// Pixel at device (x, y); y down from the top.
fn px(raster: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = (y.saturating_mul(PAGE_W).saturating_add(x)).saturating_mul(3) as usize;
    let s = &raster[i..i.saturating_add(3)];
    [s[0], s[1], s[2]]
}
fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[test]
fn hayro_renders_the_export() {
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    page_one(&mut ex);
    let (bytes, receipt) = ex.finish(&CancellationToken::default()).unwrap();
    assert_eq!((receipt.pages, receipt.fonts_embedded, receipt.images), (1, 1, 1));
    let (r, font_warnings) = render(bytes, 0);
    assert_eq!(font_warnings, 0, "the embedded CIDFontType2 is read, not substituted");
    let y = |pt: f64| (f64::from(PAGE_H) - pt) as u32;
    // Square (100..200 pt): red inside, white just outside.
    assert_eq!(px(&r, 150, y(150.0)), [255, 0, 0]);
    assert_eq!(px(&r, 50, y(50.0)), [255, 255, 255]);
    assert_eq!(px(&r, 205, y(150.0)), [255, 255, 255]);
    // Image (300..350 pt): quadrant centres.
    assert!(near(px(&r, 312, y(337.5)), [255, 0, 0], 8), "{:?}", px(&r, 312, y(337.5)));
    assert!(near(px(&r, 337, y(337.5)), [0, 255, 0], 8), "{:?}", px(&r, 337, y(337.5)));
    // Half-transparent blue over white: about (127, 127, 255).
    assert!(near(px(&r, 312, y(312.5)), [127, 127, 255], 12), "{:?}", px(&r, 312, y(312.5)));
    assert_eq!(px(&r, 337, y(312.5)), [255, 255, 255], "alpha 0 leaves the page white");
    // Text "Hi" at (100, 500), 48 pt: ink between the baseline and the cap height.
    let inked = |x0: u32, x1: u32, y0: u32, y1: u32| {
        (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (xx, yy))).filter(|(xx, yy)| px(&r, *xx, *yy) != [255, 255, 255]).count()
    };
    let text = inked(100, 150, y(540.0), y(499.0));
    assert!(text > 200, "glyph ink {text}");
    // The H stem: a dark pixel inside the left stem at mid cap height.
    assert!(px(&r, 104, y(515.0))[0] < 64, "{:?}", px(&r, 104, y(515.0)));
    // Nothing else carries ink.
    let regions = [(98, 203, y(203.0), y(97.0)), (298, 353, y(353.0), y(297.0)), (95, 160, y(545.0), y(490.0))];
    let mut stray = 0usize;
    for yy in 0..PAGE_H {
        for xx in 0..PAGE_W {
            let inside = regions.iter().any(|(x0, x1, y0, y1)| xx >= *x0 && xx < *x1 && yy >= *y0 && yy < *y1);
            if !inside && px(&r, xx, yy) != [255, 255, 255] {
                stray += 1;
            }
        }
    }
    assert_eq!(stray, 0);
}

/// Glyph `gid`'s bytes in `glyf`, read with a test-side directory/head/loca walk.
/// Test oracle over checked-in fixtures: an out-of-range read panics and fails the test.
#[allow(clippy::arithmetic_side_effects)]
fn glyph_bytes(font: &[u8], gid: usize) -> Vec<u8> {
    let u16_at = |b: &[u8], i: usize| usize::from(u16::from_be_bytes([b[i], b[i + 1]]));
    let u32_at = |b: &[u8], i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
    let table = |tag: &[u8]| {
        let n = u16_at(font, 4);
        let r = (0..n).map(|i| 12 + 16 * i).find(|r| &font[*r..*r + 4] == tag).unwrap();
        &font[u32_at(font, r + 8)..u32_at(font, r + 8) + u32_at(font, r + 12)]
    };
    let (head, loca, glyf) = (table(b"head"), table(b"loca"), table(b"glyf"));
    let (a, b) = if u16_at(head, 50) == 1 {
        (u32_at(loca, 4 * gid), u32_at(loca, 4 * gid + 4))
    } else {
        (2 * u16_at(loca, 2 * gid), 2 * u16_at(loca, 2 * gid + 2))
    };
    glyf[a..b].to_vec()
}

/// Subsetting (default on): the drawn glyphs keep their ids and outline bytes, an unused glyph is
/// empty, the font is named with a six-letter tag, and hayro still draws it (test above).
#[test]
fn subset_font_keeps_ids_and_outlines() {
    let cancel = CancellationToken::default();
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    page_one(&mut ex);
    let (bytes, _) = ex.finish(&cancel).unwrap();
    let doc = PdfDoc::open(&bytes, &Limits::default(), &cancel).unwrap();
    let res = doc.dict_of(doc.pages[0]).unwrap().get("Resources").and_then(Object::as_dict).unwrap();
    let font = res.get("Font").and_then(Object::as_dict).unwrap().get("F0").map(|o| doc.resolve(o)).and_then(Object::as_dict).unwrap();
    let cid = doc.resolve(&font.get("DescendantFonts").and_then(Object::as_array).unwrap()[0]).as_dict().unwrap();
    let fd = cid.get("FontDescriptor").map(|o| doc.resolve(o)).and_then(Object::as_dict).unwrap();
    let fname = String::from_utf8(fd.get("FontName").unwrap().as_name().unwrap().to_vec()).unwrap();
    assert!(fname.len() > 7 && fname[..6].bytes().all(|b| b.is_ascii_uppercase()) && &fname[6..7] == "+", "{fname}");
    assert_eq!(font.get("BaseFont").unwrap().as_name().unwrap(), fname.as_bytes());
    let Some(Object::Stream(ff)) = fd.get("FontFile2").map(|o| doc.resolve(o)) else { panic!() };
    let mut budget = usize::MAX;
    let sub = crate::filters::decode_stream(ff, &Limits::default(), &mut budget).unwrap();
    assert!(sub.len() < INTER.len() / 10, "{}", sub.len());
    for g in [usize::from(H), usize::from(I), 0] {
        assert_eq!(glyph_bytes(&sub, g), glyph_bytes(INTER, g), "glyph {g}");
    }
    assert!(!glyph_bytes(INTER, 937).is_empty() && glyph_bytes(&sub, 937).is_empty(), "w is not used");
    // The subset still reads as a TrueType font with the same glyph count.
    assert_eq!(super::sfnt::read(&sub, 0).map(|f| f.num_glyphs).ok(), super::sfnt::read(INTER, 0).map(|f| f.num_glyphs).ok());
}

#[test]
fn dictionaries_read_back_through_the_strict_reader() {
    // V4-08 hostile text: escapes, decoy syntax, whitespace and non-ASCII (UTF-16BE with BOM).
    for title in ["a)b\\c(d", ") >> endobj %%EOF /Evil (", "tab\there\r\nline", "Druck \u{e9}\u{1F600} ()"] {
        let cancel = CancellationToken::default();
        let ident = "FOGRA39 (x)\\";
        let options = ExportOptions {
            info_title: Some(title),
            output_intent: Some(OutputIntent {
                icc: SRGB,
                components: 3,
                output_condition_identifier: ident,
                output_condition: Some("Coated \u{e9}"),
                registry: Some("http://www.color.org"),
                subtype: OutputIntentSubtype::GtsPdfx,
            }),
            subset_fonts: false,
            ..ExportOptions::default()
        };
        let mut ex = PdfExport::new(options, &Limits::default()).unwrap();
        page_one(&mut ex);
        let trim = [9.0, 9.0, 603.0, 783.0];
        ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 612.0, 792.0], trim_box_pt: Some(trim), bleed_box_pt: Some([0.0, 0.0, 612.0, 792.0]) }).unwrap();
        let (bytes, receipt) = ex.finish(&cancel).unwrap();
        let doc = PdfDoc::open(&bytes, &Limits::default(), &cancel).unwrap();
        assert_eq!(doc.pages.len(), 2);
        let page2 = doc.dict_of(doc.pages[1]).unwrap();
        assert_eq!(doc.rect_of(page2.get("TrimBox")), Some(trim));
        assert_eq!(doc.dict_of(doc.pages[0]).unwrap().get("TrimBox"), None);
        // Output intent: identifier and the decoded profile bytes equal the input.
        let oi = doc.catalog().unwrap().get("OutputIntents").and_then(Object::as_array).unwrap()[0].as_dict().unwrap().clone();
        assert!(oi.get("S").unwrap().is_name("GTS_PDFX"));
        let Some(Object::Str(id)) = oi.get("OutputConditionIdentifier") else { panic!() };
        assert_eq!(text_string(id), ident);
        let Some(Object::Stream(icc)) = oi.get("DestOutputProfile").map(|o| doc.resolve(o)) else { panic!() };
        let mut budget = usize::MAX;
        let decoded = crate::filters::decode_stream(icc, &Limits::default(), &mut budget).unwrap();
        assert_eq!(sha256_hex(&decoded), sha256_hex(SRGB));
        assert_eq!(receipt.effective_destination_profile_sha256, Some(sha256_hex(SRGB)));
        assert_eq!(receipt.effective_output_condition.as_deref(), Some(ident));
        // Title round-trips exactly.
        let info = doc.info_ref().and_then(|r| doc.dict_of(r.num)).unwrap();
        let Some(Object::Str(t)) = info.get("Title") else { panic!() };
        assert_eq!(text_string(t), title);
        // /ToUnicode maps the two glyph ids back to "H" and "i".
        let res = doc.dict_of(doc.pages[0]).unwrap().get("Resources").and_then(Object::as_dict).unwrap();
        let font = res.get("Font").and_then(Object::as_dict).unwrap().get("F0").map(|o| doc.resolve(o)).and_then(Object::as_dict).unwrap();
        assert!(font.get("Encoding").unwrap().is_name("Identity-H"));
        let Some(Object::Stream(tu)) = font.get("ToUnicode").map(|o| doc.resolve(o)) else { panic!() };
        let cmap = parse_to_unicode(&crate::filters::decode_stream(tu, &Limits::default(), &mut budget).unwrap()).unwrap();
        assert_eq!((cmap.lookup(u32::from(H)), cmap.lookup(u32::from(I))), (Some("H"), Some("i")));
        let desc = font.get("DescendantFonts").and_then(Object::as_array).unwrap()[0].clone();
        let cid = doc.resolve(&desc).as_dict().unwrap();
        assert!(cid.get("CIDToGIDMap").unwrap().is_name("Identity"));
        let fd = cid.get("FontDescriptor").map(|o| doc.resolve(o)).and_then(Object::as_dict).unwrap();
        let Some(Object::Stream(ff)) = fd.get("FontFile2").map(|o| doc.resolve(o)) else { panic!() };
        assert_eq!(sha256_hex(&crate::filters::decode_stream(ff, &Limits::default(), &mut budget).unwrap()), sha256_hex(INTER));
    }
}

/// The soft-masked group's structure (the render-back device skips soft masks): the form XObject is
/// a transparency group, its `/ExtGState` carries `/BM`, `/ca` and a luminosity `/SMask` whose `/G`
/// is a DeviceGray transparency group drawing the mask image (bytes equal the input), `/BC [0]`.
#[test]
fn soft_mask_structure_reads_back() {
    let cancel = CancellationToken::default();
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 100.0, 100.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &square(0.0, 0.0, 100.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    let px = [10u8, 20, 30, 40];
    let mask = SoftMask::luminosity(ImageSpec { width: 2, height: 2, bits: 8, colour: ImageColour::DeviceGray, samples: &px, alpha: None }, [100.0, 0.0, 0.0, 100.0, 0.0, 0.0]);
    ex.end_group(p, GroupStyle { opacity: 0.25, blend: BlendMode::Screen, isolated: true, knockout: false, soft_mask: Some(mask) }).unwrap();
    let (bytes, _) = ex.finish(&cancel).unwrap();
    let doc = PdfDoc::open(&bytes, &Limits::default(), &cancel).unwrap();
    let res = doc.dict_of(doc.pages[0]).unwrap().get("Resources").and_then(Object::as_dict).unwrap();
    let gs = res.get("ExtGState").and_then(Object::as_dict).unwrap().entries[0].1.clone();
    let gs = doc.resolve(&gs).as_dict().unwrap();
    assert!(gs.get("BM").unwrap().is_name("Screen"));
    assert_eq!(gs.get("ca").and_then(Object::as_f64), Some(0.25));
    let sm = gs.get("SMask").and_then(Object::as_dict).unwrap();
    assert!(sm.get("S").unwrap().is_name("Luminosity"));
    assert_eq!(sm.get("BC").and_then(Object::as_array).map(|a| a.len()), Some(1));
    let Some(Object::Stream(g)) = sm.get("G").map(|o| doc.resolve(o)) else { panic!() };
    let grp = g.dict.get("Group").and_then(Object::as_dict).unwrap();
    assert!(grp.get("CS").unwrap().is_name("DeviceGray"));
    let img = g.dict.get("Resources").and_then(Object::as_dict).unwrap().get("XObject").and_then(Object::as_dict).unwrap().get("MIm0").unwrap();
    let Object::Stream(img) = doc.resolve(img) else { panic!() };
    let mut budget = usize::MAX;
    assert_eq!(crate::filters::decode_stream(img, &Limits::default(), &mut budget).unwrap(), px);
    let form = res.get("XObject").and_then(Object::as_dict).unwrap().entries[0].1.clone();
    let Object::Stream(form) = doc.resolve(&form) else { panic!() };
    let fg = form.dict.get("Group").and_then(Object::as_dict).unwrap();
    assert!(fg.get("S").unwrap().is_name("Transparency") && fg.get("I") == Some(&Object::Bool(true)));
}
