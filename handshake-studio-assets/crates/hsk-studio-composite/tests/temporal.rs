//! Temporal frame lowering (SMB-COMPOSITE-TEMPORAL slice 1). Expected placements, layer counts and
//! opacities are derived by hand from the `tiled` fixture (group 1 = raster 2 with tiles (0,0)+64x64
//! and (64,0)+64x32, then raster 3 with tile (0,64)+64x64), never produced by the code under test.
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_composite::temporal::{TRANSFORM_REQUIRES_RESAMPLING, TRANSLATE_NON_RASTER_LAYER};
use hsk_studio_composite::{
    BlendSpace, CompositeError, FrameLowering, FrameRequest, LayerFrameState, LayerTiming, Limits,
    NoTemporalSource, NodeAddr, RenderLowering, TemporalError, TemporalSource, TileInfo,
    TileInfoSource, evaluate, frame_outcome, lower, lower_frame, lower_to_render_plan,
};
use hsk_studio_folio::{Budget, Snapshot, TileRef};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

const T: i64 = TICKS_PER_SECOND as i64;
const FRAME_24: i64 = T / 24; // 10_584_000_000

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn plane_hash(object_key: &str, salt: u8) -> [u8; 32] {
    [object_key.bytes().fold(salt, |a, b| a.wrapping_mul(31).wrapping_add(b)); 32]
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: plane_hash(&tile.object_key, 1),
            alpha_sha256: plane_hash(&tile.object_key, 2),
        })
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        Some(plane_hash(&tiles.first()?.object_key, 3))
    }
}
fn tiled() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][1]["payload"] =
        json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v
}
fn with_mask(linked: bool) -> Value {
    let mut v = tiled();
    let mut m = tile(2, "m0", 0, 0, 32, 32);
    m["format"] = json!("coverage_u8");
    v["layers"][1]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,
        "clipped":false,"isolation":"isolated","mask":{"tiles":[m],"enabled":true,"linked":linked}});
    v
}
fn static_lowering(snap: &Snapshot) -> RenderLowering {
    let token = CancellationToken::default();
    let plan = lower(snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let report = evaluate(&plan, snap, &token).unwrap();
    lower_to_render_plan(&plan, &report, snap, &addr(1, "mix", "out"), target(), &Info).unwrap()
}
fn frame(
    snap: &Snapshot,
    timings: &[LayerTiming],
    source: &dyn TemporalSource,
    tick: i64,
) -> Result<FrameLowering, TemporalError> {
    let root = addr(1, "mix", "out");
    let req = FrameRequest {
        snapshot: snap,
        expected_revision: 7,
        blend_space: BlendSpace::Encoded,
        limits: Limits::default(),
        budget: Budget::default(),
        resolver: &Available,
        root: &root,
        target: target(),
        info: &Info,
        timings,
    };
    lower_frame(&req, source, tick, &CancellationToken::default())
}
/// Test double: one layer animated by a function of the tick.
struct Animated<F: Fn(i64) -> LayerFrameState> {
    layer: String,
    at: F,
}
impl<F: Fn(i64) -> LayerFrameState> TemporalSource for Animated<F> {
    fn layer_state(
        &self,
        layer_id: &str,
        tick: i64,
        _: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError> {
        Ok((layer_id == self.layer).then(|| (self.at)(tick)))
    }
}
fn animated(n: u8, at: impl Fn(i64) -> LayerFrameState) -> Animated<impl Fn(i64) -> LayerFrameState> {
    Animated { layer: identity("SLYR", n), at }
}
fn moved(dx: f64, dy: f64) -> LayerFrameState {
    LayerFrameState { translate_px: [dx, dy], ..LayerFrameState::IDENTITY }
}
fn root_group(low: &RenderLowering) -> &hsk_studio_pigment::render_plan::Group {
    match &low.plan.nodes[low.plan.root.0 as usize] {
        PlanNode::Group(g) => g,
        other => panic!("root is not a group: {other:?}"),
    }
}
fn tile_placements(low: &RenderLowering) -> Vec<(String, Rect)> {
    let keys: std::collections::BTreeMap<_, _> =
        low.sources.iter().map(|(k, t)| (*k, t.object_key.clone())).collect();
    low.plan
        .nodes
        .iter()
        .filter_map(|n| match n {
            PlanNode::Tile(t) => Some((keys[&t.key].clone(), t.placement)),
            _ => None,
        })
        .collect()
}
fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

#[test]
fn frame_at_ticks_inside_and_outside_layer_windows() {
    bounded_case("frame_at_ticks_inside_and_outside_layer_windows", || {
        let snap = open(&tiled());
        // Layer 3 shown for frames 1..2 at 24 fps: [10584000000, 21168000000).
        let timings = [LayerTiming { layer_id: identity("SLYR", 3), in_tick: FRAME_24, out_tick: 2 * FRAME_24 }];
        let at = |tick| frame(&snap, &timings, &NoTemporalSource, tick).unwrap();

        let before = at(0);
        assert_eq!(root_group(&before.lowering).layers.len(), 1);
        let keys: Vec<&str> = before.lowering.sources.iter().map(|(_, t)| t.object_key.as_str()).collect();
        assert_eq!(keys, ["t00", "t10"]);
        assert_eq!(before.hidden, [identity("SLYR", 3)]);

        // Inside the window nothing changes: the frame lowering IS the static lowering.
        let inside = at(FRAME_24);
        assert_eq!(root_group(&inside.lowering).layers.len(), 2);
        assert!(inside.hidden.is_empty() && inside.applied.is_empty());
        assert_eq!(inside.lowering, static_lowering(&snap));
        assert_eq!(at(2 * FRAME_24 - 1).lowering, inside.lowering);

        // The out tick is exclusive.
        let after = at(2 * FRAME_24);
        assert_eq!(root_group(&after.lowering).layers.len(), 1);
        assert_eq!(after.lowering.plan.digest().unwrap(), before.lowering.plan.digest().unwrap());
        assert_eq!(after.tick, 2 * FRAME_24);

        // A source can hide a layer for one frame; it never shows an authored-hidden layer.
        let hide = animated(3, |t| LayerFrameState { visible: t != 5, ..LayerFrameState::IDENTITY });
        assert_eq!(root_group(&frame(&snap, &[], &hide, 5).unwrap().lowering).layers.len(), 1);
        assert_eq!(root_group(&frame(&snap, &[], &hide, 6).unwrap().lowering).layers.len(), 2);
        let mut authored_hidden = tiled();
        authored_hidden["layers"][2]["visible"] = json!(false);
        let snap_hidden = open(&authored_hidden);
        assert_eq!(root_group(&frame(&snap_hidden, &[], &hide, 6).unwrap().lowering).layers.len(), 1);
    });
}

#[test]
fn opacity_scale_applies_per_tick() {
    bounded_case("opacity_scale_applies_per_tick", || {
        // Layer 2 authored at 50 percent; the source scales it by 0.5 at tick T, by 1 elsewhere.
        let mut v = tiled();
        v["layers"][1]["appearance"] = json!({"opacity":50.0,"fill_opacity":100.0,"blend_mode":2,
            "clipped":false,"isolation":"isolated"});
        let snap = open(&v);
        let fade = animated(2, |t| LayerFrameState {
            opacity_scale: if t == T { 0.5 } else { 1.0 },
            ..LayerFrameState::IDENTITY
        });
        let at_t = frame(&snap, &[], &fade, T).unwrap();
        assert_eq!(root_group(&at_t.lowering).layers[0].opacity, 0.25);
        assert_eq!(root_group(&at_t.lowering).layers[1].opacity, 1.0);
        assert_eq!(at_t.applied.len(), 1);
        assert_eq!(root_group(&frame(&snap, &[], &fade, 0).unwrap().lowering).layers[0].opacity, 0.5);

        #[cfg(feature = "motion")]
        {
            use hsk_studio_accord::Ticks;
            use hsk_studio_composite::temporal::motion::{LayerProperties, MotionTemporalSource};
            use hsk_studio_motion::{Interpolation, Keyframe, Property};
            // Linear opacity keys 0 percent at 0 -> 100 percent at 1 s; layer 2 authored at 50.
            let linear = Property::new(
                "opacity",
                vec![100.0],
                vec![Keyframe::new(Ticks::new(0), vec![0.0]), Keyframe::new(Ticks::new(T as u64), vec![100.0])],
            )
            .unwrap();
            let src = MotionTemporalSource::new()
                .with_layer(&identity("SLYR", 2), LayerProperties { opacity: Some(linear), ..Default::default() });
            let opacity = |tick| root_group(&frame(&snap, &[], &src, tick).unwrap().lowering).layers[0].opacity;
            assert_eq!(opacity(T / 2), 0.25); // 50 % authored x 0.5
            assert_eq!(opacity(T), 0.5);
            // A hold key stays constant until the next key: 20 % from 0 until 1 s, then 80 %.
            let hold = Property::new(
                "opacity",
                vec![100.0],
                vec![
                    Keyframe::new(Ticks::new(0), vec![20.0]).with_interpolation(Interpolation::Hold, Interpolation::Hold),
                    Keyframe::new(Ticks::new(T as u64), vec![80.0]),
                ],
            )
            .unwrap();
            let src = MotionTemporalSource::new()
                .with_layer(&identity("SLYR", 2), LayerProperties { opacity: Some(hold), ..Default::default() });
            let opacity = |tick| root_group(&frame(&snap, &[], &src, tick).unwrap().lowering).layers[0].opacity;
            assert_eq!(opacity(T - 1), 0.1); // 50 % x 0.2
            assert_eq!(opacity(T), 0.4); // 50 % x 0.8
        }
    });
}

#[test]
fn integer_translation_moves_tiles_and_linked_masks_exactly() {
    bounded_case("integer_translation_moves_tiles_and_linked_masks_exactly", || {
        let shift = animated(2, |_| moved(3.0, -2.0));
        for linked in [true, false] {
            let snap = open(&with_mask(linked));
            let before = static_lowering(&snap);
            let low = frame(&snap, &[], &shift, 0).unwrap().lowering;
            assert_eq!(
                tile_placements(&low),
                [
                    ("t00".to_owned(), rect(3, -2, 64, 64)),
                    ("t10".to_owned(), rect(67, -2, 64, 32)),
                    ("t01".to_owned(), rect(0, 64, 64, 64)),
                ]
            );
            let mask = root_group(&low).layers[0].mask.expect("mask lowered");
            let expected = if linked { rect(3, -2, 32, 32) } else { rect(0, 0, 32, 32) };
            assert_eq!(mask.placement, expected, "linked={linked}");
            // Nothing else changed: same sources, masks, ledger; the static path is untouched.
            assert_eq!(low.sources, before.sources);
            assert_eq!(low.mask_sources, before.mask_sources);
            assert_eq!(low.exactness, before.exactness);
            assert_eq!(static_lowering(&snap), before);
            // A zero translation is the static plan, byte for byte.
            let zero = animated(2, |_| LayerFrameState::IDENTITY);
            assert_eq!(frame(&snap, &[], &zero, 0).unwrap().lowering, before);
        }
    });
}

#[test]
fn resampling_and_hostile_inputs_are_refused_typed() {
    bounded_case("resampling_and_hostile_inputs_are_refused_typed", || {
        let snap = open(&tiled());
        let unsupported = |s: LayerFrameState, n: u8| frame(&snap, &[], &animated(n, move |_| s), 0);
        let resampling = Err(TemporalError::Unsupported {
            layer_id: identity("SLYR", 2),
            feature: TRANSFORM_REQUIRES_RESAMPLING,
        });
        // Fractional translation, scale and rotation need resampling: refused, never rounded.
        let half = unsupported(moved(0.5, 0.0), 2);
        assert_eq!(half, resampling);
        assert_eq!(frame_outcome(&half), Outcome::Failure(FailureCode::Unsupported));
        assert_eq!(unsupported(LayerFrameState { scale: [2.0, 1.0], ..LayerFrameState::IDENTITY }, 2), resampling);
        assert_eq!(unsupported(LayerFrameState { rotation_deg: 90.0, ..LayerFrameState::IDENTITY }, 2), resampling);
        // A group's members are not addressed by its id: a non-zero translation is refused.
        assert_eq!(
            unsupported(moved(1.0, 0.0), 1),
            Err(TemporalError::Unsupported { layer_id: identity("SLYR", 1), feature: TRANSLATE_NON_RASTER_LAYER })
        );
        // Non-finite and out-of-range state.
        let invalid = |field| Err(TemporalError::InvalidState { layer_id: identity("SLYR", 2), field });
        for scale in [f64::NAN, 1.5, -0.1] {
            assert_eq!(unsupported(LayerFrameState { opacity_scale: scale, ..LayerFrameState::IDENTITY }, 2), invalid("opacity_scale"));
        }
        assert_eq!(unsupported(moved(f64::INFINITY, 0.0), 2), invalid("translate_px"));
        assert_eq!(unsupported(LayerFrameState { rotation_deg: f64::NAN, ..LayerFrameState::IDENTITY }, 2), invalid("rotation_deg"));

        // dims_u32_max-style: a translation that pushes x past i64::MAX is Overflow (checked add).
        // Layer 3's tile at column 16 sits at x = 1024; 1024 + (2^63 - 1024) = 2^63 > i64::MAX.
        let mut far = tiled();
        far["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",16,1,64,64)]});
        let far = open(&far);
        let overflow = frame(&far, &[], &animated(3, |_| moved(9_223_372_036_854_774_784.0, 0.0)), 0);
        assert_eq!(overflow, Err(TemporalError::Composite(CompositeError::Overflow)));
        assert_eq!(unsupported(moved(9_223_372_036_854_775_808.0, 0.0), 2), Err(TemporalError::Composite(CompositeError::Overflow)));

        // aggregate_over_budget-style: more timings than Limits.max_steps refuse before any work.
        let window = |n: u8, a: i64, b: i64| LayerTiming { layer_id: identity("SLYR", n), in_tick: a, out_tick: b };
        let many = vec![window(2, 0, 1); Limits::default().max_steps + 1];
        assert_eq!(
            frame(&snap, &many, &NoTemporalSource, 0),
            Err(TemporalError::Composite(CompositeError::BudgetExceeded))
        );
        assert_eq!(
            frame(&snap, &[window(2, 5, 5)], &NoTemporalSource, 0),
            Err(TemporalError::InvalidTiming { layer_id: identity("SLYR", 2) })
        );
        assert_eq!(
            frame(&snap, &[window(9, 0, 1)], &NoTemporalSource, 0),
            Err(TemporalError::Composite(CompositeError::DanglingInput(identity("SLYR", 9))))
        );
        assert_eq!(
            frame(&snap, &[window(2, 0, 1), window(2, 1, 2)], &NoTemporalSource, 0),
            Err(TemporalError::DuplicateTiming { layer_id: identity("SLYR", 2) })
        );
        // Stale revision and cancellation.
        let root: NodeAddr = addr(1, "mix", "out");
        let req = FrameRequest {
            snapshot: &snap,
            expected_revision: 6,
            blend_space: BlendSpace::Encoded,
            limits: Limits::default(),
            budget: Budget::default(),
            resolver: &Available,
            root: &root,
            target: target(),
            info: &Info,
            timings: &[],
        };
        assert_eq!(
            lower_frame(&req, &NoTemporalSource, 0, &CancellationToken::default()),
            Err(TemporalError::Composite(CompositeError::StaleRevision { expected: 6, actual: 7 }))
        );
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(
            lower_frame(&FrameRequest { expected_revision: 7, ..req }, &NoTemporalSource, 0, &canceled),
            Err(TemporalError::Composite(CompositeError::Canceled))
        );
    });
}

