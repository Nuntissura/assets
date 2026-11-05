//! Lane SMB-PR-RETIME-OUTPUT-1: the interpolation choice reaches `frame_layers` ([STU-VID-031]
//! "`frame_sampling` | Nearest source frame", "`frame_blending` | Weighted blend of the two
//! neighbouring source frames", "`optical_flow` ... MUST be marked as an estimation"). Oracles: a
//! hand table in source frames and an exact rational oracle written here, sharing no code with
//! `retime.rs`. The table prints what each row shows, so a red run records the state before the fix.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use std::cell::Cell;

/// 25 fps ticks per frame.
const F: u64 = 10_160_640_000;

fn media(name: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: name.into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
        asset_id: None,
    }
}
fn nested(id: &str) -> ClipSource {
    ClipSource::Sequence { sequence_id: id.into() }
}
/// A clip of source frames `[si, si + frames)` at timeline frame `start`, its duration derived from
/// the speed.
fn clip(n: u32, source: ClipSource, si: u64, frames: u64, start: u64, speed: (i64, u64), method: FrameInterpolation) -> StudioClip {
    let duration = u128::from(frames * F) * u128::from(speed.1);
    let duration = u64::try_from(duration.div_ceil(u128::from(speed.0.unsigned_abs().max(1)))).unwrap();
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source,
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
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
fn sequence(id: &str, ticks_per_frame: u64, clips: Vec<StudioClip>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame },
        duration: Ticks::new(0),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: format!("STRK-019abcde-0000-7000-8000-{:012x}", id.len()),
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
struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}

use FrameInterpolation::{FrameBlending as Blend, FrameSampling as Sample, OpticalFlow as Flow};

fn frame(i: u64) -> SourceFrameRef {
    SourceFrameRef { index: i, start: Ticks::new(i * F) }
}
/// `kind from to numerator/denominator`, reduced by the caller, for readable failures.
fn show(a: &SourceFrameAnswer) -> String {
    let w = |w: &FrameWeight| format!("{}/{}", w.numerator(), w.denominator());
    match a {
        SourceFrameAnswer::Sampled(f) => format!("sampled {}", f.index),
        SourceFrameAnswer::Held(f) => format!("held {}", f.index),
        SourceFrameAnswer::Blended { from, to, weight } => format!("blended {} {} {}", from.index, to.index, w(weight)),
        SourceFrameAnswer::Estimated { from, to, weight } => format!("estimated {} {} {}", from.index, to.index, w(weight)),
    }
}

