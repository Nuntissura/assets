//! Lane SMB-AI-ARROWHEADS-1 slice 3: composite's Folio adapter draws stroke arrowheads
//! (`StudioVectorRecords::stroke_markers`, STU-VEC-146): the marker outlines are united into the
//! stroke row's coverage plane, painted with the row's paint and opacity; under `inside_path` the
//! stroke uses nib's trimmed path; dashes never dash a marker; ERD terminators are refused typed.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{
    FillRule, OwnedPath, PathElement, PathPoint, REFUSE_MARKER_BRUSH, REFUSE_MARKER_SHAPE_UNBUILT,
    REFUSE_MARKER_WIDTH_PROFILE, StrokeStyle,
};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords};
use hsk_studio_nib::markers::{MarkerAlign, MarkerEnd, MarkerOptions, MarkerShape, MarkerSpec, marker_outlines};
use hsk_studio_nib::stroke::{StrokeOptions, outline_stroke};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use serde_json::{Value, json};

mod common;
use common::*;

/// Base of the 2 px stroke's `arrow.triangle` (size s = 2, length 3 s) at the end x = 40.
const BASE_X: i64 = 34;

/// A horizontal line from (2, 8) to (40, 8).
fn polyline() -> OwnedPath {
    OwnedPath {
        elements: vec![PathElement::MoveTo(PathPoint::new(2.0, 8.0)), PathElement::LineTo(PathPoint::new(40.0, 8.0))],
        fill_rule: FillRule::NonZero,
    }
}
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == identity("SVPT", 1)).then(polyline)
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
fn stroke_row(dash: Value) -> Value {
    json!({"row_kind":"stroke","paint":{"paint_kind":"solid","colour":{"r":0.0,"g":0.0,"b":1.0,"colour_profile_id":null}},
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
        "dash":dash,"dash_offset":{"value":0.0,"unit":"px"}},"opacity":50.0,"blend_mode":2,"visible":true})
}
/// Layer 4 uses style 1: one stroke row.
fn design(dash: Value) -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
        "name":"s1","publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":"s1","is_remote":false},"rows":[stroke_row(dash)]}]}))
    .expect("design records shape")
}
fn records(end: Option<(&str, &str)>, extra: Value) -> StudioVectorRecords {
    let mut r = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)});
    if let Some((token, align)) = end {
        r["stroke_markers"] = json!([{"style_id":identity("SSTY",1),"row":0,"start":"arrow.none","end":token,
            "start_scale":100.0,"end_scale":100.0,"alignment":align}]);
    }
    if let Value::Object(m) = extra {
        for (k, v) in m {
            r[k] = v;
        }
    }
    serde_json::from_value(r).expect("vector records shape")
}
const INSIDE: &str = "arrow_align.inside_path";
const OUTSIDE: &str = "arrow_align.outside_path";

fn prepare_with(design: &StudioDesignRecords, records: &StudioVectorRecords, res: &VectorResolution) -> PreparedVectors {
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    prepare_vectors(&open(&d), design, records, &Assets, BlendSpace::Encoded, res, &CancellationToken::default()).unwrap()
}
fn prepare(design: &StudioDesignRecords, records: &StudioVectorRecords) -> PreparedVectors {
    prepare_with(design, records, &VectorResolution::default())
}
/// The stroke plane of layer 4 as (global x, global y) -> coverage.
fn stroke_pixels(p: &PreparedVectors) -> Vec<((i64, i64), u8)> {
    let plane = p.plane(&identity("SLYR", 4)).expect("layer 4 prepared");
    let r = plane.placement.expect("non-empty plane");
    let w = i64::from(r.width);
    plane.strokes[0]
        .coverage
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let i = i as i64;
            ((r.x + i % w, r.y + i / w), *c)
        })
        .collect()
}
fn area(px: &[((i64, i64), u8)], keep: impl Fn(i64) -> bool) -> f64 {
    px.iter().filter(|((x, _), _)| keep(*x)).map(|(_, c)| f64::from(*c) / 255.0).sum()
}

#[test]
fn a_triangle_marker_adds_its_analytic_area_to_the_stroke_plane() {
    let got = prepare(&design(json!([])), &records(Some(("arrow.triangle", INSIDE)), json!({})));
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    // Analytic: the trimmed stroke [2, 34] x [7, 9] = 64 px plus the triangle (base 6 at x = 34,
    // tip at x = 40) = 18 px. Tolerance 0.5 px: per-pixel rounding of the u8 plane (<= 0.5 / 255
    // over ~110 edge pixels) and nib's flattening (straight edges here: exact).
    let px = stroke_pixels(&got);
    let total = area(&px, |_| true);
    assert!((total - 82.0).abs() < 0.5, "stroke + marker area {total}");
    let marker = area(&px, |x| x >= BASE_X);
    assert!((marker - 18.0).abs() < 0.5, "marker area {marker}");
    // The adapter's row equals the folio-free spec built from nib directly: one own-geometry row
    // (stroke outline of the trimmed path + the marker outline) with the row's paint and opacity.
    let spec = MarkerSpec { start: None, end: Some(MarkerEnd { shape: MarkerShape::Triangle, scale: 100.0 }), align: MarkerAlign::InsidePath };
    let m = marker_outlines(&polyline(), &spec, 2.0, &MarkerOptions::default()).unwrap();
    let mut outline = vec![outline_stroke(&m.trimmed, &StrokeStyle::new(2.0), &StrokeOptions::default()).unwrap().path];
    outline.extend(m.outlines());
    let row = FillSpec {
        paint: FillPaint::StrokeGeometry { outline, paint: Box::new(FillPaint::Solid([0.0, 0.0, 1.0, 1.0])) },
        mode: StudioBlendMode::Normal,
        opacity: 0.5,
    };
    let spec = VectorLayerSpec { layer_id: identity("SLYR", 4), geometry: VectorGeometry::Paths(vec![polyline()]), fills: vec![row] };
    let want = prepare_vector_layers(&[spec], &VectorResolution::default(), &CancellationToken::default()).unwrap();
    assert_eq!(got, want);
}

