//! Lane SMB-AE-ROTO-WIRING: Folio `time_remap`, `frame_blending` and assisted-roto records through
//! the composition adapter (STU-MOT-045, STU-MOT-007a, STU-CMP-034). Fixture:
//! `tests/support/roto_scenes.rs` (a 16x12, 8-frame source with an opaque 4x4 square moving one
//! pixel per frame) nested as a precomposition. Frames are rendered through `render_scene` with a
//! small reference executor (`Ref`: tiles, solids, isolated groups, coverage masks, pigment
//! `composite_over`) and compared with oracles built from Pigment's own frame-blend functions on
//! the hand-written source frames. The CPU and GPU backends render the same contents in the
//! renderer tests `composition_roto_frames.rs`.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::FrameBlendMethod;
use hsk_studio_composite::temporal::composition::{
    COMPOSITION_ROTO_FRAME_BLENDING, COMPOSITION_ROTO_NOT_PROPAGATED, COMPOSITION_TIME_SOURCE, CompositionInput,
    CompositionTemporalSource, ROTO_FRAME_LOST, RotoFrameState, RotoLimits, RotoRun,
};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef, time_remap_enabled};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::frame_blend::{BlendLimits, FlowParams, frame_mix, pixel_motion};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, Exactness, ExecuteRequest, NoVector, Operator,
    PlanExtensions, PlanNode, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat, TilePlanes, TileSink,
};
use hsk_studio_pigment::Mask;
use serde_json::{Value, json};

mod common;
use common::{Available, bounded_case, identity, layer, open, tile};
#[path = "support/roto_scenes.rs"]
mod scenes;
use scenes::{FRAMES, H, Pixels, TICKS_PER_FRAME, W, fg_x, inside_fg, nested_plane};

/// Reference executor (as `tests/composition_document.rs`): per pixel, groups composite their
/// layers bottom-up over transparent with pigment's `composite_over`.
struct Ref;
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn coverage(m: &Mask<'_>, x: i64, y: i64) -> f32 {
    let (cx, cy) = (x - m.bounds.x, y - m.bounds.y);
    if cx < 0 || cy < 0 || cx >= i64::from(m.bounds.width) || cy >= i64::from(m.bounds.height) {
        return 0.0;
    }
    let row = cy as usize * m.stride_bytes as usize;
    let i = row + cx as usize * 2;
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

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        let p = self.0.get(&format!("{}/{}", t.layer_id, t.object_key))?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight,
            colour_sha256: p.colour_sha256, alpha_sha256: p.alpha_sha256 })
    }
}

fn f(frame: u64) -> u64 {
    frame * TICKS_PER_FRAME
}
fn secs(frame: f64) -> f64 {
    frame / 24.0
}
fn timing(a: u64, b: u64) -> Value {
    json!({"in_point":f(a),"out_point":f(b),"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}
/// Document: group 1 (the precomposition layer), raster 2 (bg 16x12), raster 3 (fg 4x4).
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            layer(1, "group", json!({"payload_kind":"group"})),
            layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2,"bg",0,0,W,H)]})),
            layer(3, "raster", json!({"payload_kind":"raster","tiles":[tile(3,"fg",0,0,4,4)]})),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    common::assign_all_profiles(&mut v);
    v
}
/// The outer composition (16 frames) holds layer 1 = the source composition; `extra` is merged
/// into layer 1's record; `fg_out` = the frame at which the fg square leaves the source.
fn records(extra: Value, fg_out: u64) -> Value {
    let mut outer_layer = json!({"layer_id":identity("SLYR",1),"timing":timing(0,16),
        "precomp":{"composition_id":identity("SCOM",2),"collapse_transformations":false}});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        outer_layer[k] = v;
    }
    let key = |frame: u64, x: f64| json!({"time":f(frame),"value":[x,4.0],"in_interp":"linear","out_interp":"linear"});
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",1),"name":"outer","width":W,"height":H,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(16),
         "layers":[outer_layer]},
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",2),"name":"source","width":W,"height":H,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(FRAMES),
         "layers":[
            {"layer_id":identity("SLYR",3),"timing":timing(0,fg_out),"streams":[
                {"property":"anchor","static_value":[0.0,0.0]},
                {"property":"position","static_value":[3.0,4.0],"keyframes":[key(0,3.0),key(7,10.0)]}]},
            {"layer_id":identity("SLYR",2),"timing":timing(0,FRAMES)}
         ]}]})
}
fn source(doc: &Value, rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(doc);
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
fn ctx(pixels: &Pixels) -> SceneContext<'_> {
    SceneContext {
        backend: &Ref,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    }
}
fn frame(src: &CompositionTemporalSource, pixels: &Pixels, outer: u64) -> RenderedScene {
    let cancel = CancellationToken::default();
    let scene = src.scene_at(f(outer) as i64, &cancel).expect("scene");
    render_scene(&scene, &ctx(pixels), &cancel).expect("frame renders")
}
fn remap(keys: &[(u64, f64)]) -> Value {
    let k: Vec<Value> = keys
        .iter()
        .map(|(t, v)| json!({"time":f(*t),"value":[secs(*v)],"in_interp":"linear","out_interp":"linear"}))
        .collect();
    json!({"static_value":[secs(keys[0].1)],"keyframes":k})
}

