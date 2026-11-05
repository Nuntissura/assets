//! Lane SMB-PR-RETIME-HOLD-1: source-frame resolution and frame hold ([STU-VID-031], [STU-VID-031a]).
//! Oracles: hand-computed source positions at 25 fps (F ticks per frame), exact reduced weights, the
//! Folio-validated prospective document and `plan_timeline_revert` restoring the exact prior sequence.
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;
/// 24 fps ticks per frame (STU-VID-013).
const G24: u64 = 10_584_000_000;

fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}

struct Media;
impl Resolver for Media {
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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

/// Clip `n`: source frames `[si, so)` at timeline frame `start`, unit speed.
fn clip(n: u32, si: u64, so: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(so.saturating_sub(si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}

/// A clip to resolve directly: window `[si, so)` frames, `frames` timeline frames at speed `num/den`.
fn raw(si: u64, so: u64, frames: u64, num: i64, den: u64, method: FrameInterpolation) -> StudioClip {
    let mut c = clip(1, si, so, 0);
    c.duration = Ticks::new(frames * F);
    c.speed = Speed { numerator: num, denominator: den };
    c.retime.frame_interpolation = method;
    c
}

fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind,
        index: 0,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    }
}

fn marker(id: &str, frame: u64) -> StudioMarker {
    StudioMarker { marker_id: id.into(), name: String::new(), comment: String::new(), color: LabelColor::Rose, start: Ticks::new(frame * F), duration: Ticks::new(0), role: None }
}

/// V1: clip 1 source [10, 30) at [0, 20); clip 2 source [100, 130) at [40, 70) with marker `m` at
/// source frame 110; clip 3 source [0, 20) at [70, 90) (abuts clip 2). A1: clip 4 source [10, 30) at
/// [0, 20).
fn snapshot(mutate: impl FnOnce(&mut Vec<StudioTrack>)) -> Snapshot {
    let mut two = clip(2, 100, 130, 40);
    two.markers.push(marker("m", 110));
    let mut tracks = vec![track(1, TrackKind::Video, vec![clip(1, 10, 30, 0), two, clip(3, 0, 20, 70)]), track(2, TrackKind::Audio, vec![clip(4, 10, 30, 0)])];
    mutate(&mut tracks);
    let end = tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_start.value() + c.duration.value()).max().unwrap_or(0);
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: "SDOC-019abcde-0000-7000-8000-000000000001".into(),
        revision: 3,
        geometry_unit: GeometryUnit::Px,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(end),
            tracks,
            markers: Vec::new(),
            work_area: None,
        }],
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}

struct Unknown;
impl SourceExtents for Unknown {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        None
    }
}
/// Every source is `frames` frames long.
struct Long(u64);
impl SourceExtents for Long {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(self.0 * F))
    }
}

fn with_ctx<T>(f: impl FnOnce(&EditContext) -> T) -> T {
    let cancel = CancellationToken::default();
    f(&EditContext { budget: Budget::default(), resolver: &Media, extents: &Unknown, cancel: &cancel })
}

fn grid() -> SourceFrameGrid {
    SourceFrameGrid::new(F).unwrap()
}

fn get(s: &Snapshot, n: u32) -> StudioClip {
    s.document().sequences[0].tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).expect("clip present").clone()
}

fn seq(s: &Snapshot) -> &StudioSequence {
    &s.document().sequences[0]
}

fn f(index: u64) -> SourceFrameRef {
    SourceFrameRef { index, start: Ticks::new(index * F) }
}

fn answer(c: &StudioClip, tick: u64) -> SourceFrameAnswer {
    resolve_source_frames(c, Ticks::new(tick), grid(), &Unknown).unwrap()
}

/// The sampled frame index at timeline frame `k`.
fn sampled(c: &StudioClip, k: u64) -> u64 {
    match answer(c, k * F) {
        SourceFrameAnswer::Sampled(frame) => {
            assert_eq!(frame.start, Ticks::new(frame.index * F));
            frame.index
        }
        other => panic!("expected one sampled frame, got {other:?}"),
    }
}

/// `(from, to, numerator, denominator)` of the blended answer at `tick`.
fn blended(c: &StudioClip, tick: u64) -> (u64, u64, u128, u128) {
    match answer(c, tick) {
        SourceFrameAnswer::Blended { from, to, weight } => (from.index, to.index, weight.numerator(), weight.denominator()),
        other => panic!("expected a blend, got {other:?}"),
    }
}

fn hold(s: &Snapshot, n: u32, at: HoldAt, multicam: Option<&StudioMulticamRecords>) -> RetimeResult<EditProposal> {
    let edit = SetFrameHold { clip_id: cid(n), at, grid: grid() };
    with_ctx(|ctx| plan_set_frame_hold(s, "seq-main", s.document().revision, &edit, multicam, ctx))
}

fn clear(s: &Snapshot, n: u32) -> RetimeResult<EditProposal> {
    with_ctx(|ctx| plan_clear_frame_hold(s, "seq-main", s.document().revision, &cid(n), ctx))
}

fn segment(s: &Snapshot, n: u32, from: u64, to: u64, at: HoldAt, ids: &[u32], groups: &[&str]) -> RetimeResult<EditProposal> {
    let edit = InsertFrameHoldSegment {
        clip_id: cid(n),
        span: TickRange::new(Ticks::new(from * F), Ticks::new(to * F)).unwrap(),
        at,
        grid: grid(),
        fresh_clip_ids: ids.iter().map(|i| cid(*i)).collect(),
        fresh_link_groups: groups.iter().map(|g| (*g).to_string()).collect(),
    };
    with_ctx(|ctx| plan_insert_frame_hold_segment(s, "seq-main", s.document().revision, &edit, None, ctx))
}

fn revert(p: &EditProposal) -> EditProposal {
    with_ctx(|ctx| plan_timeline_revert(p.snapshot(), "seq-main", p.target_revision(), p.inverse(), ctx)).unwrap()
}

/// Every whole timeline frame of the clip and a sub-frame tick inside each answers `Held(frame)`.
fn assert_held_everywhere(c: &StudioClip, frame: u64) {
    let (start, duration) = (c.timeline_start.value(), c.duration.value());
    let mut tick = start;
    while tick < start + duration {
        assert_eq!(answer(c, tick), SourceFrameAnswer::Held(f(frame)), "tick {tick}");
        let inside = tick + F / 3;
        if inside < start + duration {
            assert_eq!(answer(c, inside), SourceFrameAnswer::Held(f(frame)), "tick {inside}");
        }
        tick += F;
    }
    assert_eq!(answer(c, start + duration - 1), SourceFrameAnswer::Held(f(frame)));
}