#[test]
fn pixels_away_from_the_marker_match_the_unmarked_render() {
    let plain = stroke_pixels(&prepare(&design(json!([])), &records(None, json!({}))));
    assert!((area(&plain, |_| true) - 76.0).abs() < 0.5, "unmarked stroke [2, 40] x [7, 9]");
    for (align, from) in [(INSIDE, BASE_X), (OUTSIDE, 40)] {
        let marked = stroke_pixels(&prepare(&design(json!([])), &records(Some(("arrow.triangle", align)), json!({}))));
        let at = |set: &[((i64, i64), u8)], q: (i64, i64)| set.iter().find(|(p, _)| *p == q).map_or(0, |(_, c)| *c);
        // Every pixel left of the marker (and of the inside-path trim) is identical.
        for &(q, c) in plain.iter().filter(|((x, _), _)| *x < from - 1) {
            assert_eq!(at(&marked, q), c, "{align} pixel {q:?}");
        }
        for &(q, c) in marked.iter().filter(|((x, _), _)| *x < from - 1) {
            assert_eq!(at(&plain, q), c, "{align} pixel {q:?}");
        }
    }
    // Outside the path the stroke keeps its full length and the marker lies beyond the end.
    let outside = stroke_pixels(&prepare(&design(json!([])), &records(Some(("arrow.triangle", OUTSIDE)), json!({}))));
    assert!((area(&outside, |x| x >= 40) - 18.0).abs() < 0.5);
    assert!((area(&outside, |_| true) - 94.0).abs() < 0.5);
}

#[test]
fn erd_terminators_and_unsupported_combinations_are_refused_typed() {
    let d = design(json!([]));
    for token in ["ERD_ONE", "ERD_MANY", "ERD_ONE_OR_MORE", "ERD_ZERO_OR_MORE", "ERD_ZERO_OR_ONE", "ERD_EXACTLY_ONE"] {
        let got = prepare(&d, &records(Some((token, INSIDE)), json!({})));
        assert_eq!(got.refusal(&identity("SLYR", 4)), Some(REFUSE_MARKER_SHAPE_UNBUILT), "{token}");
        assert!(got.plane(&identity("SLYR", 4)).is_none(), "no substitute is drawn");
    }
    assert_eq!(REFUSE_MARKER_SHAPE_UNBUILT, "marker_shape_unbuilt");
    // `NONE` at both ends draws the unmarked stroke.
    let none = prepare(&d, &records(Some(("NONE", INSIDE)), json!({})));
    assert_eq!(stroke_pixels(&none), stroke_pixels(&prepare(&d, &records(None, json!({})))));
    // The supersampled coverage mode keeps its stroke refusal; no marker is drawn there.
    let supersampled = VectorResolution { coverage: VectorCoverage::Supersampled, ..VectorResolution::default() };
    let got = prepare_with(&d, &records(Some(("arrow.triangle", INSIDE)), json!({})), &supersampled);
    assert_eq!(got.refusal(&identity("SLYR", 4)), Some("vector_stroke_not_rendered"));
    // A marker on a width-profiled path or a brushed stroke row is refused (no defined size/shape).
    let profile = json!({"width_profiles":[{"path_id":identity("SVPT",1),"points":[{"position":0.0,"width":1.0},{"position":1.0,"width":2.0}]}]});
    let got = prepare(&d, &records(Some(("arrow.triangle", INSIDE)), profile));
    assert_eq!(got.refusal(&identity("SLYR", 4)), Some(REFUSE_MARKER_WIDTH_PROFILE));
    let brush = json!({"brushes":[{"brush_id":"nib","name":"Nib","colorization":"none","kind":{"brush_kind":"calligraphic",
        "nib_angle":{"value":30.0},"nib_roundness":{"value":60.0},"nib_size":{"value":4.0}}}],
        "brush_strokes":[{"style_id":identity("SSTY",1),"row":0,"brush_id":"nib"}]});
    let got = prepare(&d, &records(Some(("arrow.triangle", INSIDE)), brush));
    assert_eq!(got.refusal(&identity("SLYR", 4)), Some(REFUSE_MARKER_BRUSH));
}

#[test]
fn a_dashed_stroke_leaves_the_marker_solid() {
    // Dash 3 on, 3 off from x = 2 along the trimmed path [2, 34].
    let dashed = design(json!([{"value":3.0,"unit":"px"},{"value":3.0,"unit":"px"}]));
    let got = stroke_pixels(&prepare(&dashed, &records(Some(("arrow.triangle", INSIDE)), json!({}))));
    let solid = stroke_pixels(&prepare(&design(json!([])), &records(Some(("arrow.triangle", INSIDE)), json!({}))));
    let at = |set: &[((i64, i64), u8)], q: (i64, i64)| set.iter().find(|(p, _)| *p == q).map_or(0, |(_, c)| *c);
    // Past the trim only the marker is drawn, identical with and without dashes.
    for &(q, c) in solid.iter().filter(|((x, _), _)| *x >= BASE_X) {
        assert_eq!(at(&got, q), c, "marker pixel {q:?}");
    }
    assert!((area(&got, |x| x >= BASE_X) - 18.0).abs() < 0.5, "the whole triangle");
    // The dashes before it: five full dashes [2,5] .. [26,29] and [32,34] (cut by the trim) = 17 px
    // long, 2 px wide.
    assert!((area(&got, |x| x < BASE_X) - 34.0).abs() < 0.5, "dashed stroke area");
}
