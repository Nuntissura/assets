//! Lane SMB-PR-RETIME-OUTPUT-1 (feature `viewer`; the real decoder case `media`): the program monitor
//! renders the clip's interpolation choice ([STU-VID-031]) on the CPU path. Oracles: hand values over
//! solid source frames, a moving bar measured here (position and peak, not a byte hash), and typed
//! outcomes where a path cannot render the choice. Vendor parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_harness_view::view::monitor::{FrameSource, MonitorFail, ProgramMonitor, SourceFrame};
use hsk_studio_pulse::{DropPolicy, FrameInterpolation, FrameLayer, HeadlessClock, SourceExtents, CROSS_DISSOLVE, FRAME_INTERPOLATION_UNAVAILABLE};
use serde_json::Value;
use std::cell::RefCell;
use std::sync::Arc;

use FrameInterpolation::{FrameBlending as Blend, FrameSampling as Sample, OpticalFlow as Flow};

/// 25 fps ticks per frame; at 48 kHz 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: u64 = 1920;
const SIDE: u32 = 8;

fn plane(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Arc<SourceFrame> {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for y in 0..height {
        for x in 0..width {
            let p = pixel(x, y);
            for c in &p[..3] {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&p[3].to_le_bytes());
        }
    }
    Arc::new(SourceFrame {
        width,
        height,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    })
}
/// The grey of source frame `f`.
fn grey(f: u64) -> f32 {
    0.1 + 0.02 * f as f32
}
/// 40 solid frames, each its own grey; past them a typed refusal of the source. Records the ticks asked.
#[derive(Default)]
struct Numbered {
    asked: RefCell<Vec<u64>>,
}
impl FrameSource for Numbered {
    fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
        self.asked.borrow_mut().push(tick.value());
        let f = tick.value() / F;
        (f < 40).then(|| plane(SIDE, SIDE, |_, _| [grey(f), grey(f), grey(f), 1.0]))
    }
}
struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(40 * F))
    }
}
fn media(name: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: name.into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
        asset_id: None,
    }
}
fn clip(n: u32, source: ClipSource, frames: u64, start: u64, speed: (i64, u64), method: FrameInterpolation) -> StudioClip {
    let duration = u128::from(frames * F) * u128::from(speed.1);
    let duration = u64::try_from(duration.div_ceil(u128::from(speed.0.unsigned_abs()))).unwrap();
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source,
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(duration),
        speed: Speed { numerator: speed.0, denominator: speed.1 },
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime { frame_interpolation: method, ..ClipRetime::default() },
    }
}
fn sequence(id: &str, clips: Vec<StudioClip>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            volume: 1.0,
            pan: 0.0,
            clips,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
fn monitor(seq: StudioSequence, side: u32) -> ProgramMonitor {
    let mut m = ProgramMonitor::new(seq, side, side, DropPolicy::DropLate).unwrap();
    m.set_prefetch_ahead(0).unwrap();
    m
}
fn render(m: &ProgramMonitor, frame: u64, src: &dyn FrameSource) -> Result<hsk_studio_composite::RenderedScene, MonitorFail> {
    m.render_at(Ticks::new(frame * F), src, &Known, &CancellationToken::default())
}

#[test]
fn table_of_every_choice_through_the_monitor() {
    // (row, speed, method, frame, expected grey, tolerance)
    let mix = |a: u64, b: u64, w: f32| grey(a) * (1.0 - w) + grey(b) * w;
    let rows: Vec<(&str, (i64, u64), FrameInterpolation, u64, f32, f32)> = vec![
        ("sampling 3/4 at 0.75 shows frame 1", (3, 4), Sample, 1, grey(1), 1e-5),
        ("sampling 1/4 at 0.25 shows frame 0", (1, 4), Sample, 1, grey(0), 1e-5),
        ("sampling 1/2 at 0.5 shows frame 1", (1, 2), Sample, 1, grey(1), 1e-5),
        ("blending 3/4 at 0.75", (3, 4), Blend, 1, mix(0, 1, 0.75), 1e-5),
        ("blending 1/4 at 0.25", (1, 4), Blend, 1, mix(0, 1, 0.25), 1e-5),
        ("blending 3/2 at 1.5", (3, 2), Blend, 1, mix(1, 2, 0.5), 1e-5),
        ("blending on a frame is that frame", (1, 2), Blend, 2, grey(1), 1e-5),
        ("flow 3/4 over flat frames is their mix", (3, 4), Flow, 1, mix(0, 1, 0.75), 1e-3),
        ("reverse head shows the last frame", (-1, 1), Sample, 0, grey(39), 1e-5),
        ("reverse unit frame 3 shows frame 36", (-1, 1), Sample, 3, grey(36), 1e-5),
        ("reverse blending 1/2 at 38.5", (-1, 2), Blend, 1, mix(38, 39, 0.5), 1e-5),
        ("reverse flow 3/4 at 38.25", (-3, 4), Flow, 1, mix(38, 39, 0.25), 1e-3),
        ("unit sampling", (1, 1), Sample, 3, grey(3), 1e-5),
        ("unit blending on a frame", (1, 1), Blend, 3, grey(3), 1e-5),
        ("unit flow on a frame", (1, 1), Flow, 3, grey(3), 1e-5),
    ];
    let mut wrong = Vec::new();
    for (row, speed, method, frame, want, tolerance) in &rows {
        let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, *speed, *method)]), SIDE);
        let seen = match render(&m, *frame, &Numbered::default()) {
            Ok(image) => {
                let p = image.pixel(3, 3).unwrap();
                println!("TABLE {row}: {:.5} expected {want:.5}", p[0]);
                ((p[0] - want).abs() > *tolerance).then(|| format!("{row}: {:.5} expected {want:.5}", p[0]))
            }
            Err(e) => {
                println!("TABLE {row}: {} {}", e.code, e.detail);
                Some(format!("{row}: {} {}, expected {want:.5}", e.code, e.detail))
            }
        };
        wrong.extend(seen);
    }
    assert!(wrong.is_empty(), "{} of {} rows wrong:\n{}", wrong.len(), rows.len(), wrong.join("\n"));
}