/// [STU-VID-031] `frame_sampling`: "Nearest source frame." Position = source in + offset * speed,
/// computed by hand; a tie goes to the later frame (declared reading).
#[test]
fn frame_sampling_is_the_nearest_source_frame() {
    use FrameInterpolation::FrameSampling as M;
    let unit = raw(10, 30, 20, 1, 1, M);
    for k in 0..20 {
        assert_eq!(sampled(&unit, k), 10 + k);
    }
    assert_eq!(answer(&unit, F / 4), SourceFrameAnswer::Sampled(f(10)), "10.25 -> 10");
    assert_eq!(answer(&unit, F / 2), SourceFrameAnswer::Sampled(f(11)), "10.5 ties to the later frame");
    assert_eq!(answer(&unit, 3 * F / 4), SourceFrameAnswer::Sampled(f(11)), "10.75 -> 11");
    assert_eq!(answer(&unit, 20 * F - 1), SourceFrameAnswer::Sampled(f(29)), "never past the last frame of the window");
    // Half speed: 10, 10.5, 11, 11.5, ...: 10, 11, 11, 12, ...; the last timeline frame is 29.5 -> 29.
    let half = raw(10, 30, 40, 1, 2, M);
    assert_eq!([0, 1, 2, 3, 39].map(|k| sampled(&half, k)), [10, 11, 11, 12, 29]);
    // Double speed: every second frame.
    let double = raw(0, 20, 10, 2, 1, M);
    assert_eq!((0..10).map(|k| sampled(&double, k)).collect::<Vec<_>>(), [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
    // One third: 0, 1/3, 2/3, 1: 0, 0, 1, 1.
    let third = raw(0, 10, 30, 1, 3, M);
    assert_eq!([0, 1, 2, 3].map(|k| sampled(&third, k)), [0, 0, 1, 1]);
}

/// [STU-VID-031] `frame_blending`: "Weighted blend of the two neighbouring source frames." The
/// weight is the exact reduced share of the later frame.
#[test]
fn frame_blending_gives_the_two_neighbours_and_an_exact_weight() {
    use FrameInterpolation::FrameBlending as M;
    let unit = raw(10, 30, 20, 1, 1, M);
    assert_eq!(blended(&unit, F / 4), (10, 11, 1, 4));
    assert_eq!(blended(&unit, 0), (10, 11, 0, 1), "on a frame the later frame weighs nothing");
    let half = raw(10, 30, 40, 1, 2, M);
    assert_eq!(blended(&half, F), (10, 11, 1, 2));
    assert_eq!(blended(&half, 2 * F), (11, 12, 0, 1));
    let two_thirds = raw(0, 20, 30, 2, 3, M);
    assert_eq!(blended(&two_thirds, F), (0, 1, 2, 3));
    assert_eq!(blended(&two_thirds, 2 * F), (1, 2, 1, 3));
    assert_eq!(blended(&two_thirds, 3 * F), (2, 3, 0, 1));
    // The last frame of the window has no later neighbour in the window.
    assert_eq!(blended(&unit, 19 * F + F / 2), (29, 29, 0, 1));
    // A 24 fps source in a 25 fps sequence: one second holds 24 frames; timeline frame k is at
    // source position 24k/25 frames. Hand values: 24/25, 48/25 = 1 + 23/25, 576/25 = 23.04 (last).
    let mut c = raw(0, 0, 25, 1, 1, M);
    c.source_out = Ticks::new(24 * G24);
    assert_eq!(c.derived_duration(), Some(c.duration));
    let g24 = SourceFrameGrid::from_rate(FrameRate::from_import(G24).unwrap().0);
    let at = |k: u64| match resolve_source_frames(&c, Ticks::new(k * F), g24, &Unknown).unwrap() {
        SourceFrameAnswer::Blended { from, to, weight } => {
            assert_eq!((from.start, to.start), (Ticks::new(from.index * G24), Ticks::new(to.index * G24)));
            (from.index, to.index, weight.numerator(), weight.denominator())
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(at(1), (0, 1, 24, 25));
    assert_eq!(at(2), (1, 2, 23, 25));
    assert_eq!(at(24), (23, 23, 0, 1));
}

/// [STU-VID-031] `optical_flow` "MUST be marked as an estimation in any diagnostic that compares it
/// to a source frame": the same two frames as a blend, carrying the mark; a real frame never does.
#[test]
fn optical_flow_is_the_same_two_frames_marked_as_an_estimation() {
    let flow = raw(10, 30, 40, 1, 2, FrameInterpolation::OpticalFlow);
    let blend = raw(10, 30, 40, 1, 2, FrameInterpolation::FrameBlending);
    let (SourceFrameAnswer::Estimated { from, to, weight }, SourceFrameAnswer::Blended { from: bf, to: bt, weight: bw }) = (answer(&flow, F), answer(&blend, F)) else {
        panic!("wrong answers");
    };
    assert_eq!((from, to, weight), (bf, bt, bw));
    assert_eq!((from, to), (f(10), f(11)));
    assert!(answer(&flow, F).is_estimation());
    assert_eq!(answer(&flow, F).estimation_mark(), Some(ESTIMATION_MARK));
    assert_eq!(ESTIMATION_MARK, "estimation");
    // Exactly on a source frame the answer is still marked: the method estimated it.
    assert!(answer(&flow, 0).is_estimation());
    for real in [raw(10, 30, 40, 1, 2, FrameInterpolation::FrameSampling), raw(10, 30, 40, 1, 2, FrameInterpolation::FrameBlending)] {
        assert!(!answer(&real, F).is_estimation());
        assert_eq!(answer(&real, F).estimation_mark(), None);
    }
}

/// [STU-VID-031] "Reverse is a negative speed": the head of a reversed clip shows the last frame of
/// the window and the tail its first, each exactly once at unit speed.
#[test]
fn reverse_speed_resolves_exactly() {
    use FrameInterpolation::{FrameBlending, FrameSampling};
    let rev = raw(0, 20, 20, -1, 1, FrameSampling);
    let forward = raw(0, 20, 20, 1, 1, FrameSampling);
    let played: Vec<u64> = (0..20).map(|k| sampled(&rev, k)).collect();
    let mut mirrored: Vec<u64> = (0..20).map(|k| sampled(&forward, k)).collect();
    mirrored.reverse();
    assert_eq!(played, mirrored, "unit reverse is the exact mirror of forward");
    assert_eq!((played[0], played[19]), (19, 0));
    // Half reverse: 19, 18.5, 18, ...; the last timeline frame is below the first frame and stays on it.
    let half = raw(0, 20, 40, -1, 2, FrameBlending);
    assert_eq!(blended(&half, 0), (19, 19, 0, 1));
    assert_eq!(blended(&half, F), (18, 19, 1, 2));
    assert_eq!(blended(&half, 39 * F), (0, 0, 0, 1));
    let half_sampled = raw(0, 20, 40, -1, 2, FrameSampling);
    assert_eq!([0, 1, 2, 39].map(|k| sampled(&half_sampled, k)), [19, 19, 18, 0]);
    // Double reverse: 19, 17, ..., 1.
    let double = raw(0, 20, 10, -2, 1, FrameSampling);
    assert_eq!((0..10).map(|k| sampled(&double, k)).collect::<Vec<_>>(), [19, 17, 15, 13, 11, 9, 7, 5, 3, 1]);
    // A sub-frame tick: 19 - 1/4 = 18.75.
    let unit = raw(0, 20, 20, -1, 1, FrameBlending);
    assert_eq!(blended(&unit, F / 4), (18, 19, 3, 4));
    // An off-grid out point: the last frame is the one that starts before it.
    let mut off = raw(0, 20, 21, -1, 1, FrameSampling);
    off.source_out = Ticks::new(20 * F + F / 2);
    off.duration = off.derived_duration().unwrap();
    assert_eq!(sampled(&off, 0), 20);
}

/// Between any two ticks of any speed the blend stays inside the window and the sampled frame is
/// the neighbour with the larger share (no float anywhere, so this holds exactly).
#[test]
fn sampling_is_the_heavier_neighbour_at_every_speed() {
    for (num, den, frames) in [(1i64, 1u64, 20u64), (1, 2, 40), (2, 1, 10), (2, 3, 30), (3, 7, 140), (-1, 1, 20), (-1, 2, 40), (-2, 1, 10), (-3, 7, 140)] {
        let blend = raw(5, 25, frames, num, den, FrameInterpolation::FrameBlending);
        let sample = raw(5, 25, frames, num, den, FrameInterpolation::FrameSampling);
        for step in 0..(frames * 4) {
            let tick = step * (F / 4);
            let (from, to, n, d) = blended(&blend, tick);
            assert!((5..=24).contains(&from) && (5..=24).contains(&to) && to - from <= 1, "{num}/{den} at {tick}: {from} {to}");
            assert!(n < d, "{n}/{d}");
            let SourceFrameAnswer::Sampled(s) = answer(&sample, tick) else { panic!("sampled") };
            assert_eq!(s.index, if 2 * n >= d { to } else { from }, "{num}/{den} at {tick}");
        }
    }
}

/// Hostile input: every refusal is typed, nothing panics, no silent clamp of a tick off the clip.
#[test]
fn resolution_refusals_are_typed() {
    use FrameInterpolation::FrameSampling as M;
    let code = |c: &StudioClip, tick: u64| resolve_source_frames(c, Ticks::new(tick), grid(), &Unknown).unwrap_err().code();
    let mut placed = raw(10, 30, 20, 1, 1, M);
    placed.timeline_start = Ticks::new(5 * F);
    assert_eq!(code(&placed, 5 * F - 1), "outside_range");
    assert_eq!(code(&placed, 25 * F), "outside_range", "the clip end is exclusive");
    assert_eq!(SourceFrameGrid::new(0).unwrap_err().code(), "invalid_input");
    assert_eq!(code(&raw(10, 30, 20, 0, 1, M), 0), "zero_speed");
    assert_eq!(code(&raw(10, 30, 20, 1, 0, M), 0), "invalid_speed");
    assert_eq!(code(&raw(30, 30, 20, 1, 1, M), 0), "invalid_range");
    assert_eq!(code(&raw(31, 30, 20, 1, 1, M), 0), "invalid_range");
    assert_eq!(code(&raw(0, 0, 20, 1, 1, M), 0), "invalid_range");
    // The widest values fit u128 or are typed overflow, never a wrap.
    let mut wide = raw(0, 0, 0, i64::MAX, u64::MAX, M);
    wide.source_in = Ticks::new(u64::MAX - 1);
    wide.source_out = Ticks::new(u64::MAX);
    wide.duration = Ticks::new(u64::MAX);
    assert_eq!(code(&wide, u64::MAX - 1), "overflow");
    let tiny = SourceFrameGrid::new(1).unwrap();
    assert_eq!(resolve_source_frames(&wide, Ticks::new(0), tiny, &Unknown).unwrap(), SourceFrameAnswer::Sampled(SourceFrameRef { index: u64::MAX - 1, start: Ticks::new(u64::MAX - 1) }));
    let mut reverse_wide = wide.clone();
    reverse_wide.speed = Speed { numerator: i64::MIN + 1, denominator: 1 };
    assert_eq!(resolve_source_frames(&reverse_wide, Ticks::new(u64::MAX - 1), tiny, &Unknown).unwrap(), SourceFrameAnswer::Sampled(SourceFrameRef { index: u64::MAX - 1, start: Ticks::new(u64::MAX - 1) }));
}

/// [STU-VID-031a] each hold origin freezes the clip at one source frame at every timeline tick. The
/// MT-33732 oracle: four clips, one per origin, each holding source frame 3.
#[test]
fn every_hold_origin_freezes_one_source_frame_at_every_tick() {
    let s = snapshot(|_| {});
    for (at, frame, anchor) in [
        (HoldAt::InPoint, 100, FrameHoldAnchor::InPoint),
        (HoldAt::OutPoint, 129, FrameHoldAnchor::OutPoint),
        (HoldAt::Playhead(Ticks::new(45 * F)), 105, FrameHoldAnchor::Playhead),
        (HoldAt::ClipMarker("m".into()), 110, FrameHoldAnchor::ClipMarker),
    ] {
        let p = hold(&s, 2, at.clone(), None).unwrap();
        let held = get(p.snapshot(), 2);
        assert_eq!(held.retime.frame_hold, Some(ClipFrameHold { source_tick: Ticks::new(frame * F), anchor }), "{at:?}");
        assert_eq!((held.source_in, held.source_out, held.duration), (Ticks::new(100 * F), Ticks::new(130 * F), Ticks::new(30 * F)), "the window stays");
        assert_held_everywhere(&held, frame);
    }
    let holding_three = |si: u64, so: u64, at: HoldAt| {
        let s = snapshot(|t| {
            let mut c = clip(1, si, so, 0);
            c.markers.push(marker("three", 3));
            t[0].clips[0] = c;
        });
        let p = hold(&s, 1, at, None).unwrap();
        assert_held_everywhere(&get(p.snapshot(), 1), 3);
    };
    holding_three(3, 7, HoldAt::InPoint);
    holding_three(0, 4, HoldAt::OutPoint);
    holding_three(0, 4, HoldAt::Playhead(Ticks::new(3 * F)));
    holding_three(0, 4, HoldAt::ClipMarker("three".into()));
    // Every interpolation choice: a held clip is one real frame, never an estimation.
    for method in [FrameInterpolation::FrameBlending, FrameInterpolation::OpticalFlow] {
        let s = snapshot(|t| t[0].clips[1].retime.frame_interpolation = method);
        let held = get(hold(&s, 2, HoldAt::InPoint, None).unwrap().snapshot(), 2);
        assert_held_everywhere(&held, 100);
        assert!(!answer(&held, 45 * F).is_estimation());
    }
}

/// The out point is exclusive, so the frame at the out point is the last one that starts before it
/// (declared reading, no frame past the window); in and out are the source bounds at any speed.
#[test]
fn out_point_and_reverse_and_source_rate_readings() {
    let exact = hold(&snapshot(|_| {}), 2, HoldAt::OutPoint, None).unwrap();
    assert_eq!(get(exact.snapshot(), 2).retime.frame_hold.unwrap().source_tick, Ticks::new(129 * F), "source_out itself is outside the window");
    let off = snapshot(|t| {
        let c = &mut t[0].clips[1];
        c.source_out = Ticks::new(130 * F - F / 2);
        c.duration = Ticks::new(30 * F - F / 2);
    });
    assert_eq!(get(hold(&off, 2, HoldAt::OutPoint, None).unwrap().snapshot(), 2).retime.frame_hold.unwrap().source_tick, Ticks::new(129 * F), "the frame that starts before the out point");
    let one_tick = snapshot(|t| {
        let c = &mut t[0].clips[1];
        c.source_out = Ticks::new(129 * F + 1);
        c.duration = Ticks::new(29 * F + 1);
    });
    assert_eq!(get(hold(&one_tick, 2, HoldAt::OutPoint, None).unwrap().snapshot(), 2).retime.frame_hold.unwrap().source_tick, Ticks::new(129 * F), "one tick of a frame is that frame");

    let reversed = snapshot(|t| t[0].clips[1].speed = Speed { numerator: -1, denominator: 1 });
    let source_frame = |at: HoldAt| get(hold(&reversed, 2, at, None).unwrap().snapshot(), 2).retime.frame_hold.unwrap().source_tick;
    assert_eq!(source_frame(HoldAt::InPoint), Ticks::new(100 * F), "the source in point, not the timeline head");
    assert_eq!(source_frame(HoldAt::OutPoint), Ticks::new(129 * F));
    assert_eq!(source_frame(HoldAt::Playhead(Ticks::new(40 * F))), Ticks::new(129 * F), "the reversed head shows the last frame");
    assert_eq!(source_frame(HoldAt::Playhead(Ticks::new(41 * F))), Ticks::new(128 * F));

    // A 24 fps source: 100 frames of 25 fps = 96 frames of 24 fps exactly; the frame at the out
    // point of 130 F = 124.8 frames of 24 fps is frame 124.
    let g24 = SourceFrameGrid::new(G24).unwrap();
    let s = snapshot(|_| {});
    let frame_of = |at: HoldAt| {
        let edit = SetFrameHold { clip_id: cid(2), at, grid: g24 };
        get(with_ctx(|ctx| plan_set_frame_hold(&s, "seq-main", 3, &edit, None, ctx)).unwrap().snapshot(), 2)
    };
    let held_in = frame_of(HoldAt::InPoint);
    assert_eq!(held_in.retime.frame_hold.unwrap().source_tick, Ticks::new(96 * G24));
    let held_out = frame_of(HoldAt::OutPoint);
    assert_eq!(held_out.retime.frame_hold.unwrap().source_tick, Ticks::new(124 * G24));
    assert_eq!(resolve_source_frames(&held_out, Ticks::new(50 * F), g24, &Unknown).unwrap(), SourceFrameAnswer::Held(SourceFrameRef { index: 124, start: Ticks::new(124 * G24) }));
}

/// Set and clear are each one proposal with one inverse: one revision step, Folio-validated, undone
/// to the exact prior sequence, redone by the inverse's inverse; repeating is `no_change`.
#[test]
fn set_and_clear_are_one_step_with_one_inverse() {
    let s = snapshot(|_| {});
    let set = hold(&s, 2, HoldAt::InPoint, None).unwrap();
    assert_eq!((set.base_revision(), set.target_revision()), (3, 4));
    assert_eq!(set.snapshot().document().revision, 4);
    assert_eq!(s.document().revision, 3, "pulse commits nothing");
    assert_eq!(get(&s, 2).retime, ClipRetime::default(), "the input is never modified");
    assert_eq!(set.inverse().restore(), seq(&s));
    assert_eq!(set.inverse().expect(), seq(set.snapshot()));
    let undone = revert(&set);
    assert_eq!(seq(undone.snapshot()), seq(&s), "undo restores the exact sequence");
    let redone = revert(&undone);
    assert_eq!(seq(redone.snapshot()), seq(set.snapshot()), "the inverse of the undo is the redo");
    assert_eq!(hold(set.snapshot(), 2, HoldAt::InPoint, None).unwrap_err().code(), "no_change");
    // Another origin replaces the hold in one step.
    let moved = hold(set.snapshot(), 2, HoldAt::OutPoint, None).unwrap();
    assert_eq!(get(moved.snapshot(), 2).retime.frame_hold.unwrap().source_tick, Ticks::new(129 * F));
    // Clear: the clip follows its window again.
    let cleared = clear(set.snapshot(), 2).unwrap();
    assert_eq!(seq(cleared.snapshot()), seq(&s));
    assert_eq!(cleared.target_revision(), 5);
    assert_eq!(answer(&get(cleared.snapshot(), 2), 45 * F), SourceFrameAnswer::Sampled(f(105)));
    assert_eq!(seq(revert(&cleared).snapshot()), seq(set.snapshot()));
    assert_eq!(clear(&s, 2).unwrap_err().code(), "no_change");
    // The window is untouched, so the clip change list stays empty (as for a reverse-only change).
    assert!(set.changes().is_empty());
}

/// [STU-VID-031a] `insert frame hold segment` "splits out a held span as its own clip": the outer
/// pieces keep their windows and frames, the held span is an ordinary clip, nothing moves.
#[test]
fn insert_frame_hold_segment_splits_out_a_held_span() {
    let s = snapshot(|_| {});
    let p = segment(&s, 2, 45, 49, HoldAt::Playhead(Ticks::new(45 * F)), &[20, 21], &[]).unwrap();
    assert_eq!(p.target_revision(), 4);
    let shape = |c: &StudioClip| (c.source_in.value() / F, c.source_out.value() / F, c.timeline_start.value() / F, c.duration.value() / F);
    let (left, held, right) = (get(p.snapshot(), 2), get(p.snapshot(), 20), get(p.snapshot(), 21));
    assert_eq!(shape(&left), (100, 105, 40, 5));
    assert_eq!(shape(&held), (105, 109, 45, 4));
    assert_eq!(shape(&right), (109, 130, 49, 21));
    assert_eq!(left.retime, ClipRetime::default());
    assert_eq!(right.retime, ClipRetime::default());
    assert_eq!(held.retime.frame_hold, Some(ClipFrameHold { source_tick: Ticks::new(105 * F), anchor: FrameHoldAnchor::Playhead }));
    assert_held_everywhere(&held, 105);
    // The time table around the span: 104 | 105 105 105 105 | 109.
    assert_eq!(answer(&left, 44 * F), SourceFrameAnswer::Sampled(f(104)));
    assert_eq!(answer(&right, 49 * F), SourceFrameAnswer::Sampled(f(109)));
    // Frame count and everything else preserved; the marker goes to the piece that holds it.
    assert_eq!(seq(p.snapshot()).duration, seq(&s).duration);
    assert_eq!(left.duration.value() + held.duration.value() + right.duration.value(), get(&s, 2).duration.value());
    assert_eq!(get(p.snapshot(), 3), get(&s, 3), "the next clip stays");
    assert_eq!(right.markers.len(), 1);
    assert_eq!(p.splits(), [(cid(2), cid(20)), (cid(2), cid(21))]);
    assert!(matches!(p.change_for(&cid(2)), Some(ClipChange::Retime { .. })));
    assert!(matches!(p.change_for(&cid(20)), Some(ClipChange::Insert { .. })));
    assert!(matches!(p.change_for(&cid(21)), Some(ClipChange::Insert { .. })));
    assert_eq!(seq(revert(&p).snapshot()), seq(&s), "undo restores the exact sequence");
    // Any origin of the clip may feed the span: the out point of the whole clip.
    let far = segment(&s, 2, 45, 49, HoldAt::OutPoint, &[20, 21], &[]).unwrap();
    assert_held_everywhere(&get(far.snapshot(), 20), 129);

    // At the head there is no left piece: the clip itself is the held span, one new id.
    let head = segment(&s, 2, 40, 44, HoldAt::InPoint, &[20], &[]).unwrap();
    assert_eq!(shape(&get(head.snapshot(), 2)), (100, 104, 40, 4));
    assert_held_everywhere(&get(head.snapshot(), 2), 100);
    assert_eq!(get(head.snapshot(), 20).retime, ClipRetime::default());
    // At the tail there is no right piece.
    let tail = segment(&s, 2, 66, 70, HoldAt::OutPoint, &[20], &[]).unwrap();
    assert_eq!(shape(&get(tail.snapshot(), 2)), (100, 126, 40, 26));
    assert_held_everywhere(&get(tail.snapshot(), 20), 129);
    // The whole clip needs no id at all.
    let whole = segment(&s, 2, 40, 70, HoldAt::InPoint, &[], &[]).unwrap();
    assert_eq!(shape(&get(whole.snapshot(), 2)), (100, 130, 40, 30));
    assert_held_everywhere(&get(whole.snapshot(), 2), 100);
    // A reversed clip cut in the same way: 40..45 shows 129..125, so the span holds source 124.
    let reversed = snapshot(|t| t[0].clips[1].speed = Speed { numerator: -1, denominator: 1 });
    let r = segment(&reversed, 2, 45, 49, HoldAt::Playhead(Ticks::new(45 * F)), &[20, 21], &[]).unwrap();
    assert_eq!(shape(&get(r.snapshot(), 2)), (125, 130, 40, 5));
    assert_eq!(shape(&get(r.snapshot(), 20)), (121, 125, 45, 4));
    assert_eq!(shape(&get(r.snapshot(), 21)), (100, 121, 49, 21));
    assert_held_everywhere(&get(r.snapshot(), 20), 124);
}

/// A linked pair stays one clip per track per piece. Only the picture holds: the audio partner is
/// cut at the same ticks and keeps playing its own window (declared reading).
#[test]
fn a_held_clip_leaves_its_linked_audio_playing() {
    let link = |t: &mut Vec<StudioTrack>| {
        t[0].clips[0].link_group = Some("lg".into());
        t[1].clips[0].link_group = Some("lg".into());
    };
    let s = snapshot(link);
    // Set: the picture holds; the audio partner and its track are untouched.
    let p = hold(&s, 1, HoldAt::InPoint, None).unwrap();
    assert_eq!(seq(p.snapshot()).tracks[1], seq(&s).tracks[1]);
    assert_eq!(hold(&s, 4, HoldAt::InPoint, None).unwrap_err(), RetimeError::HoldNotVideo);
    assert_eq!(hold(&s, 4, HoldAt::InPoint, None).unwrap_err().code(), "frame_hold_not_video");
    // Segment: every member is cut at the same ticks; the groups stay one clip per track.
    let p = segment(&s, 1, 5, 9, HoldAt::Playhead(Ticks::new(5 * F)), &[20, 21, 30, 31], &["lg-2", "lg-3"]).unwrap();
    let shape = |n: u32| {
        let c = get(p.snapshot(), n);
        (c.source_in.value() / F, c.source_out.value() / F, c.timeline_start.value() / F, c.duration.value() / F, c.link_group.clone().unwrap())
    };
    assert_eq!(shape(1), (10, 15, 0, 5, "lg".to_string()));
    assert_eq!(shape(20), (15, 19, 5, 4, "lg-2".to_string()));
    assert_eq!(shape(21), (19, 30, 9, 11, "lg-3".to_string()));
    assert_eq!(shape(4), (10, 15, 0, 5, "lg".to_string()));
    assert_eq!(shape(30), (15, 19, 5, 4, "lg-2".to_string()));
    assert_eq!(shape(31), (19, 30, 9, 11, "lg-3".to_string()));
    assert_held_everywhere(&get(p.snapshot(), 20), 15);
    for n in [1, 21, 4, 30, 31] {
        assert_eq!(get(p.snapshot(), n).retime, ClipRetime::default(), "only the picture of the span holds (clip {n})");
    }
    assert_eq!(seq(revert(&p).snapshot()), seq(&s));
    // Refusals of the linked form.
    let code = |ids: &[u32], groups: &[&str]| segment(&s, 1, 5, 9, HoldAt::InPoint, ids, groups).unwrap_err().code();
    assert_eq!(code(&[20, 21], &["lg-2", "lg-3"]), "insufficient_ids", "ids for the partner are missing");
    assert_eq!(code(&[20, 21, 30, 31], &["lg-2"]), "insufficient_ids", "link groups are missing");
    assert_eq!(code(&[20, 21, 30, 31], &["lg", "lg-3"]), "duplicate_id", "a link group in use");
    assert_eq!(code(&[20, 21, 30, 31], &["lg-2", "lg-2"]), "duplicate_id");
    assert_eq!(code(&[20, 21, 30, 4], &["lg-2", "lg-3"]), "duplicate_id", "a clip id in use");
    assert_eq!(code(&[20, 21, 30, 20], &["lg-2", "lg-3"]), "duplicate_id");
    let locked = snapshot(|t| {
        link(t);
        t[1].locked = true;
    });
    assert_eq!(segment(&locked, 1, 5, 9, HoldAt::InPoint, &[20, 21, 30, 31], &["lg-2", "lg-3"]).unwrap_err().code(), "track_locked");
    // A partner that does not span the cut refuses by name: no piece is left out of its group.
    let shorter = snapshot(|t| {
        link(t);
        let a = &mut t[1].clips[0];
        a.source_out = Ticks::new(13 * F);
        a.duration = Ticks::new(3 * F);
    });
    assert_eq!(segment(&shorter, 1, 5, 9, HoldAt::InPoint, &[20, 21, 30, 31], &["lg-2", "lg-3"]).unwrap_err().code(), "outside_range");
}

/// Hostile input (V4-01/02) and every other refusal: named and producing nothing.
#[test]
fn hold_refusals_are_typed() {
    let s = snapshot(|_| {});
    let outside = RetimeError::HoldOutsideSourceWindow;
    assert_eq!(outside.code(), "frame_hold_outside_source_window");
    // The playhead off the clip maps outside the source window.
    assert_eq!(hold(&s, 2, HoldAt::Playhead(Ticks::new(39 * F)), None).unwrap_err(), outside);
    assert_eq!(hold(&s, 2, HoldAt::Playhead(Ticks::new(70 * F)), None).unwrap_err(), outside, "the clip end is exclusive");
    assert_eq!(hold(&s, 2, HoldAt::Playhead(Ticks::new(u64::MAX)), None).unwrap_err(), outside);
    assert!(hold(&s, 2, HoldAt::Playhead(Ticks::new(70 * F - 1)), None).is_ok());
    // A clip marker trimmed out of the window, and one exactly at the exclusive out.
    let trimmed = |frame: u64| snapshot(|t| t[0].clips[1].markers[0].start = Ticks::new(frame * F));
    assert_eq!(hold(&trimmed(99), 2, HoldAt::ClipMarker("m".into()), None).unwrap_err(), outside);
    assert_eq!(hold(&trimmed(130), 2, HoldAt::ClipMarker("m".into()), None).unwrap_err(), outside);
    assert!(hold(&trimmed(129), 2, HoldAt::ClipMarker("m".into()), None).is_ok());
    assert_eq!(hold(&s, 2, HoldAt::ClipMarker("nope".into()), None).unwrap_err().code(), "unknown_marker");
    assert_eq!(hold(&s, 1, HoldAt::ClipMarker("m".into()), None).unwrap_err().code(), "unknown_marker", "a marker of another clip");
    // A known source length bounds the frame.
    let edit = SetFrameHold { clip_id: cid(2), at: HoldAt::OutPoint, grid: grid() };
    let cancel = CancellationToken::default();
    let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &Long(120), cancel: &cancel };
    assert_eq!(plan_set_frame_hold(&s, "seq-main", 3, &edit, None, &ctx).unwrap_err().code(), "past_source_end");
    let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &Long(130), cancel: &cancel };
    assert!(plan_set_frame_hold(&s, "seq-main", 3, &edit, None, &ctx).is_ok());
    // Multicam, as rate.rs refuses reverse: a multicam clip, or any clip of a multicam source sequence.
    let mut records = StudioMulticamRecords::new(s.document().document_id.clone());
    records.clips.push(StudioMulticamClip { clip_id: cid(3), active_angle: 1, audio_angle: 1, audio_follows_video: true, follows_nest_setting: false, enabled: true });
    assert_eq!(hold(&s, 3, HoldAt::InPoint, Some(&records)).unwrap_err(), RetimeError::HoldMulticam);
    assert_eq!(hold(&s, 3, HoldAt::InPoint, Some(&records)).unwrap_err().code(), "frame_hold_multicam");
    assert!(hold(&s, 1, HoldAt::InPoint, Some(&records)).is_ok(), "other clips may hold");
    let insert = |multicam: &StudioMulticamRecords, n: u32, from: u64, to: u64| {
        let edit = InsertFrameHoldSegment { clip_id: cid(n), span: TickRange::new(Ticks::new(from * F), Ticks::new(to * F)).unwrap(), at: HoldAt::InPoint, grid: grid(), fresh_clip_ids: vec![cid(20), cid(21)], fresh_link_groups: vec![] };
        with_ctx(|ctx| plan_insert_frame_hold_segment(&s, "seq-main", 3, &edit, Some(multicam), ctx))
    };
    assert_eq!(insert(&records, 3, 72, 74).unwrap_err(), RetimeError::HoldMulticam);
    let mut source = StudioMulticamRecords::new(s.document().document_id.clone());
    source.multicam_sequences.push(StudioMulticamSequence { sequence_id: "seq-main".into(), selection_order: MulticamSelectionOrder::SourceOrder, angle_order: vec![seq(&s).tracks[0].track_id.clone()] });
    assert_eq!(hold(&s, 1, HoldAt::InPoint, Some(&source)).unwrap_err(), RetimeError::HoldMulticam);
    assert_eq!(insert(&source, 2, 45, 49).unwrap_err(), RetimeError::HoldMulticam);
    // Clearing never creates what multicam refuses, so it stays open.
    let held = snapshot(|t| t[0].clips[2].retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(F), anchor: FrameHoldAnchor::InPoint }));
    assert!(clear(&held, 3).is_ok());
    // Locks, ids, revision, cancellation.
    let locked = snapshot(|t| t[0].locked = true);
    assert_eq!(hold(&locked, 1, HoldAt::InPoint, None).unwrap_err().code(), "track_locked");
    assert_eq!(clear(&locked, 1).unwrap_err().code(), "track_locked");
    assert_eq!(hold(&s, 9, HoldAt::InPoint, None).unwrap_err().code(), "unknown_clip");
    assert_eq!(clear(&s, 9).unwrap_err().code(), "unknown_clip");
    let edit = SetFrameHold { clip_id: cid(1), at: HoldAt::InPoint, grid: grid() };
    assert_eq!(with_ctx(|ctx| plan_set_frame_hold(&s, "seq-main", 2, &edit, None, ctx)).unwrap_err().code(), "stale_revision");
    assert_eq!(with_ctx(|ctx| plan_set_frame_hold(&s, "seq-other", 3, &edit, None, ctx)).unwrap_err().code(), "unknown_sequence");
    let canceled = CancellationToken::default();
    canceled.cancel();
    let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &Unknown, cancel: &canceled };
    assert_eq!(plan_set_frame_hold(&s, "seq-main", 3, &edit, None, &ctx).unwrap_err().code(), "canceled");
    // Segment spans.
    let code = |from: u64, to: u64, ids: &[u32]| segment(&s, 2, from, to, HoldAt::InPoint, ids, &[]).unwrap_err().code();
    assert_eq!(code(39, 49, &[20, 21]), "outside_range", "starts before the clip");
    assert_eq!(code(45, 71, &[20, 21]), "outside_range", "ends after the clip");
    assert_eq!(code(45, 45, &[20, 21]), "outside_range", "an empty span");
    assert_eq!(code(45, 49, &[20]), "insufficient_ids");
    assert_eq!(code(45, 49, &[]), "insufficient_ids");
    assert_eq!(code(45, 49, &[3, 21]), "duplicate_id");
    assert_eq!(code(45, 49, &[20, 20]), "duplicate_id");
    let off_grid = InsertFrameHoldSegment { clip_id: cid(2), span: TickRange::new(Ticks::new(45 * F + 1), Ticks::new(49 * F)).unwrap(), at: HoldAt::InPoint, grid: grid(), fresh_clip_ids: vec![cid(20), cid(21)], fresh_link_groups: vec![] };
    assert_eq!(with_ctx(|ctx| plan_insert_frame_hold_segment(&s, "seq-main", 3, &off_grid, None, ctx)).unwrap_err().code(), "not_frame_aligned");
    assert_eq!(segment(&s, 4, 5, 9, HoldAt::InPoint, &[20, 21], &[]).unwrap_err(), RetimeError::HoldNotVideo);
    assert_eq!(segment(&s, 2, 45, 49, HoldAt::Playhead(Ticks::new(39 * F)), &[20, 21], &[]).unwrap_err(), outside);
    let bad_id = InsertFrameHoldSegment { clip_id: cid(2), span: TickRange::new(Ticks::new(45 * F), Ticks::new(49 * F)).unwrap(), at: HoldAt::InPoint, grid: grid(), fresh_clip_ids: vec!["not-an-id".into(), cid(21)], fresh_link_groups: vec![] };
    assert_eq!(with_ctx(|ctx| plan_insert_frame_hold_segment(&s, "seq-main", 3, &bad_id, None, ctx)).unwrap_err().code(), "folio_rejected");
    // A cut that is not a whole number of source ticks at the clip's speed (11 does not divide F).
    let elevenths = snapshot(|t| {
        let c = &mut t[0].clips[0];
        c.source_in = Ticks::new(0);
        c.source_out = Ticks::new(20 * F);
        c.speed = Speed { numerator: 1, denominator: 11 };
        c.duration = Ticks::new(220 * F);
        t[0].clips.truncate(1);
        t[1].clips.clear();
    });
    assert_eq!(segment(&elevenths, 1, 1, 3, HoldAt::InPoint, &[20, 21], &[]).unwrap_err().code(), "inexact");
    assert!(segment(&elevenths, 1, 11, 22, HoldAt::InPoint, &[20, 21], &[]).is_ok(), "11 timeline frames are one source frame");
    // A hostile stored hold past a known source length is refused when it is read.
    let hostile = get(&snapshot(|t| t[0].clips[1].retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(1000 * F), anchor: FrameHoldAnchor::Playhead })), 2);
    assert_eq!(resolve_source_frames(&hostile, Ticks::new(45 * F), grid(), &Long(500)).unwrap_err().code(), "past_source_end");
    assert_eq!(resolve_source_frames(&hostile, Ticks::new(45 * F), grid(), &Long(1000)).unwrap_err().code(), "past_source_end", "the extent is exclusive");
    assert!(resolve_source_frames(&hostile, Ticks::new(45 * F), grid(), &Long(1001)).is_ok());
    // A held clip still has an end: a tick off it is off it.
    assert_eq!(resolve_source_frames(&hostile, Ticks::new(70 * F), grid(), &Unknown).unwrap_err().code(), "outside_range");
}

