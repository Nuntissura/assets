//! SMB-ILLUSTRATOR-TOOLS-2 slice 4 (feature `viewer`): Folio global swatches (CMYK, Lab, spot,
//! tint through Prism; links persist), align and distribute, the transform tools, envelope
//! distort, art / scatter / pattern brushes and graphic styles in the viewer. Each command is one
//! undo step. Expected values are hand-derived from the geometry (document px, 1 px = 1 pixel)
//! or from CIE L* (Prism's Lab -> sRGB).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{StudioAppearanceRow, StudioArtScale, StudioBrushColorization, StudioBrushKind, StudioBrushParam, StudioPaint, StudioPatternFit, StudioPatternTiles, StudioRotationRelative, StudioWarpAxis, StudioWarpStyle};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::arrange::{Align, Distribute, ReferencePoint, TransformEach};
use hsk_studio_harness::view::vector::brushes::PatternTileLayers;
use hsk_studio_harness::view::vector::{AlignTarget, EnvelopeSpec, PaintSpec, RowKind, ShapeKind, StrokePanel, SwatchColour, SwatchSpec, TransformOp, VectorTool};

const W: usize = 96;
const H: usize = 64;
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
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
fn fill_rgb(v: &Viewer, layer: &str) -> [f64; 3] {
    let style = v.layer_paint_style(layer).unwrap();
    match style.rows.iter().find_map(|r| match r {
        StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } => Some(colour.clone()),
        _ => None,
    }) {
        Some(c) => [c.r, c.g, c.b],
        None => panic!("no solid fill"),
    }
}
fn grey(rgb: [f64; 3], want: f64) {
    assert!(rgb.iter().all(|c| (c - want).abs() < 0.01), "{rgb:?} != {want}");
}
fn bounds(v: &Viewer, layer: &str) -> [f64; 4] {
    let b = v.layer_bounds(layer).unwrap();
    [b.min[0], b.min[1], b.max[0], b.max[1]]
}
fn near(a: [f64; 4], e: [f64; 4]) {
    assert!(a.iter().zip(e).all(|(x, y)| (x - y).abs() < 1e-6), "{a:?} != {e:?}");
}
fn ids(v: &[&String]) -> Vec<String> {
    v.iter().map(|s| (*s).clone()).collect()
}

