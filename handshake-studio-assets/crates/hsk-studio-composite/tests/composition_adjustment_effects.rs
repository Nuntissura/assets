//! Lane SMB-MOTION-COMMANDS-2 ([STU-MOT-005] `solid` and `adjustment`, [STU-CMP-015]).
//!
//! Solid: a Folio `solid_layers` record lowers through the shape path as one rectangle with one
//! fill, byte for byte the equivalent shape layer, and carries an effect stack (motion-comp step
//! 17, Gradient Ramp on a solid). Adjustment: a composition layer whose document layer is kind
//! `adjustment` with a `group` payload; its effect stack runs over the composite of everything
//! beneath it, B, and the frame holds `L = B + k (E - B)` (premultiplied) with `k` its opacity times
//! its mask coverage, inside its in/out window; adjustment layers nest. Records are written as JSON;
//! frames render through `render_scene` with a reference executor (as `tests/composition_effects.rs`).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::{SURFACE_BYTES_PER_PIXEL, SceneEffect, SceneEffectEntry, apply_effects};
use hsk_studio_composite::temporal::composition::{COMPOSITION_EFFECT_CONTENT, CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::temporal::{TemporalError, TemporalSource};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, StudioMotionRecords, StudioRasterRecords, TileRef};
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, Exactness, ExecuteRequest, NoVector, Operator, PlanExtensions, PlanNode,
    PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat, TilePlanes, TileSink,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod common;
use common::{bounded_case, layer, open_bytes, tile};
#[path = "support/effect_scenes.rs"]
mod scenes;
use scenes::{H, Pixels, W, identity, source_texel};

/// Reference executor (as `tests/composition_effects.rs`): groups composite bottom-up over
/// transparent with pigment's `composite_over`.
struct Ref;
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn coverage(m: &Mask<'_>, x: i64, y: i64) -> f32 {
    let (cx, cy) = (x - m.bounds.x, y - m.bounds.y);
    if cx < 0 || cy < 0 || cx >= i64::from(m.bounds.width) || cy >= i64::from(m.bounds.height) {
        return 0.0;
    }
    let i = cy as usize * m.stride_bytes as usize + cx as usize * 2;
    (f64::from(u16::from_le_bytes([m.bytes[i], m.bytes[i + 1]])) / 65535.0) as f32
}
fn eval(plan: &RenderPlan, id: usize, src: &dyn PlanSources, x: i64, y: i64) -> Rgba {
    match &plan.nodes[id] {
        PlanNode::Tile(t) => {
            let (cx, cy) = (x - t.placement.x, y - t.placement.y);
            if cx < 0 || cy < 0 || cx >= i64::from(t.placement.width) || cy >= i64::from(t.placement.height) {
                return Rgba::TRANSPARENT;
            }
            let p: TilePlanes<'_> = src.tile(t.key).expect("tile served");
            let c = cy as usize * p.colour_stride_bytes as usize + cx as usize * 12;
            let a = cy as usize * p.alpha_stride_bytes as usize + cx as usize * 4;
            Rgba::new([f32_at(p.colour, c), f32_at(p.colour, c + 4), f32_at(p.colour, c + 8)], f32_at(p.alpha, a))
        }
        PlanNode::Solid { rect, rgba } => {
            let inside = x >= rect.x && y >= rect.y && x < rect.x + i64::from(rect.width) && y < rect.y + i64::from(rect.height);
            if inside { Rgba::new([rgba[0], rgba[1], rgba[2]], rgba[3]) } else { Rgba::TRANSPARENT }
        }
        PlanNode::Group(g) => {
            let mut acc = Rgba::TRANSPARENT;
            for l in &g.layers {
                let s = eval(plan, l.input.0 as usize, src, x, y);
                let mut k = l.opacity * l.fill_opacity;
                if let Some(m) = l.mask {
                    let c = coverage(&src.mask(m.key).expect("mask served"), x, y);
                    k *= if m.inverted { 1.0 - c } else { c };
                }
                acc = composite_over(l.mode, acc, s, k, PixelSite { x, y, seed: 0 }).unwrap();
            }
            acc
        }
        PlanNode::Vector(v) => match *v {},
    }
}
impl RenderBackend for Ref {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "ref", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute_extended(&self, plan: &RenderPlan<NoVector>, ext: &PlanExtensions, r: &ExecuteRequest, s: &dyn PlanSources, c: &CancellationToken, sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        assert!(ext.is_empty(), "the reference executor takes plain plans only");
        self.execute(plan, r, s, c, sink)
    }
    fn execute(&self, plan: &RenderPlan<NoVector>, _: &ExecuteRequest, src: &dyn PlanSources, _: &CancellationToken, sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        let e = plan.target.extent;
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in e.y..e.y + i64::from(e.height) {
            for x in e.x..e.x + i64::from(e.width) {
                let p = eval(plan, plan.root.0 as usize, src, x, y);
                for v in p.rgb {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p.a.to_le_bytes());
            }
        }
        sink.accept(e, &colour, &alpha).map_err(BackendError::Sink)?;
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: plan.digest().unwrap(),
            backend: self.identity(),
            region: e,
            target: plan.target,
            exactness: Default::default(),
            chunks: 1,
            nodes_executed: plan.nodes.len() as u64,
            peak_working_bytes: 0,
        })
    }
}
fn context(pixels: &Pixels, limits: SceneLimits) -> SceneContext<'_> {
    SceneContext {
        backend: &Ref,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits,
    }
}
/// Binds the pigment adjustment primitive (layer 7), like the harness resolvers.
struct Bound;
impl hsk_studio_folio::Resolver for Bound {
    fn primitive(&self, _: &str, _: &str) -> hsk_studio_folio::Resolution {
        hsk_studio_folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> hsk_studio_folio::Resolution {
        hsk_studio_folio::Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> hsk_studio_folio::Resolution {
        hsk_studio_folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == hsk_studio_folio::ADJUSTMENT_SCHEMA
    }
}
fn open(v: &Value) -> hsk_studio_folio::Snapshot {
    open_bytes(&serde_json::to_vec(v).unwrap(), &Bound)
}
struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        (format!("{}/{}", t.layer_id, t.object_key) == scenes::tile_id()).then_some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.0.colour_sha256,
            alpha_sha256: self.0.alpha_sha256,
        })
    }
}