/// The rate planners keep the record unchanged (brief item 4), and a held clip stays held at any
/// speed and direction.
#[test]
fn rate_planners_keep_the_retime_record() {
    let record = ClipRetime { frame_interpolation: FrameInterpolation::OpticalFlow, maintain_audio_pitch: true, frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(105 * F), anchor: FrameHoldAnchor::Playhead }) };
    let s = snapshot(|t| t[0].clips[1].retime = record);
    let stretch = RateStretch { clip_id: cid(2), edge: RateEdge::Head, new_edge: Ticks::new(25 * F) };
    let p = with_ctx(|ctx| plan_rate_stretch(&s, "seq-main", 3, &stretch, None, ctx)).unwrap();
    let c = get(p.snapshot(), 2);
    assert_eq!((c.speed.numerator, c.speed.denominator, c.retime), (2, 3, record));
    assert_held_everywhere(&c, 105);
    let duration = SpeedDuration { clip_id: cid(2), target: SpeedDurationTarget::Speed(Speed { numerator: 3, denominator: 1 }), reverse: true };
    let p = with_ctx(|ctx| plan_speed_duration(&s, "seq-main", 3, &duration, None, ctx)).unwrap();
    let c = get(p.snapshot(), 2);
    assert_eq!((c.speed.numerator, c.retime), (-3, record));
    assert_held_everywhere(&c, 105);
    // The same on a clip with only the choices and no hold.
    let choices = ClipRetime { frame_interpolation: FrameInterpolation::FrameBlending, maintain_audio_pitch: true, frame_hold: None };
    let s = snapshot(|t| t[0].clips[2].retime = choices);
    let tail = RateStretch { clip_id: cid(3), edge: RateEdge::Tail, new_edge: Ticks::new(80 * F) };
    let p = with_ctx(|ctx| plan_rate_stretch(&s, "seq-main", 3, &tail, None, ctx)).unwrap();
    assert_eq!(get(p.snapshot(), 3).retime, choices);
    assert_eq!(seq(revert(&p).snapshot()), seq(&s));
}

