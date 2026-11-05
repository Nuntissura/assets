//! Lane SMB-PR-NUDGE-1: up and down by one track (STU-VID-035a). The oracle is the stored clip before
//! the edit and hand-placed tracks, never the planner's own diff. Vendor behaviour UNVERIFIED (D5):
//! the readings these tests pin are stated in `src/nudge.rs`.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;
const SEQ: &str = "seq-main";
const V1: u32 = 1;
const A1: u32 = 2;
const V2: u32 = 3;
const CAPTIONS: u32 = 4;
const A2: u32 = 5;
const V3: u32 = 6;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    id("SCLP", n)
}
fn tid(n: u32) -> String {
    id("STRK", n)
}
fn at(frames: u64) -> Ticks {
    Ticks::new(frames * F)
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
struct Long;
impl SourceExtents for Long {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(at(1000))
    }
}

fn with_ctx<R>(f: impl FnOnce(&EditContext) -> R) -> R {
    let cancel = CancellationToken::default();
    f(&EditContext { budget: Budget::default(), resolver: &Media, extents: &Long, cancel: &cancel })
}

/// `frames` long at frame `start`, reading its source from frame 10.
fn clip(n: u32, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: format!("manifest-{n}"),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: at(10),
        source_out: at(10 + frames),
        timeline_start: at(start),
        duration: at(frames),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime::default(),
    }
}
fn linked(n: u32, start: u64, frames: u64, group: &str) -> StudioClip {
    StudioClip { link_group: Some(group.into()), ..clip(n, start, frames) }
}
fn dissolve(transition_id: &str) -> TimelineTransition {
    TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: transition_id.into(),
        transition_kind: "cross_dissolve".into(),
        duration: at(2),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    }
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        // Every track is sync locked: a track nudge shifts nothing, whatever the locks say.
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    }
}

/// The stacks are interleaved in the list, so "next of its kind" is not "next in the list". `edit`
/// places the clips of a test; every track starts empty.
fn fixture(edit: impl FnOnce(&mut [StudioTrack])) -> Snapshot {
    let mut tracks = vec![
        track(V1, TrackKind::Video, 0, Vec::new()),
        track(A1, TrackKind::Audio, 0, Vec::new()),
        track(V2, TrackKind::Video, 1, Vec::new()),
        track(CAPTIONS, TrackKind::Caption, 0, Vec::new()),
        track(A2, TrackKind::Audio, 1, Vec::new()),
        track(V3, TrackKind::Video, 2, Vec::new()),
    ];
    edit(&mut tracks);
    let unassigned = || ProfileBinding::Unassigned(());
    let end = tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_end().unwrap()).max().unwrap_or(at(0));
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
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
            sequence_id: SEQ.into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: end,
            tracks,
            markers: vec![],
            work_area: None,
        }],
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}
fn on(tracks: &mut [StudioTrack], n: u32) -> &mut Vec<StudioClip> {
    &mut tracks.iter_mut().find(|t| t.track_id == tid(n)).unwrap().clips
}
fn locked(tracks: &mut [StudioTrack], n: u32) {
    tracks.iter_mut().find(|t| t.track_id == tid(n)).unwrap().locked = true;
}

fn seq(s: &Snapshot) -> &StudioSequence {
    &s.document().sequences[0]
}
fn rev(s: &Snapshot) -> u64 {
    s.document().revision
}
fn request(list: &[u32], direction: TrackNudge, linked: LinkedSelection) -> NudgeTrack {
    NudgeTrack { clip_ids: list.iter().map(|n| cid(*n)).collect(), direction, linked }
}
fn plan(s: &Snapshot, list: &[u32], direction: TrackNudge, linked: LinkedSelection) -> NudgeResult<EditProposal> {
    with_ctx(|ctx| plan_nudge_track(s, SEQ, rev(s), &request(list, direction, linked), ctx))
}
fn up(s: &Snapshot, list: &[u32]) -> NudgeResult<EditProposal> {
    plan(s, list, TrackNudge::Up, LinkedSelection::On)
}
fn down(s: &Snapshot, list: &[u32]) -> NudgeResult<EditProposal> {
    plan(s, list, TrackNudge::Down, LinkedSelection::On)
}
fn placed(s: &Snapshot, n: u32) -> (String, StudioClip) {
    seq(s).tracks.iter().find_map(|t| t.clips.iter().find(|c| c.clip_id == cid(n)).map(|c| (t.track_id.clone(), c.clone()))).unwrap_or_else(|| panic!("clip {n} missing"))
}
fn track_of(p: &EditProposal, n: u32) -> String {
    placed(p.snapshot(), n).0
}
fn undo_restores(base: &Snapshot, p: &EditProposal) {
    let done = p.snapshot();
    let undo = with_ctx(|ctx| plan_timeline_revert(done, SEQ, rev(done), p.inverse(), ctx)).unwrap();
    assert_eq!(seq(undo.snapshot()), seq(base), "undo restores the exact sequence");
}