#[test]
fn folio_swatches_resolve_through_prism_and_their_links_persist() {
    let scratch = Scratch::new("tools2-swatch");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [40.0, 40.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [50.0, 5.0], [90.0, 40.0], false).unwrap();
    // Lab L* 50 is sRGB 0.4663 grey (CIE L* -> Y 0.1842 -> sRGB encoding).
    let lab = v.add_folio_swatch("Lab grey", &SwatchSpec::Global(SwatchColour::Lab([50.0, 0.0, 0.0]))).unwrap();
    grey(v.swatch_colour(&lab).unwrap(), 0.4663);
    // CMYK needs the document's CMYK profile (a Folio record since SMB-ILLUSTRATOR-INTEROP); with
    // Prism's synthetic profile it resolves into range.
    assert!(v.add_folio_swatch("CMYK red", &SwatchSpec::Global(SwatchColour::Cmyk([0.0, 1.0, 1.0, 0.0]))).is_err());
    v.set_document_cmyk_profile("Synthetic CMYK", CMYK_ICC).unwrap();
    let cmyk = v.add_folio_swatch("CMYK red", &SwatchSpec::Global(SwatchColour::Cmyk([0.0, 1.0, 1.0, 0.0]))).unwrap();
    let c = v.swatch_colour(&cmyk).unwrap();
    assert!(c.iter().all(|x| (0.0..=1.0).contains(x)), "{c:?}");
    // A spot uses its measured Lab; a 50 % tint of it is L* 75 (sRGB 0.7216).
    let spot = v.add_folio_swatch("Ink 1", &SwatchSpec::Spot { ink: "Ink 1".into(), measured_lab: Some([50.0, 0.0, 0.0]), alternate: Some(SwatchColour::Cmyk([0.5, 0.0, 1.0, 0.0])) }).unwrap();
    let tint = v.add_folio_swatch("Ink 1 50%", &SwatchSpec::Tint { base: spot.clone(), percent: 50.0 }).unwrap();
    grey(v.swatch_colour(&tint).unwrap(), 0.7216);
    v.apply_swatch(&a, &tint, RowKind::Fill).unwrap();
    v.apply_swatch_tinted(&b, &spot, RowKind::Fill, Some(50.0)).unwrap();
    grey(fill_rgb(&v, &a), 0.7216);
    grey(fill_rgb(&v, &b), 0.7216);
    // Editing the spot updates the tint's user and the per-use tint user in ONE step: L* 65.
    v.edit_folio_swatch(&spot, &SwatchSpec::Spot { ink: "Ink 1".into(), measured_lab: Some([30.0, 0.0, 0.0]), alternate: None }).unwrap();
    grey(fill_rgb(&v, &a), 0.6156);
    grey(fill_rgb(&v, &b), 0.6156);
    v.undo().unwrap();
    grey(fill_rgb(&v, &a), 0.7216);
    grey(fill_rgb(&v, &b), 0.7216);
    v.redo().unwrap();
    // Saved and reopened: the bindings are Folio records, so an edit still reaches both layers.
    let out = scratch.0.join("swatches.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.swatches().unwrap().len(), 4);
    again.edit_folio_swatch(&spot, &SwatchSpec::Spot { ink: "Ink 1".into(), measured_lab: Some([0.0, 0.0, 0.0]), alternate: None }).unwrap();
    grey(fill_rgb(&again, &a), 0.4663);
    grey(fill_rgb(&again, &b), 0.4663);
    // A spot without any definition cannot be shown: refused typed, nothing changes.
    assert!(again.add_folio_swatch("Bare", &SwatchSpec::Spot { ink: "Bare".into(), measured_lab: None, alternate: None }).is_err());
    assert_eq!(again.swatches().unwrap().len(), 4);
    grey(fill_rgb(&again, &a), 0.4663);
}

#[test]
fn align_distribute_and_transform_tools_are_one_step_each() {
    let scratch = Scratch::new("tools2-arrange");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 20.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [40.0, 30.0], [50.0, 50.0], false).unwrap();
    let c = v.draw_shape(ShapeKind::Rectangle, [60.0, 5.0], [90.0, 15.0], false).unwrap();
    let all = ids(&[&a, &b, &c]);
    v.align_layers(&all, &AlignTarget::Selection, Align::Left).unwrap();
    near(bounds(&v, &b), [10.0, 30.0, 20.0, 50.0]);
    near(bounds(&v, &c), [10.0, 5.0, 40.0, 15.0]);
    v.undo().unwrap();
    near(bounds(&v, &b), [40.0, 30.0, 50.0, 50.0]);
    near(bounds(&v, &c), [60.0, 5.0, 90.0, 15.0]);
    // Centres 20, 45, 75: the middle one moves to 47.5.
    v.distribute_layers(&all, &AlignTarget::Selection, Distribute::Edge(Align::CentreHorizontal)).unwrap();
    near(bounds(&v, &b), [42.5, 30.0, 52.5, 50.0]);
    // Rotate a by 90 degrees about its centre (20, 15).
    let before = frame(&mut v);
    assert_eq!(px(&before, 20, 22), WHITE);
    v.transform_layers(std::slice::from_ref(&a), TransformOp::Rotate(90.0), ReferencePoint::Centre).unwrap();
    near(bounds(&v, &a), [15.0, 5.0, 25.0, 25.0]);
    assert_eq!(px(&frame(&mut v), 20, 22), RED);
    // Transform-each: b and c doubled about their own centres.
    v.transform_each_layers(&ids(&[&b, &c]), &TransformEach { scale_horizontal: 200.0, scale_vertical: 200.0, ..TransformEach::default() }).unwrap();
    near(bounds(&v, &b), [37.5, 20.0, 57.5, 60.0]);
    near(bounds(&v, &c), [45.0, 0.0, 105.0, 20.0]);
    v.undo().unwrap();
    near(bounds(&v, &c), [60.0, 5.0, 90.0, 15.0]);
    // Reflect across the vertical axis through the selection's left edge, and shear.
    v.transform_layers(std::slice::from_ref(&c), TransformOp::Reflect { axis_deg: 90.0 }, ReferencePoint::Left).unwrap();
    near(bounds(&v, &c), [30.0, 5.0, 60.0, 15.0]);
    v.transform_layers(std::slice::from_ref(&c), TransformOp::Shear { angle_deg: 45.0, axis_deg: 0.0 }, ReferencePoint::Top).unwrap();
    near(bounds(&v, &c), [30.0, 5.0, 70.0, 15.0]);
    // Saved and reopened, the moved geometry stays; hostile transforms are refused.
    let out = scratch.0.join("arranged.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    near(bounds(&again, &a), [15.0, 5.0, 25.0, 25.0]);
    assert!(v.transform_layers(std::slice::from_ref(&a), TransformOp::Rotate(f64::NAN), ReferencePoint::Centre).is_err());
    assert!(v.transform_layers(std::slice::from_ref(&a), TransformOp::Scale { horizontal: 0.0, vertical: 100.0 }, ReferencePoint::Centre).is_err());
    assert!(v.align_layers(&all, &AlignTarget::KeyObject("nope".into()), Align::Left).is_err());
    near(bounds(&v, &a), [15.0, 5.0, 25.0, 25.0]);
}

#[test]
fn envelopes_warp_mesh_and_top_object_then_release_and_expand() {
    let scratch = Scratch::new("tools2-envelope");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    let plain = frame(&mut v);
    assert_eq!(px(&plain, 40, 17), WHITE);
    // Bulge 50 %: the top edge's middle rises half a half-height (10 px * 0.5).
    v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Bulge, axis: StudioWarpAxis::Horizontal, bend: 50.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    assert_eq!(px(&frame(&mut v), 40, 17), RED);
    v.undo().unwrap();
    assert_eq!(frame(&mut v), plain);
    // Mesh: drag the bottom-right point to (80, 55); the point (70, 47) is now inside.
    v.make_envelope(&r, &EnvelopeSpec::Mesh { rows: 1, columns: 1 }, 100.0, false).unwrap();
    assert_eq!(frame(&mut v), plain, "the plain grid changes nothing");
    v.move_envelope_point(&r, 3, [80.0, 55.0]).unwrap();
    assert_eq!(px(&frame(&mut v), 70, 47), RED);
    v.release_envelope(&r).unwrap();
    assert_eq!(frame(&mut v), plain);
    // Top object: a blue ellipse receives the red rectangle and is not drawn itself.
    let e = v.draw_shape(ShapeKind::Ellipse, [64.0, 4.0], [94.0, 34.0], false).unwrap();
    v.set_fill(&e, Some(&PaintSpec::Solid(BLUE))).unwrap();
    assert_eq!(px(&frame(&mut v), 79, 19), BLUE);
    v.make_envelope(&r, &EnvelopeSpec::TopObject { layer: e.clone() }, 100.0, false).unwrap();
    let wrapped = frame(&mut v);
    assert_eq!((px(&wrapped, 79, 19), px(&wrapped, 40, 30)), (RED, WHITE));
    v.release_envelope(&r).unwrap();
    assert_eq!(px(&frame(&mut v), 79, 19), BLUE);
    // Expand bakes the live warp into the path: same pixels, no envelope.
    v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Bulge, axis: StudioWarpAxis::Horizontal, bend: 50.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    let live = frame(&mut v);
    v.expand_envelope(&r).unwrap();
    assert!(v.envelope(&r).unwrap().is_none());
    assert_eq!(frame(&mut v), live);
    assert!((bounds(&v, &r)[1] - 15.0).abs() < 0.05);
    // Hostile: bend out of range, an unknown top object, a point index past the mesh.
    assert!(v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Arc, axis: StudioWarpAxis::Horizontal, bend: 150.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).is_err());
    assert!(v.make_envelope(&r, &EnvelopeSpec::TopObject { layer: "SLYR-019abcde-0000-7000-8000-00000000ffff".into() }, 50.0, false).is_err());
    assert!(v.move_envelope_point(&r, 9, [0.0, 0.0]).is_err());
    assert_eq!(frame(&mut v), live);
}