/// The property travels with the clip without any planner remembering it: razor, trim, slip, range
/// lift, subsequence and nest clone or edit clips and the hold still answers at every tick.
#[test]
fn edits_carry_the_retime_record() {
    let record = ClipRetime { frame_interpolation: FrameInterpolation::OpticalFlow, maintain_audio_pitch: true, frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(105 * F), anchor: FrameHoldAnchor::Playhead }) };
    let s = snapshot(|t| t[0].clips[1].retime = record);
    let edit = |e: TimelineEdit| with_ctx(|ctx| plan_timeline_edit(&s, "seq-main", 3, &e, ctx)).unwrap();
    // Razor: both pieces hold, though one window no longer contains the frame; Folio validated it.
    let p = edit(TimelineEdit::Razor { clip_id: cid(2), at: Ticks::new(50 * F), new_right_clip_id: cid(20), linked: None });
    let (left, right) = (get(p.snapshot(), 2), get(p.snapshot(), 20));
    assert_eq!((left.source_out, right.source_in), (Ticks::new(110 * F), Ticks::new(110 * F)));
    assert_eq!((left.retime, right.retime), (record, record));
    assert_held_everywhere(&left, 105);
    assert_held_everywhere(&right, 105);
    // Clearing one piece: it follows its own window again, the other stays held.
    let cleared = clear(p.snapshot(), 20).unwrap();
    assert_eq!(answer(&get(cleared.snapshot(), 20), 50 * F), SourceFrameAnswer::Estimated { from: f(110), to: f(111), weight: FrameWeight::ZERO });
    assert_held_everywhere(&get(cleared.snapshot(), 2), 105);
    // Trim the frame out of the window: still that frame.
    let p = edit(TimelineEdit::TrimOut { clip_id: cid(2), new_duration: Ticks::new(3 * F), ripple: false });
    let c = get(p.snapshot(), 2);
    assert_eq!((c.source_out, c.retime), (Ticks::new(103 * F), record));
    assert_held_everywhere(&c, 105);
    // Slip the window away from the frame.
    let p = edit(TimelineEdit::Slip { clip_id: cid(2), delta: TickDelta::new(-(50 * i128::from(F))).unwrap() });
    let c = get(p.snapshot(), 2);
    assert_eq!((c.source_in, c.retime), (Ticks::new(50 * F), record));
    assert_held_everywhere(&c, 105);
    // A range lift through the clip: both pieces carry it.
    let span = TickRange::new(Ticks::new(45 * F), Ticks::new(50 * F)).unwrap();
    let p = edit(TimelineEdit::LiftRange { span, fresh_clip_ids: vec![cid(20)] });
    assert_eq!((get(p.snapshot(), 2).retime, get(p.snapshot(), 20).retime), (record, record));
    // Subsequence copies and Nest moves: the clip inside the new sequence carries it.
    let ids = NestIds {
        sequence_id: "seq-sub".into(),
        sequence_name: "Nested".into(),
        track_ids: (60..64).map(|n| format!("STRK-019abcde-0000-7000-8000-{n:012x}")).collect(),
        clip_ids: (50..60).map(cid).collect(),
        link_group: None,
        record_ids: (0..8).map(|n| format!("rec-{n}")).collect(),
    };
    let inner = |snap: &Snapshot| -> StudioClip { snap.document().sequences.iter().find(|q| q.sequence_id == "seq-sub").unwrap().tracks[0].clips[0].clone() };
    let copied = with_ctx(|ctx| plan_make_subsequence(&s, "seq-main", 3, &SubsequenceSource::Clips(vec![cid(2)]), &ids, ctx)).unwrap();
    assert_eq!(inner(copied.snapshot()).retime, record);
    assert_eq!(get(copied.snapshot(), 2).retime, record, "the parent keeps its clip");
    let nested = with_ctx(|ctx| plan_nest(&s, "seq-main", 3, &[cid(2)], &ids, ctx)).unwrap();
    assert_eq!(inner(nested.snapshot()).retime, record);
    // The parent's nested clip is new and states no choice; the monitor reaches the held frame
    // through it at every tick.
    let document = nested.snapshot().document();
    let parent = document.sequences.iter().find(|q| q.sequence_id == "seq-main").unwrap();
    let outer = parent.tracks[0].clips.iter().find(|c| matches!(&c.source_ref, ClipSource::Sequence { .. })).unwrap();
    assert_eq!(outer.retime, ClipRetime::default());
    for k in [40, 45, 69] {
        let frame = frame_layers_nested(&document.sequences, parent, Ticks::new(k * F), &Unknown, 16, &CancellationToken::default()).unwrap();
        assert_eq!(frame.layers.len(), 1, "frame {k}");
        assert_eq!(frame.layers[0].layer.source_tick, Ticks::new(105 * F), "frame {k}");
    }
}

