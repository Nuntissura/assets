//! PDF export builder (lane SMB-LAYOUT-PDF-EXPORT slice 1) through the public API: structure read
//! back by `PdfDoc`, determinism, and typed refusals for hostile sizes, fonts and profiles. The
//! hayro render and dictionary-level read-back are unit tests in `src/export/tests.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::*;

/// Inter Regular (OFL; licence beside it). `H` = gid 161 (740/1000 em), `i` = gid 679 (237).
const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
/// Noto Sans SC Regular: OpenType with CFF outlines (`OTTO`).
const NOTO_SC_CFF: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/NotoSansSC-Regular.otf");
const SRGB: &[u8] = include_bytes!("../../hsk-studio-develop/tests/profiles/sRGB-v4.icc");
const A4: [f64; 4] = [0.0, 0.0, 595.276, 841.89];
const LETTER: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

fn page(media: [f64; 4]) -> PageSpec {
    PageSpec { media_box_pt: media, trim_box_pt: None, bleed_box_pt: None }
}
fn square() -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(100.0, 100.0),
        PathCommand::LineTo(200.0, 100.0),
        PathCommand::LineTo(200.0, 200.0),
        PathCommand::LineTo(100.0, 200.0),
        PathCommand::Close,
    ]
}

/// The two-page acceptance document.
fn build(title: &str) -> (Vec<u8>, ExportReceipt) {
    let options = ExportOptions {
        info_title: Some(title),
        output_intent: Some(OutputIntent {
            icc: SRGB,
            components: 3,
            output_condition_identifier: "sRGB IEC61966-2.1 (x)\\",
            output_condition: None,
            registry: Some("http://www.color.org"),
            subtype: OutputIntentSubtype::GtsPdfx,
        }),
        ..ExportOptions::default()
    };
    let mut ex = PdfExport::new(options, &Limits::default()).unwrap();
    let p1 = ex.add_page(page(A4)).unwrap();
    ex.fill_path(p1, &square(), FillRule::EvenOdd, Colour::DeviceCmyk([0.0, 1.0, 1.0, 0.0])).unwrap();
    let icc = ex.embed_icc(SRGB, 3).unwrap();
    let style = StrokeStyle { width_pt: 2.5, cap: LineCap::Round, join: LineJoin::Bevel };
    ex.stroke_path(p1, &square(), Colour::IccBased(icc, [0.0, 0.0, 1.0, 0.0]), style).unwrap();
    let samples = [0u8, 64, 128, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let img = ImageSpec { width: 2, height: 2, bits: 8, colour: ImageColour::IccBased(icc), samples: &samples, alpha: None };
    ex.draw_image(p1, img, [50.0, 0.0, 0.0, 50.0, 300.0, 300.0]).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740), (679, 237)], &[(161, "H"), (679, "i")]).unwrap();
    ex.show_glyphs(p1, font, 24.0, [72.0, 700.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0), (679, 17.76, 0.0)]).unwrap();
    let p2 = ex
        .add_page(PageSpec { media_box_pt: LETTER, trim_box_pt: Some([9.0, 9.0, 603.0, 783.0]), bleed_box_pt: Some(LETTER) })
        .unwrap();
    // A second font with the same bytes shares the embedded font file.
    let again = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p2, again, 12.0, [72.0, 72.0], Colour::DeviceRgb([0.0, 0.0, 0.0]), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap()
}

#[test]
fn two_page_export_reads_back() {
    let cancel = CancellationToken::default();
    let (bytes, receipt) = build("Proof (1) \\ caf\u{e9}");
    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    let doc = PdfDoc::open(&bytes, &Limits::default(), &cancel).unwrap();
    let inspection = doc.inspect();
    assert_eq!((inspection.page_count, inspection.encrypted, inspection.signed), (2, false, false));
    let proposal = doc.proposal(&cancel).unwrap();
    assert_eq!(proposal.pages[0].media_box, Some(A4));
    assert_eq!(proposal.pages[1].media_box, Some(LETTER));
    assert!(proposal.pages.iter().all(|p| p.content_streams == 1));
    // Receipt values are read back from the written file.
    assert_eq!(receipt.pages, 2);
    assert_eq!((receipt.fonts_embedded, receipt.images), (2, 1));
    assert_eq!(receipt.bytes, bytes.len());
    assert_eq!(receipt.sha256_hex, sha256_hex(&bytes));
    assert_eq!(receipt.version, "1.7");
    assert_eq!(receipt.effective_destination_profile_sha256, Some(sha256_hex(SRGB)));
    assert_eq!(receipt.effective_output_condition.as_deref(), Some("sRGB IEC61966-2.1 (x)\\"));
    assert_eq!(receipt.effective_oc_registry.as_deref(), Some("http://www.color.org"));
    assert_eq!(receipt.effective_pdfx_profile, None);
    assert_eq!(receipt.conformance, "not_proven");
    // The font file is embedded once, subset to the used glyphs (Inter is 680 KB).
    assert!(bytes.len() < 40_000, "{}", bytes.len());
}

