//! SMB-AI-TYPE-PATH-RECOLOR (feature `viewer`): recolour artwork as one undo step with global
//! swatch propagation (slice 2), gradient meshes created from a shape, edited and rasterised
//! (slice 3), and the separations preview of a vector document through Press (slice 4). Expected
//! values are hand-derived from the geometry (document px; 1 px = 0.75 pt on the page) and from
//! the swatch components. Illustrator parity is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{StudioAppearanceRow, StudioPaint};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::mesh::MeshEdit;
use hsk_studio_harness::view::vector::path_text::PathTextSpec;
use hsk_studio_harness::view::vector::{RowKind, ShapeKind, SwatchColour, SwatchSpec, VectorTool};
use hsk_studio_prism::{ColorizeMethod, RecolorAdjust, RecolorRow, RecolorTable};
use hsk_studio_type::{PathFlip, PathTextOptions};

const W: usize = 96;
const H: usize = 64;
const RED: [u8; 3] = [255, 0, 0];


fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, ..VectorTool::default() });
    v
}
fn fill(v: &Viewer, layer: &str) -> [f64; 3] {
    let style = v.layer_paint_style(layer).unwrap();
    style
        .rows
        .iter()
        .find_map(|r| match r {
            StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } => Some([colour.r, colour.g, colour.b]),
            _ => None,
        })
        .unwrap()
}
fn frame_px(v: &mut Viewer, x: usize, y: usize) -> [u8; 3] {
    v.render_if_dirty();
    let f = &v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8;
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

/// Rows of the frame (y) holding pure tool-red pixels in columns `x0..x1`.
fn red_rows(v: &mut Viewer, x0: usize, x1: usize) -> Vec<usize> {
    (0..H).filter(|y| (x0..x1).any(|x| frame_px(v, x, *y) == RED)).collect()
}

#[test]
fn ai_type_on_a_path_lays_out_and_converts_to_outlines() {
    let scratch = Scratch::new("ai-path-text");
    let mut v = paper(&scratch);
    // An open horizontal baseline from (10, 40) to (90, 40).
    let line = v.pen_point(10.0, 40.0, None, 1.0).unwrap();
    v.pen_point(90.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    let spec = |options| PathTextSpec { text: "HI".into(), family: "Ubuntu-Light".into(), size_pt: 40.0, options };
    let l = v.path_text_layout(&line, &spec(PathTextOptions::default())).unwrap();
    assert_eq!((l.glyphs.len(), l.overset_from, l.path_length_pt), (2, None, 80.0));
    // Rainbow on a horizontal line: glyph 0 at the path start, y up = document -y; glyph 1 one
    // advance on, at its arc start.
    assert!(l.glyphs[0].transform.iter().zip([1.0, 0.0, 0.0, -1.0, 10.0, 40.0]).all(|(a, b)| (a - b).abs() < 1e-9), "{:?}", l.glyphs[0].transform);
    assert!((l.glyphs[1].transform[4] - (10.0 + l.glyphs[1].arc_start_pt)).abs() < 1e-9 && l.glyphs[1].arc_start_pt > 0.0);
    // Converted to outlines: one new layer, ink above the baseline only (rows < 40), one undo step.
    let before = red_rows(&mut v, 0, W);
    assert!(before.is_empty(), "{before:?}");
    let (outlines, _) = v.text_on_path_to_outlines(&line, &spec(PathTextOptions::default())).unwrap();
    assert!(outlines.starts_with("SLYR-"));
    let rows = red_rows(&mut v, 0, W);
    assert!(!rows.is_empty() && rows.iter().all(|y| *y < 40), "{rows:?}");
    v.undo().unwrap();
    assert!(red_rows(&mut v, 0, W).is_empty());
    // Flipped: the text runs back along the line, on its other side (ink below the baseline).
    v.text_on_path_to_outlines(&line, &spec(PathTextOptions { flipped: PathFlip::Flipped, ..PathTextOptions::default() })).unwrap();
    let rows = red_rows(&mut v, 0, W);
    assert!(!rows.is_empty() && rows.iter().all(|y| *y >= 40), "{rows:?}");
    // A 5 px baseline holds no glyph: everything overset is reported, and nothing is drawn.
    let short = v.pen_point(10.0, 60.0, None, 1.0).unwrap();
    v.pen_point(15.0, 60.0, None, 1.0).unwrap();
    v.pen_end();
    let o = v.path_text_layout(&short, &spec(PathTextOptions::default())).unwrap();
    assert_eq!((o.glyphs.len(), o.overset_from, o.overset_count), (0, Some(0), 2));
    assert!(v.text_on_path_to_outlines(&short, &spec(PathTextOptions::default())).is_err());
    // Hostile: unknown font, empty or oversized text, bad size, undefined flip.
    assert!(v.path_text_layout(&line, &PathTextSpec { family: "Nope".into(), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { text: String::new(), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { text: "x".repeat(5000), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { size_pt: f64::NAN, ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &spec(PathTextOptions { flipped: PathFlip::Undefined, ..PathTextOptions::default() })).is_err());
}

#[test]
fn ai_recolor_artwork_is_one_undo_step_and_global_swatches_propagate() {
    let scratch = Scratch::new("ai-recolor");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [25.0, 25.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [35.0, 5.0], [55.0, 25.0], false).unwrap();
    let c = v.draw_shape(ShapeKind::Rectangle, [65.0, 5.0], [85.0, 25.0], false).unwrap();
    let d = v.draw_shape(ShapeKind::Rectangle, [35.0, 35.0], [55.0, 55.0], false).unwrap();
    let brand = v.add_folio_swatch("Brand", &SwatchSpec::Global(SwatchColour::Rgb([0.0, 0.0, 1.0]))).unwrap();
    let local = v.add_folio_swatch("Local", &SwatchSpec::Process(SwatchColour::Rgb([0.0, 1.0, 0.0]))).unwrap();
    v.apply_swatch(&b, &brand, RowKind::Fill).unwrap();
    v.apply_swatch(&d, &brand, RowKind::Fill).unwrap();
    v.apply_swatch(&c, &local, RowKind::Fill).unwrap();

    // Extraction over the four layers: red (a), Brand blue (b and d, one swatch), Local green (c);
    // weights = bounding-box areas (20 x 20 = 400 per layer use).
    let colours = v.artwork_colours(&[a.clone(), b.clone(), c.clone(), d.clone()]).unwrap();
    let found: Vec<([f64; 3], f64, usize, Option<String>)> = colours.iter().map(|x| (x.rgb, x.weight, x.uses, x.swatch_id.clone())).collect();
    assert_eq!(found, vec![([0.0, 0.0, 1.0], 800.0, 2, Some(brand.clone())), ([0.0, 1.0, 0.0], 400.0, 1, Some(local.clone())), ([1.0, 0.0, 0.0], 400.0, 1, None)]);

    let before = v.vector_entry().unwrap();
    let table = RecolorTable {
        rows: vec![
            RecolorRow { current: vec![[1.0, 0.0, 0.0]], new: Some([1.0, 1.0, 0.0]) },
            RecolorRow { current: vec![[0.0, 0.0, 1.0]], new: Some([1.0, 0.0, 1.0]) },
            RecolorRow { current: vec![[0.0, 1.0, 0.0]], new: Some([0.0, 0.0, 0.0]) },
        ],
        method: ColorizeMethod::Exact,
        ..RecolorTable::default()
    };
    let report = v.recolor_artwork(&[a.clone(), b.clone(), c.clone(), d.clone()], &table, &RecolorAdjust::default()).unwrap();
    assert_eq!((report.layers.clone(), report.swatches.clone()), (vec![a.clone(), c.clone()], vec![brand.clone()]));
    // Unbound row recoloured in place; the global swatch is redefined and BOTH its instances follow;
    // the process swatch binding is detached and the row recoloured locally (the swatch keeps green).
    assert_eq!(fill(&v, &a), [1.0, 1.0, 0.0]);
    assert_eq!(v.swatch(&brand).unwrap().components, vec![1.0, 0.0, 1.0]);
    assert_eq!((fill(&v, &b), fill(&v, &d)), ([1.0, 0.0, 1.0], [1.0, 0.0, 1.0]));
    assert_eq!(fill(&v, &c), [0.0, 0.0, 0.0]);
    assert_eq!(v.swatch(&local).unwrap().components, vec![0.0, 1.0, 0.0]);
    let entry = v.vector_entry().unwrap();
    let c_style = v.layer_paint_style(&c).unwrap().style_id;
    assert!(!entry.records.swatch_paints.iter().any(|p| p.style_id == c_style && p.swatch_id == local), "process binding detached");
    assert_eq!((frame_px(&mut v, 15, 15), frame_px(&mut v, 45, 45), frame_px(&mut v, 75, 15)), ([255, 255, 0], [255, 0, 255], [0, 0, 0]));

    // ONE undo step restores every layer, the swatch and the binding.
    v.undo().unwrap();
    let after = v.vector_entry().unwrap();
    assert_eq!(after.records, before.records);
    assert_eq!(after.design, before.design);
    assert_eq!((frame_px(&mut v, 15, 15), frame_px(&mut v, 45, 45)), ([255, 0, 0], [0, 0, 255]));

    // Hostile: a non-vector layer, a table that maps nothing, non-finite sliders.
    assert!(v.recolor_artwork(&["SLYR-019abcde-0000-7000-8000-000000000999".into()], &table, &RecolorAdjust::default()).is_err());
    assert!(v.recolor_artwork(&[], &RecolorTable::default(), &RecolorAdjust::default()).unwrap_err().contains("nothing changed"));
    assert!(v.recolor_artwork(&[], &table, &RecolorAdjust { hue_shift_deg: f64::NAN, ..RecolorAdjust::default() }).is_err());
}

#[test]
fn ai_gradient_mesh_from_a_shape_edits_rasterises_and_undoes() {
    let scratch = Scratch::new("ai-mesh");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 30.0], false).unwrap();
    let id = v.create_gradient_mesh(&a, 2, 2).unwrap();
    let m = v.gradient_mesh(&a).unwrap();
    assert_eq!((m.mesh_id.as_str(), m.rows, m.columns, m.points.len(), m.source_path_id.is_some()), (id.as_str(), 2, 2, 9, true));
    // The mesh conforms to the rectangle: its corners are the rectangle's, the centre point the
    // rectangle centre (30, 20); every point the layer's red fill, opaque.
    let mut corners: Vec<(i64, i64)> = [0usize, 2, 6, 8].iter().map(|i| (m.points[*i].x.round() as i64, m.points[*i].y.round() as i64)).collect();
    corners.sort_unstable();
    assert_eq!(corners, vec![(10, 10), (10, 30), (50, 10), (50, 30)]);
    assert!((m.points[4].x - 30.0).abs() < 1e-9 && (m.points[4].y - 20.0).abs() < 1e-9);
    assert!(m.points.iter().all(|p| [p.colour.r, p.colour.g, p.colour.b] == [1.0, 0.0, 0.0] && p.opacity == 100.0));

    // Centre point blue: pixel (30.5, 20.5) lies 0.5 px from it inside a 20 x 10 px patch, so the
    // centre's bilinear weight is (1 - 0.5/20)(1 - 0.5/10) = 0.92625.
    v.edit_gradient_mesh(&a, MeshEdit::SetPoint { row: 1, column: 1, rgb: [0.0, 0.0, 1.0], opacity: 100.0 }).unwrap();
    let (raster, origin) = v.gradient_mesh_raster(&a, 1.0).unwrap();
    assert_eq!((origin, raster.width, raster.height), ([10.0, 10.0], 40, 20));
    let p = raster.pixel(20, 10).unwrap();
    let expect = [0.073_75, 0.0, 0.926_25, 1.0];
    assert!(p.iter().zip(expect).all(|(x, y)| (x - y).abs() < 1e-5), "{p:?}");
    // Mesh lines: insert a column, then remove it again; each edit is one undo step.
    v.edit_gradient_mesh(&a, MeshEdit::InsertColumn { column: 0, at: 0.5 }).unwrap();
    assert_eq!((v.gradient_mesh(&a).unwrap().columns, v.gradient_mesh(&a).unwrap().points.len()), (3, 12));
    v.edit_gradient_mesh(&a, MeshEdit::RemoveColumn(1)).unwrap();
    assert_eq!(v.gradient_mesh(&a).unwrap().columns, 2);
    v.undo().unwrap();
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(v.gradient_mesh(&a).unwrap().points[4].colour.b, 0.0, "colour edit undone");
    v.undo().unwrap();
    assert!(v.gradient_mesh(&a).is_err(), "creation undone");
    // Hostile: grids over the cap, a second mesh, a bad point.
    assert!(v.create_gradient_mesh(&a, 0, 2).is_err());
    assert!(v.create_gradient_mesh(&a, 257, 1).is_err());
    assert!(v.create_gradient_mesh(&a, 200, 200).is_err());
    v.create_gradient_mesh(&a, 1, 1).unwrap();
    assert!(v.create_gradient_mesh(&a, 1, 1).is_err());
    assert!(v.edit_gradient_mesh(&a, MeshEdit::SetPoint { row: 5, column: 0, rgb: [0.0; 3], opacity: 100.0 }).is_err());
    assert!(v.edit_gradient_mesh(&a, MeshEdit::SetPoint { row: 0, column: 0, rgb: [0.0; 3], opacity: 101.0 }).is_err());
}