/// 24 fps.
const TPF: u64 = 10_584_000_000;
fn f(frame: u64) -> u64 {
    frame * TPF
}
fn adjustment(n: u8) -> Value {
    layer(n, "adjustment", json!({"payload_kind":"group"}))
}
/// Raster 2 (the 16x12 source tile), composition adjustment layers 3 and 4, empty rasters 5 and 6
/// (procedural hosts), a pigment adjustment layer 7, a group 8 (precomposition host).
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2,"src",0,0,W,H)]})), adjustment(3), adjustment(4),
            layer(5, "raster", json!({"payload_kind":"raster","tiles":[]})), layer(6, "raster", json!({"payload_kind":"raster","tiles":[]})),
            layer(7, "adjustment", json!({"payload_kind":"primitive_reference","schema_id":"hsk.studio.adjustment@1","object_id":identity("SLYR",7)})),
            layer(8, "group", json!({"payload_kind":"group"}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    common::assign_all_profiles(&mut v);
    v
}
/// Composition layer `n` over frames `0..4` with `extra` merged in.
fn comp_layer(n: u8, extra: Value) -> Value {
    let mut l = json!({"layer_id":identity("SLYR",n),
        "timing":{"in_point":0,"out_point":f(4),"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        l[k] = v;
    }
    l
}
/// One 16x12 composition `SCOM 1` holding `layers` (top first), plus an optional nested one.
fn records(layers: Vec<Value>, nested: Option<Vec<Value>>) -> Value {
    let comp = |n: u8, layers: Vec<Value>| json!({"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",n),"name":format!("c{n}"),
        "width":W,"height":H,"pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(4),"layers":layers});
    let mut comps = vec![comp(1, layers)];
    if let Some(n) = nested {
        comps.push(comp(2, n));
    }
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":comps})
}
fn motion(v: Value) -> StudioMotionRecords {
    let mut m = json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1)});
    for (k, x) in v.as_object().cloned().unwrap_or_default() {
        m[k] = x;
    }
    serde_json::from_value(m).expect("motion records shape")
}
fn source_with(rec: &Value, pixels: &Pixels, raster: Option<&StudioRasterRecords>) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(&document());
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &identity("SCOM", 1),
        budget: Budget::default(),
        resolver: &Bound,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
        space: scenes::space(),
        inputs: RenderInputs { records: raster, ..RenderInputs::default() },
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}
fn source(rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    source_with(rec, pixels, None)
}
fn pixels_of(r: &RenderedScene) -> Vec<[f32; 4]> {
    (0..(r.width * r.height) as usize).map(|i| [f32_at(&r.colour, i * 12), f32_at(&r.colour, i * 12 + 4), f32_at(&r.colour, i * 12 + 8), f32_at(&r.alpha, i * 4)]).collect()
}
fn frame(rec: &Value, pixels: &Pixels, tick: u64) -> Vec<[f32; 4]> {
    let cancel = CancellationToken::default();
    let scene = source(rec, pixels).expect("source").scene_at(tick as i64, &cancel).expect("scene");
    pixels_of(&render_scene(&scene, &context(pixels, SceneLimits::default()), &cancel).expect("frame renders"))
}
fn refusal(r: Result<impl std::fmt::Debug, TemporalError>) -> (String, &'static str) {
    match r {
        Err(TemporalError::Unsupported { layer_id, feature }) => (layer_id, feature),
        other => panic!("not a typed refusal: {other:?}"),
    }
}
fn constant(v: Value) -> Value {
    json!({ "static_value": v })
}
fn invert() -> Value {
    json!({"effect_id":"inv","filter_kind":"invert"})
}
/// The source as straight RGBA (colour 0 where transparent, as the scene path places it).
fn beneath() -> Vec<[f32; 4]> {
    (0..W * H).map(|i| source_texel(i % W, i / W)).map(|p| if p[3] > 0.0 { p } else { [0.0; 4] }).collect()
}
/// Hand values: Invert (`1 - c` on the straight colour, alpha kept) mixed by `k` per pixel.
fn inverted(b: &[[f32; 4]], k: impl Fn(u32, u32) -> f32) -> Vec<[f32; 4]> {
    b.iter()
        .enumerate()
        .map(|(i, p)| {
            if p[3] <= 0.0 {
                return [0.0; 4];
            }
            let k = k(i as u32 % W, i as u32 / W);
            [p[0] + k * (1.0 - 2.0 * p[0]), p[1] + k * (1.0 - 2.0 * p[1]), p[2] + k * (1.0 - 2.0 * p[2]), p[3]]
        })
        .collect()
}
fn max_delta(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).flat_map(|(p, q)| p.iter().zip(q).map(|(x, y)| (x - y).abs())).fold(0.0, f32::max)
}