#[test]
fn table_of_every_choice_at_mid_frame_positions_through_frame_layers() {
    let cancel = CancellationToken::default();
    // (row, speed, method, timeline tick, answer, source tick, estimation mark)
    let half = F / 2;
    let rows: Vec<(&str, (i64, u64), FrameInterpolation, u64, &str, u64, bool)> = vec![
        ("sampling 3/4 at 0.75 goes to the nearer later frame", (3, 4), Sample, F, "sampled 1", F, false),
        ("sampling 1/4 at 0.25 stays on the nearer earlier frame", (1, 4), Sample, F, "sampled 0", 0, false),
        ("sampling 1/2 at 0.5 ties to the later frame", (1, 2), Sample, F, "sampled 1", F, false),
        ("sampling 2 on a frame", (2, 1), Sample, 3 * F, "sampled 6", 6 * F, false),
        ("sampling 3/4 past the last frame stays on it", (3, 4), Sample, 53 * F, "sampled 39", 39 * F, false),
        ("blending 3/4 at 0.75", (3, 4), Blend, F, "blended 0 1 3/4", 0, false),
        ("blending 3/2 at 1.5", (3, 2), Blend, F, "blended 1 2 1/2", F, false),
        ("blending 1/2 on a frame needs one", (1, 2), Blend, 2 * F, "blended 1 2 0/1", F, false),
        ("blending 3/4 past the last frame", (3, 4), Blend, 53 * F, "blended 39 39 0/1", 39 * F, false),
        ("flow 3/4 at 0.75 is marked", (3, 4), Flow, F, "estimated 0 1 3/4", 0, true),
        ("flow 1/2 on a frame is marked", (1, 2), Flow, 2 * F, "estimated 1 2 0/1", F, true),
        ("reverse head shows the last frame", (-1, 1), Sample, 0, "sampled 39", 39 * F, false),
        ("reverse unit mirrors forward", (-1, 1), Sample, 3 * F, "sampled 36", 36 * F, false),
        ("reverse 1/2 at 38.5 ties later", (-1, 2), Sample, F, "sampled 39", 39 * F, false),
        ("reverse blending 1/2 at 38.5", (-1, 2), Blend, F, "blended 38 39 1/2", 38 * F, false),
        ("reverse flow 3/4 at 38.25", (-3, 4), Flow, F, "estimated 38 39 1/4", 38 * F, true),
        ("unit sampling on a frame", (1, 1), Sample, 3 * F, "sampled 3", 3 * F, false),
        ("unit sampling mid frame keeps the frame holding it", (1, 1), Sample, half, "sampled 0", half, false),
        ("unit blending on a frame needs one", (1, 1), Blend, 3 * F, "blended 3 4 0/1", 3 * F, false),
        ("unit blending mid frame", (1, 1), Blend, half, "blended 0 1 1/2", half, false),
        ("unit flow mid frame is marked", (1, 1), Flow, half, "estimated 0 1 1/2", half, true),
    ];
    let mut wrong = Vec::new();
    for (row, speed, method, tick, answer, source_tick, marked) in &rows {
        let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, *speed, *method)]);
        let layers = frame_layers(&seq, Ticks::new(*tick), &Known, &cancel).unwrap();
        assert_eq!(layers.len(), 1, "{row}");
        let l = &layers[0];
        let seen = format!(
            "{} tick {:.2}F mark {:?}",
            show(&l.answer),
            l.source_tick.value() as f64 / F as f64,
            l.estimation_mark()
        );
        println!("TABLE {row}: {seen}");
        if show(&l.answer) != *answer || l.source_tick.value() != *source_tick || l.estimation_mark().is_some() != *marked {
            wrong.push(format!("{row}: {seen}, expected {answer} tick {:.2}F marked {marked}", *source_tick as f64 / F as f64));
        }
    }
    assert!(wrong.is_empty(), "{} of {} rows wrong:\n{}", wrong.len(), rows.len(), wrong.join("\n"));
}

#[test]
fn a_hold_answers_held_at_every_choice() {
    let cancel = CancellationToken::default();
    for method in [Sample, Blend, Flow] {
        let mut c = clip(1, media("a"), 0, 40, 0, (3, 4), method);
        c.retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(7 * F), anchor: FrameHoldAnchor::InPoint });
        let seq = sequence("seq-a", F, vec![c]);
        for tick in [0, F, 5 * F + F / 3] {
            let l = &frame_layers(&seq, Ticks::new(tick), &Known, &cancel).unwrap()[0];
            assert_eq!(l.answer, SourceFrameAnswer::Held(frame(7)), "{method:?} {tick}");
            assert_eq!((l.source_tick.value(), l.estimation_mark(), l.answer.pair()), (7 * F, None, None));
        }
    }
}

/// The nearest frame, the pair and the weight by exact fractions over `b` of a source frame.
fn oracle(speed: (i64, u64), n: u64, last: i128) -> (i128, i128, (i128, i128)) {
    let (a, b) = (i128::from(speed.0), i128::from(speed.1));
    // Position in source frames = p / b.
    let p = if a > 0 { i128::from(n) * a } else { last * b + i128::from(n) * a };
    if p < 0 {
        return (0, 0, (0, 1));
    }
    let (i, r) = (p / b, p % b);
    if i >= last {
        return (last, last, (0, 1));
    }
    (i, i + 1, (r, b))
}
fn same_share(w: &FrameWeight, r: i128, b: i128) -> bool {
    w.numerator() as i128 * b == r * w.denominator() as i128
}