#[test]
fn time_remap_records_lower_and_frame_blending_renders() {
    bounded_case("time_remap_records_lower_and_frame_blending_renders", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let plain = source(&document(), &records(json!({}), FRAMES), &pixels).unwrap();
        for k in 0..FRAMES {
            assert!(scenes::max_delta(&frame(&plain, &pixels, k), &nested_plane(k)) <= 1e-6, "plain frame {k}");
        }
        // Enabling (Folio helper = motion's enable keys) changes nothing (STU-MOT-045 contract 1).
        let enabled = serde_json::to_value(time_remap_enabled(0, f(FRAMES - 1)).unwrap()).unwrap();
        let src = source(&document(), &records(json!({"time_remap": enabled}), FRAMES), &pixels).unwrap();
        for k in 0..FRAMES {
            assert_eq!(frame(&src, &pixels, k).digest(), frame(&plain, &pixels, k).digest(), "enabled frame {k}");
            assert_eq!(src.source_tick(&identity("SLYR", 1), f(k) as i64).unwrap(), f(k) as i64);
        }
        // Flat = freeze, decreasing = reverse, out of range = clamp to the last frame (contract 3, 5).
        let frozen = source(&document(), &records(json!({"time_remap": remap(&[(0, 2.0), (15, 2.0)])}), FRAMES), &pixels).unwrap();
        let reverse = source(&document(), &records(json!({"time_remap": remap(&[(0, 7.0), (7, 0.0)])}), FRAMES), &pixels).unwrap();
        let wild = source(&document(), &records(json!({"time_remap": remap(&[(0, 1.0e6)])}), FRAMES), &pixels).unwrap();
        for k in 0..FRAMES {
            assert!(scenes::max_delta(&frame(&frozen, &pixels, k), &nested_plane(2)) <= 1e-6, "freeze {k}");
            assert!(scenes::max_delta(&frame(&reverse, &pixels, k), &nested_plane(7 - k)) <= 1e-6, "reverse {k}");
            assert!(scenes::max_delta(&frame(&wild, &pixels, k), &nested_plane(7)) <= 1e-6, "clamp {k}");
        }
        // Half speed: outer frame 2k+1 sits halfway between source frames k and k+1.
        let half = remap(&[(0, 0.0), (14, 7.0)]);
        let off = source(&document(), &records(json!({"time_remap": half}), FRAMES), &pixels).unwrap();
        assert!(scenes::max_delta(&frame(&off, &pixels, 1), &nested_plane(0)) <= 1e-6, "off = floor frame");
        assert!(scenes::max_delta(&frame(&off, &pixels, 3), &nested_plane(1)) <= 1e-6, "off = floor frame");
        for (blending, method) in [("frame_mix", FrameBlendMethod::FrameMix), ("pixel_motion", FrameBlendMethod::PixelMotion)] {
            let src = source(&document(), &records(json!({"time_remap": half, "frame_blending": blending}), FRAMES), &pixels).unwrap();
            // An integral source frame shows that frame (weight 0: no blend).
            let on_frame = src.scene_at(f(2) as i64, &cancel).unwrap();
            assert!(matches!(on_frame.layers[0].content, SceneContent::Precomp(_)), "{blending}: {:?}", on_frame.layers[0].content);
            for k in [1u64, 5, 11] {
                let scene = src.scene_at(f(k) as i64, &cancel).unwrap();
                let SceneContent::FrameBlended(fb) = &scene.layers[0].content else { panic!("{blending} {k}: not blended") };
                assert_eq!(fb.method, method);
                assert!((fb.weight_b - 0.5).abs() < 1e-6, "{blending} {k}: weight {}", fb.weight_b);
                let (a, b) = (k / 2, k / 2 + 1);
                let want = match method {
                    FrameBlendMethod::FrameMix => frame_mix(&nested_plane(a), &nested_plane(b), fb.weight_b, &BlendLimits::DEFAULT).unwrap(),
                    FrameBlendMethod::PixelMotion => pixel_motion(&nested_plane(a), &nested_plane(b), fb.weight_b, &FlowParams::DEFAULT, &BlendLimits::DEFAULT, &cancel).unwrap(),
                };
                let got = render_scene(&scene, &ctx(&pixels), &cancel).unwrap();
                let delta = scenes::max_delta(&got, &want);
                println!("{blending} outer frame {k}: digest {} max |delta| {delta:e}", scenes::hex(&got.digest()));
                assert!(delta <= 1e-5, "{blending} {k}: {delta}");
                assert!(got.exactness.operators.iter().any(|o| o.key == "frame_blend" && o.exactness == Exactness::Approximate));
                assert_eq!(render_scene(&scene, &ctx(&pixels), &cancel).unwrap().digest(), got.digest(), "repeatable");
            }
            // Held at the last frame: the remap clamps at 7 frames, the pair is (7, 7), no blend.
            let late = src.scene_at(f(15) as i64, &cancel).unwrap();
            assert!(matches!(late.layers[0].content, SceneContent::Precomp(_)));
        }
        // A caller's remap overrides the record (lane SMB-AE-ROTO-TIMEREMAP entry point).
        let st = hsk_studio_motion::SourceTiming::new(f(FRAMES), f(1)).unwrap();
        let over = source(&document(), &records(json!({"time_remap": remap(&[(0, 2.0)])}), FRAMES), &pixels)
            .unwrap()
            .with_time_remap(&identity("SLYR", 1), hsk_studio_motion::TimeRemap::with_keys(st, &[(0, secs(5.0))]).unwrap())
            .unwrap();
        assert!(scenes::max_delta(&frame(&over, &pixels, 0), &nested_plane(5)) <= 1e-6);
        // Refusals (typed): a remap / blending / roto on a layer without frames, roto with
        // blending, a malformed remap record.
        let mut rec = records(json!({}), FRAMES);
        rec["compositions"][1]["layers"][0]["frame_blending"] = json!("frame_mix");
        let e = source(&document(), &rec, &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_TIME_SOURCE, .. }), "{e:?}");
        let e = source(&document(), &records(json!({"frame_blending":"pixel_motion","roto":roto(3, vec![])}), FRAMES), &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_ROTO_FRAME_BLENDING, .. }), "{e:?}");
        let bad = json!({"time_remap": {"static_value":[0.0, 1.0]}});
        assert!(matches!(source(&document(), &records(bad, FRAMES), &pixels), Err(TemporalError::Document(_))));
    });
}

