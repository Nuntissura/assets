//! Lane SMB-LAYOUT-PDFX-VERAPDF items 1-2: one PDF/X-4 export that exercises every feature of the
//! builder (TrueType and CFF fonts, ICC and device colour, images with alpha, clips, text clips,
//! groups, soft masks, blend modes, gradients). The file is self-checked here; with
//! `HSK_STUDIO_PDF_CORPUS_DIR` set it is also written there for the external veraPDF run.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::*;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const NOTO_SC_CFF: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/NotoSansSC-Regular.otf");
const SRGB: &[u8] = include_bytes!("../../hsk-studio-develop/tests/profiles/sRGB-v4.icc");
/// Synthetic CMYK output (`prtr`) profile from the prism oracle set.
const CMYK: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(x0, y0),
        PathCommand::LineTo(x1, y0),
        PathCommand::LineTo(x1, y1),
        PathCommand::LineTo(x0, y1),
        PathCommand::Close,
    ]
}

fn page() -> PageSpec {
    PageSpec { media_box_pt: [0.0, 0.0, 420.0, 300.0], trim_box_pt: Some([10.0, 10.0, 410.0, 290.0]), bleed_box_pt: Some([5.0, 5.0, 415.0, 295.0]) }
}

pub fn options() -> ExportOptions<'static> {
    let intent = OutputIntent {
        icc: CMYK,
        components: 4,
        output_condition_identifier: "HSK synthetic CMYK",
        output_condition: Some("Handshake synthetic CMYK oracle condition"),
        registry: None,
        subtype: OutputIntentSubtype::GtsPdfx,
    };
    let x4 = PdfX4 { created: "D:20261015000000Z", modified: "D:20261015000000Z", trapped: false };
    ExportOptions { version: "1.6", output_intent: Some(intent), info_title: Some("Studio press corpus"), pdfx4: Some(x4), pdfa2b: true, ..ExportOptions::default() }
}

