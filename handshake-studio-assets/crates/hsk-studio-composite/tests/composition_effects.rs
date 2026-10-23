//! Lane SMB-AE-EFFECT-STACK-1: the per-layer effect stack (STU-FX-002, STU-CMP-010 steps 2-3) and
//! the distort / generate CPU kernels (parity AE-17). Fixture: `tests/support/effect_scenes.rs`
//! (a 16x12 source with stepped alpha). Kernels are pinned by digest on synthetic input and checked
//! against hand values; Folio records lower through `CompositionTemporalSource` into
//! `SceneContent::Effected` and render through `render_scene` with a small reference executor
//! (`Ref`, as `tests/composition_roto.rs`); the CPU backend renders the same scene in render-cpu
//! `tests/composition_effect_stack.rs`.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::{
    EFFECT_STACK_OPERATOR, EffectError, EffectedContent, GeneratorComposite, MAX_SCENE_EFFECTS, SceneEffect, SceneEffectEntry,
    apply_effects,
};
use hsk_studio_composite::temporal::composition::{COMPOSITION_EFFECT_OPTION, CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, Exactness, ExecuteRequest, NoVector, Operator,
    PlanExtensions, PlanNode, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat, TilePlanes, TileSink,
};
use serde_json::{Value, json};

mod common;
use common::{Available, bounded_case, layer, open, tile};
#[path = "support/effect_scenes.rs"]
mod scenes;
use scenes::{H, Pixels, W, digest, effected_scene, entry, golden_cases, identity, premultiplied_source, source_texel, straight};

/// Digests of [`golden_cases`] on [`premultiplied_source`] (pinned on the build platform; module
/// docs of `composition::effect_stack` on `libm`).
const GOLDEN: &[(&str, &str)] = &[
    ("twirl", "PIN"),
    ("spherize", "PIN"),
    ("offset", "PIN"),
    ("polar_rect_to_polar", "PIN"),
    ("polar_polar_to_rect", "PIN"),
    ("ramp_linear", "PIN"),
    ("ramp_radial", "PIN"),
    ("circle_over_feathered", "PIN"),
    ("circle_replace", "PIN"),
    ("checkerboard_over", "PIN"),
    ("twirl_then_offset", "PIN"),
    ("offset_then_twirl_mixed", "PIN"),
];

fn run(effects: &[SceneEffectEntry]) -> Vec<[f32; 4]> {
    let mut b = premultiplied_source();
    apply_effects(W, H, &mut b, effects, &CancellationToken::default()).expect("stack runs");
    b
}
fn at(buf: &[[f32; 4]], x: u32, y: u32) -> [f32; 4] {
    buf[(y * W + x) as usize]
}

