//! Lane SMB-AI-LIVE-EFFECTS-1: composite's Folio adapter evaluates a vector layer's live filter
//! stack (Folio `StudioVectorRecords::live_filters`, STU-FX-002/003/027, STU-VEC-154) through nib's
//! `live_effect::evaluate`, in list order, before the appearance is drawn. Expected layers are
//! folio-free specs built from nib directly and prepared by `prepare_vector_layers` (the method of
//! `tests/vector_tools.rs`). Records are written as JSON, so the red commit compiles and fails at run
//! time while Folio refuses the member as unknown. Laws are nib's declared Studio readings v1;
//! vendor parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint, StrokeStyle, expand_vectors};
use hsk_studio_composite::*;
use hsk_studio_folio::{Snapshot, StudioDesignRecords, StudioImageSource, StudioVectorNetwork, StudioVectorRecords};
use hsk_studio_nib::live_effect::{EffectRecord, ParamValue, evaluate};
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
/// SVPT 1: the layer path (a 40 x 20 box at 10, 10); SVPT 2: the second layer's box.
fn path(n: u8) -> OwnedPath {
    match n {
        1 => poly(&[(10.0, 10.0), (50.0, 10.0), (50.0, 30.0), (10.0, 30.0)], true),
        _ => poly(&[(60.0, 40.0), (80.0, 40.0), (80.0, 55.0), (60.0, 55.0)], true),
    }
}
/// The 2x2 grid network of `tests/vector_network.rs` on SVPT 1.
fn network() -> Value {
    json!({"schema_id":"hsk.studio.vector_network@1","path_id":identity("SVPT",1),
        "vertices":[{"x":2.0,"y":3.0},{"x":7.0,"y":3.0},{"x":12.0,"y":3.0},{"x":12.0,"y":13.0},{"x":7.0,"y":13.0},{"x":2.0,"y":13.0}],
        "segments":[{"start":0,"end":1},{"start":1,"end":2},{"start":2,"end":3},{"start":3,"end":4},{"start":4,"end":5},{"start":5,"end":0},{"start":1,"end":4}],
        "regions":[{"winding_rule":"NONZERO","loops":[[0,6,4,5]],"fills":true}]})
}
struct Assets {
    network: bool,
}
const PLAIN: Assets = Assets { network: false };
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (1..=2).find(|n| identity("SVPT", *n) == path_id).map(path)
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
    fn network(&self, path_id: &str) -> Option<StudioVectorNetwork> {
        (self.network && path_id == identity("SVPT", 1)).then(|| serde_json::from_value(network()).expect("network shape"))
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
fn red() -> StudioDesignRecords {
    design(vec![fill_row(solid(1.0, 0.0, 0.0))])
}
fn records(members: Value) -> StudioVectorRecords {
    let mut r = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)});
    for (k, v) in members.as_object().cloned().unwrap_or_default() {
        r[k] = v;
    }
    serde_json::from_value(r).expect("records shape")
}
/// The common mixed fixture in px; `second` adds vector layer 6 (SVPT 2).
fn doc(unit: &str, second: bool) -> Snapshot {
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!(unit);
    if second {
        d["layers"].as_array_mut().unwrap().push(layer(6, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",2)]})));
        d["graph"]["operations"].as_array_mut().unwrap().push(source(6, "vector"));
        d["graph"]["outputs"].as_array_mut().unwrap().push(address(6, "source", "out"));
    }
    open(&d)
}
fn prepare_in(snap: &Snapshot, design: &StudioDesignRecords, records: &StudioVectorRecords, assets: &Assets) -> PreparedVectors {
    prepare_vectors(snap, design, records, assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn prepare(design: &StudioDesignRecords, records: &StudioVectorRecords) -> PreparedVectors {
    prepare_in(&doc("px", false), design, records, &PLAIN)
}
fn expand(design: &StudioDesignRecords, records: &StudioVectorRecords) -> vector::Expanded {
    expand_vectors(&doc("px", false), design, records, &PLAIN, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn expected(geometry: Vec<OwnedPath>, fills: Vec<FillSpec>) -> PreparedVectors {
    let spec = VectorLayerSpec { layer_id: identity("SLYR", 4), geometry: VectorGeometry::Paths(geometry), fills };
    prepare_vector_layers(&[spec], &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn fill(c: [f32; 4]) -> FillSpec {
    FillSpec { paint: FillPaint::Solid(c), mode: StudioBlendMode::Normal, opacity: 1.0 }
}
fn own(outline: Vec<OwnedPath>, c: [f32; 4]) -> FillSpec {
    FillSpec { paint: FillPaint::StrokeGeometry { outline, paint: Box::new(FillPaint::Solid(c)) }, mode: StudioBlendMode::Normal, opacity: 1.0 }
}

// Typed parameters (STU-VEC-154: `{type, key, value}`, type one of Int | Real | Bool | String).
fn real(key: &str, v: f64) -> Value {
    json!({"type":"Real","key":key,"value":v})
}
fn int(key: &str, v: i64) -> Value {
    json!({"type":"Int","key":key,"value":v})
}
fn token(key: &str, v: &str) -> Value {
    json!({"type":"String","key":key,"value":v})
}
fn filter(id: &str, kind: &str, params: &[Value]) -> Value {
    json!({"instance_id":id,"filter_kind":format!("hsk.studio.vector_effect.{kind}"),"version":1,"enabled":true,"params":params})
}
fn live(layer_n: u8, filters: Vec<Value>) -> Value {
    json!({"live_filters":[{"layer_id":identity("SLYR",layer_n),"filters":filters}]})
}
/// nib's record from the same typed parameters (the oracle reads them independently of composite).
fn nib_record(kind: &str, params: &[Value]) -> EffectRecord {
    let entries: Vec<(String, ParamValue)> = params
        .iter()
        .map(|p| {
            let key = p["key"].as_str().unwrap().to_string();
            let value = match p["type"].as_str().unwrap() {
                "Int" => ParamValue::Int(p["value"].as_i64().unwrap()),
                "Real" => ParamValue::Real(p["value"].as_f64().unwrap()),
                "Bool" => ParamValue::Bool(p["value"].as_bool().unwrap()),
                _ => ParamValue::String(p["value"].as_str().unwrap().to_string()),
            };
            (key, value)
        })
        .collect();
    let borrowed: Vec<(&str, ParamValue)> = entries.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    EffectRecord::from_parts(&format!("hsk.studio.vector_effect.{kind}"), 1, &borrowed).unwrap_or_else(|e| panic!("{kind}: {e}"))
}
fn evaluated(sources: &[OwnedPath], chain: &[EffectRecord]) -> Vec<OwnedPath> {
    let mut current = sources.to_vec();
    for r in chain {
        current = evaluate(&current, r, &CancellationToken::default()).unwrap_or_else(|e| panic!("{:?}: {e}", r.kind()));
    }
    current
}

/// The seven evaluating effects of STU-FX-027 with every parameter their law reads.
fn seven() -> Vec<(&'static str, Vec<Value>)> {
    let corners = |prefix: &str, q: [[f64; 2]; 4]| -> Vec<Value> {
        q.iter().enumerate().flat_map(|(i, [h, v])| [real(&format!("{prefix}{i}_h"), *h), real(&format!("{prefix}{i}_v"), *v)]).collect()
    };
    let mut distort = corners("src", [[10.0, 10.0], [50.0, 10.0], [50.0, 30.0], [10.0, 30.0]]);
    distort.extend(corners("dst", [[5.0, 8.0], [55.0, 10.0], [48.0, 34.0], [12.0, 28.0]]));
    vec![
        ("roughen", vec![token("roughen_mode", "mode.absolute"), real("roughen_size_absolute", 3.0), real("roughen_detail", 10.0), token("roughen_points", "points.corner"), int("seed", 7)]),
        ("zig_zag", vec![token("zigzag_mode", "mode.absolute"), real("zigzag_size", 2.0), real("zigzag_ridges_per_segment", 3.0), token("zigzag_points", "points.corner")]),
        ("pucker_bloat", vec![real("pucker_bloat_factor", -30.0)]),
        ("twist", vec![real("twist_angle", 45.0)]),
        ("tweak", vec![token("tweak_mode", "mode.absolute"), real("tweak_horizontal", 3.0), real("tweak_vertical", 2.0), int("seed", 5)]),
        ("free_distort", distort),
        ("step_repeat", vec![int("copies", 2), real("move_horizontal", 5.0), real("move_vertical", 3.0)]),
    ]
}

#[test]
fn each_evaluating_effect_draws_through_prepare_and_expand() {
    let red_fill = fill([1.0, 0.0, 0.0, 1.0]);
    for (kind, params) in seven() {
        let r = records(live(4, vec![filter("f1", kind, &params)]));
        let effected = evaluated(&[path(1)], &[nib_record(kind, &params)]);
        assert_ne!(effected, vec![path(1)], "{kind}: the effect changes the geometry");
        let got = prepare(&red(), &r);
        assert_eq!(got.refusal(&identity("SLYR", 4)), None, "{kind}");
        assert_eq!(got, expected(effected.clone(), vec![red_fill.clone()]), "{kind}");
        // Export reads the same geometry (SVG, PDF and separations go through expand_vectors).
        let (layers, refused) = expand(&red(), &r);
        assert!(refused.is_empty(), "{kind}: {refused:?}");
        let rows: Vec<(Vec<OwnedPath>, FillPaint)> = layers[0].rows.iter().map(|x| (x.paths.clone(), x.paint.clone())).collect();
        assert_eq!(rows, vec![(effected, FillPaint::Solid([1.0, 0.0, 0.0, 1.0]))], "{kind}");
    }
}

#[test]
fn a_stack_of_disabled_filters_draws_byte_identically_to_no_stack() {
    let (kind, params) = seven().swap_remove(3);
    let mut off = filter("f1", kind, &params);
    off["enabled"] = json!(false);
    let none = prepare(&red(), &records(json!({})));
    assert_eq!(prepare(&red(), &records(live(4, vec![off.clone(), off]))), none);
    assert_eq!(none, expected(vec![path(1)], vec![fill([1.0, 0.0, 0.0, 1.0])]));
}

#[test]
fn filters_apply_in_list_order_and_each_result_feeds_the_next() {
    let pucker = vec![real("pucker_bloat_factor", -30.0)];
    let twist = vec![real("twist_angle", 45.0)];
    let one_then_two = evaluated(&[path(1)], &[nib_record("pucker_bloat", &pucker), nib_record("twist", &twist)]);
    let two_then_one = evaluated(&[path(1)], &[nib_record("twist", &twist), nib_record("pucker_bloat", &pucker)]);
    assert_ne!(one_then_two, two_then_one, "the order matters for this pair");
    let a = prepare(&red(), &records(live(4, vec![filter("a", "pucker_bloat", &pucker), filter("b", "twist", &twist)])));
    assert_eq!(a, expected(one_then_two, vec![fill([1.0, 0.0, 0.0, 1.0])]));
    let b = prepare(&red(), &records(live(4, vec![filter("b", "twist", &twist), filter("a", "pucker_bloat", &pucker)])));
    assert_eq!(b, expected(two_then_one, vec![fill([1.0, 0.0, 0.0, 1.0])]));
    // A disabled entry is skipped in place; the others keep their order.
    let mut off = filter("c", "roughen", &seven()[0].1);
    off["enabled"] = json!(false);
    let c = prepare(&red(), &records(live(4, vec![filter("a", "pucker_bloat", &pucker), off, filter("b", "twist", &twist)])));
    assert_eq!(c, a);
}

#[test]
fn strokes_follow_the_effected_path_and_an_envelope_wraps_it() {
    let (kind, params) = seven().swap_remove(0);
    let effected = evaluated(&[path(1)], &[nib_record(kind, &params)]);
    // A stroke row strokes the effected path, not the source.
    let blue = design(vec![stroke_row(solid(0.0, 0.0, 1.0))]);
    let outline = outline_stroke(&effected[0], &StrokeStyle::new(2.0), &StrokeOptions::default()).unwrap().path;
    let got = prepare(&blue, &records(live(4, vec![filter("f1", kind, &params)])));
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    assert_eq!(got, expected(effected.clone(), vec![own(vec![outline], [0.0, 0.0, 1.0, 1.0])]));
    // Filters, then the envelope (Studio reading, parity UNVERIFIED): the warp wraps the effect.
    let warp = Warp { style: WarpStyle::Bulge, axis: WarpAxis::Horizontal, bend: 0.5, distortion_horizontal: 0.0, distortion_vertical: 0.0 };
    let mut members = live(4, vec![filter("f1", kind, &params)]);
    members["envelopes"] = json!([{"layer_id":identity("SLYR",4),"envelope":{"envelope_kind":"warp","warp_style":"bulge","warp_axis":"horizontal","warp_bend":0.5,"warp_distortion_horizontal":0.0,"warp_distortion_vertical":0.0},"envelope_fidelity":50.0}]);
    // The adapter wraps the fill and stroke bases in one frame (as tests/vector_tools.rs).
    let wrapped = envelope_paths(&[effected[0].clone(), effected[0].clone()], &Envelope::Warp(warp), 50.0).unwrap();
    assert_eq!(prepare(&red(), &records(members)), expected(vec![wrapped[0].clone()], vec![fill([1.0, 0.0, 0.0, 1.0])]));
}

#[test]
fn step_repeat_copies_keep_the_width_profile_of_their_source() {
    // Step repeat returns the source then each copy; output k strokes with source k % n's profile,
    // so a naive one-to-one zip would silently drop both copies.
    let blue = design(vec![stroke_row(solid(0.0, 0.0, 1.0))]);
    let mut members = live(4, vec![filter("f1", "step_repeat", &[int("copies", 2), real("move_horizontal", 50.0)])]);
    members["width_profiles"] = json!([{"path_id":identity("SVPT",1),"points":[{"position":0.0,"width":2.0},{"position":1.0,"width":6.0}]}]);
    let (layers, refused) = expand(&blue, &records(members.clone()));
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(layers[0].rows.len(), 1);
    assert_eq!(layers[0].rows[0].paths.len(), 3, "the source and two copies, each with the variable outline");
    let left: Vec<f64> = layers[0].rows[0]
        .paths
        .iter()
        .map(|p| p.elements.iter().filter_map(|e| match e { PathElement::MoveTo(q) | PathElement::LineTo(q) | PathElement::CubicTo(_, _, q) => Some(q.x), _ => None }).fold(f64::INFINITY, f64::min))
        .collect();
    assert!(left[1] > left[0] + 40.0 && left[2] > left[1] + 40.0, "{left:?}");
    assert_eq!(prepare(&blue, &records(members)).refusal(&identity("SLYR", 4)), None);
}

fn refusal_of(members: Value) -> Option<&'static str> {
    prepare(&red(), &records(members)).refusal(&identity("SLYR", 4))
}

#[test]
fn hostile_or_unsupported_entries_refuse_the_layer_typed() {
    let twist = |params: &[Value]| live(4, vec![filter("f1", "twist", params)]);
    let mut other_version = filter("f1", "twist", &[real("twist_angle", 10.0)]);
    other_version["version"] = json!(2);
    let cases: Vec<(Value, &str)> = vec![
        (live(4, vec![filter("f1", "scribble", &[])]), "vector_effect_unknown_effect"),
        (json!({"live_filters":[{"layer_id":identity("SLYR",4),"filters":[other_version]}]}), "vector_effect_unsupported_version"),
        (twist(&[real("twist_angle", 10.0), real("twist_radius", 3.0)]), "vector_effect_unknown_parameter"),
        (twist(&[int("twist_angle", 10)]), "vector_effect_wrong_parameter_type"),
        (live(4, vec![filter("f1", "roughen", &[real("roughen_size_absolute", 7200.5)])]), "vector_effect_out_of_hard_range"),
        // A parameter whose table default is unknown and that is not set: never an invented default.
        (twist(&[]), "vector_effect_parameter_unset"),
        (live(4, vec![filter("f1", "convert_to_shape", &[token("target_shape", "shape.ellipse")])]), "vector_effect_convert_to_shape_sizing_law_not_recovered"),
        (live(4, vec![filter("f1", "object_mosaic", &[])]), "vector_effect_object_mosaic_grid_law_not_recovered"),
        // Copies near i64::MAX: refused by nib's budget before any allocation.
        (live(4, vec![filter("f1", "step_repeat", &[int("copies", i64::MAX)])]), "vector_effect_output_over_budget"),
        (live(4, vec![filter("f1", "step_repeat", &[int("copies", -1)])]), "vector_effect_not_a_count"),
        (live(4, vec![filter("f1", "step_repeat", &[real("move_horizontal", 4000.5)])]), "vector_effect_out_of_hard_range"),
        (live(4, vec![filter("f1", "zig_zag", &[token("zigzag_mode", "mode.absolute"), real("zigzag_size", 2.0e9), real("zigzag_ridges_per_segment", 3.0), token("zigzag_points", "points.corner")])]), "vector_effect_beyond_engine_range"),
    ];
    for (members, code) in cases {
        assert_eq!(refusal_of(members.clone()), Some(code), "{members}");
    }
    // A huge move inside the hard range: the plane is refused before it is allocated.
    let far = refusal_of(live(4, vec![filter("f1", "step_repeat", &[int("copies", 2000), real("move_horizontal", 4000.0)])]));
    assert!(matches!(far, Some("vector_over_limit" | "vector_work_limit")), "{far:?}");
    // A refused entry later in the stack refuses the layer too; nothing is drawn from the source.
    let tail = live(4, vec![filter("f1", "twist", &[real("twist_angle", 10.0)]), filter("f2", "scribble", &[])]);
    let got = prepare(&red(), &records(tail));
    assert_eq!(got.refusal(&identity("SLYR", 4)), Some("vector_effect_unknown_effect"));
    assert!(got.plane(&identity("SLYR", 4)).is_none(), "a refused layer is not drawn from its raw source");
}

#[test]
fn layers_the_slice_does_not_evaluate_refuse_typed() {
    let twist = live(4, vec![filter("f1", "twist", &[real("twist_angle", 10.0)])]);
    // A network path: next slice.
    let net = prepare_in(&doc("px", false), &red(), &records(twist.clone()), &Assets { network: true });
    assert_eq!(net.refusal(&identity("SLYR", 4)), Some("vector_effect_network"));
    // Path text bound to the layer would follow the raw path.
    let mut bound = twist.clone();
    bound["path_texts"] = json!([{"story_id":identity("STXT",1),"path_layer_id":identity("SLYR",4),"path_id":identity("SVPT",1),"start_t":0.0,"end_t":1.0}]);
    assert_eq!(refusal_of(bound), Some("vector_effect_text_bound"));
    // A document whose unit is not px (nib's pt reads as px only there).
    let mm = prepare_in(&doc("mm", false), &red(), &records(twist.clone()), &PLAIN);
    assert_eq!(mm.refusal(&identity("SLYR", 4)), Some("vector_geometry_unit"));
    // The same layers without the stack are drawn.
    assert_eq!(prepare_in(&doc("px", false), &red(), &records(json!({})), &Assets { network: true }).refusal(&identity("SLYR", 4)), None);
    assert_eq!(prepare_in(&doc("mm", false), &red(), &records(json!({})), &PLAIN).refusal(&identity("SLYR", 4)), None);
}

#[test]
fn a_filtered_layer_used_as_raw_geometry_refuses_rather_than_using_its_raw_shape() {
    // Layer 6 carries an enabled filter; clip, vector mask, envelope top object and boolean operand
    // readers would take its raw shape: each refuses `vector_effect_source` (applying the effect
    // there is a later slice).
    let snap = doc("px", true);
    let filtered = |extra: Value| {
        let mut m = live(6, vec![filter("f1", "twist", &[real("twist_angle", 10.0)])]);
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            m[k] = v;
        }
        records(m)
    };
    let clip = filtered(json!({"clips":[{"clip_id":"c","source_layer_id":identity("SLYR",6),"clip_rule":"NONZERO"}],"attachments":[{"layer_id":identity("SLYR",4),"clip_id":"c"}]}));
    let got = prepare_in(&snap, &red(), &clip, &PLAIN);
    assert_eq!(got.clip(&identity("SLYR", 4)).map(|c| c.err()), Some(Some("vector_effect_source")));
    assert_eq!(got.refusal(&identity("SLYR", 6)), None, "the filtered layer itself draws");
    let mask = filtered(json!({"masks":[{"mask_id":"m","source_layer_id":identity("SLYR",6),"mode":"VECTOR"}],"attachments":[{"layer_id":identity("SLYR",4),"mask_id":"m"}]}));
    let got = prepare_in(&snap, &red(), &mask, &PLAIN);
    assert_eq!(got.mask(&identity("SLYR", 4)).map(|m| m.err()), Some(Some("vector_effect_source")));
    let top = filtered(json!({"envelopes":[{"layer_id":identity("SLYR",4),"envelope":{"envelope_kind":"top_object","source_layer_id":identity("SLYR",6)},"envelope_fidelity":50.0}]}));
    assert_eq!(prepare_in(&snap, &red(), &top, &PLAIN).refusal(&identity("SLYR", 4)), Some("vector_effect_source"));
    let boolean = filtered(json!({"boolean_groups":[{"layer_id":identity("SLYR",1),"operation":"UNION","operands":[identity("SLYR",6)]}]}));
    assert_eq!(prepare_in(&snap, &red(), &boolean, &PLAIN).refusal(&identity("SLYR", 1)), Some("vector_effect_source"));
    // Without the filter every reader takes the shape.
    let plain = records(json!({"clips":[{"clip_id":"c","source_layer_id":identity("SLYR",6),"clip_rule":"NONZERO"}],"attachments":[{"layer_id":identity("SLYR",4),"clip_id":"c"}]}));
    assert!(matches!(prepare_in(&snap, &red(), &plain, &PLAIN).clip(&identity("SLYR", 4)), Some(Ok(_))));
}

#[test]
fn a_canceled_token_returns_nothing() {
    let cancel = CancellationToken::default();
    cancel.cancel();
    let r = records(live(4, vec![filter("f1", "twist", &[real("twist_angle", 10.0)])]));
    let got = prepare_vectors(&doc("px", false), &red(), &r, &PLAIN, BlendSpace::Encoded, &VectorResolution::default(), &cancel);
    assert!(matches!(got, Err(CompositeError::Canceled)), "{got:?}");
    let got = expand_vectors(&doc("px", false), &red(), &r, &PLAIN, BlendSpace::Encoded, &VectorResolution::default(), &cancel);
    assert!(matches!(got, Err(CompositeError::Canceled)), "{got:?}");
}

/// A document of `n` vector layers (layer k paints SVPT k with the red style) for the budget tests.
fn many_layers(n: u32) -> (Snapshot, StudioDesignRecords) {
    let id = |prefix: &str, k: u32| format!("{prefix}-019abcde-0000-7000-8000-{k:012x}");
    let layers: Vec<Value> = (1..=n)
        .map(|k| json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",k),"name":format!("l{k}"),"parent":null,"kind":"vector","payload":{"payload_kind":"vector","path_ids":[id("SVPT",k)]},"visible":true}))
        .collect();
    let ops: Vec<Value> = (1..=n)
        .map(|k| json!({"layer_id":id("SLYR",k),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"vector","cardinality":"one","semantic":"source"}]}))
        .collect();
    let outputs: Vec<Value> = (1..=n).map(|k| json!({"layer_id":id("SLYR",k),"operation_key":"source","port_key":"out"})).collect();
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    d["layers"] = Value::Array(layers);
    d["graph"] = json!({"schema_id":"hsk.studio.layer_graph@1","operations":ops,"edges":[],"outputs":outputs});
    let mut budget = hsk_studio_folio::Budget::default();
    budget.nodes = 1 << 20;
    budget.edges = 1 << 20;
    budget.input_bytes = 64 << 20;
    budget.payload_bytes = 64 << 20;
    let snap = match hsk_studio_folio::inspect_bytes(&serde_json::to_vec(&d).unwrap(), budget, &CancellationToken::default(), &Available).unwrap() {
        hsk_studio_folio::Inspection::Editable(s) => s,
        hsk_studio_folio::Inspection::ReadOnly(_) => panic!("read-only"),
    };
    let nodes: Vec<Value> = (1..=n).map(|k| json!({"node":{"node_kind":"layer","node_id":id("SLYR",k)},"style_refs":{"fill_style_id":identity("SSTY",1)}})).collect();
    let mut design_value = serde_json::to_value(red()).unwrap();
    design_value["nodes"] = Value::Array(nodes);
    (snap, serde_json::from_value(design_value).unwrap())
}

/// A polyline of `n` elements (every path of the many layers).
struct Lines(usize);
impl VectorAssets for Lines {
    fn path(&self, _: &str) -> Option<OwnedPath> {
        let points: Vec<(f64, f64)> = (0..self.0).map(|i| ((i % 64) as f64 + 1.0, (i / 64) as f64 * 0.5 + (i % 2) as f64 + 1.0)).collect();
        Some(poly(&points, false))
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

fn twist_stack(layer_k: u32, n: usize) -> hsk_studio_folio::StudioLiveFilterStack {
    use hsk_studio_folio::{StudioLiveFilter, StudioLiveFilterParam, StudioLiveFilterStack};
    StudioLiveFilterStack {
        layer_id: format!("SLYR-019abcde-0000-7000-8000-{layer_k:012x}"),
        filters: (0..n)
            .map(|i| StudioLiveFilter {
                instance_id: format!("f{i}"),
                filter_kind: "hsk.studio.vector_effect.twist".into(),
                version: 1,
                enabled: true,
                params: vec![StudioLiveFilterParam::Real { key: "twist_angle".into(), value: 1.0 }],
            })
            .collect(),
    }
}

#[test]
fn one_aggregate_budget_bounds_every_evaluation_of_a_call() {
    use hsk_studio_composite::vector_live_filters::{MAX_LIVE_FILTER_ELEMENTS, REFUSE_EFFECT_WORK};
    use hsk_studio_nib::shape_ops::MAX_OP_ELEMENTS;
    bounded_case("live_filter_aggregate_budget", || {
        // A chain of step repeats (zero copies) over a path just under nib's per-evaluation limit:
        // each evaluation alone is admitted; the chain is refused once the budget of the call is spent.
        let (snap, design) = many_layers(1);
        let per = MAX_OP_ELEMENTS - 8;
        let fits = usize::try_from(MAX_LIVE_FILTER_ELEMENTS / per as u64).unwrap();
        let chain = |n: usize| {
            let mut r = StudioVectorRecords::new(identity("SDOC", 1));
            let mut s = twist_stack(1, n);
            for f in &mut s.filters {
                f.filter_kind = "hsk.studio.vector_effect.step_repeat".into();
                f.params = vec![hsk_studio_folio::StudioLiveFilterParam::Int { key: "copies".into(), value: 0 }];
            }
            r.live_filters.push(s);
            r
        };
        let run = |r: &StudioVectorRecords| expand_vectors(&snap, &design, r, &Lines(per), BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
        let (ok, refused) = run(&chain(fits));
        assert!(refused.is_empty() && ok.len() == 1, "{refused:?}");
        let (_, refused) = run(&chain(fits + 1));
        assert_eq!(refused, vec![(format!("SLYR-019abcde-0000-7000-8000-{:012x}", 1), REFUSE_EFFECT_WORK)]);
    });
}

#[test]
fn many_layers_each_with_a_full_stack_share_the_budget() {
    use hsk_studio_composite::vector_live_filters::{MAX_LIVE_FILTER_ELEMENTS, REFUSE_EFFECT_WORK};
    use hsk_studio_folio::LIVE_FILTERS_MAX_PER_STACK;
    // 4096 layers, each with a full stack of twists over a 20-element path: one evaluation budget
    // would admit every layer; the aggregate of the call refuses the layers past it, typed.
    let (layers, elements) = (4096_u32, 20_usize);
    let (snap, design) = many_layers(layers);
    let mut r = StudioVectorRecords::new(identity("SDOC", 1));
    r.live_filters = (1..=layers).map(|k| twist_stack(k, LIVE_FILTERS_MAX_PER_STACK)).collect();
    let started = std::time::Instant::now();
    let (ok, refused) = expand_vectors(&snap, &design, &r, &Lines(elements), BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
    let took = started.elapsed();
    let per_layer = (LIVE_FILTERS_MAX_PER_STACK * elements) as u64;
    let fit = usize::try_from(MAX_LIVE_FILTER_ELEMENTS / per_layer).unwrap();
    assert_eq!(ok.len(), fit);
    assert_eq!(refused.len(), layers as usize - fit);
    assert!(refused.iter().all(|(_, code)| *code == REFUSE_EFFECT_WORK));
    assert_eq!(refused[0].0, format!("SLYR-019abcde-0000-7000-8000-{:012x}", fit + 1), "the layers past the budget, in document order");
    // Timing record for the lane notes (debug and release differ; no bound asserted here).
    println!("live_filter_budget_4096_layers: {fit} layers x {LIVE_FILTERS_MAX_PER_STACK} filters x {elements} elements in {took:?} (profile {})", if cfg!(debug_assertions) { "debug" } else { "release" });
}

#[test]
fn a_token_canceled_during_a_long_stack_returns_promptly_with_nothing() {
    use hsk_studio_folio::LIVE_FILTERS_MAX_PER_STACK;
    use hsk_studio_nib::shape_ops::MAX_OP_ELEMENTS;
    bounded_case("live_filter_cancel", || {
        let (snap, design) = many_layers(1);
        let mut r = StudioVectorRecords::new(identity("SDOC", 1));
        r.live_filters.push(twist_stack(1, LIVE_FILTERS_MAX_PER_STACK));
        let cancel = CancellationToken::default();
        let remote = cancel.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1));
            remote.cancel();
        });
        let started = std::time::Instant::now();
        let got = expand_vectors(&snap, &design, &r, &Lines(MAX_OP_ELEMENTS - 8), BlendSpace::Encoded, &VectorResolution::default(), &cancel);
        let took = started.elapsed();
        stopper.join().unwrap();
        assert!(matches!(got, Err(CompositeError::Canceled)), "{:?}", got.map(|(l, r)| (l.len(), r)));
        println!("live_filter_cancel: returned after {took:?}");
    });
}
