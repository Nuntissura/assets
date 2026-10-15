//! SMB-ILLUSTRATOR-INTEROP (feature `viewer`): the document CMYK profile is a Folio record saved
//! with the document; calligraphic and bristle brushes draw; the vector document exports as PDF
//! with swatches resolved and brushes and envelopes expanded (read back through Press).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioBrushColorization, StudioBrushKind, StudioBrushParam, StudioWarpAxis, StudioWarpStyle};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::brushes::PatternTileLayers;
use hsk_studio_harness::view::vector::{EnvelopeSpec, GradientStop, PaintSpec, RowKind, ShapeKind, SwatchColour, SwatchSpec, VectorTool};
use hsk_studio_press::{VectorEl, VectorItem, VectorMode, VectorPaint, read_vector_pages};

const W: usize = 96;
const H: usize = 64;
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];
const CMYK_ICC: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    v
}
fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}
fn px(f: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

#[test]
fn the_cmyk_profile_comes_from_folio_and_nib_brushes_draw() {
    let scratch = Scratch::new("interop-cmyk");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [40.0, 40.0], false).unwrap();
    let profile = v.set_document_cmyk_profile("Synthetic CMYK", CMYK_ICC).unwrap();
    assert!(profile.starts_with("SCPF-"));
    let ink = v.add_folio_swatch("Process red", &SwatchSpec::Global(SwatchColour::Cmyk([0.0, 1.0, 1.0, 0.0]))).unwrap();
    v.apply_swatch(&a, &ink, RowKind::Fill).unwrap();
    let colour = v.swatch_colour(&ink).unwrap();
    let painted = frame(&mut v);
    // Saved and reopened with nothing supplied by the caller: the profile record and its bytes
    // come back with the document, so the CMYK swatch still resolves and can be edited.
    let out = scratch.0.join("cmyk.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.swatch_colour(&ink).unwrap(), colour);
    assert_eq!(frame(&mut again), painted);
    again.edit_folio_swatch(&ink, &SwatchSpec::Global(SwatchColour::Cmyk([1.0, 0.0, 0.0, 0.0]))).unwrap();
    assert_ne!(again.swatch_colour(&ink).unwrap(), colour);
    // Calligraphic (a 25 % round 8 px nib turned 90 degrees: 8 px tall) along a horizontal line.
    again.set_vector_tool(VectorTool { fill_on: false, stroke_on: true, stroke: BLUE, stroke_width: 2.0, ..again.vector_tool().clone() });
    let line = again.draw_shape(ShapeKind::Line, [50.0, 30.0], [90.0, 30.0], false).unwrap();
    let p = StudioBrushParam::fixed;
    let nib = again.add_brush("Nib", StudioBrushKind::Calligraphic { nib_angle: p(90.0), nib_roundness: p(25.0), nib_size: p(8.0) }, StudioBrushColorization::None, &[], &PatternTileLayers::default()).unwrap();
    again.apply_brush(&line, &nib, None).unwrap();
    let f = frame(&mut again);
    assert_eq!((px(&f, 70, 27), px(&f, 70, 24)), (BLUE, WHITE), "blue 3 px above the line, white 6 px above");
    // A 2 mm (7.6 px) bristle brush stays within 4 px of the line.
    let bristle = again.add_brush("Bristle", StudioBrushKind::Bristle { bristle_size: 2.0, bristle_length: 100.0, bristle_density: 50.0 }, StudioBrushColorization::None, &[], &PatternTileLayers::default()).unwrap();
    again.apply_brush(&line, &bristle, None).unwrap();
    let f = frame(&mut again);
    assert_eq!(px(&f, 70, 24), WHITE);
    assert!((26..=34).any(|y| px(&f, 70, y) == BLUE));
    // Hostile: an empty or non-ICC profile is refused.
    assert!(again.set_document_cmyk_profile("empty", &[]).is_err());
    assert!(again.set_document_cmyk_profile("not icc", b"garbage").is_err());
}

#[test]
fn vector_documents_export_as_pdf_with_everything_expanded() {
    let scratch = Scratch::new("interop-pdf");
    let mut v = paper(&scratch);
    // A red rectangle (10..30, 10..20 px), a swatch-filled square, a gradient, an enveloped shape.
    v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 20.0], false).unwrap();
    let sq = v.draw_shape(ShapeKind::Rectangle, [40.0, 10.0], [50.0, 20.0], false).unwrap();
    let blue = v.add_swatch("Blue", &PaintSpec::Solid(BLUE)).unwrap();
    v.apply_swatch(&sq, &blue, RowKind::Fill).unwrap();
    let g = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [90.0, 20.0], false).unwrap();
    let ramp = vec![GradientStop { position: 0.0, colour: [0, 0, 0], opacity: 1.0 }, GradientStop { position: 1.0, colour: [255, 255, 255], opacity: 1.0 }];
    v.set_fill(&g, Some(&PaintSpec::Linear { from: [60.0, 15.0], to: [90.0, 15.0], stops: ramp })).unwrap();
    let e = v.draw_shape(ShapeKind::Rectangle, [10.0, 40.0], [40.0, 55.0], false).unwrap();
    v.make_envelope(&e, &EnvelopeSpec::Warp { style: StudioWarpStyle::Bulge, axis: StudioWarpAxis::Horizontal, bend: 50.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    let (bytes, losses) = v.vector_pdf_bytes().unwrap();
    assert!(losses.iter().all(|l| l.contains("raster_layer_not_exported")), "{losses:?}");
    let pages = read_vector_pages(&bytes, 1 << 24, &CancellationToken::default()).unwrap();
    assert_eq!(pages[0].media_box, [0.0, 0.0, W as f64 * 0.75, H as f64 * 0.75]);
    let fills: Vec<([f32; 4], Vec<[f64; 2]>)> = pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { path, mode: VectorMode::Fill { .. }, paint: VectorPaint::Solid(c), .. } => Some((
                *c,
                path.iter()
                    .filter_map(|e| match e {
                        VectorEl::MoveTo(a) | VectorEl::LineTo(a) => Some(*a),
                        _ => None,
                    })
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    // The red rectangle's corner (10, 10) px is (7.5, 40.5) pt from the bottom left (48 - 7.5).
    let red = fills.iter().find(|(c, _)| c[0] > 0.99 && c[1] < 0.01 && c[2] < 0.01).expect("red");
    assert!(red.1.iter().any(|q| (q[0] - 7.5).abs() < 1e-6 && (q[1] - 40.5).abs() < 1e-6), "{:?}", red.1);
    // The swatch's stand-in colour, the gradient as a shading, the bulged envelope above y = 40 px.
    assert!(fills.iter().any(|(c, _)| c[2] > 0.99 && c[0] < 0.01));
    assert!(pages[0].items.iter().any(|i| matches!(i, VectorItem::Draw { paint: VectorPaint::Gradient { axial: true, .. }, .. })));
    let top = (H as f64 - 40.0) * 0.75;
    let bulged = fills.iter().filter(|(c, _)| c[0] > 0.99 && c[1] < 0.01).flat_map(|(_, p)| p.iter()).any(|q| q[1] > top + 1.0 && q[0] > 7.5 && q[0] < 30.0);
    assert!(bulged, "the envelope's top edge rises above its source");
    // Written atomically, never over an existing file without overwrite.
    let out = scratch.0.join("doc.pdf");
    v.export_vector_pdf(&out, false).unwrap();
    assert!(v.export_vector_pdf(&out, false).is_err());
    assert!(std::fs::read(&out).unwrap().starts_with(b"%PDF-"));
}