#[test]
fn a_pair_decodes_both_frames_and_a_single_answer_one() {
    let src = Numbered::default();
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Blend)]), SIDE);
    render(&m, 1, &src).unwrap();
    assert_eq!(*src.asked.borrow(), [0, F]);
    let src = Numbered::default();
    render(&m, 4, &src).unwrap();
    assert_eq!(*src.asked.borrow(), [3 * F], "position 3 exactly: one frame, no blend");
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Sample)]), SIDE);
    let src = Numbered::default();
    render(&m, 1, &src).unwrap();
    assert_eq!(*src.asked.borrow(), [F]);
}

/// A white bar 4 pixels wide at `x` on black, in a 32 x 8 frame stack; `occluder`: a grey post that
/// stays put at columns 22..24, in front of the bar.
struct Bar {
    step: u32,
    occluder: bool,
}
impl FrameSource for Bar {
    fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
        let f = u32::try_from(tick.value() / F).ok()?;
        let x0 = 6 + f * self.step;
        let occluder = self.occluder;
        Some(plane(32, 32, move |x, _| {
            let v = if occluder && (22..24).contains(&x) {
                0.5
            } else if (x0..x0 + 4).contains(&x) {
                1.0
            } else {
                0.0
            };
            [v, v, v, 1.0]
        }))
    }
}
/// Mean luminance of each column of the monitor's middle rows.
fn columns(image: &hsk_studio_composite::RenderedScene) -> Vec<f32> {
    (0..image.width).map(|x| (12..20).map(|y| image.pixel(x, y).unwrap()[0]).sum::<f32>() / 8.0).collect()
}
fn centroid(c: &[f32]) -> f32 {
    let total: f32 = c.iter().sum();
    c.iter().enumerate().map(|(i, v)| i as f32 * v).sum::<f32>() / total
}