#[test]
fn art_scatter_and_pattern_brushes_paint_strokes_and_persist() {
    let scratch = Scratch::new("tools2-brush");
    let mut v = paper(&scratch);
    // Brush art: a green 10 x 4 bar (its own colour is kept: colorize none).
    let art = v.draw_shape(ShapeKind::Rectangle, [2.0, 55.0], [12.0, 59.0], false).unwrap();
    v.set_fill(&art, Some(&PaintSpec::Solid(GREEN))).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: true, stroke: BLUE, stroke_width: 2.0, ..v.vector_tool().clone() });
    let line = v.draw_shape(ShapeKind::Line, [10.0, 30.0], [90.0, 30.0], false).unwrap();
    assert_eq!(px(&frame(&mut v), 50, 30), BLUE);
    let p = StudioBrushParam::fixed;
    let art_kind = StudioBrushKind::Art { art_width: p(100.0), art_scale_mode: StudioArtScale::StretchToStrokeLength, art_guide_start: None, art_guide_end: None, flip_along: false, flip_across: false, art: Vec::new() };
    let leaf = v.add_brush("Leaf", art_kind, StudioBrushColorization::None, std::slice::from_ref(&art), &PatternTileLayers::default()).unwrap();
    v.apply_brush(&line, &leaf, None).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 50, 30), px(&f, 50, 31), px(&f, 50, 33)), (GREEN, GREEN, WHITE), "the bar spans y 28..32 along the line");
    // Scatter: half-size copies (5 x 2) every 10 px from x = 15.
    let scatter = StudioBrushKind::Scatter { scatter_size: p(50.0), scatter_spacing: p(200.0), scatter_offset: p(0.0), scatter_rotation: p(0.0), scatter_rotation_relative_to: StudioRotationRelative::Path, seed: 1, art: Vec::new() };
    let dots = v.add_brush("Dots", scatter, StudioBrushColorization::None, std::slice::from_ref(&art), &PatternTileLayers::default()).unwrap();
    v.apply_brush(&line, &dots, None).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 15, 30), px(&f, 20, 30)), (GREEN, WHITE));
    // Pattern: side tiles stretched to fit: a continuous band again.
    let pattern = StudioBrushKind::Pattern { pattern_brush_scale: p(100.0), fit_mode: StudioPatternFit::StretchToFit, flip_along: false, flip_across: false, show_auto_generated_corner_tiles: false, tiles: StudioPatternTiles::default() };
    let rope = v.add_brush("Rope", pattern, StudioBrushColorization::None, &[], &PatternTileLayers { side: vec![art.clone()], ..PatternTileLayers::default() }).unwrap();
    v.apply_brush(&line, &rope, None).unwrap();
    let patterned = frame(&mut v);
    assert_eq!((px(&patterned, 20, 30), px(&patterned, 50, 31)), (GREEN, GREEN));
    // Saved and reopened: brushes and the binding are Folio records.
    let out = scratch.0.join("brushes.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.brushes().unwrap().len(), 3);
    assert_eq!(frame(&mut again), patterned);
    // Removing the brush strokes plainly again (one step); undo restores it.
    again.remove_brush(&line).unwrap();
    assert_eq!(px(&frame(&mut again), 50, 30), BLUE);
    again.undo().unwrap();
    assert_eq!(frame(&mut again), patterned);
    // Hostile: an unknown brush, a bad override, a calligraphic brush (not drawn: refused at render).
    assert!(again.apply_brush(&line, "brush-99", None).is_err());
    assert!(again.apply_brush(&line, &leaf, Some(f64::NAN)).is_err());
    assert_eq!(frame(&mut again), patterned);
}