#[test]
fn every_frame_of_every_speed_matches_an_independent_rational_oracle() {
    let cancel = CancellationToken::default();
    let speeds: [(i64, u64); 12] = [(1, 2), (1, 3), (1, 4), (3, 4), (2, 3), (3, 2), (2, 1), (5, 1), (-1, 1), (-1, 2), (-3, 4), (-2, 1)];
    let mut checked = 0u32;
    for speed in speeds {
        for method in [Sample, Blend, Flow] {
            let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, speed, method)]);
            let frames = seq.duration.value() / F;
            for n in 0..frames {
                let l = &frame_layers(&seq, Ticks::new(n * F), &Known, &cancel).unwrap()[0];
                let (from, to, (r, b)) = oracle(speed, n, 39);
                match (&l.answer, method) {
                    (SourceFrameAnswer::Sampled(f), Sample) => {
                        // Nearer of the two; a tie goes to the later source frame.
                        let want = if r * 2 >= b && to != from { to } else { from };
                        assert_eq!(i128::from(f.index), want, "{speed:?} frame {n}");
                        assert_eq!(l.source_tick, f.start, "{speed:?} frame {n}");
                    }
                    (SourceFrameAnswer::Blended { from: x, to: y, weight }, Blend) | (SourceFrameAnswer::Estimated { from: x, to: y, weight }, Flow) => {
                        assert_eq!((i128::from(x.index), i128::from(y.index)), (from, to), "{speed:?} {method:?} frame {n}");
                        assert!(same_share(weight, r, b), "{speed:?} {method:?} frame {n} {weight:?} against {r}/{b}");
                        assert_eq!(l.source_tick, x.start, "{speed:?} {method:?} frame {n}");
                        assert_eq!(l.estimation_mark().is_some(), method == Flow);
                    }
                    other => panic!("{speed:?} {method:?} frame {n}: {other:?}"),
                }
                checked += 1;
            }
        }
    }
    println!("oracle rows {checked}");
    assert!(checked > 1000, "{checked}");
}

#[test]
fn unit_speed_sampling_output_does_not_change() {
    let cancel = CancellationToken::default();
    // On and off the frame grid, in the source and on the timeline: the tick clip_source_at answers.
    for (si, start) in [(0u64, 0u64), (3 * F, 2 * F), (3 * F + F / 3, 2 * F), (F / 2, 0)] {
        let mut c = clip(1, media("a"), 0, 40, 0, (1, 1), Sample);
        c.source_in = Ticks::new(si);
        c.source_out = Ticks::new(si + 40 * F);
        c.timeline_start = Ticks::new(start);
        let seq = sequence("seq-a", F, vec![c.clone()]);
        for tick in (0..40).flat_map(|n| [start + n * F, start + n * F + F / 4, start + n * F + F / 2, start + n * F + F - 1]) {
            let (want, held) = clip_source_at(&c, Ticks::new(tick), &Known, false).unwrap();
            let l = &frame_layers(&seq, Ticks::new(tick), &Known, &cancel).unwrap()[0];
            assert_eq!((l.source_tick, l.held, l.weight), (want, held, 1.0), "{si} {start} {tick}");
            assert_eq!(l.answer.pair(), None, "{si} {start} {tick}");
        }
    }
}

struct Grid24;
impl SourceGrids for Grid24 {
    fn grid(&self, _: &ClipSource) -> Option<SourceFrameGrid> {
        Some(SourceFrameGrid::new(T24).unwrap())
    }
}
/// 24 fps ticks per frame. `F / T24` = 24/25.
const T24: u64 = 10_584_000_000;

#[test]
fn the_source_grid_comes_from_the_port() {
    let cancel = CancellationToken::default();
    let on = |speed, method, tick: u64| {
        let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, speed, method)]);
        frame_layers_on_grids(&seq, Ticks::new(tick), &Known, &Grid24, &cancel).unwrap().remove(0)
    };
    // Unit speed, sampling: the tick is kept, the answer names the frame holding it.
    let l = on((1, 1), Sample, F);
    assert_eq!((show(&l.answer), l.source_tick.value()), ("sampled 0".to_owned(), F));
    // Unit speed, blending: 24/25 of the way to frame 1 of the source.
    let l = on((1, 1), Blend, F);
    assert_eq!((show(&l.answer), l.source_tick.value(), l.second_source_tick()), ("blended 0 1 24/25".to_owned(), F, Some(Ticks::new(T24))));
    // Half speed, sampling: position F is 0.96 of a source frame, nearest frame 1.
    let l = on((1, 2), Sample, 2 * F);
    assert_eq!((show(&l.answer), l.source_tick.value()), ("sampled 1".to_owned(), T24));
    // Without a stated grid the sequence's own grid: a declared reading.
    let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (1, 2), Sample)]);
    let l = frame_layers(&seq, Ticks::new(2 * F), &Known, &cancel).unwrap().remove(0);
    assert_eq!((show(&l.answer), l.source_tick.value()), ("sampled 1".to_owned(), F));
}