/// The monitor's own path reads the hold too: `clip_source_at` and `frame_layers` answer the held
/// frame at every tick, a stored hold past a known length is typed, an unheld clip is as before.
#[test]
fn the_program_monitor_path_reads_the_hold() {
    let s = snapshot(|_| {});
    let p = hold(&s, 2, HoldAt::Playhead(Ticks::new(45 * F)), None).unwrap();
    let held = get(p.snapshot(), 2);
    let cancel = CancellationToken::default();
    for k in 40..70 {
        let tick = Ticks::new(k * F);
        assert_eq!(clip_source_at(&held, tick, &Unknown, false).unwrap(), (Ticks::new(105 * F), false));
        let layers = frame_layers(seq(p.snapshot()), tick, &Unknown, &cancel).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!((layers[0].clip_id.as_str(), layers[0].source_tick, layers[0].held), (cid(2).as_str(), Ticks::new(105 * F), false));
    }
    // A handle tick off the clip reads the same frame (a transition's outgoing or incoming side).
    assert_eq!(clip_source_at(&held, Ticks::new(70 * F), &Unknown, false).unwrap().0, Ticks::new(105 * F));
    assert_eq!(clip_source_at(&held, Ticks::new(39 * F), &Unknown, false).unwrap().0, Ticks::new(105 * F));
    assert_eq!(clip_source_at(&held, Ticks::new(45 * F), &Long(105), false).unwrap_err().code(), "past_source_end");
    assert_eq!(frame_layers(seq(p.snapshot()), Ticks::new(45 * F), &Long(105), &cancel).unwrap_err().code(), "past_source_end");
    // Unheld clips keep the old mapping.
    assert_eq!(clip_source_at(&get(&s, 2), Ticks::new(45 * F), &Unknown, false).unwrap(), (Ticks::new(105 * F), false));
    assert_eq!(clip_source_at(&get(&s, 2), Ticks::new(46 * F), &Unknown, false).unwrap(), (Ticks::new(106 * F), false));
    assert_eq!(clip_source_at(&get(p.snapshot(), 1), Ticks::new(3 * F), &Unknown, false).unwrap(), (Ticks::new(13 * F), false));
}

