//! Lane SMB-PR-RATE-STRETCH-1: rate stretch and Speed/Duration planners ([STU-VID-026] "Rate
//! stretch", [STU-VID-031]). Oracles: hand-computed ticks at 25 fps (F ticks per frame), exact
//! reduced speeds, the Folio-validated prospective document and `plan_timeline_revert` restoring the
//! exact prior sequence.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

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
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
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

/// V1: clip 1 source [10, 30) at [0, 20); clip 2 source [100, 130) at [40, 70); clip 3 source
/// [0, 20) at [70, 90) (abuts clip 2). A1: clip 4 source [10, 30) at [0, 20).
fn snapshot(mutate: impl FnOnce(&mut Vec<StudioTrack>)) -> Snapshot {
    let mut tracks = vec![track(1, TrackKind::Video, vec![clip(1, 10, 30, 0), clip(2, 100, 130, 40), clip(3, 0, 20, 70)]), track(2, TrackKind::Audio, vec![clip(4, 10, 30, 0)])];
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

fn with_ctx<T>(f: impl FnOnce(&EditContext) -> T) -> T {
    let cancel = CancellationToken::default();
    f(&EditContext { budget: Budget::default(), resolver: &Media, extents: &Unknown, cancel: &cancel })
}

fn stretch(s: &Snapshot, n: u32, edge: RateEdge, frames: u64, mc: Option<&StudioMulticamRecords>) -> RateResult<EditProposal> {
    let edit = RateStretch { clip_id: cid(n), edge, new_edge: Ticks::new(frames * F) };
    with_ctx(|ctx| plan_rate_stretch(s, "seq-main", s.document().revision, &edit, mc, ctx))
}

fn speed_duration(s: &Snapshot, n: u32, target: SpeedDurationTarget, reverse: bool, mc: Option<&StudioMulticamRecords>) -> RateResult<EditProposal> {
    let edit = SpeedDuration { clip_id: cid(n), target, reverse };
    with_ctx(|ctx| plan_speed_duration(s, "seq-main", s.document().revision, &edit, mc, ctx))
}

fn get(s: &Snapshot, n: u32) -> StudioClip {
    s.document().sequences[0].tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).expect("clip present").clone()
}

/// (source in, source out, timeline start, duration) in frames, and the speed.
fn shape(s: &Snapshot, n: u32) -> ([u64; 4], (i64, u64)) {
    let c = get(s, n);
    ([c.source_in.value() / F, c.source_out.value() / F, c.timeline_start.value() / F, c.duration.value() / F], (c.speed.numerator, c.speed.denominator))
}

fn sp(numerator: i64, denominator: u64) -> Speed {
    Speed { numerator, denominator }
}

/// Tail of clip 1 dragged from 20 to 40: the source window [10, 30) stays, duration 40, speed
/// 20/40 = 1/2. Head of clip 2 dragged from 40 to 25: end stays 70, duration 45, speed 30/45 = 2/3.
/// One revision step each; `plan_timeline_revert` restores the exact prior sequence.
#[test]
fn rate_stretch_keeps_the_source_window_and_derives_speed() {
    let s = snapshot(|_| {});
    let p = stretch(&s, 1, RateEdge::Tail, 40, None).unwrap();
    assert_eq!(p.target_revision(), s.document().revision + 1);
    assert_eq!(shape(p.snapshot(), 1), ([10, 30, 0, 40], (1, 2)));
    assert_eq!(shape(p.snapshot(), 2), ([100, 130, 40, 30], (1, 1)), "neighbours untouched (no ripple)");
    let back = with_ctx(|ctx| plan_timeline_revert(p.snapshot(), "seq-main", p.target_revision(), p.inverse(), ctx)).unwrap();
    assert_eq!(back.snapshot().document().sequences[0], s.document().sequences[0], "undo restores the exact sequence");
    let p = stretch(&s, 2, RateEdge::Head, 25, None).unwrap();
    assert_eq!(shape(p.snapshot(), 2), ([100, 130, 25, 45], (2, 3)));
    assert_eq!(get(p.snapshot(), 2).timeline_end(), Some(Ticks::new(70 * F)), "the other edge stays");
    // Shortening: tail of clip 3 from 90 to 80 -> duration 10, speed 2.
    let p = stretch(&s, 3, RateEdge::Tail, 80, None).unwrap();
    assert_eq!(shape(p.snapshot(), 3), ([0, 20, 70, 10], (2, 1)));
    assert!(matches!(p.change_for(&cid(3)), Some(ClipChange::Retime { .. })));
}