#[test]
fn a_solid_is_the_equivalent_rectangle_shape_byte_for_byte_and_carries_effects() {
    let pixels = Pixels::fixture();
    let cancel = CancellationToken::default();
    let rec = records(vec![comp_layer(5, json!({}))], None);
    let colour = json!([0.25, 0.5, 0.75, 1.0]);
    let solid = motion(json!({"solid_layers":[{"layer_id":identity("SLYR",5),"width":W,"height":H,"colour":colour}]}));
    let shape = motion(json!({"shape_layers":[{"layer_id":identity("SLYR",5),"root":{"items":[
        {"kind":"rect","position":[f64::from(W) / 2.0, f64::from(H) / 2.0],"size":[W,H]},
        {"kind":"fill","rgba":colour,"opacity":100.0,"rule":"non_zero"}]}}]}));
    let at = |m: &StudioMotionRecords| {
        let s = source(&rec, &pixels).unwrap().with_motion_records(m, &BTreeMap::new(), scenes::space(), 7, None).expect("motion records attach");
        s.scene_at(0, &cancel).expect("scene").layers[0].content.clone()
    };
    let (a, b) = (at(&solid), at(&shape));
    assert!(matches!(a, SceneContent::Static(_)), "{a:?}");
    assert_eq!(a, b, "a solid draws through the shape path, not a second rasteriser");
    // A solid of another size: the rectangle follows it.
    let small = motion(json!({"solid_layers":[{"layer_id":identity("SLYR",5),"width":4,"height":2,"colour":colour}]}));
    let SceneContent::Static(c) = at(&small) else { panic!() };
    assert_eq!((c.rect.width, c.rect.height), (4, 2));
    // A shape record and a solid record on one layer are refused (one procedural record per layer).
    let twice = motion(json!({"solid_layers":[{"layer_id":identity("SLYR",5),"width":W,"height":H,"colour":colour}],
        "shape_layers":[{"layer_id":identity("SLYR",5),"root":{"items":[]}}]}));
    assert_eq!(
        source(&rec, &pixels).unwrap().with_motion_records(&twice, &BTreeMap::new(), scenes::space(), 7, None).map(|_| ()).unwrap_err(),
        TemporalError::Source { layer_id: identity("SLYR", 5), code: "procedural_layer_twice".into() }
    );
    // Step 17: Gradient Ramp on the solid (a stack on a procedural solid has the solid's rect).
    let ramp = json!({"effect_id":"r","filter_kind":"gradient_ramp","values":{"start_of_ramp":constant(json!([0.0,0.0])),"end_of_ramp":constant(json!([1.0,0.0]))},
        "colour_profiles":{"start_color":{"colour_profile_id":identity("SCPF",1)},"end_color":{"colour_profile_id":identity("SCPF",1)}}});
    let rec_fx = records(vec![comp_layer(5, json!({"effects":[ramp]}))], None);
    let s = source(&rec_fx, &pixels).expect("a stack on an empty raster host waits for its procedural record");
    let s = s.with_motion_records(&solid, &BTreeMap::new(), scenes::space(), 7, None).expect("solid attaches");
    let scene = s.scene_at(0, &cancel).expect("scene");
    let SceneContent::Effected(e) = &scene.layers[0].content else { panic!("{:?}", scene.layers[0].content) };
    assert_eq!((e.rect.width, e.rect.height), (W, H));
    assert!(matches!(e.effects[0].effect, SceneEffect::GradientRamp { start: [0.0, 0.0], end: [16.0, 0.0], .. }), "{:?}", e.effects[0].effect);
    // The same stack on an empty raster layer that no procedural record makes content is refused.
    let none = source(&rec_fx, &pixels).unwrap().with_motion_records(&motion(json!({})), &BTreeMap::new(), scenes::space(), 7, None);
    assert_eq!(refusal(none.map(|_| ())), (identity("SLYR", 5), COMPOSITION_EFFECT_CONTENT));
    assert_eq!(refusal(source(&rec_fx, &pixels).unwrap().scene_at(0, &cancel)), (identity("SLYR", 5), COMPOSITION_EFFECT_CONTENT));
}