fn dissolve(id: &str, frames: u64) -> TimelineTransition {
    TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: id.into(),
        transition_kind: CROSS_DISSOLVE.into(),
        duration: Ticks::new(frames * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    }
}

#[test]
fn a_transition_handle_carries_the_choice_past_the_window() {
    let cancel = CancellationToken::default();
    // Outgoing 1/2 speed blending over 20 timeline frames (source 0..10), cut at 20; dissolve over
    // 18..22; incoming unit sampling from source 30.
    let mut out = clip(1, media("a"), 0, 10, 0, (1, 2), Blend);
    out.transitions.tail = Some(dissolve("tr-1", 4));
    let inc = clip(2, media("b"), 30, 10, 20, (1, 1), Sample);
    let seq = sequence("seq-a", F, vec![out, inc]);
    // Tick 21: the outgoing clip is in its tail handle at source 10.5, the incoming at 31.
    let layers = frame_layers(&seq, Ticks::new(21 * F), &Known, &cancel).unwrap();
    assert_eq!(layers.len(), 2);
    assert_eq!((show(&layers[0].answer), layers[0].weight), ("blended 10 11 1/2".to_owned(), 1.0));
    assert_eq!(layers[0].source_tick.value(), 10 * F);
    assert_eq!((show(&layers[1].answer), layers[1].source_tick.value(), layers[1].weight), ("sampled 31".to_owned(), 31 * F, 0.75));
    // Reversed incoming clip, its head handle lies after its window: 2 frames early at -1 is the
    // last frame plus 2.
    let mut out = clip(1, media("a"), 0, 20, 0, (1, 1), Sample);
    out.transitions.tail = Some(dissolve("tr-2", 4));
    let inc = clip(2, media("b"), 30, 10, 20, (-1, 2), Blend);
    let seq = sequence("seq-a", F, vec![out, inc]);
    let layers = frame_layers(&seq, Ticks::new(19 * F), &Known, &cancel).unwrap();
    // Offset -1 frame at speed -1/2 from frame 39: position 39.5.
    assert_eq!(show(&layers[1].answer), "blended 39 40 1/2");
}

#[test]
fn nested_clips_resolve_on_the_inner_grid_and_refuse_a_pair() {
    let cancel = CancellationToken::default();
    // Inner at 50 fps: F / 2 per frame, a media clip over its whole length.
    let inner = sequence("seq-b", F / 2, vec![clip(2, media("m"), 0, 100, 0, (1, 1), Sample)]);
    // Speed 1/4, frame 1: position F / 4 is half an inner frame, the tie goes to inner frame 1.
    let outer = sequence("seq-a", F, vec![clip(1, nested("seq-b"), 0, 40, 0, (1, 4), Sample)]);
    let all = [inner.clone(), outer.clone()];
    let got = frame_layers_nested(&all, &outer, Ticks::new(F), &Known, 16, &cancel).unwrap();
    assert!(got.unresolved.is_empty(), "{:?}", got.unresolved);
    assert_eq!(got.layers[0].layer.source_tick.value(), F / 2, "inner tick from the inner grid");
    // A nested clip cannot blend two inner frames here: typed, not a floored frame.
    let outer = sequence("seq-a", F, vec![clip(1, nested("seq-b"), 0, 40, 0, (1, 4), Blend)]);
    let all = [inner.clone(), outer.clone()];
    let got = frame_layers_nested(&all, &outer, Ticks::new(F), &Known, 16, &cancel).unwrap();
    assert!(got.layers.is_empty(), "{:?}", got.layers);
    assert_eq!(got.unresolved.iter().map(|u| u.code).collect::<Vec<_>>(), [FRAME_INTERPOLATION_UNAVAILABLE]);
    // On a frame of the inner grid the same clip needs one frame and resolves.
    let got = frame_layers_nested(&all, &outer, Ticks::new(2 * F), &Known, 16, &cancel).unwrap();
    assert!(got.unresolved.is_empty(), "{:?}", got.unresolved);
    assert_eq!(got.layers[0].layer.source_tick.value(), F / 2);
    // The inner clip's own choice travels with its layer.
    let inner = sequence("seq-b", F, vec![clip(2, media("m"), 0, 40, 0, (3, 4), Flow)]);
    let outer = sequence("seq-a", F, vec![clip(1, nested("seq-b"), 0, 40, 0, (1, 1), Sample)]);
    let all = [inner, outer.clone()];
    let got = frame_layers_nested(&all, &outer, Ticks::new(F), &Known, 16, &cancel).unwrap();
    assert_eq!(got.layers[0].layer.estimation_mark(), Some(ESTIMATION_MARK));
    assert_eq!(show(&got.layers[0].layer.answer), "estimated 0 1 3/4");
}

