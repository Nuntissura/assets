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
use hsk_studio_harness::view::vector::separations::VectorSeparationOptions;
use hsk_studio_harness::view::vector::{RowKind, ShapeKind, SwatchColour, SwatchSpec, VectorTool};
use hsk_studio_press::{InkLimits, LimitKind, SepNote};
use hsk_studio_prism::{ColorizeMethod, RecolorAdjust, RecolorRow, RecolorTable};
use hsk_studio_type::{PathFlip, PathTextOptions};

const W: usize = 96;
const H: usize = 64;
const RED: [u8; 3] = [255, 0, 0];
const CMYK_ICC: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

/// sRGB v4 -> the synthetic CMYK profile through Prism's wide engine (f32 output as the plates hold).
fn prism_cmyk(rgb: [f32; 3]) -> [f64; 4] {
    use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
    use hsk_studio_prism::{Intent, ProfileInput, WideEngine, WideSource, WideTransformRequest, profile_hash};
    struct Quiet;
    impl hsk_studio_observe::SinkPort for Quiet {
        fn try_send(&mut self, _class: hsk_studio_observe::DeliveryClass, _bytes: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
            Ok(())
        }
    }
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "test").unwrap();
    let (dst, src, res) = (DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000b001").unwrap(), DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000b002").unwrap(), DomainId::parse("SSWT-019abcde-0000-7000-8000-00000000b003").unwrap());
    let srgb = hsk_studio_harness::view::develop::SRGB_V4_ICC;
    let request = WideTransformRequest {
        source: WideSource::Profile(ProfileInput { profile_id: &src, bytes: srgb, expected_sha256: profile_hash(srgb) }),
        destination: ProfileInput { profile_id: &dst, bytes: CMYK_ICC, expected_sha256: profile_hash(CMYK_ICC) },
        pixels: &rgb,
        intent: Intent::RelativeColorimetric,
        black_point_compensation: false,
        revision: 0,
        expected_revision: 0,
        correlation_id: 1,
        resource_id: &res,
        actor: &actor,
    };
    let out = WideEngine::new(actor.clone(), 2).unwrap().transform(&request, &CancellationToken::default(), &mut Quiet).unwrap();
    let p = &out.pixels;
    [0, 1, 2, 3].map(|i| f64::from(p[i]).clamp(0.0, 1.0) as f32 as f64)
}

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