#[test]
fn optical_flow_moves_the_edge_where_blending_ghosts_it() {
    let bar = Bar { step: 6, occluder: false };
    // Half speed, frame 1: the bar sits at x 6 in source frame 0 and x 12 in source frame 1, so the
    // frame half way has it at x 9 (columns 9..13). The two positions do not overlap, so the estimate
    // leaves faint residue behind (pigment module docs: no occlusion reasoning); the proof is where
    // the brightest columns are: the frame blend has nothing there.
    let flow = monitor(sequence("s", vec![clip(1, media("a"), 8, 0, (1, 2), Flow)]), 32);
    let image = render(&flow, 1, &bar).unwrap();
    let c = columns(&image);
    let (at, peak) = (centroid(&c), c.iter().copied().fold(0.0, f32::max));
    println!("FLOW centroid {at:.3}, peak {peak:.3}, columns {c:.2?}");
    assert!((at - 10.5).abs() < 1.5, "the bar is near half way: {at}");
    assert!(peak > 0.75, "a bright bar, not two half ghosts: {peak}");
    let bright: Vec<usize> = (0..c.len()).filter(|x| c[*x] > 0.75).collect();
    assert!(!bright.is_empty() && bright.iter().all(|x| (9..13).contains(x)), "bright columns {bright:?} lie in the bar half way");
    assert!(image.exactness.operators.iter().any(|o| o.key == "frame_blend"), "the estimator ran");
    // The same frames blended: two half-bright bars at the source positions, nothing in between.
    let blend = monitor(sequence("s", vec![clip(1, media("a"), 8, 0, (1, 2), Blend)]), 32);
    let c = columns(&render(&blend, 1, &bar).unwrap());
    println!("BLEND columns {c:.2?}");
    assert!((c[7] - 0.5).abs() < 1e-4 && (c[13] - 0.5).abs() < 1e-4, "{c:?}");
    assert!(c[10] < 1e-4, "no bar between: {c:?}");
    // A quarter of the way: the position follows the weight (speed 1/4, frame 1): centre 9.
    let quarter = monitor(sequence("s", vec![clip(1, media("a"), 8, 0, (1, 4), Flow)]), 32);
    let early = centroid(&columns(&render(&quarter, 1, &bar).unwrap()));
    println!("FLOW quarter centroid {early:.3}");
    assert!((early - 9.0).abs() < 1.5 && early < at, "{early} against {at}");
}

#[test]
fn optical_flow_over_an_occlusion_is_an_estimation_and_says_so() {
    // The bar passes behind a post. The estimator has no occlusion reasoning (pigment module docs),
    // so it paints the bar over the post, the artefact [STU-VID-031] names: not a source frame and
    // not their mix. The frame says so through the mark.
    let bar = Bar { step: 6, occluder: true };
    let mut m = monitor(sequence("s", vec![clip(1, media("a"), 8, 0, (1, 2), Flow)]), 32);
    let clock = HeadlessClock::new(48_000);
    m.scrub(Ticks::new(5 * F), &clock).unwrap();
    m.refresh(&clock, &bar, &Known, &CancellationToken::default()).unwrap();
    let c = columns(&m.shown().expect("a frame").image);
    println!("OCCLUSION columns {c:.2?}");
    let mix = monitor(sequence("s", vec![clip(1, media("a"), 8, 0, (1, 2), Blend)]), 32);
    let mixed = columns(&render(&mix, 5, &bar).unwrap());
    println!("OCCLUSION mixed {mixed:.2?}");
    assert!((mixed[22] - 0.5).abs() < 1e-4, "both source frames hold the post: {mixed:?}");
    assert!(c.iter().zip(&mixed).any(|(a, b)| (a - b).abs() > 0.1), "the estimate is not the mix: {c:?}");
    let state = m.state_json();
    let layer = &state["shown_layers"][0];
    assert_eq!(layer["estimation_mark"], "estimation", "{state}");
    assert_eq!(layer["answer"], "estimated", "{state}");
    assert_eq!((layer["from"].as_u64(), layer["to"].as_u64(), layer["weight"].as_str()), (Some(2), Some(3), Some("1/2")), "{state}");
}

#[test]
fn the_report_names_each_answer() {
    let clock = HeadlessClock::new(48_000);
    let cancel = CancellationToken::default();
    for (method, answer, marked) in [(Sample, "sampled", false), (Blend, "blended", false), (Flow, "estimated", true)] {
        let mut m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), method)]), SIDE);
        m.scrub(Ticks::new(F), &clock).unwrap();
        m.refresh(&clock, &Numbered::default(), &Known, &cancel).unwrap();
        let shown = m.shown().expect("a frame");
        assert_eq!(shown.layers.len(), 1);
        assert_eq!(shown.layers[0].estimation_mark.is_some(), marked);
        let state = m.state_json();
        assert_eq!(state["shown_layers"][0]["answer"], answer, "{state}");
        assert_eq!(state["shown_layers"][0]["estimation_mark"].is_string(), marked, "{state}");
    }
    // A hold is one real source frame, never marked.
    let mut held = clip(1, media("a"), 40, 0, (3, 4), Flow);
    held.retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(7 * F), anchor: FrameHoldAnchor::InPoint });
    let mut m = monitor(sequence("s", vec![held]), SIDE);
    m.scrub(Ticks::new(F), &clock).unwrap();
    m.refresh(&clock, &Numbered::default(), &Known, &cancel).unwrap();
    assert!((m.shown().unwrap().image.pixel(1, 1).unwrap()[0] - grey(7)).abs() < 1e-5);
    let state = m.state_json();
    assert_eq!(state["shown_layers"][0]["answer"], "held", "{state}");
    assert_eq!(state["shown_layers"][0]["estimation_mark"], Value::Null, "{state}");
}