/// A clip that holds a frame, states every retime choice, is disabled and has a marker; one at half
/// speed; one reversed. Each arrives equal to what left, and no other clip changes.
#[test]
fn a_track_nudge_changes_only_the_track_of_a_clip() {
    let mut held = clip(1, 0, 20);
    held.enabled = false;
    held.retime = ClipRetime {
        frame_interpolation: FrameInterpolation::FrameBlending,
        maintain_audio_pitch: true,
        frame_hold: Some(ClipFrameHold { source_tick: at(17), anchor: FrameHoldAnchor::Playhead }),
    };
    held.markers.push(StudioMarker {
        marker_id: "mk-1".into(),
        name: "keep".into(),
        comment: String::new(),
        color: LabelColor::Rose,
        start: at(12),
        duration: Ticks::new(0),
        role: None,
    });
    let mut slow = clip(2, 30, 10);
    slow.speed = Speed { numerator: 1, denominator: 2 };
    slow.duration = at(20);
    let mut reversed = clip(3, 60, 10);
    reversed.speed = Speed { numerator: -1, denominator: 1 };
    let s = fixture(|t| {
        on(t, V1).extend([held, slow, reversed, clip(4, 80, 5)]);
        on(t, A1).push(clip(5, 0, 90));
    });
    for n in [1, 2, 3] {
        let (from, before) = placed(&s, n);
        assert_eq!(from, tid(V1));
        let p = up(&s, &[n]).unwrap();
        assert_eq!(placed(p.snapshot(), n), (tid(V2), before.clone()), "clip {n} moved whole");
        assert_eq!(p.target_revision(), rev(&s) + 1, "one revision step");
        for other in [1, 2, 3, 4, 5].into_iter().filter(|o| *o != n) {
            assert_eq!(placed(p.snapshot(), other), placed(&s, other), "clip {other} stays");
        }
        assert_eq!(seq(p.snapshot()).duration, seq(&s).duration);
        undo_restores(&s, &p);
    }
    let shown = |c: &StudioClip, frames: u64| clip_source_at(c, Ticks::new(c.timeline_start.value() + frames * F), &Long, false).unwrap().0;
    let p = up(&s, &[1]).unwrap();
    assert_eq!(shown(&placed(p.snapshot(), 1).1, 15), at(17), "the hold still shows its frame");
}

/// Up on caption and video goes to the next higher index, up on audio to the next lower: the rows
/// harness-view's `track_order` draws. Tracks of another kind in the list are skipped.
#[test]
fn up_and_down_are_the_drawn_rows_and_skip_other_kinds() {
    let s = fixture(|t| {
        on(t, V1).push(clip(1, 0, 5));
        on(t, V2).push(clip(2, 10, 5));
        on(t, V3).push(clip(3, 20, 5));
        on(t, A1).push(clip(4, 0, 5));
        on(t, A2).push(clip(5, 10, 5));
    });
    assert_eq!(track_of(&up(&s, &[1]).unwrap(), 1), tid(V2), "past A1 in the list");
    assert_eq!(track_of(&up(&s, &[2]).unwrap(), 2), tid(V3), "past the captions and A2 in the list");
    assert_eq!(track_of(&down(&s, &[2]).unwrap(), 2), tid(V1));
    assert_eq!(track_of(&down(&s, &[3]).unwrap(), 3), tid(V2));
    assert_eq!(track_of(&up(&s, &[5]).unwrap(), 5), tid(A1), "A1 is drawn above A2");
    assert_eq!(track_of(&down(&s, &[4]).unwrap(), 4), tid(A2));
}