#[test]
fn distort_and_generate_kernels_match_pinned_digests_and_hand_values() {
    bounded_case("distort_and_generate_kernels_match_pinned_digests_and_hand_values", || {
        let cases = golden_cases();
        assert_eq!(cases.len(), GOLDEN.len());
        let mut seen = Vec::new();
        for ((name, effects), (pinned_name, pinned)) in cases.iter().zip(GOLDEN) {
            assert_eq!(name, pinned_name);
            let got = digest(&run(effects));
            println!("GOLDEN {name} {got}");
            assert_eq!(&got, pinned, "{name}");
            assert!(!seen.contains(&got), "{name} equals an earlier case");
            seen.push(got);
        }
        let src = premultiplied_source();
        // Identities: a zero twirl angle, a zero sphere radius, zero interpolation, zero shift and a
        // zero entry mix leave the source bit for bit.
        for e in [
            SceneEffect::Twirl { angle_deg: 0.0, radius_px: 7.0, centre: [8.0, 6.0] },
            SceneEffect::Spherize { radius_px: 0.0, centre: [8.0, 6.0] },
            SceneEffect::PolarCoordinates { interpolation: 0.0, rect_to_polar: true },
            SceneEffect::Offset { shift: [0.0, 0.0], blend: 0.0 },
        ] {
            assert_eq!(run(&[entry("i", e.clone())]), src, "{e:?}");
        }
        assert_eq!(run(&[SceneEffectEntry { mix: 0.0, ..entry("m", scenes::twirl()) }]), src);
        // Offset (3, 2) with wrap-around is an exact cyclic shift; blend 1 is the original.
        let shifted = run(&[entry("o", scenes::offset())]);
        for y in 0..H {
            for x in 0..W {
                assert_eq!(at(&shifted, x, y), at(&src, (x + W - 3) % W, (y + H - 2) % H), "({x}, {y})");
            }
        }
        assert_eq!(run(&[entry("o", SceneEffect::Offset { shift: [3.0, 2.0], blend: 1.0 })]), src);
        // Twirl outside its radius is the identity.
        let tw = run(&[entry("t", scenes::twirl())]);
        assert_eq!(at(&tw, 0, 0), at(&src, 0, 0));
        assert_eq!(at(&tw, 15, 11), at(&src, 15, 11));
        // Linear ramp black -> white across the width, opaque, no blend: value = (x + 0.5) / 16.
        let ramp = run(&[entry("r", SceneEffect::GradientRamp {
            start: [0.0, 0.0], start_colour: [0.0, 0.0, 0.0, 1.0], end: [16.0, 0.0], end_colour: [1.0; 4], radial: false, blend: 0.0,
        })]);
        for y in 0..H {
            for x in 0..W {
                let want = (f64::from(x) + 0.5) / 16.0;
                let p = at(&ramp, x, y);
                assert!(p[..3].iter().all(|c| (f64::from(*c) - want).abs() < 1e-7) && p[3] == 1.0, "({x}, {y}) {p:?}");
            }
        }
        // Circle (replace): radius 4 around (8, 6), white, opaque: coverage clamp(4.5 - d).
        let disc = run(&[entry("c", SceneEffect::Circle {
            centre: [8.0, 6.0], radius_px: 4.0, feather_px: 0.0, colour: [1.0; 4], opacity: 1.0, composite: GeneratorComposite::Replace,
        })]);
        for y in 0..H {
            for x in 0..W {
                let d = (f64::from(x) + 0.5 - 8.0).hypot(f64::from(y) + 0.5 - 6.0);
                let want = (4.5 - d).clamp(0.0, 1.0);
                assert!(at(&disc, x, y).iter().all(|c| (f64::from(*c) - want).abs() < 1e-6), "({x}, {y})");
            }
        }
        assert_eq!(at(&disc, 8, 6), [1.0; 4]);
        assert_eq!(at(&disc, 0, 0), [0.0; 4]);
        // Checkerboard (replace): anchor (0, 0), 4x4 cells, blue: the anchor's cell is coloured.
        let board = run(&[entry("k", SceneEffect::Checkerboard {
            anchor: [0.0, 0.0], cell: [4.0, 4.0], colour: [0.0, 0.0, 1.0, 1.0], opacity: 1.0, composite: GeneratorComposite::Replace,
        })]);
        for y in 0..H {
            for x in 0..W {
                let coloured = (x / 4 + y / 4) % 2 == 0;
                assert_eq!(at(&board, x, y), if coloured { [0.0, 0.0, 1.0, 1.0] } else { [0.0; 4] }, "({x}, {y})");
            }
        }
        // Over: a transparent board cell keeps the layer.
        let over = run(&[entry("k", scenes::checkerboard(GeneratorComposite::Over))]);
        // Anchor (1, 2), 3x2 cells: (4, 2) lies in an uncoloured cell, (1, 2) in a coloured one.
        assert_eq!(at(&over, 4, 2), at(&src, 4, 2));
        assert_eq!(at(&over, 1, 2), [0.0, 0.0, 1.0, 1.0]);
    });
}