/// Frames up to `last` only; the refusal is typed by the source.
struct Until(u64);
impl FrameSource for Until {
    fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
        let f = tick.value() / F;
        (f <= self.0).then(|| plane(SIDE, SIDE, |_, _| [grey(f), grey(f), grey(f), 1.0]))
    }
}
#[test]
fn a_path_that_cannot_render_the_choice_answers_typed_never_a_floored_frame() {
    // The second frame is missing: the source's refusal, not the first frame alone.
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Blend)]), SIDE);
    assert_eq!(render(&m, 1, &Until(0)).unwrap_err().code, "frame_unavailable");
    // Samples out of the estimator's domain: typed.
    struct Hot;
    impl FrameSource for Hot {
        fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
            Some(plane(SIDE, SIDE, |_, _| [2.0, 2.0, 2.0, 1.0]))
        }
    }
    for method in [Blend, Flow] {
        let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), method)]), SIDE);
        let e = render(&m, 1, &Hot).unwrap_err();
        assert_eq!(e.code, FRAME_INTERPOLATION_UNAVAILABLE, "{method:?}: {}", e.detail);
    }
    // A nested clip that needs two inner frames: typed, under its own code.
    let inner = sequence("seq-b", vec![clip(2, media("a"), 40, 0, (1, 1), Sample)]);
    let outer = sequence("seq-a", vec![clip(1, ClipSource::Sequence { sequence_id: "seq-b".into() }, 40, 0, (3, 4), Blend)]);
    let mut m = monitor(outer.clone(), SIDE);
    m.set_document(Some(1), vec![inner, outer]);
    let e = render(&m, 1, &Numbered::default()).unwrap_err();
    assert_eq!(e.code, FRAME_INTERPOLATION_UNAVAILABLE, "{}", e.detail);
    assert!(e.detail.contains("SCLP-"), "{}", e.detail);
    // Cancellation stays a cancellation.
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Flow)]), SIDE);
    let stopped = CancellationToken::default();
    stopped.cancel();
    let e = m.render_at(Ticks::new(F), &Numbered::default(), &Known, &stopped).unwrap_err();
    assert_ne!(e.code, FRAME_INTERPOLATION_UNAVAILABLE, "{}", e.detail);
}

#[test]
fn optical_flow_over_the_work_budget_is_refused_before_the_estimate_and_a_mix_is_not() {
    // Pigment's budget: 2e10 sample visits. One estimate pair at FlowParams::DEFAULT charges
    // 24192 per pixel, so a raster above 826719 pixels is refused; 920 x 920 is 846400.
    struct Big;
    impl FrameSource for Big {
        fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
            let g = grey(tick.value() / F);
            Some(plane(SIDE, SIDE, move |_, _| [g, g, g, 1.0]))
        }
    }
    let start = std::time::Instant::now();
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Flow)]), 920);
    let e = render(&m, 1, &Big).unwrap_err();
    println!("OVER BUDGET {} {} after {:?}", e.code, e.detail, start.elapsed());
    assert_eq!(e.code, FRAME_INTERPOLATION_UNAVAILABLE, "{}", e.detail);
    // The same raster blends: the mix has no flow budget.
    let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), Blend)]), 920);
    let p = render(&m, 1, &Big).unwrap().pixel(460, 460).unwrap();
    assert!((p[0] - (grey(0) * 0.25 + grey(1) * 0.75)).abs() < 1e-3, "{p:?}");
}

#[test]
fn frames_of_another_size_fit_then_blend_near_the_top_of_the_domain() {
    // 4 x 2 frames on an 8 x 8 raster: letterboxed, bilinear, samples at 1.0 stay in the domain.
    struct Small;
    impl FrameSource for Small {
        fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
            let v = if tick.value() / F == 0 { 1.0 } else { 0.999_999_9 };
            Some(plane(4, 2, move |_, _| [v, v, v, 1.0]))
        }
    }
    for method in [Blend, Flow] {
        let m = monitor(sequence("s", vec![clip(1, media("a"), 40, 0, (3, 4), method)]), SIDE);
        let image = render(&m, 1, &Small).unwrap();
        // Fitted: 8 x 4 in the middle, black above it.
        assert!(image.pixel(4, 4).unwrap()[0] > 0.99, "{:?}", image.pixel(4, 4));
        assert!(image.pixel(4, 0).unwrap()[0] < 0.01, "{:?}", image.pixel(4, 0));
    }
}