#[test]
fn export_is_deterministic() {
    let (a, ra) = build("same");
    let (b, rb) = build("same");
    assert_eq!(a, b, "two exports of the same input are byte-identical");
    assert_eq!(ra, rb);
    let (c, _) = build("other");
    assert_ne!(a, c);
}

#[test]
fn hostile_sizes_are_typed_refusals() {
    let cancel = CancellationToken::default();
    let lim = Limits::default();
    let mut ex = PdfExport::new(ExportOptions::default(), &lim).unwrap();
    let p = ex.add_page(page(A4)).unwrap();
    let img = |w, h, bits, samples: &'static [u8]| ImageSpec { width: w, height: h, bits, colour: ImageColour::DeviceCmyk, samples, alpha: None };
    let at = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    // dims_u32_max: width x height x 4 components x 2 bytes overflows; refused before allocation.
    assert_eq!(ex.draw_image(p, img(u32::MAX, u32::MAX, 16, &[0; 16]), at), Err(ExportError::ImageSizeOverflow));
    assert_eq!(ex.draw_image(p, img(65_536, 65_536, 8, &[0; 16]), at), Err(ExportError::StreamOverBudget));
    assert_eq!(ex.draw_image(p, img(2, 2, 8, &[0; 15]), at), Err(ExportError::ImageSamplesLength));
    assert_eq!(ex.draw_image(p, img(0, 2, 8, &[]), at), Err(ExportError::ImageDims));
    assert_eq!(ex.draw_image(p, img(1, 1, 4, &[0; 4]), at), Err(ExportError::UnsupportedBits));
    assert_eq!(ex.draw_image(p, img(1, 1, 8, &[0; 4]), [f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]), Err(ExportError::NonFinite));
    // Non-finite and out-of-range geometry and colour.
    assert_eq!(ex.add_page(page([0.0, 0.0, f64::NAN, 10.0])), Err(ExportError::NonFinite));
    assert_eq!(ex.add_page(page([0.0, 0.0, 0.0, 10.0])), Err(ExportError::InvalidBox));
    let outside = PageSpec { media_box_pt: LETTER, trim_box_pt: Some([0.0, 0.0, 700.0, 10.0]), bleed_box_pt: None };
    assert_eq!(ex.add_page(outside), Err(ExportError::BoxOutsideMedia));
    let inf = [PathCommand::MoveTo(0.0, 0.0), PathCommand::LineTo(f64::INFINITY, 1.0)];
    assert_eq!(ex.fill_path(p, &inf, FillRule::NonZero, Colour::DeviceGray(0.5)), Err(ExportError::NonFinite));
    assert_eq!(ex.fill_path(p, &[PathCommand::LineTo(1.0, 1.0)], FillRule::NonZero, Colour::DeviceGray(0.5)), Err(ExportError::InvalidPath));
    assert_eq!(ex.fill_path(p, &square(), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.5, 0.0])), Err(ExportError::ColourOutOfRange));
    assert_eq!(ex.fill_path(p, &square(), FillRule::NonZero, Colour::DeviceGray(f64::NAN)), Err(ExportError::NonFinite));
    let bad_stroke = StrokeStyle { width_pt: -1.0, cap: LineCap::Butt, join: LineJoin::Miter };
    assert_eq!(ex.stroke_path(p, &square(), Colour::DeviceGray(0.0), bad_stroke), Err(ExportError::InvalidSize));
    let icc = ex.embed_icc(SRGB, 3).unwrap();
    assert_eq!(ex.fill_path(p, &square(), FillRule::NonZero, Colour::IccBased(icc, [2.0; 4])), Err(ExportError::ColourOutOfRange));
    assert_eq!(ex.add_page(page([0.0, 0.0, 2.0e9, 10.0])), Err(ExportError::ValueOutOfRange));
    assert!(PdfExport::new(ExportOptions { version: "1.7\n%evil", ..ExportOptions::default() }, &lim).is_err());
    let long = "x".repeat(MAX_EXPORT_TEXT + 1);
    assert_eq!(PdfExport::new(ExportOptions { info_title: Some(&long), ..ExportOptions::default() }, &lim).err(), Some(ExportError::TextTooLong));
    // Refusals change nothing: the builder still finishes with one page and no image.
    let (bytes, r) = ex.finish(&cancel).unwrap();
    assert_eq!((r.pages, r.images), (1, 0));
    assert_eq!(PdfDoc::open(&bytes, &lim, &cancel).unwrap().inspect().page_count, 1);
    // aggregate_over_budget: two images each under max_decoded_bytes, together over max_decoded_total.
    let tight = Limits { max_decoded_bytes: 4096, max_decoded_total: 6000, ..lim };
    let mut ex = PdfExport::new(ExportOptions::default(), &tight).unwrap();
    let p = ex.add_page(page(A4)).unwrap();
    let big = vec![7u8; 32 * 32 * 3];
    let rgb = |s: &'static [u8]| ImageSpec { width: 32, height: 32, bits: 8, colour: ImageColour::DeviceRgb, samples: s, alpha: None };
    let big: &'static [u8] = Box::leak(big.into_boxed_slice());
    ex.draw_image(p, rgb(big), at).unwrap();
    assert_eq!(ex.draw_image(p, rgb(big), at), Err(ExportError::AggregateOverBudget));
    // A font over the raw aggregate is refused before it is compressed or copied.
    assert_eq!(ex.embed_truetype(INTER, 0, &[], &[]), Err(ExportError::StreamOverBudget));
    // Whole-font embedding charges the written font at embedding; a subset is charged at finish.
    let roomy = Limits { max_decoded_total: INTER.len() + 100, max_bytes: 4096, ..lim };
    let whole = ExportOptions { subset_fonts: false, ..ExportOptions::default() };
    let mut ex = PdfExport::new(whole, &roomy).unwrap();
    assert_eq!(ex.embed_truetype(INTER, 0, &[], &[]), Err(ExportError::AggregateOverBudget));
    let tight = Limits { max_decoded_total: INTER.len() + 100_000, max_bytes: 2000, ..lim };
    let mut ex = PdfExport::new(ExportOptions::default(), &tight).unwrap();
    let p = ex.add_page(page(A4)).unwrap();
    let f = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p, f, 12.0, [10.0, 10.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    assert_eq!(ex.finish(&cancel).err(), Some(ExportError::AggregateOverBudget));
    // Page count and object numbers.
    let few = Limits { max_pages: 2, ..lim };
    let mut ex = PdfExport::new(ExportOptions::default(), &few).unwrap();
    ex.add_page(page(A4)).unwrap();
    ex.add_page(page(A4)).unwrap();
    assert_eq!(ex.add_page(page(A4)), Err(ExportError::TooManyPages));
    let numbers = Limits { max_object_number: 7, ..lim };
    let mut ex = PdfExport::new(ExportOptions::default(), &numbers).unwrap();
    ex.add_page(page(A4)).unwrap();
    ex.add_page(page(A4)).unwrap();
    assert_eq!(ex.add_page(page(A4)), Err(ExportError::TooManyObjects));
    assert!(ex.finish(&cancel).is_ok());
    // Operations per page.
    let ops = Limits { max_content_ops: 8, ..lim };
    let mut ex = PdfExport::new(ExportOptions::default(), &ops).unwrap();
    let p = ex.add_page(page(A4)).unwrap();
    ex.fill_path(p, &square()[..1], FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    assert_eq!(ex.fill_path(p, &square(), FillRule::NonZero, Colour::DeviceGray(0.0)), Err(ExportError::TooManyOperations));
    // Nothing to write, and a tripped token.
    assert_eq!(PdfExport::new(ExportOptions::default(), &lim).unwrap().finish(&cancel).err(), Some(ExportError::NoPages));
    let tripped = CancellationToken::default();
    tripped.cancel();
    let mut ex = PdfExport::new(ExportOptions::default(), &lim).unwrap();
    ex.add_page(page(A4)).unwrap();
    assert_eq!(ex.finish(&tripped).err(), Some(ExportError::Canceled));
}

/// Offset of the OS/2 table in `font` (test-side directory walk).
fn os2_offset(font: &[u8]) -> usize {
    let n = u16::from_be_bytes([font[4], font[5]]) as usize;
    (0..n)
        .map(|i| 12 + i * 16)
        .find(|r| &font[*r..*r + 4] == b"OS/2")
        .map(|r| u32::from_be_bytes([font[r + 8], font[r + 9], font[r + 10], font[r + 11]]) as usize)
        .unwrap()
}

#[test]
fn hostile_fonts_and_profiles_are_typed_refusals() {
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let p = ex.add_page(page(A4)).unwrap();
    // Garbage, truncated and corrupted fonts never panic.
    for bad in [&b""[..], b"\0\x01\0\0", b"garbage bytes that are not a font", &INTER[..64], &INTER[..4096]] {
        assert_eq!(ex.embed_truetype(bad, 0, &[], &[]), Err(ExportError::FontUnreadable), "{} bytes", bad.len());
    }
    let mut huge_dir = INTER.to_vec();
    huge_dir[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_eq!(ex.embed_truetype(&huge_dir, 0, &[], &[]), Err(ExportError::FontUnreadable));
    // CFF outlines, collections, face index.
    assert_eq!(ex.embed_truetype(NOTO_SC_CFF, 0, &[], &[]), Err(ExportError::FontCffNotEmbedded));
    let mut ttc = INTER.to_vec();
    ttc[0..4].copy_from_slice(b"ttcf");
    assert_eq!(ex.embed_truetype(&ttc, 0, &[], &[]), Err(ExportError::FontCollectionNotEmbedded));
    assert_eq!(ex.embed_truetype(INTER, 1, &[], &[]), Err(ExportError::FontCollectionNotEmbedded));
    // OS/2 fsType: Restricted License (2) and Bitmap embedding only (0x0200) refuse; Editable (8) embeds.
    let os2 = os2_offset(INTER);
    for (fs, want) in [(0x0002u16, Err(ExportError::FontEmbeddingRestricted)), (0x0200, Err(ExportError::FontEmbeddingRestricted)), (0x0008, Ok(()))] {
        let mut f = INTER.to_vec();
        f[os2 + 8..os2 + 10].copy_from_slice(&fs.to_be_bytes());
        assert_eq!(ex.embed_truetype(&f, 0, &[], &[]).map(|_| ()), want, "fsType {fs:#06x}");
    }
    // Glyph ids past the font, empty or overlong /ToUnicode text.
    assert_eq!(ex.embed_truetype(INTER, 0, &[(u16::MAX, 500)], &[]), Err(ExportError::FontGlyphOutOfRange));
    assert_eq!(ex.embed_truetype(INTER, 0, &[], &[(2548, "x")]), Err(ExportError::FontGlyphOutOfRange));
    assert_eq!(ex.embed_truetype(INTER, 0, &[], &[(161, "")]), Err(ExportError::ToUnicodeInvalid));
    assert_eq!(ex.embed_truetype(INTER, 0, &[], &[(161, &"x".repeat(17))]), Err(ExportError::ToUnicodeInvalid));
    let font = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    assert_eq!(ex.show_glyphs(p, font, 12.0, [0.0, 0.0], Colour::DeviceGray(0.0), &[(2548, 0.0, 0.0)]), Err(ExportError::FontGlyphOutOfRange));
    assert_eq!(ex.show_glyphs(p, font, 0.0, [0.0, 0.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]), Err(ExportError::InvalidSize));
    assert_eq!(ex.show_glyphs(p, font, 12.0, [f64::NAN, 0.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]), Err(ExportError::NonFinite));
    assert_eq!(ex.show_glyphs(p, font, 12.0, [0.0, 0.0], Colour::DeviceGray(0.0), &[(161, f64::MAX, 0.0)]), Err(ExportError::ValueOutOfRange));
    assert_eq!(ex.show_glyphs(p, font, 12.0, [MAX_EXPORT_COORD, 0.0], Colour::DeviceGray(0.0), &[(161, MAX_EXPORT_COORD, 0.0)]), Err(ExportError::ValueOutOfRange));
    // Tiny and limit-sized values write and read back (numbers stay within the canonical writer's length).
    ex.show_glyphs(p, font, 12.0, [1.0e-300, -MAX_EXPORT_COORD], Colour::DeviceGray(0.0), &[(161, 0.5e-12, 0.0)]).unwrap();
    let (bytes, _) = ex.finish(&CancellationToken::default()).unwrap();
    assert_eq!(PdfDoc::open(&bytes, &Limits::default(), &CancellationToken::default()).unwrap().inspect().page_count, 1);
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    // ICC profiles: short, no `acsp`, declared size past the data, component mismatch.
    assert_eq!(ex.embed_icc(&SRGB[..100], 3), Err(ExportError::IccUnreadable));
    let mut no_sig = SRGB.to_vec();
    no_sig[36..40].copy_from_slice(b"xxxx");
    assert_eq!(ex.embed_icc(&no_sig, 3), Err(ExportError::IccUnreadable));
    let mut long_decl = SRGB.to_vec();
    long_decl[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(ex.embed_icc(&long_decl, 3), Err(ExportError::IccUnreadable));
    assert_eq!(ex.embed_icc(SRGB, 4), Err(ExportError::IccComponentsMismatch));
    assert_eq!(ExportError::FontCffNotEmbedded.code(), "font_cff_not_embedded");
}

/// `render_page`: hayro-interpret reads the export back into pixels; hostile scales, pages and
/// budgets are typed refusals.
#[test]
fn render_back_reads_the_export() {
    let cancel = CancellationToken::default();
    let lim = Limits::default();
    let mut ex = PdfExport::new(ExportOptions::default(), &lim).unwrap();
    let p = ex.add_page(page([0.0, 0.0, 300.0, 200.0])).unwrap();
    ex.fill_path(p, &square(), FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    let (bytes, _) = ex.finish(&cancel).unwrap();
    for k in [1.0, 2.0] {
        let r = render_page(&bytes, 0, k, &lim, &cancel).unwrap();
        assert_eq!((r.width, r.height), ((300.0 * k) as u32, (200.0 * k) as u32));
        // Square 100..200 pt; device y = (200 - y) * k.
        let at = |x: f64, y: f64| r.pixel((x * k) as u32, ((200.0 - y) * k) as u32).unwrap();
        assert_eq!(at(150.0, 150.0), [255, 0, 0]);
        assert_eq!(at(50.0, 50.0), [255, 255, 255]);
        assert_eq!((r.font_warnings, r.image_warnings), (0, 0));
    }
    assert_eq!(render_page(&bytes, 0, f64::NAN, &lim, &cancel), Err(ExportError::NonFinite));
    assert_eq!(render_page(&bytes, 0, 0.0, &lim, &cancel), Err(ExportError::ValueOutOfRange));
    assert_eq!(render_page(&bytes, 1, 1.0, &lim, &cancel), Err(ExportError::NoSuchPage));
    let small = Limits { max_flatten_pixels: 1000, ..lim };
    assert_eq!(render_page(&bytes, 0, 1.0, &small, &cancel), Err(ExportError::RenderOverBudget));
    assert!(matches!(render_page(b"%PDF-1.7 garbage", 0, 1.0, &lim, &cancel), Err(ExportError::Press(_))));
    let r = render_page(&bytes, 0, 1.0, &lim, &cancel).unwrap();
    assert_eq!(r.pixel(300, 0), None);
}

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(x0, y0),
        PathCommand::LineTo(x1, y0),
        PathCommand::LineTo(x1, y1),
        PathCommand::LineTo(x0, y1),
        PathCommand::Close,
    ]
}

/// Item 6: constant alpha and blend modes through `/ExtGState`, transparency groups as form
/// XObjects painted with their own `/ExtGState`, luminosity soft masks; read back by `render_page`.
/// Region A: red at fill alpha 0.5 over white. B: cyan Multiply over yellow = green. C: a group at
/// opacity 0.5 holding one opaque black square. D: a soft-masked group (the paint device skips
/// soft-masked content and says so: soft mask compositing is NOT_PROVEN by this render).
#[test]
fn transparency_through_extgstate_groups_and_soft_masks() {
    let cancel = CancellationToken::default();
    let lim = Limits::default();
    let mut ex = PdfExport::new(ExportOptions::default(), &lim).unwrap();
    let p = ex.add_page(page([0.0, 0.0, 400.0, 100.0])).unwrap();
    ex.set_draw_state(p, DrawState { fill_alpha: 0.5, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect_path(0.0, 0.0, 100.0, 100.0), FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.set_draw_state(p, DrawState::default()).unwrap();
    ex.fill_path(p, &rect_path(100.0, 0.0, 200.0, 100.0), FillRule::NonZero, Colour::DeviceRgb([1.0, 1.0, 0.0])).unwrap();
    ex.set_draw_state(p, DrawState { blend: BlendMode::Multiply, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect_path(100.0, 0.0, 200.0, 100.0), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.0, 1.0])).unwrap();
    ex.set_draw_state(p, DrawState::default()).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect_path(200.0, 0.0, 300.0, 100.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    let plain = GroupStyle { opacity: 0.5, blend: BlendMode::Normal, isolated: true, knockout: false, soft_mask: None };
    ex.end_group(p, plain).unwrap();
    // An empty group writes nothing.
    ex.begin_group(p).unwrap();
    ex.end_group(p, plain).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect_path(300.0, 0.0, 400.0, 100.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    let mask_px = [255u8, 0];
    let mask = SoftMask {
        image: ImageSpec { width: 2, height: 1, bits: 8, colour: ImageColour::DeviceGray, samples: &mask_px, alpha: None },
        placement: [100.0, 0.0, 0.0, 100.0, 300.0, 0.0],
    };
    ex.end_group(p, GroupStyle { soft_mask: Some(mask), opacity: 1.0, ..plain }).unwrap();
    let (bytes, _) = ex.finish(&cancel).unwrap();
    let r = render_page(&bytes, 0, 1.0, &lim, &cancel).unwrap();
    let near = |a: [u8; 3], b: [u8; 3], t: u8| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= t);
    let at = |x: u32| r.pixel(x, 50).unwrap();
    assert!(near(at(50), [255, 128, 128], 3), "alpha {:?}", at(50));
    assert!(near(at(150), [0, 255, 0], 3), "multiply {:?}", at(150));
    assert!(near(at(250), [128, 128, 128], 3), "group opacity {:?}", at(250));
    assert!(r.notes.contains(&"soft_mask"), "{:?}", r.notes);
    println!("NOT_PROVEN: soft mask compositing (press paint device skips soft-masked content; structure is checked in src/export/tests.rs)");
    // Typed refusals.
    let mut ex = PdfExport::new(ExportOptions::default(), &lim).unwrap();
    let p = ex.add_page(page([0.0, 0.0, 10.0, 10.0])).unwrap();
    assert_eq!(ex.end_group(p, plain), Err(ExportError::UnbalancedGroup));
    assert_eq!(ex.set_draw_state(p, DrawState { fill_alpha: 1.5, ..DrawState::default() }), Err(ExportError::AlphaOutOfRange));
    assert_eq!(ex.set_draw_state(p, DrawState { stroke_alpha: f64::NAN, ..DrawState::default() }), Err(ExportError::NonFinite));
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect_path(0.0, 0.0, 5.0, 5.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    assert_eq!(ex.end_group(p, GroupStyle { opacity: -0.1, ..plain }), Err(ExportError::AlphaOutOfRange));
    let rgb = [0u8; 6];
    let bad = SoftMask { image: ImageSpec { width: 2, height: 1, bits: 8, colour: ImageColour::DeviceRgb, samples: &rgb, alpha: None }, placement: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] };
    assert_eq!(ex.end_group(p, GroupStyle { soft_mask: Some(bad), ..plain }), Err(ExportError::InvalidMask));
    // One group is still open (the refused end_group calls left it); 31 more reach the 32 cap.
    for _ in 0..31 {
        ex.begin_group(p).unwrap();
    }
    assert_eq!(ex.begin_group(p), Err(ExportError::UnbalancedGroup));
    assert_eq!(ex.finish(&cancel).err(), Some(ExportError::UnbalancedGroup));
}