#[test]
fn frames_are_identical_across_thread_counts() {
    bounded_case("frames_are_identical_across_thread_counts", || {
        let ticks = [0, FRAME_24, 2 * FRAME_24, T];
        let run = || -> Vec<Vec<u8>> {
            let snap = open(&with_mask(true));
            let timings = [LayerTiming { layer_id: identity("SLYR", 3), in_tick: FRAME_24, out_tick: 2 * FRAME_24 }];
            let src = animated(2, |t| LayerFrameState {
                opacity_scale: if t >= FRAME_24 { 0.75 } else { 1.0 },
                translate_px: [(t / FRAME_24) as f64, 0.0],
                ..LayerFrameState::IDENTITY
            });
            ticks
                .iter()
                .map(|&t| {
                    let f = frame(&snap, &timings, &src, t).unwrap();
                    format!("{:?}", (f.lowering.plan.digest().unwrap(), &f.lowering.sources, f.applied, f.hidden)).into_bytes()
                })
                .collect()
        };
        let reference = run();
        // The four frames differ from each other (the animation is real).
        for (i, a) in reference.iter().enumerate() {
            for b in &reference[i + 1..] {
                assert_ne!(a, b);
            }
        }
        for threads in [1usize, 2, 8] {
            let results: Vec<Vec<Vec<u8>>> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..threads).map(|_| s.spawn(run)).collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for r in results {
                assert_eq!(r, reference, "threads={threads}");
            }
        }
    });
}