#[test]
fn a_dissolve_blends_each_clip_by_its_own_choice() {
    // Outgoing: 1/2 speed blending, source 0..10 over 20 frames, cut at 20, dissolve 16..24.
    // Incoming: unit sampling. Frame 17 is progress 1/8.
    let mut out = clip(1, media("a"), 10, 0, (1, 2), Blend);
    out.transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "tr-1".into(),
        transition_kind: CROSS_DISSOLVE.into(),
        duration: Ticks::new(8 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    });
    let mut inc = clip(2, media("b"), 40, 20, (1, 1), Sample);
    inc.source_in = Ticks::new(10 * F);
    inc.source_out = Ticks::new(30 * F);
    inc.duration = Ticks::new(20 * F);
    let m = monitor(sequence("s", vec![out, inc]), SIDE);
    let src = Numbered::default();
    let image = render(&m, 17, &src).unwrap();
    // Outgoing at 8.5: 0.5 * 8 + 0.5 * 9. Incoming head handle 3 frames early, at source 7.
    let outgoing = 0.5 * grey(8) + 0.5 * grey(9);
    let want = 0.875 * outgoing + 0.125 * grey(7);
    let p = image.pixel(3, 3).unwrap()[0];
    assert!((p - want).abs() < 1e-5, "{p} against {want}, asked {:?}", src.asked.borrow());
}

/// States a 24 fps grid; frames by that grid.
struct Film(RefCell<Vec<FrameLayerSeen>>);
struct FrameLayerSeen(u64, Option<u64>);
const T24: u64 = 10_584_000_000;
impl FrameSource for Film {
    fn frame(&self, _: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
        let f = tick.value() / T24;
        Some(plane(SIDE, SIDE, move |_, _| [grey(f), grey(f), grey(f), 1.0]))
    }
    fn source_grid(&self, _: &ClipSource) -> Option<u64> {
        Some(T24)
    }
    fn prefetch(&self, layers: &[FrameLayer], _: &CancellationToken) -> u64 {
        for l in layers {
            self.0.borrow_mut().push(FrameLayerSeen(l.source_tick.value(), l.second_source_tick().map(Ticks::value)));
        }
        0
    }
}
#[test]
fn the_grid_comes_from_the_frame_source_and_reaches_prefetch() {
    let cancel = CancellationToken::default();
    let clock = HeadlessClock::new(48_000);
    let src = Film(RefCell::new(Vec::new()));
    let mut m = ProgramMonitor::new(sequence("s", vec![clip(1, media("a"), 40, 0, (1, 2), Blend)]), SIDE, SIDE, DropPolicy::DropLate).unwrap();
    m.set_prefetch_ahead(2).unwrap();
    m.scrub(Ticks::new(2 * F), &clock).unwrap();
    m.refresh(&clock, &src, &Known, &cancel).unwrap();
    // Position F = 0.96 of a 24 fps frame.
    let p = m.shown().expect("a frame").image.pixel(1, 1).unwrap()[0];
    let want = grey(0) * (1.0 - 0.96) + grey(1) * 0.96;
    assert!((p - want).abs() < 1e-4, "{p} against {want}");
    assert_eq!(m.state_json()["shown_layers"][0]["weight"], "24/25");
    // The next frames were named with both of their ticks. Frame 3 is position 1.5 F = 1.44 frames.
    let seen = src.0.borrow();
    assert_eq!((seen[0].0, seen[0].1), (T24, Some(2 * T24)), "the frame after the one shown");
}