#[test]
fn hostile_effect_parameters_are_refused_before_allocation() {
    bounded_case("hostile_effect_parameters_are_refused_before_allocation", || {
        let cancel = CancellationToken::default();
        let mut b = premultiplied_source();
        let bad = [
            SceneEffect::Twirl { angle_deg: f64::NAN, radius_px: 7.0, centre: [8.0, 6.0] },
            SceneEffect::Spherize { radius_px: -1.0, centre: [8.0, 6.0] },
            SceneEffect::Offset { shift: [f64::INFINITY, 0.0], blend: 0.0 },
            SceneEffect::PolarCoordinates { interpolation: 1.5, rect_to_polar: false },
            SceneEffect::GradientRamp { start: [0.0; 2], start_colour: [0.0; 4], end: [1.0; 2], end_colour: [1.0; 4], radial: false, blend: -0.1 },
            SceneEffect::Circle { centre: [0.0; 2], radius_px: 1.0, feather_px: -1.0, colour: [1.0; 4], opacity: 1.0, composite: GeneratorComposite::Over },
            SceneEffect::Checkerboard { anchor: [0.0; 2], cell: [0.0, 1.0], colour: [1.0; 4], opacity: 1.0, composite: GeneratorComposite::Over },
        ];
        for (i, e) in bad.into_iter().enumerate() {
            let stack = [entry("ok", scenes::offset()), entry("bad", e)];
            assert_eq!(apply_effects(W, H, &mut b, &stack, &cancel), Err(EffectError::Parameter(1)), "case {i}");
        }
        assert_eq!(b, premultiplied_source(), "nothing ran before the refusal");
        assert_eq!(apply_effects(W, H, &mut b, &[SceneEffectEntry { mix: 1.5, ..entry("m", scenes::offset()) }], &cancel), Err(EffectError::Parameter(0)));
        assert_eq!(apply_effects(W, H + 1, &mut b, &[], &cancel), Err(EffectError::Shape));
        let many: Vec<SceneEffectEntry> = (0..=MAX_SCENE_EFFECTS).map(|_| entry("o", scenes::offset())).collect();
        assert_eq!(apply_effects(W, H, &mut b, &many, &cancel), Err(EffectError::OverBudget));
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(apply_effects(W, H, &mut b, &[entry("o", scenes::offset())], &canceled), Err(EffectError::Canceled));
        // Scene path: a mask of the wrong size, a bad parameter and a stack whose work exceeds the
        // bound are refused before the content renders.
        let pixels = Pixels::fixture();
        let ctx = context(&pixels);
        let scene = effected_scene(&pixels, vec![entry("o", scenes::offset())], Some(vec![1.0; 3]));
        assert!(matches!(render_scene(&scene, &ctx, &cancel), Err(TemporalError::InvalidState { field: "effect_rect", .. })));
        let scene = effected_scene(&pixels, vec![entry("t", SceneEffect::Twirl { angle_deg: 1.0, radius_px: f64::NAN, centre: [0.0; 2] })], None);
        assert!(matches!(render_scene(&scene, &ctx, &cancel), Err(TemporalError::InvalidState { field: "effect_parameter", .. })));
        let mut huge = effected_scene(&pixels, vec![entry("o", scenes::offset()); 8], None);
        if let SceneContent::Effected(e) = &mut huge.layers[0].content {
            e.rect = Rect { x: 0, y: 0, width: 65_535, height: 65_535 };
        }
        assert!(matches!(render_scene(&huge, &ctx, &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
    });
}

/// Reference executor (as `tests/composition_roto.rs`): per pixel, groups composite their layers
/// bottom-up over transparent with pigment's `composite_over`.
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
    fn execute_extended(&self, plan: &RenderPlan<NoVector>, ext: &PlanExtensions, r: &ExecuteRequest,
        s: &dyn PlanSources, c: &CancellationToken, sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        assert!(ext.is_empty(), "the reference executor takes plain plans only");
        self.execute(plan, r, s, c, sink)
    }
    fn execute(&self, plan: &RenderPlan<NoVector>, _: &ExecuteRequest, src: &dyn PlanSources, _: &CancellationToken,
        sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
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

fn context(pixels: &Pixels) -> SceneContext<'_> {
    SceneContext {
        backend: &Ref,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    }
}

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        (format!("{}/{}", t.layer_id, t.object_key) == scenes::tile_id()).then(|| TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.0.colour_sha256,
            alpha_sha256: self.0.alpha_sha256,
        })
    }
}

/// 24 fps.
const TICKS_PER_FRAME: u64 = 10_584_000_000;
fn f(frame: u64) -> u64 {
    frame * TICKS_PER_FRAME
}
/// Document: raster layer 2 (the 16x12 source tile).
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2,"src",0,0,W,H)]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    common::assign_all_profiles(&mut v);
    v
}
/// One 16x12 composition (4 frames) holding layer 2 with `extra` merged into its record.
fn records(extra: Value) -> Value {
    let mut l = json!({"layer_id":identity("SLYR",2),
        "timing":{"in_point":0,"out_point":f(4),"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        l[k] = v;
    }
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",1),"name":"main","width":W,"height":H,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(4),
         "layers":[l]}]})
}
fn source(rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(&document());
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &identity("SCOM", 1),
        budget: Budget::default(),
        resolver: &Available,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
        space: scenes::space(),
        inputs: RenderInputs::default(),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}