#[test]
fn ai_vector_separations_preview_through_press() {
    let scratch = Scratch::new("ai-separations");
    let mut v = paper(&scratch);
    v.set_document_cmyk_profile("Synthetic CMYK", CMYK_ICC).unwrap();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 20.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [40.0, 10.0], [50.0, 20.0], false).unwrap();
    let c = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [90.0, 20.0], false).unwrap();
    let e = v.draw_shape(ShapeKind::Rectangle, [20.0, 45.0], [50.0, 60.0], false).unwrap();
    let d = v.draw_shape(ShapeKind::Rectangle, [10.0, 40.0], [40.0, 55.0], false).unwrap();
    let f = v.draw_shape(ShapeKind::Rectangle, [60.0, 40.0], [90.0, 55.0], false).unwrap();
    let cmyk = v.add_folio_swatch("C20 M40", &SwatchSpec::Global(SwatchColour::Cmyk([0.2, 0.4, 0.0, 0.0]))).unwrap();
    let spot = v.add_folio_swatch("Ink 1", &SwatchSpec::Spot { ink: "Ink 1".into(), measured_lab: Some([50.0, 0.0, 0.0]), alternate: Some(SwatchColour::Cmyk([0.5, 0.0, 1.0, 0.0])) }).unwrap();
    let grey = v.add_folio_swatch("Grey", &SwatchSpec::Global(SwatchColour::Gray(0.25))).unwrap();
    let cyan = v.add_folio_swatch("Cyan", &SwatchSpec::Global(SwatchColour::Cmyk([1.0, 0.0, 0.0, 0.0]))).unwrap();
    let black = v.add_folio_swatch("Black", &SwatchSpec::Global(SwatchColour::Cmyk([0.0, 0.0, 0.0, 1.0]))).unwrap();
    v.apply_swatch(&a, &cmyk, RowKind::Fill).unwrap();
    v.apply_swatch_tinted(&b, &spot, RowKind::Fill, Some(50.0)).unwrap();
    v.apply_swatch(&c, &grey, RowKind::Fill).unwrap();
    v.apply_swatch(&e, &cyan, RowKind::Fill).unwrap();
    v.apply_swatch(&d, &black, RowKind::Fill).unwrap();
    // f keeps the tool's unbound RGB red.
    let _ = f;

    let inks = v.vector_ink_manager().unwrap();
    assert_eq!(inks.inks.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), vec!["Process Cyan", "Process Magenta", "Process Yellow", "Process Black", "Ink 1"]);
    assert_eq!(inks.ink("Ink 1").unwrap().process_alternate, Some([0.5, 0.0, 1.0, 0.0]));

    let opts = VectorSeparationOptions { overprint_black: true, ..VectorSeparationOptions::default() };
    let (sep, losses) = v.vector_separations(&opts).unwrap();
    assert!(losses.iter().all(|l| l.ends_with("raster_layer_not_separated")), "{losses:?}");
    assert_eq!((sep.width, sep.height), (72, 48));
    // Document px (x, y) -> page points (0.75 x, 0.75 (64 - y)).
    let read = |x: f64, y: f64| -> Vec<(String, f64)> { sep.sample_pt(x * 0.75, (64.0 - y) * 0.75).unwrap().into_iter().map(|r| (r.ink, (r.percent * 1e4).round() / 1e4)).collect() };
    let tone = |x: f64, y: f64, ink: &str| read(x, y).into_iter().find(|r| r.0 == ink).map_or(0.0, |r| r.1);
    // CMYK swatch: its own components.
    assert_eq!((tone(20.0, 15.0, "Process Cyan"), tone(20.0, 15.0, "Process Magenta"), tone(20.0, 15.0, "Process Black")), (20.0, 40.0, 0.0));
    // Spot at a 50 % per-use tint on its own plate, nothing on process.
    assert_eq!((tone(45.0, 15.0, "Ink 1"), tone(45.0, 15.0, "Process Cyan")), (50.0, 0.0));
    // GRAY 0.25 -> K 75.
    assert_eq!(tone(75.0, 15.0, "Process Black"), 75.0);
    // 100 % K overprints (overprint_black): the cyan below stays where they overlap.
    assert_eq!((tone(30.0, 50.0, "Process Cyan"), tone(30.0, 50.0, "Process Black")), (100.0, 100.0));
    assert_eq!((tone(45.0, 57.0, "Process Cyan"), tone(15.0, 42.0, "Process Black")), (100.0, 100.0));
    // Unbound RGB red goes through Prism (sRGB v4 -> the document CMYK profile, relative
    // colorimetric): the plates equal an independent Prism conversion of (1, 0, 0).
    let want = prism_cmyk([1.0, 0.0, 0.0]);
    for (ink, w) in ["Process Cyan", "Process Magenta", "Process Yellow", "Process Black"].iter().zip(want) {
        assert!((tone(75.0, 47.0, ink) - w * 100.0).abs() < 1e-3, "{ink}: {} vs {}", tone(75.0, 47.0, ink), w * 100.0);
    }
    assert!(want.iter().any(|v| *v > 0.5), "{want:?}");
    // Ink limit: the overprinted overlap holds 200 %.
    let report = sep.check_ink_limits(&InkLimits { total_ink_limit: Some(150.0), ..InkLimits::default() }).unwrap();
    assert!(report.findings.iter().any(|f| f.limit == LimitKind::Total && f.max_percent >= 200.0 - 1e-6), "{report:?}");
    // Knockout (no overprint policy): the black knocks the cyan out in the overlap.
    let (knock, _) = v.vector_separations(&VectorSeparationOptions::default()).unwrap();
    let k = knock.sample_pt(30.0 * 0.75, (64.0 - 50.0) * 0.75).unwrap();
    assert_eq!(k.iter().find(|r| r.ink == "Process Cyan").map_or(0.0, |r| r.percent), 0.0);

    // Without a CMYK profile an RGB object is not separated: counted and noted, never guessed.
    let scratch2 = Scratch::new("ai-separations-rgb");
    let mut w = paper(&scratch2);
    w.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 20.0], false).unwrap();
    let (rgb, losses) = w.vector_separations(&VectorSeparationOptions::default()).unwrap();
    assert_eq!(rgb.unseparated, 1);
    assert!(rgb.notes.contains(&SepNote::Unseparated));
    assert!(losses.iter().any(|l| l.ends_with("unseparated_no_cmyk_profile")));
    // Hostile resolution.
    assert!(w.vector_separations(&VectorSeparationOptions { px_per_pt: f64::NAN, ..VectorSeparationOptions::default() }).is_err());
    assert!(w.vector_separations(&VectorSeparationOptions { px_per_pt: 1.0e6, ..VectorSeparationOptions::default() }).is_err());
}