fn path(points: &[[f64; 2]]) -> Value {
    json!(points)
}
/// A roto record: fg stroke inside the square of `base`, two bg strokes along the top and bottom
/// rows, one span over the whole source with base frame `base`, plus `strokes`.
fn roto(base: u64, strokes: Vec<Value>) -> Value {
    let x = fg_x(base) as f64;
    let mut all = vec![
        json!({"frame":base,"polarity":"foreground","diameter":2.0,"path":path(&[[x + 1.0, 6.0], [x + 3.0, 6.0]])}),
        json!({"frame":base,"polarity":"background","diameter":2.0,"path":path(&[[1.0, 1.0], [14.0, 1.0]])}),
        json!({"frame":base,"polarity":"background","diameter":2.0,"path":path(&[[1.0, 10.5], [14.0, 10.5]])}),
    ];
    all.extend(strokes);
    json!({"strokes":all,"spans":[{"base_frame":base,"in_frame":0,"out_frame":FRAMES}]})
}
fn run(src: &CompositionTemporalSource, pixels: &Pixels, cache: Option<&hsk_studio_composite::temporal::composition::RotoSegmentation>) -> Result<RotoRun, TemporalError> {
    src.propagate_roto(&identity("SLYR", 1), &ctx(pixels), cache, &RotoLimits::default(), &CancellationToken::default())
}
fn iou(run: &RotoRun, k: u64) -> f64 {
    let m = run.mattes.frames[&k].matte.as_ref().expect("matte");
    let (mut inter, mut union) = (0u32, 0u32);
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let a = m.coverage.data[(y * W as i64 + x) as usize] >= 32768;
            let b = inside_fg(k, x, y);
            inter += u32::from(a && b);
            union += u32::from(a || b);
        }
    }
    f64::from(inter) / f64::from(union.max(1))
}