/// Frames of 16 bytes; counts decoder calls and names the ticks asked for.
#[derive(Default)]
struct Fake {
    decoded: Cell<u32>,
}
#[derive(Debug)]
struct Bytes;
impl FrameCost for Bytes {
    fn cost_bytes(&self) -> u64 {
        16
    }
}
impl FrameDecoder for Fake {
    type Frame = Bytes;
    fn declared_bytes(&self, _: &ClipSource, _: &CancellationToken) -> std::result::Result<u64, DecodeRefusal> {
        Ok(16)
    }
    fn decode_run(&self, _: &ClipSource, tick: Ticks, max: u32, _: &CancellationToken) -> std::result::Result<Vec<DecodedFrame<Bytes>>, DecodeRefusal> {
        self.decoded.set(self.decoded.get() + 1);
        let first = tick.value() / F;
        Ok((first..first + u64::from(max))
            .map(|f| DecodedFrame { span: TickRange::new(Ticks::new(f * F), Ticks::new((f + 1) * F)).unwrap(), frame: Bytes })
            .collect())
    }
}
fn pipeline(max_bytes: u64) -> FramePipeline<Fake> {
    FramePipeline::new(Fake::default(), FrameCacheLimits { max_bytes, max_entries: 64, max_frame_bytes: 16 }, 1).unwrap()
}

#[test]
fn prefetch_decodes_both_frames_of_a_pair_within_its_budget() {
    let cancel = CancellationToken::default();
    let (a, seq) = (media("a"), sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (3, 4), Blend)]));
    let layer = frame_layers(&seq, Ticks::new(F), &Known, &cancel).unwrap();
    // One decoder call allowed: the first frame only, the second stays for the next call.
    let mut p = pipeline(1024);
    assert_eq!(p.prefetch(&layer, 1, &cancel), 1);
    assert_eq!(p.decoder().decoded.get(), 1);
    assert!(p.cache().contains(&a, Ticks::new(0)));
    assert!(!p.cache().contains(&a, Ticks::new(F)));
    assert_eq!(p.prefetch(&layer, 1, &cancel), 1);
    assert!(p.cache().contains(&a, Ticks::new(F)), "the second frame of the pair");
    // Everything cached: no decoder call.
    assert_eq!(p.prefetch(&layer, 8, &cancel), 0);
    assert_eq!(p.decoder().decoded.get(), 2);
    // A sampled layer and a pair with a zero share ask for one frame.
    for (method, tick) in [(Sample, F), (Blend, 4 * F)] {
        let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (3, 4), method)]);
        let layer = frame_layers(&seq, Ticks::new(tick), &Known, &cancel).unwrap();
        let mut p = pipeline(1024);
        assert_eq!(p.prefetch(&layer, 8, &cancel), 1, "{method:?}");
        assert_eq!(p.decoder().decoded.get(), 1, "{method:?}");
    }
    // A pair per frame over 8 frames, room for 3 frames in the cache: its budget holds, the LRU
    // takes the rest and the counters say so.
    let frames: Vec<Ticks> = (0..8).map(|n| Ticks::new(n * F)).collect();
    let mut layers = Vec::new();
    for t in &frames {
        layers.extend(frame_layers(&seq, *t, &Known, &cancel).unwrap());
    }
    let mut p = pipeline(48);
    let got = p.prefetch(&layers, 256, &cancel);
    assert!(got >= 6, "{got}");
    assert!(p.cache().used_bytes() <= 48, "{}", p.cache().used_bytes());
    assert!(p.cache().stats().evictions > 0);
    // Canceled: nothing decoded.
    let stopped = CancellationToken::default();
    stopped.cancel();
    let mut p = pipeline(1024);
    assert_eq!(p.prefetch(&layers, 256, &stopped), 0);
    assert_eq!(p.decoder().decoded.get(), 0);
}