#[test]
fn no_track_that_way_is_refused_by_name() {
    let s = fixture(|t| {
        on(t, V1).push(clip(1, 0, 5));
        on(t, V3).push(clip(2, 0, 5));
        on(t, A1).push(clip(3, 0, 5));
        on(t, A2).push(clip(4, 0, 5));
    });
    for (result, n) in [(down(&s, &[1]), 1), (up(&s, &[2]), 2), (up(&s, &[3]), 3), (down(&s, &[4]), 4)] {
        let e = result.unwrap_err();
        assert_eq!(e, NudgeError::NoTrackThatWay { clip_id: cid(n) });
        assert_eq!((e.code(), e.clip_id()), ("nudge_no_track", Some(cid(n).as_str())));
        assert_eq!(e.failure_code(), Some(hsk_studio_observe::FailureCode::Validation));
    }
}

#[test]
fn a_locked_track_either_way_is_refused_by_name() {
    let from = fixture(|t| {
        on(t, V1).push(clip(1, 0, 5));
        locked(t, V1);
    });
    assert_eq!(up(&from, &[1]).unwrap_err(), NudgeError::TrackLocked { clip_id: cid(1), track_id: tid(V1) });
    let to = fixture(|t| {
        on(t, V1).push(clip(1, 0, 5));
        locked(t, V2);
    });
    let e = up(&to, &[1]).unwrap_err();
    assert_eq!(e, NudgeError::TrackLocked { clip_id: cid(1), track_id: tid(V2) });
    assert_eq!(e.code(), "track_locked");
}

/// An occupied range refuses, naming both clips; a range that only touches is free; nothing is
/// overwritten, trimmed or shifted to make room.
#[test]
fn an_occupied_range_is_refused_by_name_and_touching_is_free() {
    let s = fixture(|t| {
        on(t, V1).extend([clip(1, 100, 10), clip(2, 200, 10), clip(3, 300, 10)]);
        on(t, V2).extend([clip(4, 105, 10), clip(5, 190, 10), clip(6, 305, 2)]);
    });
    let e = up(&s, &[1]).unwrap_err();
    assert_eq!(e, NudgeError::RangeOccupied { clip_id: cid(1), occupied_by: cid(4) });
    assert_eq!(e.code(), "nudge_range_occupied");
    assert_eq!(up(&s, &[3]).unwrap_err(), NudgeError::RangeOccupied { clip_id: cid(3), occupied_by: cid(6) }, "inside the moved clip");
    let p = up(&s, &[2]).unwrap();
    assert_eq!(track_of(&p, 2), tid(V2), "ends exactly where clip 2 starts");
    assert_eq!(placed(p.snapshot(), 5), placed(&s, 5));
    let on_v2: Vec<String> = seq(p.snapshot()).tracks.iter().find(|t| t.track_id == tid(V2)).unwrap().clips.iter().map(|c| c.clip_id.clone()).collect();
    assert_eq!(on_v2, [cid(4), cid(5), cid(2), cid(6)], "the track stays in time order");
}

/// The selection moves as one step: a range another selected clip leaves is free, and one refusal
/// leaves nothing.
#[test]
fn a_selection_moves_as_one_step() {
    let s = fixture(|t| {
        on(t, V1).push(clip(1, 100, 10));
        on(t, V2).push(clip(2, 100, 10));
        on(t, V3).push(clip(3, 300, 10));
    });
    let p = up(&s, &[1, 2]).unwrap();
    assert_eq!((track_of(&p, 1), track_of(&p, 2)), (tid(V2), tid(V3)));
    assert_eq!(p.target_revision(), rev(&s) + 1);
    undo_restores(&s, &p);
    assert_eq!(up(&s, &[1]).unwrap_err().code(), "nudge_range_occupied", "clip 2 stays in the way when it is not selected");
    assert_eq!(up(&s, &[2, 3]).unwrap_err(), NudgeError::NoTrackThatWay { clip_id: cid(3) });
}