#[test]
fn an_adjustment_stack_applies_to_the_composite_beneath_mixed_by_opacity() {
    bounded_case("an_adjustment_stack_applies_to_the_composite_beneath_mixed_by_opacity", || {
        let pixels = Pixels::fixture();
        let b = beneath();
        // Without effects an adjustment layer has no content of its own: the frame is the source.
        let bare = records(vec![comp_layer(3, json!({})), comp_layer(2, json!({}))], None);
        assert!(max_delta(&frame(&bare, &pixels, 0), &b) <= 1e-6);
        // k = 0, 0.5, 1 by the opacity stream over a semi-transparent B (alpha 1, 0.75, 0.5, 0).
        for (opacity, k) in [(0.0, 0.0f32), (50.0, 0.5), (100.0, 1.0)] {
            let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"streams":[{"property":"opacity","static_value":[opacity]}]})), comp_layer(2, json!({}))], None);
            let got = frame(&rec, &pixels, 0);
            let want = inverted(&b, |_, _| k);
            assert!(max_delta(&got, &want) <= 1e-6, "k {k}: {}", max_delta(&got, &want));
        }
        // The hand value agrees with the stack's own kernel on B (E), mixed premultiplied.
        let mut e: Vec<[f32; 4]> = b.iter().map(|p| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]).collect();
        let entry = SceneEffectEntry { effect_id: "inv".into(), effect: SceneEffect::Colour(Box::new(hsk_studio_composite::composition::ColourEffect {
            ops: vec![hsk_studio_composite::composition::ChannelAdjustment { op: hsk_studio_pigment::adjust_math::AdjustmentOp::Invert, channels: [true; 3] }], blend: 0.0 })), mix: 1.0 };
        apply_effects(W, H, &mut e, &[entry], &CancellationToken::default()).unwrap();
        let e: Vec<[f32; 4]> = e.iter().map(|p| if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]] } else { [0.0; 4] }).collect();
        assert!(max_delta(&e, &inverted(&b, |_, _| 1.0)) <= 1e-6);
        // Masks scope WHERE (layer space = composition space): the left half only.
        let mask = json!([{"mask_id":"m","mode":"add","closed":true,"static_value":[
            0.0,0.0,0.0,0.0,0.0,0.0, 8.0,0.0,0.0,0.0,0.0,0.0, 8.0,12.0,0.0,0.0,0.0,0.0, 0.0,12.0,0.0,0.0,0.0,0.0]}]);
        let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"masks":mask})), comp_layer(2, json!({}))], None);
        assert!(max_delta(&frame(&rec, &pixels, 0), &inverted(&b, |x, _| if x < 8 { 1.0 } else { 0.0 })) <= 1e-6);
        // In/out scope WHEN: out of its window at frame 0, in it at frame 1.
        let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"timing":{"in_point":f(1),"out_point":f(4),"start_time":0,"stretch":{"numerator":1,"denominator":1}}})), comp_layer(2, json!({}))], None);
        assert!(max_delta(&frame(&rec, &pixels, f(0)), &b) <= 1e-6);
        assert!(max_delta(&frame(&rec, &pixels, f(1)), &inverted(&b, |_, _| 1.0)) <= 1e-6);
        // Layers above an adjustment layer are not adjusted: an adjustment layer at the bottom of
        // the stack has nothing beneath it, and the source over it stays the source.
        let rec = records(vec![comp_layer(2, json!({})), comp_layer(3, json!({"effects":[invert()]}))], None);
        assert!(max_delta(&frame(&rec, &pixels, 0), &b) <= 1e-6);
    });
}