fn frame_policy_run(policy: DropPolicy) -> (Vec<u64>, u64, u64) {
    let cancel = CancellationToken::default();
    let mut clock = HeadlessClock::new(48_000);
    let bar = Bar { step: 1, occluder: false };
    let mut m = ProgramMonitor::new(sequence("s", vec![clip(1, media("a"), 40, 0, (1, 2), Flow)]), 32, 32, policy).unwrap();
    m.set_prefetch_ahead(0).unwrap();
    m.play(&clock).unwrap();
    let mut shown = Vec::new();
    // Each render takes as long as 3 frames: the clock moves on between refreshes.
    for _ in 0..4 {
        if let Some(f) = m.refresh(&clock, &bar, &Known, &cancel).unwrap() {
            shown.push(f);
        }
        clock.advance(3 * SPF);
    }
    let stats = m.scheduler().stats();
    (shown, stats.dropped, stats.max_lag_ticks)
}
#[test]
fn a_late_estimate_follows_the_existing_drop_policy() {
    let (late, dropped, _) = frame_policy_run(DropPolicy::DropLate);
    println!("DROP_LATE shown {late:?} dropped {dropped}");
    assert_eq!(late, [0, 3, 6, 9], "the due frame, estimated");
    assert_eq!(dropped, 6, "two frames dropped at each step");
    let (every, dropped, lag) = frame_policy_run(DropPolicy::PresentEveryFrame);
    println!("PRESENT_EVERY shown {every:?} dropped {dropped} lag {lag}");
    assert_eq!(every, [0, 1, 2, 3], "every frame in order");
    assert_eq!(dropped, 0);
    assert_eq!(lag, 6 * F, "and the picture falls behind");
}

/// 8 frames of 16 x 16 planar 4:4:4 Y4M at 24 fps, luma 40 + 20 f.
#[cfg(feature = "media")]
fn y4m() -> Vec<u8> {
    let mut out = b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C444\n".to_vec();
    for f in 0..8u8 {
        out.extend_from_slice(b"FRAME\n");
        out.extend(std::iter::repeat_n(40 + 20 * f, 256));
        out.extend(std::iter::repeat_n(128u8, 512));
    }
    out
}
#[cfg(feature = "media")]
#[test]
fn real_decoded_frames_blend_through_the_frame_cache() {
    use hsk_studio_harness_view::view::media::{MediaFrames, ReelFrameDecoder};
    use hsk_studio_pulse::FrameCacheLimits;
    let a = media("clip");
    let mut decoder = ReelFrameDecoder::new();
    decoder.register(&a, Arc::from(y4m())).unwrap();
    // Room for 3 decoded frames: 16 x 16 x 16 bytes each.
    let limits = FrameCacheLimits { max_bytes: 3 * 4096, max_entries: 8, max_frame_bytes: 4096 };
    let frames = MediaFrames::new(decoder, limits, 1).unwrap();
    assert_eq!(frames.source_grid(&a), Some(T24), "from the container");
    let raw = |f: u64| frames.frame_result(&a, Ticks::new(f * T24)).unwrap().colour[0..4].try_into().map(f32::from_le_bytes).unwrap();
    let (d0, d1) = (raw(0), raw(1));
    assert!(d1 > d0, "{d0} {d1}");
    // Speed 1/2, timeline frame 1: F / 2 is 0.48 of a 24 fps frame, so the share of frame 1 is 12/25.
    let mut seq = sequence("s", vec![clip(1, a.clone(), 8, 0, (1, 2), Blend)]);
    seq.tracks[0].clips[0].source_out = Ticks::new(8 * T24);
    seq.tracks[0].clips[0].duration = Ticks::new(16 * T24);
    seq.duration = Ticks::new(16 * T24);
    let m = monitor(seq, 16);
    let cancel = CancellationToken::default();
    let image = m.render_at(Ticks::new(F), &frames, &UnknownLength, &cancel).unwrap();
    let want = d0 * (1.0 - 0.48) + d1 * 0.48;
    let p = image.pixel(8, 8).unwrap()[0];
    assert!((p - want).abs() < 1e-4, "{p} against {want}");
    frames.with_pipeline(|p| {
        assert!(p.cache().used_bytes() <= 3 * 4096, "{}", p.cache().used_bytes());
        assert!(p.cache().stats().hits >= 2, "{:?}", p.cache().stats());
    });
}
#[cfg(feature = "media")]
struct UnknownLength;
#[cfg(feature = "media")]
impl SourceExtents for UnknownLength {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        None
    }
}

#[test]
fn the_false_text_is_gone_from_the_speed_form() {
    let rate = include_str!("../src/view/timeline/rate.rs");
    let ui = include_str!("../src/view/timeline_ui.rs");
    assert!(!rate.contains("no clip field"), "rate.rs still says the clip has no field");
    assert!(!ui.contains("No clip field stores this"), "timeline_ui.rs still says the clip has no field");
    assert!(rate.contains("StudioClip.retime"), "rate.rs names where the value is stored");
}