/// The corpus document.
pub fn corpus(options: ExportOptions<'_>) -> (Vec<u8>, ExportReceipt) {
    let lim = Limits::default();
    let cancel = CancellationToken::default();
    let mut ex = PdfExport::new(options, &lim).unwrap();
    let srgb = ex.embed_icc(SRGB, 3).unwrap();
    let cmyk = ex.embed_icc(CMYK, 4).unwrap();
    let inter = ex.embed_font(INTER, 0, &[(161, 740), (679, 237)], &[(161, "H"), (679, "i")]).unwrap();
    let noto = ex.embed_font(NOTO_SC_CFF, 0, &[(41, 728), (8805, 1000)], &[(41, "H"), (8805, "\u{4e2d}")]).unwrap();

    // Page 1: colour, strokes, images, text and clips.
    let p = ex.add_page(page()).unwrap();
    ex.fill_path(p, &rect(20.0, 20.0, 120.0, 120.0), FillRule::EvenOdd, Colour::DeviceCmyk([0.0, 1.0, 1.0, 0.0])).unwrap();
    ex.fill_path(p, &rect(130.0, 20.0, 230.0, 120.0), FillRule::NonZero, Colour::IccBased(srgb, [0.2, 0.4, 0.8, 0.0])).unwrap();
    ex.fill_path(p, &rect(240.0, 20.0, 340.0, 120.0), FillRule::NonZero, Colour::IccBased(cmyk, [0.1, 0.2, 0.3, 0.4])).unwrap();
    let style = StrokeStyle { width_pt: 2.5, cap: LineCap::Round, join: LineJoin::Bevel };
    ex.stroke_path(p, &rect(20.0, 20.0, 340.0, 120.0), Colour::DeviceGray(0.0), style).unwrap();
    let px = [0u8, 64, 128, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let alpha = [255u8, 128, 64, 0];
    ex.draw_image(p, ImageSpec { width: 2, height: 2, bits: 8, colour: ImageColour::IccBased(srgb), samples: &px, alpha: Some(&alpha) }, [40.0, 0.0, 0.0, 40.0, 350.0, 20.0]).unwrap();
    let cm = [0u8, 0, 0, 255, 255, 0, 0, 0];
    ex.draw_image(p, ImageSpec { width: 2, height: 1, bits: 8, colour: ImageColour::DeviceCmyk, samples: &cm, alpha: None }, [40.0, 0.0, 0.0, 20.0, 350.0, 70.0]).unwrap();
    ex.show_glyphs(p, inter, 24.0, [30.0, 200.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0), (679, 17.76, 0.0)]).unwrap();
    ex.show_glyphs(p, noto, 24.0, [90.0, 200.0], Colour::DeviceCmyk([0.0, 0.0, 0.0, 1.0]), &[(41, 0.0, 0.0), (8805, 24.0, 0.0)]).unwrap();
    ex.push_clip(p, &rect(200.0, 150.0, 260.0, 280.0), FillRule::NonZero).unwrap();
    ex.fill_path(p, &rect(180.0, 150.0, 300.0, 280.0), FillRule::NonZero, Colour::DeviceCmyk([1.0, 0.0, 0.0, 0.0])).unwrap();
    ex.pop_clip(p).unwrap();
    ex.push_clip_text(p, &[(inter, 100.0, &[(161, 300.0, 160.0)])]).unwrap();
    ex.fill_path(p, &rect(290.0, 150.0, 400.0, 280.0), FillRule::NonZero, Colour::DeviceCmyk([0.0, 0.0, 1.0, 0.0])).unwrap();
    ex.pop_clip(p).unwrap();

    // Page 2: transparency.
    let p = ex.add_page(page()).unwrap();
    ex.set_draw_state(p, DrawState { fill_alpha: 0.5, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect(20.0, 20.0, 100.0, 100.0), FillRule::NonZero, Colour::DeviceCmyk([0.0, 1.0, 0.0, 0.0])).unwrap();
    ex.set_draw_state(p, DrawState { blend: BlendMode::Multiply, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect(60.0, 60.0, 140.0, 140.0), FillRule::NonZero, Colour::DeviceCmyk([1.0, 0.0, 0.0, 0.0])).unwrap();
    ex.set_draw_state(p, DrawState { blend: BlendMode::Luminosity, stroke_alpha: 0.7, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect(100.0, 20.0, 160.0, 80.0), FillRule::NonZero, Colour::DeviceGray(0.3)).unwrap();
    ex.set_draw_state(p, DrawState::default()).unwrap();
    let plain = GroupStyle { opacity: 1.0, blend: BlendMode::Normal, isolated: true, knockout: false, soft_mask: None };
    ex.begin_group(p).unwrap();
    ex.set_draw_state(p, DrawState { fill_alpha: 0.5, ..DrawState::default() }).unwrap();
    ex.fill_path(p, &rect(180.0, 20.0, 240.0, 80.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    ex.fill_path(p, &rect(210.0, 50.0, 270.0, 110.0), FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    ex.end_group(p, GroupStyle { knockout: true, opacity: 0.8, blend: BlendMode::Screen, ..plain }).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect(280.0, 20.0, 400.0, 140.0), FillRule::NonZero, Colour::DeviceCmyk([0.0, 0.0, 0.0, 1.0])).unwrap();
    ex.end_group(p, GroupStyle { isolated: false, opacity: 0.6, ..plain }).unwrap();
    let mask_px = [255u8, 0, 128, 64];
    let mask_image = ImageSpec { width: 2, height: 2, bits: 8, colour: ImageColour::DeviceGray, samples: &mask_px, alpha: None };
    for (i, (alpha_mask, bc, tr)) in [(false, 0.0, None), (false, 1.0, Some(2.0)), (true, 0.0, None)].into_iter().enumerate() {
        let x = 20.0 + 130.0 * i as f64;
        let placed = [MaskImage { image: mask_image, placement: [100.0, 0.0, 0.0, 100.0, x, 160.0] }];
        ex.begin_group(p).unwrap();
        ex.fill_path(p, &rect(x, 160.0, x + 100.0, 260.0), FillRule::NonZero, Colour::DeviceCmyk([0.5, 0.0, 1.0, 0.0])).unwrap();
        let mask = SoftMask { images: &placed, alpha: alpha_mask, backdrop: bc, transfer_exponent: tr };
        ex.end_group(p, GroupStyle { soft_mask: Some(mask), ..plain }).unwrap();
    }

    // Page 3: gradients.
    let p = ex.add_page(page()).unwrap();
    let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let bw = [GradientStop::new(0.0, [0.0; 3]), GradientStop::new(1.0, [1.0; 3])];
    let rgb = [GradientStop::new(0.0, [1.0, 0.0, 0.0]), GradientStop { midpoint: Some(0.3), ..GradientStop::new(0.5, [0.0, 1.0, 0.0]) }, GradientStop::new(1.0, [0.0, 0.0, 1.0])];
    let fade = [GradientStop { opacity: 0.0, ..GradientStop::new(0.0, [1.0, 0.0, 0.0]) }, GradientStop::new(1.0, [1.0, 0.0, 0.0])];
    let axial = |x0: f64, x1: f64| GradientGeometry::Axial { from: [x0, 0.0], to: [x1, 0.0] };
    let cells: [(&[GradientStop], GradientGeometry, GradientSpread); 6] = [
        (&bw, axial(20.0, 120.0), GradientSpread::Pad),
        (&rgb, axial(140.0, 240.0), GradientSpread::Pad),
        (&bw, axial(260.0, 285.0), GradientSpread::Repeat),
        (&bw, axial(20.0, 45.0), GradientSpread::Reflect),
        (&fade, axial(140.0, 240.0), GradientSpread::Pad),
        (&bw, GradientGeometry::Radial { from: [310.0, 210.0], from_radius: 0.0, to: [310.0, 210.0], to_radius: 50.0 }, GradientSpread::Pad),
    ];
    for (i, (stops, geometry, spread)) in cells.into_iter().enumerate() {
        let (x, y) = (20.0 + 120.0 * (i % 3) as f64, 20.0 + 140.0 * (i / 3) as f64);
        let g = Gradient { geometry, stops, extend: [true, true], spread, matrix: id, profile: Some(srgb) };
        ex.fill_path_gradient(p, &rect(x, y, x + 100.0, y + 100.0), FillRule::NonZero, &g).unwrap();
    }
    ex.finish(&cancel).unwrap()
}

#[test]
fn verapdf_corpus_self_checks_and_is_written_on_request() {
    let lim = Limits::default();
    let cancel = CancellationToken::default();
    let (bytes, receipt) = corpus(options());
    assert_eq!(receipt.pages, 3);
    assert_eq!(receipt.conformance, "not_proven");
    let report = check_pdfx4(&bytes, &lim, &cancel).unwrap();
    assert!(report.all_passed(), "{:?}", report.failed());
    for i in 0..3 {
        let r = render_page(&bytes, i, 0.5, &lim, &cancel).unwrap();
        assert_eq!((r.font_warnings, r.image_warnings), (0, 0));
    }
    if let Some(dir) = std::env::var_os("HSK_STUDIO_PDF_CORPUS_DIR") {
        let path = std::path::Path::new(&dir).join("press-pdfx4-corpus.pdf");
        std::fs::write(&path, &bytes).unwrap();
    }
}

/// veraPDF 1.30.3 `-f 2b --format xml` on the corpus written by the test above (artifact
/// `studio-team-dev/layout-harden/verapdf/reports/press-pdfx4-corpus.pdfa-2b.xml`, byte-identical).
const REPORT: &[u8] = include_bytes!("fixtures/verapdf/press-pdfx4-corpus.pdfa-2b.xml");
/// sha256 of the corpus bytes that report was produced for.
const REPORTED_PDF_SHA256: &str = "e8605a4cbcd2d845cc4a33ee58e1106653f4246cddce7ee4aa7f17ff0bc10028";
const REPORT_SHA256: &str = "626703968de9190cf3e9ec79c947343bd847a5f9052b0944faed9b91992f6d88";

#[test]
fn passing_verapdf_report_is_the_only_way_out_of_not_proven() {
    let (bytes, receipt) = corpus(options());
    assert_eq!(receipt.sha256_hex, REPORTED_PDF_SHA256, "the export changed: re-run veraPDF on the new corpus and refresh the fixture");
    assert_eq!((receipt.conformance, &receipt.conformance_report_sha256), (EXPORT_CONFORMANCE, &None));
    let proven = receipt.clone().with_verapdf_report(&bytes, REPORT).unwrap();
    assert_eq!(proven.conformance, CONFORMANCE_PDFA2B_VERAPDF);
    assert_eq!(proven.conformance_report_sha256.as_deref(), Some(REPORT_SHA256));
    assert_eq!(proven.conformance_validator.as_deref(), Some("veraPDF 1.30.3: PDF/A-2b validation profile"));
    assert_eq!(proven.effective_pdfx_profile.as_deref(), Some(PDFX4_VERSION), "PDF/X-4 itself stays self-checked only");
    let text = std::str::from_utf8(REPORT).unwrap();
    let refused = |pdf: &[u8], report: &[u8]| receipt.clone().with_verapdf_report(pdf, report).err();
    let edited = |from: &str, to: &str| {
        assert!(text.contains(from), "{from}");
        text.replacen(from, to, 1).into_bytes()
    };
    assert_eq!(refused(b"%PDF-1.6 other", REPORT), Some(ExportError::ReportRefused("pdf_hash_mismatch")));
    assert_eq!(refused(&bytes, &[0xff, 0xfe]), Some(ExportError::ReportRefused("report_not_utf8")));
    assert_eq!(refused(&bytes, &edited("isCompliant=\"true\"", "isCompliant=\"false\"")), Some(ExportError::ReportRefused("not_compliant")));
    assert_eq!(refused(&bytes, &edited("failedRules=\"0\"", "failedRules=\"1\"")), Some(ExportError::ReportRefused("not_compliant")));
    assert_eq!(refused(&bytes, &edited("PDF/A-2b validation profile", "PDF/A-1b validation profile")), Some(ExportError::ReportRefused("wrong_profile")));
    let size = format!("<item size=\"{}\">", bytes.len());
    assert_eq!(refused(&bytes, &edited(&size, "<item size=\"1\">")), Some(ExportError::ReportRefused("item_size_mismatch")));
    let job = &text[text.find("<job>").unwrap()..text.find("</job>").unwrap() + 6];
    assert_eq!(refused(&bytes, &edited(job, &format!("{job}{job}"))), Some(ExportError::ReportRefused("not_one_verapdf_job")));
    assert_eq!(refused(&bytes, &edited("id=\"core\"", "id=\"other\"")), Some(ExportError::ReportRefused("no_verapdf_version")));
    assert_eq!(refused(&bytes, &vec![b' '; MAX_REPORT_BYTES + 1]), Some(ExportError::ReportRefused("report_too_large")));
    assert_eq!(ExportError::ReportRefused("x").code(), "report_refused");
}

/// PDF/A-2b requirements are typed refusals; the identification lands in the XMP and a second
/// `GTS_PDFA1` output intent shares the destination profile.
#[test]
fn pdfa2b_identification_and_refusals() {
    let lim = Limits::default();
    let refused = |o: ExportOptions<'_>| PdfExport::new(o, &lim).err();
    assert_eq!(refused(ExportOptions { output_intent: None, pdfx4: None, ..options() }), Some(ExportError::PdfARequirement("output_intent")));
    assert_eq!(refused(ExportOptions { version: "2.0", ..options() }), Some(ExportError::PdfARequirement("version_1_x")));
    assert_eq!(ExportError::PdfARequirement("x").code(), "pdfa_requirement");
    let (bytes, _) = corpus(options());
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("<pdfaid:part>2</pdfaid:part>") && s.contains("<pdfaid:conformance>B</pdfaid:conformance>"));
    assert!(s.contains("<pdfaSchema:prefix>pdfxid</pdfaSchema:prefix>") && s.contains("<pdfaProperty:name>Trapped</pdfaProperty:name>"));
    assert!(s.contains("/S /GTS_PDFX") && s.contains("/S /GTS_PDFA1"));
    // PDF/A-2b alone (no PDF/X-4): identification without the PDF/X properties or extension schemas.
    let (bytes, receipt) = corpus(ExportOptions { pdfx4: None, ..options() });
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("<pdfaid:part>2</pdfaid:part>") && !s.contains("pdfxid") && !s.contains("pdfaExtension"));
    assert_eq!(receipt.effective_pdfx_profile, None);
}