#[test]
fn roto_records_propagate_into_the_matte_pipeline() {
    bounded_case("roto_records_propagate_into_the_matte_pipeline", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let id1 = identity("SLYR", 1);
        let src = source(&document(), &records(json!({"roto":roto(3, vec![])}), FRAMES), &pixels).unwrap();
        // Not propagated yet: refused, never shown unmatted.
        let e = src.scene_at(0, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_ROTO_NOT_PROPAGATED, .. }), "{e:?}");
        let first = run(&src, &pixels, None).unwrap();
        let states: Vec<_> = first.receipt.frames.iter().map(|f| (f.frame, f.state)).collect();
        assert_eq!(states.len(), FRAMES as usize);
        for (k, s) in &states {
            assert_eq!(*s, if *k == 3 { RotoFrameState::Base } else { RotoFrameState::Propagated }, "frame {k}");
            let q = iou(&first, *k);
            println!("roto frame {k}: {} IoU {q:.4}", s.wire());
            assert!(q >= 0.85, "frame {k}: IoU {q}");
        }
        assert!(first.receipt.lost().is_empty());
        assert!(first.receipt.frames.iter().all(|f| f.engine == hsk_studio_folio::StudioRotoEngine::Classic));
        assert_eq!(first.receipt.engine_working_values, vec!["search_radius"]);
        // Deterministic (STU-CMP-075a) and cached.
        let second = run(&src, &pixels, None).unwrap();
        assert_eq!(second.mattes, first.mattes);
        assert_eq!(second.receipt.inputs_digest, first.receipt.inputs_digest);
        let cached = run(&src, &pixels, Some(&first.segmentation)).unwrap();
        assert!(cached.receipt.reused_cache && !first.receipt.reused_cache);
        assert_eq!(cached.mattes, first.mattes);
        // The matte enters the scene: alpha = source alpha * coverage.
        let shown = source(&document(), &records(json!({"roto":roto(3, vec![])}), FRAMES), &pixels)
            .unwrap()
            .with_roto(&id1, first.mattes.clone())
            .unwrap();
        for k in 0..FRAMES {
            let m = first.mattes.frames[&k].matte.as_ref().unwrap();
            let got = frame(&shown, &pixels, k);
            assert!(scenes::max_delta(&got, &scenes::roto_oracle(k, m)) <= 1e-6, "roto frame {k}");
            assert!(got.exactness.operators.iter().any(|o| o.key == "roto_matte"));
        }
        // The layer's frame past the source (outer frames 8..16) shows nothing (stretch rule).
        // Learned engine: typed refusal naming the model lane, no fallback (item 4).
        let mut r = roto(3, vec![]);
        r["engine"] = json!("learned");
        let learned = source(&document(), &records(json!({"roto":r}), FRAMES), &pixels).unwrap();
        match run(&learned, &pixels, None) {
            Err(TemporalError::Source { code, .. }) => assert_eq!(code, "model_unavailable:roto-learned-propagation"),
            other => panic!("{other:?}"),
        }
        // Admission before allocation (item 8).
        let tiny = RotoLimits { max_source_bytes: 100, ..RotoLimits::default() };
        let e = src.propagate_roto(&id1, &ctx(&pixels), None, &tiny, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Composite(CompositeError::BudgetExceeded)), "{e:?}");
        let mut few = RotoLimits::default();
        few.engine.max_frames = 2;
        match src.propagate_roto(&id1, &ctx(&pixels), None, &few, &cancel) {
            Err(TemporalError::Source { code, .. }) => assert_eq!(code, "budget_exceeded"),
            other => panic!("{other:?}"),
        }
        // A span past the source's frames is refused, not truncated.
        let mut r = roto(3, vec![]);
        r["spans"][0]["out_frame"] = json!(20);
        let long = source(&document(), &records(json!({"roto":r}), FRAMES), &pixels).unwrap();
        assert!(matches!(run(&long, &pixels, None), Err(TemporalError::InvalidState { field: "roto_spans", .. })));
        // A span that does not cover a frame leaves it unmatted (outside every span).
        let mut r = roto(3, vec![]);
        r["spans"][0]["out_frame"] = json!(5);
        let short = source(&document(), &records(json!({"roto":r.clone()}), FRAMES), &pixels).unwrap();
        let s = run(&short, &pixels, None).unwrap();
        assert_eq!(s.receipt.frames.len(), 5);
        let short = source(&document(), &records(json!({"roto":r}), FRAMES), &pixels).unwrap().with_roto(&id1, s.mattes).unwrap();
        assert!(matches!(short.scene_at(f(6) as i64, &cancel).unwrap().layers[0].content, SceneContent::Precomp(_)));
        assert!(matches!(short.scene_at(f(4) as i64, &cancel).unwrap().layers[0].content, SceneContent::Rotoscoped(_)));
        // Roto on a layer without frames is refused when the source is read.
        let mut rec = records(json!({}), FRAMES);
        rec["compositions"][1]["layers"][1]["roto"] = roto(3, vec![]);
        let e = source(&document(), &rec, &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_TIME_SOURCE, .. }), "{e:?}");
    });
}