/// `Maintain audio pitch` "is an independent boolean" and the interpolation is a stated choice: one
/// proposal and one inverse each, changing only what is stated.
#[test]
fn retime_options_are_independent_and_undoable() {
    let s = snapshot(|_| {});
    let set = |s: &Snapshot, n: u32, method: Option<FrameInterpolation>, pitch: Option<bool>| {
        let edit = RetimeOptions { clip_id: cid(n), frame_interpolation: method, maintain_audio_pitch: pitch };
        with_ctx(|ctx| plan_set_retime_options(s, "seq-main", s.document().revision, &edit, ctx))
    };
    let p = set(&s, 2, Some(FrameInterpolation::OpticalFlow), None).unwrap();
    assert_eq!(p.target_revision(), 4);
    assert_eq!(get(p.snapshot(), 2).retime, ClipRetime { frame_interpolation: FrameInterpolation::OpticalFlow, maintain_audio_pitch: false, frame_hold: None });
    let q = set(p.snapshot(), 2, None, Some(true)).unwrap();
    assert_eq!(get(q.snapshot(), 2).retime, ClipRetime { frame_interpolation: FrameInterpolation::OpticalFlow, maintain_audio_pitch: true, frame_hold: None });
    assert_eq!(seq(revert(&q).snapshot()), seq(p.snapshot()));
    assert_eq!(seq(revert(&p).snapshot()), seq(&s));
    // Back to the defaults is the original sequence again, byte for byte on the wire.
    let back = set(q.snapshot(), 2, Some(FrameInterpolation::FrameSampling), Some(false)).unwrap();
    assert_eq!(seq(back.snapshot()), seq(&s));
    assert!(!String::from_utf8_lossy(back.snapshot().encoded_bytes()).contains("retime"));
    // A hold is untouched by the choices; the audio clip takes the pitch boolean.
    let held = hold(&s, 2, HoldAt::InPoint, None).unwrap();
    let r = set(held.snapshot(), 2, Some(FrameInterpolation::FrameBlending), Some(true)).unwrap();
    assert_eq!(get(r.snapshot(), 2).retime.frame_hold, get(held.snapshot(), 2).retime.frame_hold);
    assert!(get(set(&s, 4, None, Some(true)).unwrap().snapshot(), 4).retime.maintain_audio_pitch);
    // Refusals.
    assert_eq!(set(&s, 2, None, None).unwrap_err().code(), "no_change");
    assert_eq!(set(&s, 2, Some(FrameInterpolation::FrameSampling), Some(false)).unwrap_err().code(), "no_change");
    assert_eq!(set(&s, 9, None, Some(true)).unwrap_err().code(), "unknown_clip");
    let locked = snapshot(|t| t[0].locked = true);
    assert_eq!(set(&locked, 1, None, Some(true)).unwrap_err().code(), "track_locked");
    let edit = RetimeOptions { clip_id: cid(1), frame_interpolation: None, maintain_audio_pitch: Some(true) };
    assert_eq!(with_ctx(|ctx| plan_set_retime_options(&s, "seq-main", 2, &edit, ctx)).unwrap_err().code(), "stale_revision");
}

#[test]
fn descriptor_states_the_retime_surface() {
    for word in ["plan_set_frame_hold", "plan_clear_frame_hold", "plan_insert_frame_hold_segment", "plan_set_retime_options", "resolve_source_frames", "frame_hold_multicam", "frame_hold_outside_source_window", "frame_hold_not_video", "estimation"] {
        assert!(hsk_studio_pulse::DESCRIPTOR.contains(word), "{word}");
    }
}