/// Speed/Duration: 200 % on clip 3 derives duration 10; duration 20 derives speed 1 again (the
/// exact original sequence); 1/3 derives 60 and back; the percent text parses exactly.
#[test]
fn speed_duration_round_trip_exact() {
    let s = snapshot(|_| {});
    let fast = speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(2, 1)), false, None).unwrap();
    assert_eq!(shape(fast.snapshot(), 3), ([0, 20, 70, 10], (2, 1)));
    let back = speed_duration(fast.snapshot(), 3, SpeedDurationTarget::Duration(Ticks::new(20 * F)), false, None).unwrap();
    assert_eq!(back.snapshot().document().sequences[0], s.document().sequences[0], "round trip is exact");
    let slow = speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(1, 3)), false, None).unwrap();
    assert_eq!(shape(slow.snapshot(), 3), ([0, 20, 70, 60], (1, 3)));
    let back = speed_duration(slow.snapshot(), 3, SpeedDurationTarget::Duration(Ticks::new(20 * F)), false, None).unwrap();
    assert_eq!(shape(back.snapshot(), 3), ([0, 20, 70, 20], (1, 1)));
    assert_eq!(speed_from_percent("50"), Some(sp(1, 2)));
    assert_eq!(speed_from_percent(" 33.5 % "), Some(sp(67, 200)));
    assert_eq!(speed_from_percent("200.000"), Some(sp(2, 1)));
    for junk in ["", "0", "-50", "1e3", "12.3456789", "abc", ".", "99999999999999999999"] {
        assert_eq!(speed_from_percent(junk), None, "{junk:?}");
    }
    assert_eq!(speed_percent_label(sp(1, 3)), "33.33");
    assert_eq!(speed_percent_label(sp(-2, 1)), "-200.00");
    // A non-positive or unreduced speed magnitude is refused by name.
    assert_eq!(speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(-1, 1)), false, None).unwrap_err().code(), "invalid_speed");
    assert_eq!(speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(2, 2)), false, None).unwrap_err().code(), "invalid_speed");
}

/// [STU-VID-031] "Reverse is a negative speed": reverse at 100 % keeps window and duration and makes
/// the speed -1; a rate stretch of the reversed clip keeps the sign; forward again restores it.
#[test]
fn reverse_is_a_negative_speed() {
    let s = snapshot(|_| {});
    let rev = speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(1, 1)), true, None).unwrap();
    assert_eq!(shape(rev.snapshot(), 3), ([0, 20, 70, 20], (-1, 1)));
    assert!(get(rev.snapshot(), 3).speed.is_reversed());
    let stretched = stretch(rev.snapshot(), 3, RateEdge::Tail, 110, None).unwrap();
    assert_eq!(shape(stretched.snapshot(), 3), ([0, 20, 70, 40], (-1, 2)), "the sign is kept");
    let fwd = speed_duration(rev.snapshot(), 3, SpeedDurationTarget::Speed(sp(1, 1)), false, None).unwrap();
    assert_eq!(fwd.snapshot().document().sequences[0], s.document().sequences[0]);
    // Reverse together with a new speed: 50 % reverse -> duration 40, speed -1/2.
    let p = speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(1, 2)), true, None).unwrap();
    assert_eq!(shape(p.snapshot(), 3), ([0, 20, 70, 40], (-1, 2)));
}