#[test]
fn lost_frames_are_reported_and_corrective_strokes_recover_them() {
    bounded_case("lost_frames_are_reported_and_corrective_strokes_recover_them", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let id1 = identity("SLYR", 1);
        // The square leaves the source at frame 5.
        let rec = records(json!({"roto":roto(3, vec![])}), 5);
        let src = source(&document(), &rec, &pixels).unwrap();
        let r = run(&src, &pixels, None).unwrap();
        println!("lost frames {:?}", r.receipt.lost());
        assert_eq!(r.receipt.lost(), vec![5, 6, 7]);
        assert!(r.mattes.frames[&6].matte.is_none(), "never filled");
        let shown = source(&document(), &rec, &pixels).unwrap().with_roto(&id1, r.mattes.clone()).unwrap();
        match shown.scene_at(f(6) as i64, &cancel) {
            Err(TemporalError::Source { code, .. }) => assert_eq!(code, ROTO_FRAME_LOST),
            other => panic!("{other:?}"),
        }
        assert!(shown.scene_at(f(4) as i64, &cancel).is_ok());
        // A corrective foreground stroke on frame 6 recovers frame 6 and carries on (item 2).
        let fix = json!({"frame":6,"polarity":"foreground","diameter":2.0,"path":[[8.0, 6.0]]});
        let fixed = source(&document(), &records(json!({"roto":roto(3, vec![fix])}), 5), &pixels).unwrap();
        let r = run(&fixed, &pixels, None).unwrap();
        let six = r.receipt.frames.iter().find(|f| f.frame == 6).unwrap();
        assert_eq!((six.state, six.corrected), (RotoFrameState::Propagated, true));
        assert_eq!(r.receipt.frames.iter().find(|f| f.frame == 5).unwrap().state, RotoFrameState::Lost);
    });
}