#[test]
fn adjustment_layers_nest_on_the_already_adjusted_result() {
    bounded_case("adjustment_layers_nest_on_the_already_adjusted_result", || {
        let pixels = Pixels::fixture();
        let b = beneath();
        // Lower: Invert at 100 %. Upper: Invert at 50 % over the lower's result.
        let rec = records(vec![
            comp_layer(4, json!({"effects":[invert()],"streams":[{"property":"opacity","static_value":[50.0]}]})),
            comp_layer(3, json!({"effects":[invert()]})),
            comp_layer(2, json!({})),
        ], None);
        let lower = inverted(&b, |_, _| 1.0);
        let want = inverted(&lower, |_, _| 0.5);
        assert!(max_delta(&frame(&rec, &pixels, 0), &want) <= 1e-6);
        // A layer between them is adjusted by the upper one only.
        let levels = json!({"effect_id":"lv","filter_kind":"levels","values":{"output_white":constant(json!([0.5]))}});
        let rec = records(vec![comp_layer(4, json!({"effects":[levels]})), comp_layer(3, json!({"effects":[invert()]})), comp_layer(2, json!({}))], None);
        let got = frame(&rec, &pixels, 0);
        let half: Vec<[f32; 4]> = lower.iter().map(|p| if p[3] > 0.0 { [p[0] * 0.5, p[1] * 0.5, p[2] * 0.5, p[3]] } else { *p }).collect();
        assert!(max_delta(&got, &half) <= 1e-5, "{}", max_delta(&got, &half));
    });
}

/// The left-half mask of the composition (x < 8), with `mode` and `closed`.
fn left_half_mask(mode: &str, closed: bool) -> Value {
    json!([{"mask_id":"m","mode":mode,"closed":closed,"static_value":[
        0.0,0.0,0.0,0.0,0.0,0.0, 8.0,0.0,0.0,0.0,0.0,0.0, 8.0,12.0,0.0,0.0,0.0,0.0, 0.0,12.0,0.0,0.0,0.0,0.0]}])
}

/// Review F2 (lead ruling 2026-11-11): masks that contribute nothing (every mask mode none, or an
/// open path) leave the adjustment layer unmasked, as every other layer path reads them.
#[test]
fn an_adjustment_layer_whose_masks_contribute_nothing_is_unmasked() {
    bounded_case("an_adjustment_layer_whose_masks_contribute_nothing_is_unmasked", || {
        let pixels = Pixels::fixture();
        let whole = inverted(&beneath(), |_, _| 1.0);
        for (mode, closed) in [("none", true), ("add", false)] {
            let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"masks":left_half_mask(mode, closed)})), comp_layer(2, json!({}))], None);
            let got = frame(&rec, &pixels, 0);
            assert!(max_delta(&got, &whole) <= 1e-6, "mask mode {mode} closed {closed}: {}", max_delta(&got, &whole));
        }
    });
}