/// Every refusal is named and produces nothing: overlap with the abutting neighbour, below one
/// frame, an off-grid derived duration on video, an unrepresentable speed, reverse on a multicam
/// clip (or in a multicam source sequence), a locked track, an unknown clip, a stale revision.
#[test]
fn stretch_refusals_are_named() {
    let s = snapshot(|_| {});
    assert_eq!(stretch(&s, 2, RateEdge::Tail, 71, None).unwrap_err(), RateError::Overlap);
    assert_eq!(stretch(&s, 2, RateEdge::Tail, 71, None).unwrap_err().code(), "rate_stretch_overlap");
    assert_eq!(stretch(&s, 2, RateEdge::Head, 19, None).unwrap_err().code(), "rate_stretch_overlap");
    assert_eq!(stretch(&s, 1, RateEdge::Tail, 0, None).unwrap_err().code(), "rate_stretch_below_one_frame");
    assert_eq!(stretch(&s, 2, RateEdge::Head, 70, None).unwrap_err().code(), "rate_stretch_below_one_frame");
    assert_eq!(speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(3, 1)), false, None).unwrap_err().code(), "not_frame_aligned");
    assert_eq!(speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(1_000_003, 1)), false, None).unwrap_err().code(), "rate_stretch_unrepresentable");
    let edit = RateStretch { clip_id: cid(3), edge: RateEdge::Tail, new_edge: Ticks::new(90 * F + F / 2) };
    assert_eq!(with_ctx(|ctx| plan_rate_stretch(&s, "seq-main", 3, &edit, None, ctx)).unwrap_err().code(), "not_frame_aligned");
    // Multicam: a record for clip 3, or the sequence declared as a multicam source.
    let mut records = StudioMulticamRecords::new(s.document().document_id.clone());
    records.clips.push(StudioMulticamClip { clip_id: cid(3), active_angle: 1, audio_angle: 1, audio_follows_video: true, follows_nest_setting: false, enabled: true });
    assert_eq!(speed_duration(&s, 3, SpeedDurationTarget::Speed(sp(1, 1)), true, Some(&records)).unwrap_err().code(), "rate_stretch_multicam_reverse");
    let fwd = stretch(&s, 3, RateEdge::Tail, 110, Some(&records)).unwrap();
    assert_eq!(shape(fwd.snapshot(), 3).1, (1, 2), "forward speed on a multicam clip is admitted");
    assert!(speed_duration(&s, 1, SpeedDurationTarget::Speed(sp(1, 1)), true, Some(&records)).is_ok(), "other clips may reverse");
    let locked = snapshot(|t| t[0].locked = true);
    assert_eq!(stretch(&locked, 1, RateEdge::Tail, 30, None).unwrap_err().code(), "track_locked");
    assert_eq!(stretch(&s, 9, RateEdge::Tail, 30, None).unwrap_err().code(), "unknown_clip");
    let edit = RateStretch { clip_id: cid(1), edge: RateEdge::Tail, new_edge: Ticks::new(30 * F) };
    assert_eq!(with_ctx(|ctx| plan_rate_stretch(&s, "seq-main", 2, &edit, None, ctx)).unwrap_err().code(), "stale_revision");
    assert_eq!(stretch(&s, 1, RateEdge::Tail, 20, None).unwrap_err().code(), "no_change");
}

/// Linked partners stretch together: clip 1 (V1) and clip 4 (A1) share a link group; a tail
/// stretch of clip 1 to 40 gives both duration 40 and speed 1/2 in one step; a head stretch moves
/// both heads; a partner on a locked track refuses the whole edit.
#[test]
fn linked_clips_stretch_together() {
    let link = |t: &mut Vec<StudioTrack>| {
        t[0].clips[0].link_group = Some("lg".into());
        t[1].clips[0].link_group = Some("lg".into());
    };
    let s = snapshot(link);
    let p = stretch(&s, 1, RateEdge::Tail, 40, None).unwrap();
    assert_eq!(shape(p.snapshot(), 1), ([10, 30, 0, 40], (1, 2)));
    assert_eq!(shape(p.snapshot(), 4), ([10, 30, 0, 40], (1, 2)), "the partner follows");
    let shorter = stretch(&s, 4, RateEdge::Tail, 10, None).unwrap();
    assert_eq!((shape(shorter.snapshot(), 1).1, shape(shorter.snapshot(), 4).1), ((2, 1), (2, 1)));
    let locked = snapshot(|t| {
        link(t);
        t[1].locked = true;
    });
    assert_eq!(stretch(&locked, 1, RateEdge::Tail, 40, None).unwrap_err().code(), "track_locked");
}