fn constant(v: Value) -> Value {
    json!({ "static_value": v })
}
/// The frame's straight RGBA, row-major.
fn pixels_of(r: &RenderedScene) -> Vec<[f32; 4]> {
    (0..(r.width * r.height) as usize)
        .map(|i| [f32_at(&r.colour, i * 12), f32_at(&r.colour, i * 12 + 4), f32_at(&r.colour, i * 12 + 8), f32_at(&r.alpha, i * 4)])
        .collect()
}
fn max_delta(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).flat_map(|(p, q)| p.iter().zip(q).map(|(x, y)| (x - y).abs())).fold(0.0, f32::max)
}

#[test]
fn folio_effect_stacks_render_through_render_scene_in_stack_order() {
    bounded_case("folio_effect_stacks_render_through_render_scene_in_stack_order", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let ctx = context(&pixels);
        let render = |extra: Value, frame: u64| -> (SceneComposition, RenderedScene) {
            let src = source(&records(extra), &pixels).expect("source");
            let scene = src.scene_at(f(frame) as i64, &cancel).expect("scene");
            let r = render_scene(&scene, &ctx, &cancel).expect("frame renders");
            (scene, r)
        };
        // Offset: the layer centre (8, 6) moves to (11/16, 9/12) of the layer = (11, 9): shift (3, 3).
        let offset = json!({"effect_id":"o","filter_kind":"offset","values":{"shift_center_to":constant(json!([0.6875,0.75]))}});
        let board = json!({"effect_id":"k","filter_kind":"checkerboard","values":{
            "anchor":constant(json!([0.0625,0.25])),"size_from":constant(json!([3.0])),"width":constant(json!([3.0])),
            "height":constant(json!([2.0])),"color":constant(json!([0.0,0.0,1.0,1.0])),"blending_mode":constant(json!([2.0]))}});
        let k_offset = SceneEffect::Offset { shift: [3.0, 3.0], blend: 0.0 };
        let k_board = SceneEffect::Checkerboard {
            anchor: [1.0, 3.0], cell: [3.0, 2.0], colour: [0.0, 0.0, 1.0, 1.0], opacity: 1.0, composite: GeneratorComposite::Over,
        };
        let (scene, frame) = render(json!({"effects":[offset, board]}), 0);
        let SceneContent::Effected(e) = &scene.layers[0].content else { panic!("not effected: {:?}", scene.layers[0].content) };
        assert_eq!(e.effects.iter().map(|x| x.effect.clone()).collect::<Vec<_>>(), vec![k_offset.clone(), k_board.clone()]);
        let want = straight(&run(&[entry("o", k_offset.clone()), entry("k", k_board.clone())]));
        assert!(max_delta(&pixels_of(&frame), &want) <= 1e-6, "{}", max_delta(&pixels_of(&frame), &want));
        assert!(frame.exactness.operators.iter().any(|o| o.key == EFFECT_STACK_OPERATOR));
        // Stack order is render order: the board under the offset differs.
        let (_, reversed) = render(json!({"effects":[board, offset]}), 0);
        let want_rev = straight(&run(&[entry("k", k_board.clone()), entry("o", k_offset.clone())]));
        assert!(max_delta(&pixels_of(&reversed), &want_rev) <= 1e-6);
        assert!(max_delta(&want, &want_rev) > 0.1);
        // A disabled entry is skipped; a soloed entry runs alone; the entry opacity mixes.
        let mut off = json!({"effect_id":"c","filter_kind":"circle","enabled":false});
        let (_, skipped) = render(json!({"effects":[offset, off.clone(), board]}), 0);
        assert_eq!(skipped.digest(), frame.digest());
        let mut solo = offset.clone();
        solo["solo"] = json!(true);
        let (_, soloed) = render(json!({"effects":[solo, board]}), 0);
        assert!(max_delta(&pixels_of(&soloed), &straight(&run(&[entry("o", k_offset.clone())]))) <= 1e-6);
        let mut half = board.clone();
        half["opacity"] = json!(50.0);
        let (_, mixed) = render(json!({"effects":[half]}), 0);
        let want_half = straight(&run(&[SceneEffectEntry { mix: 0.5, ..entry("k", k_board.clone()) }]));
        assert!(max_delta(&pixels_of(&mixed), &want_half) <= 1e-6);
        // Masks before effects (STU-CMP-010 step 2 then 3): a left-half mask is folded into the
        // content, so the offset moves the masked edge; no composition-space matte remains.
        let mask = json!([{"mask_id":"m","mode":"add","closed":true,"static_value":[
            0.0,0.0,0.0,0.0,0.0,0.0, 8.0,0.0,0.0,0.0,0.0,0.0, 8.0,12.0,0.0,0.0,0.0,0.0, 0.0,12.0,0.0,0.0,0.0,0.0]}]);
        let (scene, masked) = render(json!({"masks":mask, "effects":[offset]}), 0);
        assert!(scene.layers[0].matte.is_none());
        let SceneContent::Effected(e) = &scene.layers[0].content else { panic!("not effected") };
        let cov = e.mask.as_ref().expect("folded mask");
        for y in 0..H {
            for x in 0..W {
                let c = cov[(y * W + x) as usize];
                assert!((c - if x < 8 { 1.0 } else { 0.0 }).abs() <= 1e-6, "({x}, {y}) {c}");
            }
        }
        let mut src = premultiplied_source();
        for (i, p) in src.iter_mut().enumerate() {
            let c = f64::from(cov[i]);
            *p = p.map(|v| (f64::from(v) * c) as f32);
        }
        apply_effects(W, H, &mut src, &[entry("o", k_offset.clone())], &cancel).unwrap();
        assert!(max_delta(&pixels_of(&masked), &straight(&src)) <= 1e-6);
        // Animated values are sampled per frame (STU-FX-011b): angle 0 -> 90 over frames 0..2.
        let twirl = json!({"effect_id":"t","filter_kind":"twirl","values":{"angle":{"static_value":[0.0],"keyframes":[
            {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
            {"time":f(2),"value":[90.0],"in_interp":"linear","out_interp":"linear"}]}}});
        let (scene, first) = render(json!({"effects":[twirl]}), 0);
        let SceneContent::Effected(e) = &scene.layers[0].content else { panic!() };
        assert_eq!(e.effects[0].effect, SceneEffect::Twirl { angle_deg: 0.0, radius_px: 0.3 * 16.0, centre: [8.0, 6.0] });
        let identity_frame = (0..W * H).map(|i| source_texel(i % W, i / W)).collect::<Vec<_>>();
        assert!(max_delta(&pixels_of(&first), &straight(&premultiplied_source())) <= 1e-6);
        assert!(max_delta(&straight(&premultiplied_source()), &identity_frame) <= 1e-6);
        let (scene, _) = render(json!({"effects":[twirl]}), 1);
        let SceneContent::Effected(e) = &scene.layers[0].content else { panic!() };
        let SceneEffect::Twirl { angle_deg, .. } = e.effects[0].effect else { panic!() };
        assert!((angle_deg - 45.0).abs() < 1e-9, "{angle_deg}");
        // Options this lane does not implement are refused typed, never approximated.
        let unsupported = |extra: Value| match source(&records(extra), &pixels).and_then(|s| s.scene_at(0, &cancel)) {
            Err(TemporalError::Unsupported { feature, .. }) => feature,
            other => panic!("{other:?}"),
        };
        off["enabled"] = json!(true);
        off["values"] = json!({"edge":constant(json!([2.0]))});
        assert_eq!(unsupported(json!({"effects":[off]})), COMPOSITION_EFFECT_OPTION);
        let mut mode = board.clone();
        mode["values"]["blending_mode"] = constant(json!([5.0]));
        assert_eq!(unsupported(json!({"effects":[mode]})), COMPOSITION_EFFECT_OPTION);
        let scatter = json!({"effect_id":"r","filter_kind":"gradient_ramp","values":{"ramp_scatter":constant(json!([1.0]))}});
        assert_eq!(unsupported(json!({"effects":[scatter]})), COMPOSITION_EFFECT_OPTION);
        // A Folio refusal (hard bound) surfaces as a document error.
        let over = json!({"effect_id":"t","filter_kind":"twirl","values":{"twirl_radius":constant(json!([101.0]))}});
        assert!(matches!(source(&records(json!({"effects":[over]})), &pixels), Err(TemporalError::Document(t)) if t.contains("effect_param_out_of_range")));
        let _ = EffectedContent { content: SceneContent::None, rect: Rect { x: 0, y: 0, width: 1, height: 1 }, mask: None, effects: Vec::new() };
    });
}