#[test]
fn freeze_locks_segmentation_and_matte_controls_stay_live() {
    bounded_case("freeze_locks_segmentation_and_matte_controls_stay_live", || {
        let pixels = Pixels::fixture();
        let live = source(&document(), &records(json!({"roto":roto(3, vec![])}), FRAMES), &pixels).unwrap();
        let base = run(&live, &pixels, None).unwrap();
        // A later background stroke over the right half of the square on the base frame.
        let x = fg_x(3) as f64;
        let cut = json!({"frame":3,"polarity":"background","diameter":2.0,"path":[[x + 3.5, 4.5], [x + 3.5, 7.5]]});
        let mut frozen = roto(3, vec![cut.clone()]);
        frozen["freeze"] = json!({"stroke_count":3,"spans":[{"base_frame":3,"in_frame":0,"out_frame":FRAMES}]});
        let f_src = source(&document(), &records(json!({"roto":frozen.clone()}), FRAMES), &pixels).unwrap();
        let f_run = run(&f_src, &pixels, None).unwrap();
        assert!(f_run.receipt.frozen);
        assert_eq!(f_run.receipt.inputs_digest, base.receipt.inputs_digest, "the new stroke does not change a frozen result");
        assert_eq!(f_run.mattes, base.mattes);
        // Unfrozen: the stroke counts.
        let thawed = source(&document(), &records(json!({"roto":roto(3, vec![cut])}), FRAMES), &pixels).unwrap();
        let t_run = run(&thawed, &pixels, None).unwrap();
        assert_ne!(t_run.receipt.inputs_digest, base.receipt.inputs_digest);
        assert_ne!(t_run.mattes, base.mattes);
        // Matte controls stay live while frozen; the cache is reused. An unknown-bounded value
        // (chatter 150, STU-RAS-106) is used as given, never refused or clamped.
        frozen["parameters"] = json!({"fine_tune_matte":true,"matte_feather":1.0,"matte_reduce_chatter":150.0,"quality":"best"});
        let tuned = source(&document(), &records(json!({"roto":frozen}), FRAMES), &pixels).unwrap();
        let tuned_run = run(&tuned, &pixels, Some(&f_run.segmentation)).unwrap();
        assert!(tuned_run.receipt.reused_cache);
        assert_ne!(tuned_run.mattes, f_run.mattes);
        assert_eq!(tuned_run.receipt.not_modelled, Vec::<&str>::new(), "quality is a live row; frozen snapshot has standard");
    });
}

#[test]
fn refine_stage_consumes_the_edge_refinement_engine() {
    bounded_case("refine_stage_consumes_the_edge_refinement_engine", || {
        let pixels = Pixels::fixture();
        let x = fg_x(3) as f64;
        let edge = json!({"frame":3,"diameter":2.0,"path":[[x, 4.0], [x + 4.0, 4.0], [x + 4.0, 8.0], [x, 8.0], [x, 4.0]]});
        let mut r = roto(3, vec![]);
        r["edge_strokes"] = json!([edge]);
        r["parameters"] = json!({"refine_edge_base_radius":1.0,"fine_tune_refine_edge_matte":true,
            "refine":{"decontaminate_colours":true},"quality":"best","motion_damping":-3.0});
        let src = source(&document(), &records(json!({"roto":r}), FRAMES), &pixels).unwrap();
        let out = run(&src, &pixels, None).unwrap();
        assert!(out.receipt.frames.iter().all(|f| f.refined), "base radius refines every frame");
        assert!(out.mattes.frames[&3].matte.as_ref().unwrap().colours.is_some(), "decontaminated colours");
        assert_eq!(out.receipt.engine_working_values, vec!["search_radius", "refine/decontamination_amount"]);
        assert_eq!(out.receipt.not_modelled, vec!["quality", "motion_damping"]);
        for k in 0..FRAMES {
            assert!(iou(&out, k) >= 0.8, "refined frame {k}: {}", iou(&out, k));
        }
    });
}