/// Review F4 (lead ruling 2026-11-11): a frame rendered below the composition size (render-queue
/// resolution) keeps a composition adjustment layer at the identity and scales its stack's pixel
/// parameters and its coverage with the frame, so it renders as the scaled frame adjusted.
#[test]
fn a_scaled_scene_keeps_the_adjustment_layer_untouched_and_scales_its_stack() {
    bounded_case("a_scaled_scene_keeps_the_adjustment_layer_untouched_and_scales_its_stack", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let (hw, hh) = (W / 2, H / 2);
        let render = |scene: &SceneComposition| pixels_of(&render_scene(scene, &context(&pixels, SceneLimits::default()), &cancel).expect("a half-resolution frame renders"));
        // The adjustment layer over the composition background only: the reference executor runs
        // plain plans, and a scaled content layer would need the resampling extension.
        let background = [0.2f32, 0.4, 0.6, 1.0];
        let at = |rec: &Value| {
            let mut scene = source(rec, &pixels).expect("source").scene_at(0, &cancel).expect("scene");
            scene.layers.retain(|l| l.layer_id == identity("SLYR", 3));
            scene.background = Some(background);
            scene
        };
        let bare: Vec<[f32; 4]> = vec![background; (hw * hh) as usize];
        let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"masks":left_half_mask("add", true)})), comp_layer(2, json!({}))], None);
        let half = scale_scene(&at(&rec), hw, hh).expect("scales");
        assert_eq!(half.layers[0].matrix.m, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "the adjustment layer keeps the identity");
        let SceneContent::AdjustmentEffects(a) = &half.layers[0].content else { panic!("{:?}", half.layers[0].content) };
        assert_eq!(a.coverage.as_ref().map(Vec::len), Some((hw * hh) as usize), "coverage over the scaled frame");
        let got = render(&half);
        let want: Vec<[f32; 4]> = bare
            .iter()
            .enumerate()
            .map(|(i, p)| if p[3] > 0.0 && (i as u32 % hw) < hw / 2 { [1.0 - p[0], 1.0 - p[1], 1.0 - p[2], p[3]] } else { *p })
            .collect();
        assert!(max_delta(&got, &want) <= 1e-6, "{}", max_delta(&got, &want));
        // A pixel parameter follows the frame: a ramp from the left to the right edge.
        let ramp = json!({"effect_id":"r","filter_kind":"gradient_ramp","values":{"start_of_ramp":constant(json!([0.0,0.0])),"end_of_ramp":constant(json!([1.0,0.0]))},
            "colour_profiles":{"start_color":{"colour_profile_id":identity("SCPF",1)},"end_color":{"colour_profile_id":identity("SCPF",1)}}});
        let rec = records(vec![comp_layer(3, json!({"effects":[ramp]})), comp_layer(2, json!({}))], None);
        let half = scale_scene(&at(&rec), hw, hh).expect("scales");
        let SceneContent::AdjustmentEffects(a) = &half.layers[0].content else { panic!() };
        assert!(matches!(a.effects[0].effect, SceneEffect::GradientRamp { start: [0.0, 0.0], end: [8.0, 0.0], .. }), "{:?}", a.effects[0].effect);
        render(&half);
    });
}