#[test]
fn graphic_styles_link_redefine_break_merge_clear_and_reduce() {
    let scratch = Scratch::new("tools2-graphic");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [40.0, 40.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [50.0, 5.0], [90.0, 40.0], false).unwrap();
    v.set_fill(&a, Some(&PaintSpec::Solid(GREEN))).unwrap();
    v.set_stroke(&a, Some(&StrokePanel::solid(BLUE, 4.0))).unwrap();
    let g = v.add_graphic_style("Styles/Green", &a).unwrap();
    assert_eq!(v.graphic_styles().unwrap(), vec![(g.clone(), "Styles/Green".to_string())]);
    v.apply_graphic_style(&b, &g).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 70, 20), px(&f, 50, 20)), (GREEN, BLUE));
    // Redefine from a: b follows.
    v.set_fill(&a, Some(&PaintSpec::Solid(RED))).unwrap();
    v.redefine_graphic_style(&g, &a).unwrap();
    assert_eq!(px(&frame(&mut v), 70, 20), RED);
    // Break the link: b keeps red while the style moves on.
    v.break_graphic_style(&b).unwrap();
    v.set_fill(&a, Some(&PaintSpec::Solid(BLUE))).unwrap();
    v.redefine_graphic_style(&g, &a).unwrap();
    assert_eq!(px(&frame(&mut v), 70, 20), RED);
    // Merge adds the style's fill above b's own: blue shows.
    v.merge_graphic_style(&b, &g).unwrap();
    assert_eq!(px(&frame(&mut v), 70, 20), BLUE);
    // Clear, undo; reduce to basic keeps the first fill (red) and the first stroke.
    v.clear_appearance(&b).unwrap();
    assert_eq!(px(&frame(&mut v), 70, 20), WHITE);
    v.undo().unwrap();
    v.reduce_to_basic_appearance(&b).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 70, 20), px(&f, 50, 20)), (RED, BLUE));
    assert!(v.apply_graphic_style(&b, "SSTY-019abcde-0000-7000-8000-00000000ffff").is_err());
}
