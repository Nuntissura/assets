//! Lane SMB-ILLUSTRATOR-TOOLS-2 slice 3: composite's Folio adapter draws brushed stroke rows
//! (`StudioBrushStroke` -> nib art / scatter brushes, colorize none or tints) and envelopes
//! (`StudioEnvelope` -> nib `envelope_paths`, before or after the appearance). Expected layers are
//! folio-free specs built from nib directly and prepared by `prepare_vector_layers`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint, REFUSE_BRUSH_COLORIZATION, REFUSE_ENVELOPE_PAINT, StrokeStyle};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords};
use hsk_studio_nib::brush::{ArtBrush, ArtScale, Dyn, RotationRelative, ScatterBrush, art_brush, scatter_brush};
use hsk_studio_nib::stroke::{StrokeOptions, outline_stroke};
use hsk_studio_nib::warp::{Envelope, Warp, WarpAxis, WarpStyle, envelope_paths};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use serde_json::{Value, json};

mod common;
use common::*;

fn poly(points: &[(f64, f64)], closed: bool) -> OwnedPath {
    let mut o = OwnedPath { elements: Vec::new(), fill_rule: FillRule::NonZero };
    for (i, (x, y)) in points.iter().enumerate() {
        let q = PathPoint::new(*x, *y);
        o.elements.push(if i == 0 { PathElement::MoveTo(q) } else { PathElement::LineTo(q) });
    }
    if closed {
        o.elements.push(PathElement::Close);
    }
    o
}
/// SVPT 1: the layer path (a 20 x 10 box); 2 and 3: brush art.
fn path(n: u8) -> OwnedPath {
    match n {
        1 => poly(&[(2.0, 3.0), (22.0, 3.0), (22.0, 13.0), (2.0, 13.0)], true),
        2 => poly(&[(0.0, -1.0), (4.0, -1.0), (4.0, 1.0), (0.0, 1.0)], true),
        _ => poly(&[(4.0, -1.0), (6.0, 0.0), (4.0, 1.0)], true),
    }
}
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (1..=3).find(|n| identity("SVPT", *n) == path_id).map(path)
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r":r,"g":g,"b":b,"colour_profile_id":null})
}
fn solid(r: f64, g: f64, b: f64) -> Value {
    json!({"paint_kind":"solid","colour":colour(r, g, b)})
}
fn fill_row(paint: Value) -> Value {
    json!({"row_kind":"fill","paint":paint,"opacity":100.0,"blend_mode":2,"visible":true})
}
fn stroke_row(paint: Value) -> Value {
    json!({"row_kind":"stroke","paint":paint,
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
        "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true})
}
fn design(rows: Vec<Value>) -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT","name":"s",
        "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"s","is_remote":false},"rows":rows}]}))
    .expect("design shape")
}
fn art() -> Value {
    json!([{"path_id":identity("SVPT",2),"colour":colour(1.0,0.0,0.0)},{"path_id":identity("SVPT",3),"colour":colour(0.0,0.0,1.0)}])
}
fn records(members: Value) -> StudioVectorRecords {
    let mut r = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)});
    for (k, v) in members.as_object().cloned().unwrap_or_default() {
        r[k] = v;
    }
    serde_json::from_value(r).expect("records shape")
}
fn brush(kind: Value, colorization: &str) -> Value {
    json!({"brushes":[{"brush_id":"b","name":"B","colorization":colorization,"kind":kind}],
        "brush_strokes":[{"style_id":identity("SSTY",1),"row":0,"brush_id":"b"}]})
}
fn prepare(design: &StudioDesignRecords, records: &StudioVectorRecords) -> PreparedVectors {
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    prepare_vectors(&open(&d), design, records, &Assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn expected(geometry: Vec<OwnedPath>, fills: Vec<FillSpec>) -> PreparedVectors {
    let spec = VectorLayerSpec { layer_id: identity("SLYR", 4), geometry: VectorGeometry::Paths(geometry), fills };
    prepare_vector_layers(&[spec], &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn own(outline: Vec<OwnedPath>, c: [f32; 4]) -> FillSpec {
    FillSpec { paint: FillPaint::StrokeGeometry { outline, paint: Box::new(FillPaint::Solid(c)) }, mode: StudioBlendMode::Normal, opacity: 1.0 }
}

#[test]
fn brushed_stroke_rows_draw_each_art_path_with_its_colour() {
    let arts = [path(2), path(3)];
    // Art brush, stretched along the box outline: one row per art path in its own colour.
    let kind = json!({"brush_kind":"art","art_width":{"value":100.0},"art_scale_mode":"stretch_to_stroke_length","flip_along":false,"flip_across":false,"art":art()});
    let got = prepare(&design(vec![stroke_row(solid(0.0, 1.0, 0.0))]), &records(brush(kind, "none")));
    let out = art_brush(&path(1), &arts, &ArtBrush { width: 100.0, scale: ArtScale::Stretch, flip_along: false, flip_across: false }).unwrap();
    let want = expected(vec![path(1)], vec![own(vec![out[0].clone()], [1.0, 0.0, 0.0, 1.0]), own(vec![out[1].clone()], [0.0, 0.0, 1.0, 1.0])]);
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    assert_eq!(got, want);
    // Scatter with tints of the stroke colour: red art (L = 0.2126) and blue art (L = 0.0722).
    let kind = json!({"brush_kind":"scatter","scatter_size":{"value":100.0},"scatter_spacing":{"value":100.0},"scatter_offset":{"value":0.0},"scatter_rotation":{"value":0.0},"scatter_rotation_relative_to":"path","seed":3,"art":art()});
    let got = prepare(&design(vec![stroke_row(solid(0.0, 0.5, 1.0))]), &records(brush(kind, "tints")));
    let fixed = |v: f64| Dyn::fixed(v);
    let out = scatter_brush(&path(1), &arts, &ScatterBrush { size: fixed(100.0), spacing: fixed(100.0), offset: fixed(0.0), rotation: fixed(0.0), relative: RotationRelative::Path, seed: 3 }).unwrap();
    let tint = |l: f32, s: f32| 1.0 - (1.0 - l) * (1.0 - s);
    let (lr, lb) = (0.2126_f32, 0.0722_f32);
    let want = expected(
        vec![path(1)],
        vec![own(vec![out[0].clone()], [tint(lr, 0.0), tint(lr, 0.5), tint(lr, 1.0), 1.0]), own(vec![out[1].clone()], [tint(lb, 0.0), tint(lb, 0.5), tint(lb, 1.0), 1.0])],
    );
    assert_eq!(got, want);
    // A calligraphic brush (lane SMB-ILLUSTRATOR-INTEROP) sweeps its nib in the stroke colour.
    let nib = json!({"brush_kind":"calligraphic","nib_angle":{"value":30.0},"nib_roundness":{"value":60.0},"nib_size":{"value":4.0}});
    let got = prepare(&design(vec![stroke_row(solid(0.0, 1.0, 0.0))]), &records(brush(nib, "none")));
    let swept = hsk_studio_nib::brush::calligraphic_brush(&path(1), &hsk_studio_nib::brush::CalligraphicBrush { angle: 30.0, roundness: 60.0, size: 4.0 }).unwrap();
    assert_eq!(got, expected(vec![path(1)], vec![own(vec![swept], [0.0, 1.0, 0.0, 1.0])]));
    // Refusal: tints need a solid stroke paint; hue shift is not drawn.
    let shades = json!({"brush_kind":"art","art_width":{"value":100.0},"art_scale_mode":"proportional","flip_along":false,"flip_across":false,"art":art()});
    assert_eq!(prepare(&design(vec![stroke_row(solid(0.0, 1.0, 0.0))]), &records(brush(shades, "hue_shift"))).refusal(&identity("SLYR", 4)), Some(REFUSE_BRUSH_COLORIZATION));
}

#[test]
fn envelopes_wrap_the_paths_or_the_drawn_appearance() {
    let warp = Warp { style: WarpStyle::Bulge, axis: WarpAxis::Horizontal, bend: 0.5, distortion_horizontal: 0.0, distortion_vertical: 0.0 };
    let env = |appearance: bool| {
        json!({"envelopes":[{"layer_id":identity("SLYR",4),"envelope":{"envelope_kind":"warp","warp_style":"bulge","warp_axis":"horizontal","warp_bend":0.5,"warp_distortion_horizontal":0.0,"warp_distortion_vertical":0.0},"envelope_fidelity":50.0,"distort_appearance":appearance}]})
    };
    let rows = vec![fill_row(solid(1.0, 0.0, 0.0)), stroke_row(solid(0.0, 0.0, 1.0))];
    let style = StrokeStyle::new(2.0);
    // Paths first: the stroke follows the wrapped path.
    let wrapped = envelope_paths(&[path(1), path(1)], &Envelope::Warp(warp), 50.0).unwrap();
    let outline = outline_stroke(&wrapped[1], &style, &StrokeOptions::default()).unwrap().path;
    let want = expected(vec![wrapped[0].clone()], vec![FillSpec { paint: FillPaint::Solid([1.0, 0.0, 0.0, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 }, own(vec![outline], [0.0, 0.0, 1.0, 1.0])]);
    let got = prepare(&design(rows.clone()), &records(env(false)));
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    assert_eq!(got, want);
    // Appearance first: the stroke outline itself is wrapped, in one frame with the fill.
    let flat = outline_stroke(&path(1), &style, &StrokeOptions::default()).unwrap().path;
    let both = envelope_paths(&[path(1), flat], &Envelope::Warp(warp), 50.0).unwrap();
    let want = expected(vec![both[0].clone()], vec![FillSpec { paint: FillPaint::Solid([1.0, 0.0, 0.0, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 }, own(vec![both[1].clone()], [0.0, 0.0, 1.0, 1.0])]);
    assert_eq!(prepare(&design(rows), &records(env(true))), want);
    // Distorting gradient fills is asked for but composite does not distort paints: refused typed.
    let gradient = json!({"paint_kind":"gradient","gradient":{"geometry":{"gradient_kind":"linear","x1":2.0,"y1":0.0,"x2":22.0,"y2":0.0},"transform":[1.0,0.0,0.0,1.0,0.0,0.0],"spread":"pad","interpolation":"classic","dither":false,
        "stops":[{"position":0.0,"midpoint":null,"opacity":100.0,"colour":colour(0.0,0.0,0.0)},{"position":100.0,"midpoint":null,"opacity":100.0,"colour":colour(1.0,1.0,1.0)}]}});
    let mut e = env(false);
    e["envelopes"][0]["distort_linear_gradient_fills"] = json!(true);
    assert_eq!(prepare(&design(vec![fill_row(gradient)]), &records(e)).refusal(&identity("SLYR", 4)), Some(REFUSE_ENVELOPE_PAINT));
}

#[test]
fn expand_vectors_returns_the_drawn_shapes_for_export() {
    // Lane SMB-ILLUSTRATOR-INTEROP: fill over the geometry, the stroke as its outline, a bristle
    // brush as its swept shape (bristle size in mm: 96 / 25.4 px per mm).
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    let snap = open(&d);
    let design = design(vec![fill_row(solid(1.0, 0.0, 0.0)), stroke_row(solid(0.0, 0.0, 1.0))]);
    let (layers, refused) = vector::expand_vectors(&snap, &design, &records(json!({})), &Assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
    assert!(refused.is_empty());
    let outline = outline_stroke(&path(1), &StrokeStyle::new(2.0), &StrokeOptions::default()).unwrap().path;
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].layer_id, identity("SLYR", 4));
    assert_eq!(layers[0].rows.iter().map(|r| (r.paths.clone(), r.paint.clone())).collect::<Vec<_>>(), vec![(vec![path(1)], FillPaint::Solid([1.0, 0.0, 0.0, 1.0])), (vec![outline], FillPaint::Solid([0.0, 0.0, 1.0, 1.0]))]);
    let bristle = json!({"brush_kind":"bristle","bristle_size":1.0,"bristle_length":100.0,"bristle_density":50.0});
    let mut bound = brush(bristle, "none");
    bound["brush_strokes"][0]["row"] = json!(1);
    let (layers, _) = vector::expand_vectors(&snap, &design, &records(bound), &Assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
    let swept = hsk_studio_nib::brush::bristle_brush(&path(1), &hsk_studio_nib::brush::BristleBrush { size: 96.0 / 25.4, length: 100.0, density: 50.0 }).unwrap();
    assert_eq!(layers[0].rows[1].paths, vec![swept]);
}