/// `On`: the whole group takes one index step, so V1 + A1 go to V2 + A2 whichever way up the named
/// clip asks. A partner that cannot go refuses the whole edit, naming the partner.
#[test]
fn link_partners_follow_with_one_index_step() {
    let s = fixture(|t| {
        on(t, V1).push(linked(1, 500, 10, "lg-1"));
        on(t, A1).push(linked(2, 500, 10, "lg-1"));
    });
    let p = up(&s, &[1]).unwrap();
    assert_eq!((track_of(&p, 1), track_of(&p, 2)), (tid(V2), tid(A2)), "the pair stays a pair");
    assert_eq!(placed(p.snapshot(), 2).1, placed(&s, 2).1, "the partner keeps every field");
    undo_restores(&s, &p);
    let p = down(&s, &[2]).unwrap();
    assert_eq!((track_of(&p, 1), track_of(&p, 2)), (tid(V2), tid(A2)), "down on the audio row is the same index step");
    // The first named clip of a group decides.
    let p = up(&s, &[1, 2]).unwrap();
    assert_eq!((track_of(&p, 1), track_of(&p, 2)), (tid(V2), tid(A2)));
    assert_eq!(up(&s, &[2, 1]).unwrap_err(), NudgeError::NoTrackThatWay { clip_id: cid(1) }, "up on A1 asks for a lower index, and V1 is the first track the walk meets with none");
    // A partner in the way of its own step.
    let busy = fixture(|t| {
        on(t, V1).push(linked(1, 500, 10, "lg-1"));
        on(t, A1).push(linked(2, 500, 10, "lg-1"));
        on(t, A2).push(clip(3, 505, 1));
    });
    assert_eq!(up(&busy, &[1]).unwrap_err(), NudgeError::RangeOccupied { clip_id: cid(2), occupied_by: cid(3) });
    let shut = fixture(|t| {
        on(t, V1).push(linked(1, 500, 10, "lg-1"));
        on(t, A1).push(linked(2, 500, 10, "lg-1"));
        locked(t, A2);
    });
    assert_eq!(up(&shut, &[1]).unwrap_err(), NudgeError::TrackLocked { clip_id: cid(2), track_id: tid(A2) });
}

/// `Suspended`: the named clip moves alone and keeps its group id. Two members on one track refuse.
#[test]
fn a_suspended_link_moves_the_named_clip_alone() {
    let s = fixture(|t| {
        on(t, V1).push(linked(1, 500, 10, "lg-1"));
        on(t, A1).push(linked(2, 500, 10, "lg-1"));
    });
    let p = plan(&s, &[1], TrackNudge::Up, LinkedSelection::Suspended).unwrap();
    assert_eq!(track_of(&p, 1), tid(V2));
    assert_eq!(placed(p.snapshot(), 2), placed(&s, 2));
    assert_eq!(placed(p.snapshot(), 1).1.link_group.as_deref(), Some("lg-1"));
    let spread = fixture(|t| {
        on(t, V1).push(linked(1, 500, 10, "lg-1"));
        on(t, V2).push(linked(2, 600, 10, "lg-1"));
    });
    let e = plan(&spread, &[1], TrackNudge::Up, LinkedSelection::Suspended).unwrap_err();
    assert_eq!(e, NudgeError::Pulse(PulseError::LinkedEditUnsupported));
    assert_eq!(e.code(), "linked_edit_unsupported");
    let p = up(&spread, &[1]).unwrap();
    assert_eq!((track_of(&p, 1), track_of(&p, 2)), (tid(V2), tid(V3)), "with the link on both take the step");
}

/// A transition stays where it still plays; one that stops playing is removed and listed; undo brings
/// it back.
#[test]
fn a_transition_stays_only_where_it_still_plays() {
    let s = fixture(|t| {
        let mut outgoing = clip(1, 0, 10);
        outgoing.transitions.tail = Some(dissolve("tr-1"));
        on(t, V1).extend([outgoing, clip(2, 10, 10)]);
    });
    let tail = |p: &EditProposal, n: u32| placed(p.snapshot(), n).1.transitions.tail.map(|t| t.transition_id);
    let incoming = up(&s, &[2]).unwrap();
    assert_eq!(incoming.dropped_transitions(), ["tr-1"]);
    assert_eq!(tail(&incoming, 1), None);
    undo_restores(&s, &incoming);
    let outgoing = up(&s, &[1]).unwrap();
    assert_eq!(outgoing.dropped_transitions(), ["tr-1"]);
    assert_eq!(tail(&outgoing, 1), None);
    undo_restores(&s, &outgoing);
    let both = up(&s, &[1, 2]).unwrap();
    assert!(both.dropped_transitions().is_empty(), "{:?}", both.dropped_transitions());
    assert_eq!(tail(&both, 1).as_deref(), Some("tr-1"), "the pair still abuts on V2");
}

