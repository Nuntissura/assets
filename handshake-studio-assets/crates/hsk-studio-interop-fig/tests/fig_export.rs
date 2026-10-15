//! Lane SMB-INTEROP-FIG-EXPORT: Figma frames exported to PDF through press `PdfExport` and rendered
//! back by press's independent interpreter for the raster export, compared with the Operator PNGs
//! at tolerances from the run; `.fig` export settings (format, scale, suffix) planned, unsupported
//! settings typed losses; prototype interactions kept as records through a package round trip;
//! Inter Regular 3.019 vs 4.1 measured on the text line.
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::Budget;
use hsk_studio_interop_fig::*;

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

/// Per-pixel largest channel difference between a rendered page and a PNG of the same size.
fn page_diffs(page: &hsk_studio_press::RenderedPage, png: &(usize, usize, Vec<u8>)) -> Vec<f64> {
    let mut d = Vec::with_capacity(png.0 * png.1);
    for y in 0..png.1 {
        for x in 0..png.0 {
            let ours = page.pixel(x as u32, y as u32).expect("pixel");
            let o = (y * png.0 + x) * 4;
            d.push((0..3).map(|c| (f64::from(ours[c]) - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max));
        }
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    d
}

/// (mean, share within 8/255, p99).
fn stats(d: &[f64]) -> (f64, f64, f64) {
    let n = d.len() as f64;
    let mean = d.iter().sum::<f64>() / n;
    let within8 = d.iter().filter(|v| **v <= 8.0).count() as f64 / n;
    (mean, within8, d[(n * 0.99) as usize])
}

fn losses(l: &[NodeLoss]) -> Vec<(&str, &str, u32)> {
    l.iter().map(|x| (x.path.as_str(), x.code, x.count)).collect()
}

#[test]
fn frame_pdf_export_renders_like_the_operator_png() {
    let cancel = CancellationToken::default();
    let doc = document("fig-prototype-pages.fig");
    let p = map(&doc, Budget::default());
    for (name, frame, png_name) in [
        ("start", sid(2, 3), "fig-prototype-pages-start.png"),
        ("detail", sid(2, 6), "fig-prototype-pages-detail.png"),
        ("animated", sid(2, 9), "fig-prototype-pages-animated.png"),
    ] {
        // No export settings in the file: Figma's default PNG 1x, named after the frame.
        let (plans, loss) = plan_frame_exports(&doc, &p, frame);
        assert!(loss.is_empty(), "{loss:?}");
        let plan = &plans[0];
        assert!(plan.default_setting);
        assert_eq!((plan.format, plan.scale), (FigExportFormat::Png, 1.0));
        assert_eq!(plan.file_name, format!("{name}-screen.png"));
        let pdf = export_frame_pdf(&p, plan, None, &cancel).unwrap();
        // Text is written as Figma's own glyph outlines (two labels per screen).
        assert_eq!(losses(&pdf.loss), [("export.text", "text_as_outlines", 2)]);
        assert_eq!(pdf.receipt.pages, 1);
        let page = render_frame_export(&pdf, &cancel).unwrap();
        let png = decode_png(&fixture(png_name));
        assert_eq!((page.width as usize, page.height as usize), (png.0, png.1));
        let (mean, within8, p99) = stats(&page_diffs(&page, &png));
        eprintln!("{name}: mean {mean:.6} <=8 {within8:.6} p99 {p99:.6}");
        // Measured (press render_page, 1 pt = 1 px): mean 0.377676 / 0.339327 / 0.348830,
        // within 8/255 0.992260 / 0.992831 / 0.991971, p99 0. Residue: glyph and orb edge
        // antialiasing of the two rasterisers.
        assert!(mean <= 0.378, "{name}: mean {mean:.6}");
        assert!(within8 >= 0.99197, "{name}: within 8/255 {within8:.6}");
        assert!(p99 <= 0.0, "{name}: p99 {p99}");
    }
}

#[test]
fn basic_shapes_artboard_pdf_export_with_image() {
    let cancel = CancellationToken::default();
    let fig = fixture("fig-basic-shapes.fig");
    let doc = document("fig-basic-shapes.fig");
    let p = map(&doc, Budget::default());
    let images = Images::from_fig(&fig, &p);
    let (plans, _) = plan_frame_exports(&doc, &p, sid(3, 2));
    let pdf = export_frame_pdf(&p, &plans[0], Some(&images), &cancel).unwrap();
    // What the page cannot carry is typed: the drop shadow and the blur, the mask (written as a
    // clip of its geometry), PDF's default miter limit for the three mitred strokes, text outlines.
    assert_eq!(
        losses(&pdf.loss),
        [
            ("export.effects", "effect_not_exported", 2),
            ("export.mask", "mask_written_as_geometry_clip", 1),
            ("export.stroke.miter_limit", "stroke_miter_limit_pdf_default", 3),
            ("export.text", "text_as_outlines", 1),
        ]
    );
    assert_eq!(pdf.receipt.images, 1);
    let page = render_frame_export(&pdf, &cancel).unwrap();
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let (mean, within8, p99) = stats(&page_diffs(&page, &png));
    eprintln!("basic: mean {mean:.6} <=8 {within8:.6} p99 {p99:.6} notes {:?}", page.notes);
    // Measured: mean 0.896394/255, 98.0469 % within 8/255, p99 23 (shadow and blur not drawn).
    assert!(mean <= 0.8964, "mean {mean:.6}");
    assert!(within8 >= 0.98046, "within 8/255 {within8:.6}");
    // Without decoded images the image paint is a typed loss, never a substitute.
    let pdf = export_frame_pdf(&p, &plans[0], None, &cancel).unwrap();
    assert!(losses(&pdf.loss).contains(&("export.image", "image_paint_source_not_supplied", 1)));
}

#[test]
fn export_settings_scale_suffix_format_and_typed_losses() {
    let cancel = CancellationToken::default();
    let mut doc = document("fig-prototype-pages.fig");
    let set = |suffix: &str, ty: &str, c: Option<(&str, f32)>| ExportSetting {
        suffix: Some(suffix.into()),
        image_type: Some(ty.into()),
        constraint: c.map(|(k, v)| (k.into(), v)),
        ..ExportSetting::default()
    };
    let start = doc.nodes.iter_mut().find(|n| n.id == sid(2, 3)).unwrap();
    start.export_settings = vec![
        set("@2x", "PNG", Some(("CONTENT_SCALE", 2.0))),
        set("-print", "PDF", None),
        set("-half", "PNG", Some(("CONTENT_WIDTH", 180.0))),
        ExportSetting { quality: Some(0.8), ..set("", "JPEG", None) },
        set("", "SVG", None),
        ExportSetting { color_profile: Some("DISPLAY_P3_V4".into()), ..set("-p3", "PNG", None) },
        set("@100x", "PNG", Some(("CONTENT_SCALE", 100.0))),
        ExportSetting { unsupported: vec!["svgOutlineText".into()], ..set("-x", "PNG", None) },
    ];
    let p = map(&doc, Budget::default());
    let (plans, loss) = plan_frame_exports(&doc, &p, sid(2, 3));
    let got: Vec<(&str, FigExportFormat, f64)> = plans.iter().map(|x| (x.file_name.as_str(), x.format, x.scale)).collect();
    assert_eq!(
        got,
        [
            ("start-screen@2x.png", FigExportFormat::Png, 2.0),
            ("start-screen-print.pdf", FigExportFormat::Pdf, 1.0),
            ("start-screen-half.png", FigExportFormat::Png, 0.5),
            ("start-screen.jpg", FigExportFormat::Jpeg, 1.0),
        ]
    );
    assert_eq!(
        losses(&loss),
        [
            ("export.color_profile=DISPLAY_P3_V4", "export_color_profile_unsupported", 1),
            ("export.constraint", "export_scale_out_of_range", 1),
            ("export.format=SVG", "export_format_unsupported", 1),
            ("export.svgOutlineText", "export_setting_unsupported", 1),
        ]
    );
    // The 2x export: page and raster at twice the size; each 2x2 block's bottom-right pixel vs the
    // 1x PNG pixel.
    let pdf = export_frame_pdf(&p, &plans[0], None, &cancel).unwrap();
    let page = render_frame_export(&pdf, &cancel).unwrap();
    assert_eq!((page.width, page.height), (720, 1040));
    let png = decode_png(&fixture("fig-prototype-pages-start.png"));
    let mut d = Vec::new();
    for y in 0..png.1 {
        for x in 0..png.0 {
            let ours = page.pixel(2 * x as u32 + 1, 2 * y as u32 + 1).unwrap();
            let o = (y * png.0 + x) * 4;
            d.push((0..3).map(|c| (f64::from(ours[c]) - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max));
        }
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (mean, within8, _) = stats(&d);
    eprintln!("start@2x sampled: mean {mean:.6} <=8 {within8:.6}");
    // Measured: mean 0.680737/255, 99.1245 % within 8/255 (vector content re-rasterised at 2x).
    assert!(mean <= 0.6808, "2x mean {mean:.6}");
    assert!(within8 >= 0.99124, "2x within 8/255 {within8:.6}");
    // Hostile plans are refused before any page exists.
    let mut bad = plans[0].clone();
    bad.scale = f64::NAN;
    assert_eq!(export_frame_pdf(&p, &bad, None, &cancel).unwrap_err(), FigError::ExportRefused("export_scale_out_of_range"));
    bad.scale = MAX_EXPORT_SCALE * 2.0;
    assert_eq!(export_frame_pdf(&p, &bad, None, &cancel).unwrap_err(), FigError::ExportRefused("export_scale_out_of_range"));
    let (none, loss) = plan_frame_exports(&doc, &p, sid(999, 999));
    assert!(none.is_empty());
    assert_eq!(losses(&loss), [("export.frame", "export_frame_not_in_document", 1)]);
}

#[test]
fn inter_regular_versions_measured_on_the_text_line() {
    // basic-shapes "Regular text + bold text" drawn by composite with Type-rasterised fonts: the
    // Regular run with Inter 3.019 (type fixture) and with Inter 4.1 (this crate's fixture), the
    // Bold run with Inter Bold 4.1, text line region (canvas 370..690 x 560..600) vs the PNG.
    let fig = fixture("fig-basic-shapes.fig");
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    let images = Images::from_fig(&fig, &p);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let mut measured = Vec::new();
    for regular in [FontFile::inter_regular(), FontFile::inter_regular_41()] {
        let label = regular.identity;
        let files = [regular, FontFile::inter_bold()];
        let grants = [
            FigFontGrant { family: "Inter", style: "Regular", resource: files[0].resource() },
            FigFontGrant { family: "Inter", style: "Bold", resource: files[1].resource() },
        ];
        let out = resolve_text_outputs(&p, &grants);
        let text = prepare_outputs(&p, &out.outputs, &files);
        let r = render_with(&p, Some(&text), Some(&images));
        let bg = [png.2[0], png.2[1], png.2[2]];
        let mut d = Vec::new();
        for y in 560..600usize {
            for x in 370..690usize {
                let ours = over(&r, x, y, bg);
                let o = ((y - 100) * png.0 + (x - 100)) * 4;
                d.push((0..3).map(|c| (ours[c] - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max));
            }
        }
        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (mean, within8, _) = stats(&d);
        eprintln!("{label}: text line mean {mean:.6} <=8 {within8:.6}");
        measured.push((label, mean, within8));
    }
    // Measured: Inter 3.019 mean 3.862939/255 (91.4453 % within 8/255); Inter 4.1 mean 12.404327
    // (85.1172 %): Figma's own Inter build lays the Regular run out closer to 3.019, so the 3.019
    // fixture stays the Regular grant of the composite proofs; the PDF export needs no font (it
    // writes Figma's glyph outlines; fig_export::frame_pdf_export_renders_like_the_operator_png).
    assert!(measured[0].1 <= 3.863, "3.019 mean {}", measured[0].1);
    assert!(measured[1].1 <= 12.405, "4.1 mean {}", measured[1].1);
    assert!(measured[1].1 > measured[0].1);
}

#[test]
fn prototype_interactions_are_records_and_survive_a_package_round_trip() {
    let bytes = fixture("fig-prototype-pages.fig");
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) =
        import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap()
    else {
        panic!("imports")
    };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let p = map(&doc, Budget::default());
    let source_of = |id: &str| p.id_map.iter().find(|e| e.node.node_id == id).map(|e| e.source);
    let mut found = Vec::new();
    for d in &p.design.nodes {
        for r in &d.reactions {
            for a in &r.actions {
                let hsk_studio_folio::StudioAction::Node { destination_id, navigation, transition, .. } = a else {
                    panic!("node action")
                };
                found.push((
                    source_of(&d.node.node_id).unwrap(),
                    format!("{:?}", r.trigger),
                    format!("{navigation:?}"),
                    destination_id.as_deref().and_then(source_of),
                    transition.as_ref().map(|t| (format!("{:?}", t.transition_type), t.duration)),
                ));
            }
        }
    }
    found.sort();
    // Triggers, actions, destinations (the screens 2:6 detail, 2:9 animated, 2:3 start) and the
    // smart-animate transition with its 350 ms duration, from the file's prototype interactions.
    assert_eq!(
        found,
        [
            (sid(2, 12), "Some(OnClick)".into(), "Navigate".into(), Some(sid(2, 6)), None),
            (sid(2, 14), "Some(OnClick)".into(), "Navigate".into(), Some(sid(2, 9)), Some(("SmartAnimate".into(), 350))),
            (sid(2, 16), "Some(OnClick)".into(), "Navigate".into(), Some(sid(2, 3)), None),
        ]
    );
    // Easing curve, flow starting point and scroll overflow are records since
    // SMB-INTEROP-FIG-PROTOTYPE (fig_prototype::flow_start_overflow_and_easing_are_records).
    assert_eq!(p.design.prototype_pages.len(), 1);
    assert_eq!(p.design.frame_overflow.len(), 1);
    // Native round trip: the records import entry carries the reactions unchanged and the id map
    // still names every source node (the package reopen of fig_native proves the entry survives).
    let entries = p.import_entries(&import, &bytes).unwrap();
    let records = entries.iter().find(|e| e.role == hsk_studio_package::ImportRole::Records).unwrap();
    let back = read_native_records(&records.bytes).unwrap();
    assert_eq!(back.design.nodes, p.design.nodes);
    assert_eq!(back.id_map, p.id_map);
}
