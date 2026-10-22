//! Lane SMB-PS-FILL-LAYERS: composite's lowering of Folio fill layers (STU-RAS-007 kind `fill`).
//! A solid fill is a `Solid` node over its rectangle with the layer's own mode, opacity and fill
//! opacity. A gradient or pattern fill is an opaque `Solid` shape whose content the plan layer hides
//! (normal mode, fill opacity 0) while one effect carries the fill with the layer's mode and fill
//! opacity. The lowered plan is rendered here with pigment's single-owner reference
//! (`render_layer_reference_with`) and compared with values derived by hand from the gradient
//! definition (linear angle 0: `t = (x + 0.5) / width`, colour and stop opacity interpolated).
//! Typed refusals are checked for every case the engine cannot honour. Photoshop pixel parity is
//! UNVERIFIED (no D5 case covers fill layers).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, CompositeError, ExtendedLowering, Limits, TileInfo, TileInfoSource, evaluate_extended, lower,
    lower_to_render_plan_extended,
};
use hsk_studio_folio::{Resolution, Resolver, StudioRasterRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::{EffectOp, EffectPlane, LayerParams, NoPatterns, render_layer_reference_with};
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat};
use serde_json::{Value, json};

mod common;
use common::*;

const W: u32 = 16;
const H: u32 = 8;

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "fill" && ["hsk.studio.swatch@1", "hsk.studio.gradient@1", "hsk.studio.pattern@1"].contains(&schema_id)
    }
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: [tile.object_key.len() as u8; 32],
            alpha_sha256: [9; 32],
        })
    }
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: W, height: H },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r":r,"g":g,"b":b,"colour_profile_id":null})
}
fn bounds() -> Value {
    json!({"x":0,"y":0,"width":W,"height":H})
}
fn solid(c: Value) -> Value {
    json!({"bounds":bounds(),"fill":{"fill_kind":"solid","colour":c}})
}
fn gradient(style: &str, angle: f64, stops: Value) -> Value {
    json!({"bounds":bounds(),"fill":{"fill_kind":"gradient","style":style,"stops":stops,"angle":angle,
        "scale":100.0,"reverse":false,"dither":false,"align_with_layer":true,"offset_x":0.0,"offset_y":0.0}})
}
fn black_white(white_opacity: f64) -> Value {
    json!([{"position":0.0,"midpoint":50.0,"opacity":100.0,"colour":colour(0.0,0.0,0.0)},
           {"position":100.0,"midpoint":null,"opacity":white_opacity,"colour":colour(1.0,1.0,1.0)}])
}
fn appearance(opacity: f64, fill: f64, mode: u16) -> Value {
    json!({"opacity":opacity,"fill_opacity":fill,"blend_mode":mode,"clipped":false,"isolation":"isolated"})
}
const SWATCH: &str = "hsk.studio.swatch@1";
const GRADIENT: &str = "hsk.studio.gradient@1";
const PATTERN: &str = "hsk.studio.pattern@1";
/// Group 1 over the fill layers `(layer number, payload schema, appearance)`, bottom first.
fn doc(fills: &[(u8, &str, Value)]) -> Value {
    let mut layers = vec![layer(1, "group", json!({"payload_kind":"group"}))];
    let mut operations = vec![composite(1)];
    let mut edges = Vec::new();
    for (ordinal, (n, schema, look)) in fills.iter().enumerate() {
        let mut l = layer(
            *n,
            "fill",
            json!({"payload_kind":"primitive_reference","schema_id":schema,"object_id":identity("SLYR",*n)}),
        );
        l["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",1)});
        l["appearance"] = look.clone();
        layers.push(l);
        operations.push(source(*n, "image"));
        edges.push(edge(address(*n, "source", "out"), address(1, "mix", "inputs"), ordinal as u64));
    }
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px","colour_mode":3,
        "bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":operations,"edges":edges,
            "outputs":[address(1,"mix","out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn records(entries: Vec<(u8, Value)>) -> StudioRasterRecords {
    let layers: Vec<Value> =
        entries.into_iter().map(|(n, fill)| json!({"layer_id":identity("SLYR",n),"fill_layer":fill})).collect();
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":layers}))
    .expect("records shape")
}
fn lowered(v: &Value, recs: &StudioRasterRecords) -> Result<ExtendedLowering, CompositeError> {
    let snap = open_bytes(&serde_json::to_vec(v).unwrap(), &Host);
    let token = CancellationToken::default();
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let report = evaluate_extended(&plan, &snap, recs, &token)?;
    lower_to_render_plan_extended(&plan, &report, &snap, recs, &addr(1, "mix", "out"), target(), &Info)
}
fn refusal(r: Result<ExtendedLowering, CompositeError>) -> String {
    match r {
        Err(CompositeError::Unavailable(s)) => s,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// Renders layer `index` of the root group over `backdrop` with pigment's reference, from the
/// lowered plan alone (the Solid content, the layer's mode, opacities and the extension's effects).
fn render_layer(low: &ExtendedLowering, index: usize, backdrop: &EffectPlane) -> EffectPlane {
    let PlanNode::Group(group) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
    let layer = group.layers[index];
    let PlanNode::Solid { rect, rgba } = low.plan.nodes[layer.input.0 as usize] else { panic!("solid content") };
    assert_eq!((rect.width, rect.height), (W, H));
    let n = (W * H) as usize;
    let content = EffectPlane::new(W, H, vec![[rgba[0], rgba[1], rgba[2]]; n], vec![rgba[3]; n]).unwrap();
    let ops: Vec<EffectOp> = low
        .extensions
        .extension(low.plan.root, u32::try_from(index).unwrap())
        .map(|e| e.effects.clone())
        .unwrap_or_default();
    render_layer_reference_with(
        backdrop,
        &content,
        LayerParams { mode: layer.mode, opacity: layer.opacity, fill_opacity: layer.fill_opacity },
        &ops,
        &NoPatterns,
    )
    .unwrap()
}
fn transparent() -> EffectPlane {
    EffectPlane::transparent(W, H).unwrap()
}
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 2e-6
}

#[test]
fn solid_fill_is_a_solid_node_with_the_layers_own_appearance() {
    bounded_case("solid_fill_is_a_solid_node_with_the_layers_own_appearance", || {
        // Multiply at opacity 50, fill 100 over nothing: a pixel layer of the same colour gives the
        // colour at alpha 0.5.
        let v = doc(&[(2, SWATCH, appearance(50.0, 100.0, 5))]);
        let low = lowered(&v, &records(vec![(2, solid(colour(0.2, 0.4, 0.6)))])).expect("lowers");
        let PlanNode::Group(group) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
        let layer = group.layers[0];
        assert_eq!((layer.opacity, layer.fill_opacity), (0.5, 1.0));
        assert!(low.extensions.layers.is_empty(), "a solid fill needs no envelope entry");
        let PlanNode::Solid { rect, rgba } = low.plan.nodes[layer.input.0 as usize] else { panic!("solid") };
        assert_eq!(rect, Rect { x: 0, y: 0, width: W, height: H });
        assert_eq!(rgba, [0.2f64 as f32, 0.4f64 as f32, 0.6f64 as f32, 1.0]);
        let out = render_layer(&low, 0, &transparent());
        let p = out.pixel(5, 3).unwrap();
        assert!(close(p.rgb[0], 0.2) && close(p.rgb[1], 0.4) && close(p.rgb[2], 0.6) && close(p.a, 0.5), "{p:?}");
    });
}

#[test]
fn gradient_fill_renders_the_hand_derived_gradient_with_opacity_and_fill() {
    bounded_case("gradient_fill_renders_the_hand_derived_gradient_with_opacity_and_fill", || {
        // Linear, angle 0 (left to right), black to white, white stop at 50 percent opacity; layer
        // opacity 80, fill 60, Normal: over nothing, alpha = 0.8 * 0.6 * stop opacity and colour = t.
        let v = doc(&[(2, GRADIENT, appearance(80.0, 60.0, 2))]);
        let low = lowered(&v, &records(vec![(2, gradient("linear", 0.0, black_white(50.0)))])).expect("lowers");
        let PlanNode::Group(group) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
        let layer = group.layers[0];
        // The plan layer hides its own content; the effect carries the fill.
        assert_eq!((layer.opacity, layer.fill_opacity), (0.8, 0.0));
        let ext = low.extensions.extension(low.plan.root, 0).expect("envelope entry");
        let [EffectOp::GradientOverlay(g)] = ext.effects.as_slice() else { panic!("{:?}", ext.effects) };
        assert_eq!((g.opacity, g.scale, g.reverse, g.offset, g.stop_count), (0.6, 1.0, false, [0.0, 0.0], 2));
        let out = render_layer(&low, 0, &transparent());
        for x in [0u32, 3, 7, 12, 15] {
            let t = (x as f32 + 0.5) / W as f32;
            let want_alpha = 0.8 * 0.6 * (1.0 - 0.5 * t);
            let p = out.pixel(x, 4).unwrap();
            assert!(close(p.rgb[0], t) && close(p.rgb[1], t) && close(p.rgb[2], t), "x {x}: {p:?} want {t}");
            assert!(close(p.a, want_alpha), "x {x}: alpha {} want {want_alpha}", p.a);
        }
        // Reverse flips the ramp; angle 90 runs bottom to top (image y grows downwards).
        let mut reversed = gradient("linear", 0.0, black_white(100.0));
        reversed["fill"]["reverse"] = json!(true);
        let low = lowered(&doc(&[(2, GRADIENT, appearance(100.0, 100.0, 2))]), &records(vec![(2, reversed)])).unwrap();
        let p = render_layer(&low, 0, &transparent()).pixel(3, 0).unwrap();
        let t = 1.0 - 3.5 / W as f32;
        assert!(close(p.rgb[0], t) && close(p.a, 1.0), "{p:?}");
        let up = gradient("linear", 90.0, black_white(100.0));
        let low = lowered(&doc(&[(2, GRADIENT, appearance(100.0, 100.0, 2))]), &records(vec![(2, up)])).unwrap();
        let out = render_layer(&low, 0, &transparent());
        let (top, bottom) = (out.pixel(4, 0).unwrap(), out.pixel(4, H - 1).unwrap());
        assert!(top.rgb[0] > bottom.rgb[0], "angle 90 runs upwards: top {top:?} bottom {bottom:?}");
        let t = 1.0 - (H as f32 - 1.0 + 0.5) / H as f32;
        assert!(close(bottom.rgb[0], t), "bottom {bottom:?} want {t}");
    });
}

#[test]
fn gradient_fill_blends_with_the_layers_mode_over_the_backdrop() {
    bounded_case("gradient_fill_blends_with_the_layers_mode_over_the_backdrop", || {
        // Layer 2 solid (0.5, 0.5, 0.5) Normal, layer 3 gradient black to white Multiply at opacity
        // 100 and fill 100: multiply of a grey backdrop with the ramp = 0.5 * t.
        let v = doc(&[(2, SWATCH, appearance(100.0, 100.0, 2)), (3, GRADIENT, appearance(100.0, 100.0, 5))]);
        let low = lowered(
            &v,
            &records(vec![(2, solid(colour(0.5, 0.5, 0.5))), (3, gradient("linear", 0.0, black_white(100.0)))]),
        )
        .expect("lowers");
        let below = render_layer(&low, 0, &transparent());
        let out = render_layer(&low, 1, &below);
        for x in [0u32, 8, 15] {
            let t = (x as f32 + 0.5) / W as f32;
            let p = out.pixel(x, 2).unwrap();
            assert!(close(p.rgb[0], 0.5 * t) && close(p.a, 1.0), "x {x}: {p:?} want {}", 0.5 * t);
        }
    });
}

#[test]
fn unsupported_fill_content_is_a_typed_refusal_never_a_default() {
    bounded_case("unsupported_fill_content_is_a_typed_refusal_never_a_default", || {
        let one = |schema: &str, fill: Value| {
            lowered(&doc(&[(2, schema, appearance(100.0, 100.0, 2))]), &records(vec![(2, fill)]))
        };
        let mut dither = gradient("linear", 0.0, black_white(100.0));
        dither["fill"]["dither"] = json!(true);
        assert!(refusal(one(GRADIENT, dither)).ends_with("effect_gradient_dither"));
        let mut unaligned = gradient("linear", 0.0, black_white(100.0));
        unaligned["fill"]["align_with_layer"] = json!(false);
        assert!(refusal(one(GRADIENT, unaligned)).ends_with("effect_gradient_not_aligned_with_layer"));
        let mut skew = gradient("linear", 0.0, black_white(100.0));
        skew["fill"]["stops"][0]["midpoint"] = json!(30.0);
        assert!(refusal(one(GRADIENT, skew)).ends_with("effect_gradient_midpoint"));
        let mut huge = gradient("linear", 0.0, black_white(100.0));
        huge["fill"]["scale"] = json!(500.0);
        assert!(refusal(one(GRADIENT, huge)).ends_with("effect_parameter"));
        // A pattern without its resolved tile.
        let unresolved = json!({"bounds":bounds(),"fill":{"fill_kind":"pattern",
            "pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"PAT-1"},"scale":100.0,"phase_x":0.0,
            "phase_y":0.0,"link_with_layer":true}});
        assert!(refusal(one(PATTERN, unresolved)).ends_with("pattern_overlay_unresolved"));
        // A mid-tone solid colour in a profile other than the blending profile.
        let mut other = solid(colour(0.5, 0.5, 0.5));
        other["fill"]["colour"]["colour_profile_id"] = json!(identity("SCPF", 2));
        assert!(refusal(one(SWATCH, other)).ends_with("effect_colour_profile"));
        // A fill layer without its record.
        let none = lowered(&doc(&[(2, SWATCH, appearance(100.0, 100.0, 2))]), &records(vec![]));
        assert!(matches!(none, Err(CompositeError::Unavailable(ref s)) if s.ends_with("fill_record_missing")), "{none:?}");
        // A gradient or pattern fill cannot be the render root (a solid one can).
        let mut root = doc(&[(2, GRADIENT, appearance(100.0, 100.0, 2))]);
        root["graph"]["outputs"] = json!([address(2, "source", "out")]);
        let snap = open_bytes(&serde_json::to_vec(&root).unwrap(), &Host);
        let token = CancellationToken::default();
        let recs = records(vec![(2, gradient("linear", 0.0, black_white(100.0)))]);
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let report = evaluate_extended(&plan, &snap, &recs, &token).unwrap();
        let as_root =
            lower_to_render_plan_extended(&plan, &report, &snap, &recs, &addr(2, "source", "out"), target(), &Info);
        assert!(refusal(as_root).ends_with("fill_layer_as_root"));
    });
}

#[test]
fn pattern_fill_registers_its_tile_and_lowers_to_a_pattern_overlay() {
    bounded_case("pattern_fill_registers_its_tile_and_lowers_to_a_pattern_overlay", || {
        let mut tile = tile(2, "pt", 0, 0, 4, 3);
        tile["format"] = json!("rgb_f32le");
        let fill = json!({"bounds":bounds(),"fill":{"fill_kind":"pattern",
            "pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"PAT-1"},"pattern_tile":tile,
            "scale":150.0,"phase_x":2.0,"phase_y":-1.0,"link_with_layer":false}});
        let low =
            lowered(&doc(&[(2, PATTERN, appearance(100.0, 50.0, 2))]), &records(vec![(2, fill)])).expect("lowers");
        let ext = low.extensions.extension(low.plan.root, 0).expect("envelope entry");
        let [EffectOp::PatternOverlay(o)] = ext.effects.as_slice() else { panic!("{:?}", ext.effects) };
        assert_eq!((o.pattern.width, o.pattern.height), (4, 3));
        assert_eq!((o.scale, o.phase, o.link_with_layer, o.opacity), (1.5, [2.0, -1.0], false, 0.5));
        assert!(low.sources.iter().any(|(k, t)| *k == o.pattern.key && t.object_key == "pt"));
        let PlanNode::Group(group) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
        assert_eq!(group.layers[0].fill_opacity, 0.0);
    });
}