/// The change list and write footprint name the move; the read footprint names the destination and
/// what plays on it, so a concurrent edit of that clip is seen as dependent (STU-ARC-011).
#[test]
fn the_footprints_name_the_move_and_the_destination() {
    let s = fixture(|t| {
        on(t, V1).push(clip(1, 0, 10));
        on(t, V2).push(clip(2, 20, 10));
    });
    let p = up(&s, &[1]).unwrap();
    let window = ClipWindow { source_in: at(10), source_out: at(20), timeline_start: at(0), duration: at(10) };
    assert_eq!(p.changes(), [ClipChange::TrackMove { clip_id: cid(1), track_before: tid(V1), track_after: tid(V2), window }]);
    assert_eq!(p.changes()[0].kind(), "track_move");
    assert_eq!(p.write_footprint(), [cid(1), tid(V1), tid(V2)]);
    for read in [tid(V1), tid(V2), cid(1), cid(2)] {
        assert!(p.read_footprint().contains(&read), "{read}: {:?}", p.read_footprint());
    }
    let trim = with_ctx(|ctx| plan_timeline_edit(&s, SEQ, rev(&s), &TimelineEdit::TrimOut { clip_id: cid(2), new_duration: at(5), ripple: false }, ctx)).unwrap();
    assert!(trim.write_footprint().iter().any(|w| p.read_footprint().contains(w)), "a trim of what plays on the destination");
}

#[test]
fn bad_input_is_refused_before_any_change() {
    let s = fixture(|t| on(t, V1).push(clip(1, 0, 10)));
    let code = |r: NudgeResult<EditProposal>| r.unwrap_err().code();
    assert_eq!(code(up(&s, &[])), "invalid_input");
    assert_eq!(code(up(&s, &[1, 1])), "duplicate_id");
    assert_eq!(code(up(&s, &[9])), "unknown_clip");
    let one = request(&[1], TrackNudge::Up, LinkedSelection::On);
    assert_eq!(code(with_ctx(|ctx| plan_nudge_track(&s, SEQ, rev(&s) + 1, &one, ctx))), "stale_revision");
    assert_eq!(code(with_ctx(|ctx| plan_nudge_track(&s, "no-such", rev(&s), &one, ctx))), "unknown_sequence");
    let cancel = CancellationToken::default();
    cancel.cancel();
    let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &Long, cancel: &cancel };
    let canceled = plan_nudge_track(&s, SEQ, rev(&s), &one, &ctx).unwrap_err();
    assert_eq!((canceled.code(), canceled.failure_code()), ("canceled", None));
}

/// Hostile selection (V4-02): over the existing bound is refused before anything is walked, even with
/// a stale revision and an unknown sequence.
#[test]
fn a_selection_over_the_bound_is_refused_first() {
    let s = fixture(|t| on(t, V1).push(clip(1, 0, 10)));
    let over = NudgeTrack { clip_ids: vec![String::new(); MAX_MOVE_SELECTION + 1], direction: TrackNudge::Up, linked: LinkedSelection::On };
    let e = with_ctx(|ctx| plan_nudge_track(&s, "no-such", 0, &over, ctx)).unwrap_err();
    assert_eq!(e.code(), "limit_exceeded");
}

#[test]
fn the_descriptor_states_the_nudge() {
    for word in ["plan_nudge_track", "nudge_no_track", "nudge_range_occupied", "a_track_nudge_changes_only_the_track_of_a_clip"] {
        assert!(hsk_studio_pulse::DESCRIPTOR.contains(word), "{word}");
    }
}