#[test]
fn adjustment_layer_refusals_are_typed() {
    let pixels = Pixels::fixture();
    let cancel = CancellationToken::default();
    let id3 = identity("SLYR", 3);
    let under = comp_layer(2, json!({}));
    // A 3D adjustment layer.
    let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"three_d":{}})), under.clone()], None);
    assert_eq!(refusal(source(&rec, &pixels).map(|_| ())), (id3.clone(), "composition_3d_content"));
    // A track matte on it.
    let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"track_matte":{"source_layer_id":identity("SLYR",2),"mode":"alpha","inverted":false}})), under.clone()], None);
    assert_eq!(refusal(source(&rec, &pixels).and_then(|s| s.scene_at(0, &cancel))), (id3.clone(), "composition_adjustment_matte"));
    // A transform that moves it (its layer space is the composition).
    let rec = records(vec![comp_layer(3, json!({"effects":[invert()],"streams":[{"property":"position","static_value":[3.0,0.0]}]})), under.clone()], None);
    let scene = source(&rec, &pixels).unwrap().scene_at(0, &cancel).unwrap();
    assert_eq!(refusal(render_scene(&scene, &context(&pixels, SceneLimits::default()), &cancel)), (id3.clone(), "composition_adjustment_transform"));
    // The document-graph path has no surface for any stack.
    let rec = records(vec![comp_layer(3, json!({"effects":[invert()]})), under.clone()], None);
    assert_eq!(refusal(source(&rec, &pixels).unwrap().layer_state(&id3, 0, &cancel)), (id3.clone(), COMPOSITION_EFFECT_CONTENT));
    // Inside a collapsed precomposition (its beneath would be another composition's stack).
    let rec = records(vec![comp_layer(8, json!({"precomp":{"composition_id":identity("SCOM",2),"collapse_transformations":true}}))], Some(vec![comp_layer(3, json!({"effects":[invert()]})), under.clone()]));
    assert_eq!(refusal(source(&rec, &pixels).and_then(|s| s.scene_at(0, &cancel))), (id3.clone(), "composition_adjustment_collapsed"));
    // Not collapsed, the nested composition renders its own adjustment over its own stack.
    let mut open_rec = rec.clone();
    open_rec["compositions"][0]["layers"][0]["precomp"]["collapse_transformations"] = json!(false);
    assert!(max_delta(&frame(&open_rec, &pixels, 0), &inverted(&beneath(), |_, _| 1.0)) <= 1e-6);
    // A pigment adjustment layer (primitive reference) with a stack stays refused.
    let raster: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":identity("SLYR",7),"adjustment":{"kind":"invert"}}]})).unwrap();
    let rec = records(vec![comp_layer(7, json!({"effects":[invert()]})), under.clone()], None);
    assert_eq!(refusal(source_with(&rec, &pixels, Some(&raster)).map(|_| ())), (identity("SLYR", 7), COMPOSITION_EFFECT_CONTENT));
    // A blend mode other than normal (STU-CMP-015 is silent; only the normal reading is established).
    let mut doc = document();
    doc["layers"][1]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":5,"clipped":false,"isolation":"isolated"});
    let rec = records(vec![comp_layer(3, json!({"effects":[invert()]})), under.clone()], None);
    assert_eq!(refusal(source_doc(&doc, &rec, &pixels).map(|_| ())), (id3.clone(), "composition_adjustment_blend"));
}

/// A document holding the source tile and `n` composition adjustment layers (ids 10..), and the
/// records stacking them over the source, each with eight Invert entries (an even count: the
/// identity).
fn stacked(n: u8) -> (Value, Value) {
    let mut doc = document();
    let mut layers = Vec::new();
    for i in 0..n {
        doc["layers"].as_array_mut().unwrap().push(adjustment(10 + i));
        layers.insert(0, comp_layer(10 + i, json!({"effects":(0..8).map(|k| json!({"effect_id":format!("i{k}"),"filter_kind":"invert"})).collect::<Vec<_>>()})));
    }
    layers.push(comp_layer(2, json!({})));
    (doc, records(layers, None))
}
fn source_doc(doc: &Value, rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(doc);
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &identity("SCOM", 1),
        budget: Budget::default(),
        resolver: &Bound,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
        space: scenes::space(),
        inputs: RenderInputs::default(),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}

