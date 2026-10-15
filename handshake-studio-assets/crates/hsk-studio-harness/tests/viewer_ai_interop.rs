//! SMB-ILLUSTRATOR-INTEROP (feature `viewer`): a PDF-compatible `.ai` file imports through Press's
//! PDF reading and interop-vector into an editable vector document, and the document exports as
//! SVG through interop-vector. The fixture is generated (Press `PdfExport`, page 150 x 75 pt =
//! 200 x 100 px, an Illustrator private-data marker in the info dictionary); an Illustrator-saved
//! file is case AI-NS2 of the attended D5 session.
#![cfg(all(feature = "viewer", feature = "vector-interop"))]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_press::*;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![PathCommand::MoveTo(x0, y0), PathCommand::LineTo(x1, y0), PathCommand::LineTo(x1, y1), PathCommand::LineTo(x0, y1), PathCommand::Close]
}

/// Red rectangle (6..30 x 6..24 pt), a gradient clipped to 60..90 x 6..24 pt, a 50 % multiply
/// group with a green rectangle (105..135 x 6..24 pt), one glyph.
fn fixture() -> Vec<u8> {
    let options = ExportOptions { info_title: Some("AIPrivateData Illustrator fixture"), ..ExportOptions::default() };
    let mut ex = PdfExport::new(options, &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 150.0, 75.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.fill_path(p, &rect(6.0, 6.0, 30.0, 24.0), FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.push_clip(p, &rect(60.0, 6.0, 90.0, 24.0), FillRule::NonZero).unwrap();
    let stops = [GradientStop::new(0.0, [0.0, 0.0, 1.0]), GradientStop::new(1.0, [0.0, 0.0, 1.0])];
    let g = Gradient { geometry: GradientGeometry::Axial { from: [50.0, 0.0], to: [100.0, 0.0] }, stops: &stops, extend: [true, true], spread: GradientSpread::Pad, matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], profile: None };
    ex.fill_path_gradient(p, &rect(50.0, 0.0, 100.0, 40.0), FillRule::NonZero, &g).unwrap();
    ex.pop_clip(p).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect(105.0, 6.0, 135.0, 24.0), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.0, 0.0])).unwrap();
    ex.end_group(p, GroupStyle { opacity: 0.5, blend: BlendMode::Multiply, isolated: true, knockout: false, soft_mask: None }).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p, font, 18.0, [10.0, 45.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

/// Straight RGBA of a frame pixel (the PDF has no background: unpainted pixels are transparent).
fn px(f: &hsk_studio_harness::view::render::Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width + x) * 4) as usize;
    [f.rgba8[i], f.rgba8[i + 1], f.rgba8[i + 2], f.rgba8[i + 3]]
}

#[test]
fn ai_imports_as_editable_vectors_and_exports_as_svg() {
    let scratch = Scratch::new("ai-interop");
    let src = scratch.0.join("fixture.ai");
    std::fs::write(&src, fixture()).unwrap();
    let dest = scratch.0.join("fixture.handshake");
    let mut v = Viewer::new(Prefer::Cpu);
    let losses = v.import_ai(&src, &dest, false).unwrap();
    for code in ["ai_native_editability_not_imported", "ai_private_data_not_imported", "ai_text_as_outlines"] {
        assert!(losses.iter().any(|l| l.ends_with(code)), "{code} in {losses:?}");
    }
    // Editable: the vector layers carry Folio styles; pixels where the PDF drew them (y flipped,
    // x 4/3): red at (24, 80) px, the clipped gradient blue only inside 80..120 px, the green group
    // at 50 % opacity, transparent where nothing is drawn (the PDF has no background).
    assert!(v.layers().iter().filter(|r| r.kind == "vector").count() >= 4);
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).clone();
    assert_eq!((f.width, f.height), (200, 100));
    assert_eq!(px(&f, 24, 80), [255, 0, 0, 255]);
    assert_eq!(px(&f, 100, 80), [0, 0, 255, 255], "inside the clip");
    assert_eq!(px(&f, 72, 80)[3], 0, "gradient outside the clip");
    let green = px(&f, 160, 80);
    assert!(green[1] == 255 && green[0] == 0 && (120..=135).contains(&green[3]), "{green:?}");
    // The imported document exports as SVG (a red path, the clip reported as a loss).
    let (svg, svg_losses) = v.vector_svg().unwrap();
    assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
    assert!(svg.contains("#ff0000") || svg.contains("#f00"), "{svg}");
    assert!(svg_losses.iter().any(|l| l.contains("clip")), "{svg_losses:?}");
    let out = scratch.0.join("export.svg");
    v.export_vector_svg(&out, false).unwrap();
    assert!(v.export_vector_svg(&out, false).is_err());
    // Not an AI file, and the package is never written over an existing file.
    std::fs::write(scratch.0.join("plain.txt"), b"not an ai file").unwrap();
    assert!(v.import_ai(&scratch.0.join("plain.txt"), &scratch.0.join("x.handshake"), false).is_err());
    assert!(v.import_ai(&src, &dest, false).is_err());
}