#[test]
fn prefetch_layers_on_grids_states_the_grid() {
    let cancel = CancellationToken::default();
    let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (1, 2), Blend)]);
    let scheduler = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let layers = prefetch_layers_on_grids(&seq, &scheduler, &[2], &Known, &Grid24, &cancel).unwrap();
    // Position F on the 24 fps grid: 24/25 of the way from frame 0 to frame 1.
    assert_eq!(show(&layers[0].answer), "blended 0 1 24/25");
    assert_eq!(layers[0].second_source_tick(), Some(Ticks::new(T24)));
}

#[test]
fn degenerate_speeds_and_grids_answer_typed_never_panic() {
    let cancel = CancellationToken::default();
    let raw = |speed: (i64, u64), duration: u64| {
        let mut c = clip(1, media("a"), 0, 40, 0, (1, 1), Blend);
        c.speed = Speed { numerator: speed.0, denominator: speed.1 };
        c.duration = Ticks::new(duration);
        let mut seq = sequence("seq-a", F, vec![c]);
        seq.duration = Ticks::new(duration);
        frame_layers(&seq, Ticks::new(duration / 2), &Known, &cancel)
    };
    assert_eq!(raw((0, 1), 40 * F).unwrap_err().code(), "zero_speed");
    assert_eq!(raw((1, 0), 40 * F).unwrap_err().code(), "invalid_speed");
    for numerator in [i64::MAX, i64::MIN, i64::MIN + 1] {
        for denominator in [1, 3, u64::MAX] {
            for duration in [40 * F, u64::MAX / 2, u64::MAX - 1] {
                // Either a typed refusal or a frame of the window.
                match raw((numerator, denominator), duration) {
                    Ok(layers) => assert!(layers[0].answer.frame().index <= 39, "{numerator} {denominator} {duration}"),
                    Err(e) => assert!(!e.code().is_empty()),
                }
            }
        }
    }
    // A source frame longer than anything: one frame at index 0, no overflow.
    struct Wide;
    impl SourceGrids for Wide {
        fn grid(&self, _: &ClipSource) -> Option<SourceFrameGrid> {
            SourceFrameGrid::new(u64::MAX).ok()
        }
    }
    let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (3, 4), Blend)]);
    let l = frame_layers_on_grids(&seq, Ticks::new(F), &Known, &Wide, &cancel).unwrap();
    assert_eq!(l[0].answer.frame().index, 0);
    assert_eq!(l[0].answer.pair(), None, "the window lies in one source frame");
    // Canceled.
    let stopped = CancellationToken::default();
    stopped.cancel();
    assert_eq!(frame_layers(&seq, Ticks::new(F), &Known, &stopped).unwrap_err().code(), "canceled");
}

#[test]
fn estimation_mark_is_named_and_the_share_is_exact() {
    let cancel = CancellationToken::default();
    let seq = sequence("seq-a", F, vec![clip(1, media("a"), 0, 40, 0, (3, 4), Flow)]);
    let l = frame_layers(&seq, Ticks::new(F), &Known, &cancel).unwrap().remove(0);
    assert_eq!(l.estimation_mark(), Some("estimation"));
    let (from, to, w) = l.answer.pair().unwrap();
    assert_eq!((from, to, w.as_f64()), (frame(0), frame(1), 0.75));
    assert_eq!(l.second_source_tick(), Some(Ticks::new(F)));
}