#[test]
fn adjustment_work_is_charged_before_allocation_and_canceled_promptly() {
    bounded_case("adjustment_work_is_charged_before_allocation_and_canceled_promptly", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let (doc, rec) = stacked(1);
        let one = source_doc(&doc, &rec, &pixels).expect("source").scene_at(0, &cancel).expect("scene");
        let got = pixels_of(&render_scene(&one, &context(&pixels, SceneLimits::default()), &cancel).expect("renders"));
        assert!(max_delta(&got, &beneath()) <= 1e-6, "eight inverts are the identity");
        // B and E are charged: a budget that holds the frame and B but not E refuses typed.
        let frame_bytes = u64::from(W) * u64::from(H) * SURFACE_BYTES_PER_PIXEL;
        let tight = SceneLimits { max_surface_bytes: frame_bytes * 2, ..SceneLimits::default() };
        assert!(matches!(render_scene(&one, &context(&pixels, tight), &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        // 64 nested adjustment layers each with a full stack: charged against the scene's budget.
        let (doc, rec) = stacked(64);
        let deep = source_doc(&doc, &rec, &pixels).expect("source").scene_at(0, &cancel).expect("scene");
        assert!(matches!(render_scene(&deep, &context(&pixels, SceneLimits::default()), &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        // A 30000 x 30000 composition: refused by the adjustment's surface charge before B or E is
        // allocated (the byte bound is lifted, the surface count is not).
        let (doc, mut rec) = stacked(1);
        rec["compositions"][0]["width"] = json!(30_000);
        rec["compositions"][0]["height"] = json!(30_000);
        let huge = source_doc(&doc, &rec, &pixels).expect("source").scene_at(0, &cancel).expect("scene");
        let limits = SceneLimits { max_surface_bytes: u64::MAX, max_surfaces: 3, ..SceneLimits::default() };
        let started = std::time::Instant::now();
        assert!(matches!(render_scene(&huge, &context(&pixels, limits), &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        assert!(started.elapsed().as_secs() < 5, "refused before any 30000 x 30000 buffer");
        // Cancel: a canceled render returns at once with the typed code.
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert!(matches!(render_scene(&one, &context(&pixels, SceneLimits::default()), &canceled), Err(TemporalError::Composite(CompositeError::Canceled))));
    });
}

#[test]
fn a_raster_document_render_refuses_a_composition_adjustment_layer_typed() {
    // Outside a composition the group-payload adjustment layer has nothing to adjust: the static
    // document path refuses its source step typed (`unsupported_payload`), never an empty draw.
    let mut v = document();
    v["layers"] = json!([layer(1, "group", json!({"payload_kind":"group"})), adjustment(3)]);
    v["graph"] = json!({"schema_id":"hsk.studio.layer_graph@1","operations":[common::source(3,"image"),common::composite(1)],
        "edges":[common::edge(common::address(3,"source","out"),common::address(1,"mix","inputs"),0)],"outputs":[common::address(1,"mix","out")]});
    let snap = open(&v);
    let plan = common::plan_of(&snap);
    let eval = evaluate(&plan, &snap, &CancellationToken::default()).expect("evaluates with a typed status");
    let status = eval.steps.iter().find(|s| s.addr.layer_id == identity("SLYR", 3)).map(|s| s.status.clone());
    assert_eq!(status, Some(StepStatus::Unavailable("unsupported_payload".into())));
}

#[test]
fn motion_ease_and_hold_write_the_values_the_harness_mirror_writes() {
    // harness-core has no motion edge: its keyframe.ease and keyframe.toggle_hold mirror these two
    // assistants. The harness test pins the mirror to the same stored values (bezier, speed 0,
    // influence 0.16666666666 on the eased side; hold, then linear without tangents).
    use hsk_studio_motion::{EaseSide, Interpolation, Keyframe, Property, TemporalTangent, ease, toggle_hold};
    let key = |t: u64, v: f64| Keyframe::new(hsk_studio_accord::Ticks::new(t), vec![v]);
    let mut p = Property::new("opacity", vec![0.0], vec![key(0, 0.0), key(10, 1.0), key(20, 0.0)]).unwrap();
    ease(&mut p, &[1], EaseSide::Both).unwrap();
    let k = &p.keyframes()[1];
    assert_eq!((k.in_interp, k.out_interp), (Interpolation::Bezier, Interpolation::Bezier));
    assert_eq!((k.in_tangent.clone(), k.out_tangent.clone()), (vec![TemporalTangent::EASE], vec![TemporalTangent::EASE]));
    assert_eq!((TemporalTangent::EASE.speed, TemporalTangent::EASE.influence), (0.0, 0.16666666666));
    ease(&mut p, &[0], EaseSide::Out).unwrap();
    assert_eq!((p.keyframes()[0].in_interp, p.keyframes()[0].out_interp), (Interpolation::Linear, Interpolation::Bezier));
    ease(&mut p, &[2], EaseSide::In).unwrap();
    assert_eq!((p.keyframes()[2].in_interp, p.keyframes()[2].out_interp), (Interpolation::Bezier, Interpolation::Linear));
    toggle_hold(&mut p, &[0, 1]).unwrap();
    assert!(p.keyframes()[..2].iter().all(|k| k.out_interp == Interpolation::Hold));
    assert_eq!(p.keyframes()[1].out_tangent, vec![TemporalTangent::EASE], "hold keeps the stored tangent");
    toggle_hold(&mut p, &[0, 1]).unwrap();
    assert!(p.keyframes()[..2].iter().all(|k| k.out_interp == Interpolation::Linear && k.out_tangent.is_empty()));
}
