//! STU-VID-029 (link, group, sync lock) and STU-VID-029a (merge clips, synchronize), lane
//! SMB-PR-MERGE-SYNC-1. Expected ticks are computed by hand from the fixture, in frames of the
//! 25 fps table rate; refusals are asserted by code and the input snapshot stays unchanged.
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;
/// 29.97 fps ticks per frame (STU-VID-013).
const F2997: u64 = 8_475_667_200;
const SEQ: &str = "seq-main";

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    id("SCLP", n)
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

struct Extent(Option<u64>);
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        self.0.map(Ticks::new)
    }
}

/// Source timecode by artifact manifest id; a source that is not listed has none.
struct Timecodes(Vec<(&'static str, SourceTimecode)>);
impl SourceTimecodes for Timecodes {
    fn timecode(&self, source: &ClipSource) -> Option<SourceTimecode> {
        let ClipSource::Media { artifact_manifest_id, .. } = source else {
            return None;
        };
        self.0.iter().find(|(m, _)| *m == artifact_manifest_id.as_str()).map(|(_, t)| *t)
    }
}

fn clip(n: u32, source_in: u64, source_out: u64, start: u64, duration: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: format!("manifest-{n}"),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: Ticks::new(source_in),
        source_out: Ticks::new(source_out),
        timeline_start: Ticks::new(start),
        duration: Ticks::new(duration),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime::default(),
    }
}
/// A clip `frames` long at frame `start`, reading its source from frame 0.
fn fclip(n: u32, start: u64, frames: u64) -> StudioClip {
    clip(n, 0, frames * F, start * F, frames * F)
}
fn linked(mut c: StudioClip, group: &str) -> StudioClip {
    c.link_group = Some(group.into());
    c
}
fn marked(mut c: StudioClip, name: &str, marker_no: u32, at: u64) -> StudioClip {
    c.markers.push(StudioMarker {
        marker_id: format!("mk-{marker_no}"),
        name: name.into(),
        comment: String::new(),
        color: LabelColor::Rose,
        start: Ticks::new(at),
        duration: Ticks::new(0),
        role: None,
    });
    c
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id("STRK", n),
        kind,
        index,
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
fn snapshot(tracks: Vec<StudioTrack>) -> Snapshot {
    let end = tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_start.value() + c.duration.value()).max().unwrap_or(0);
    let unassigned = || ProfileBinding::Unassigned(());
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
            duration: Ticks::new(end),
            tracks,
            markers: Vec::new(),
            work_area: None,
        }],
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}
fn with_ctx<R>(extent: Option<u64>, f: impl FnOnce(&EditContext) -> R) -> R {
    let cancel = CancellationToken::default();
    let extents = Extent(extent);
    f(&EditContext { budget: Budget::default(), resolver: &Media, extents: &extents, cancel: &cancel })
}
fn rev(s: &Snapshot) -> u64 {
    s.document().revision
}
fn find(s: &Snapshot, n: u32) -> StudioClip {
    s.document().sequences[0].tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).expect("clip present").clone()
}
fn start_f(s: &Snapshot, n: u32) -> u64 {
    let t = find(s, n).timeline_start.value();
    assert_eq!(t % F, 0, "clip {n} start is on a frame");
    t / F
}
fn group(s: &Snapshot, n: u32) -> Option<String> {
    find(s, n).link_group
}
fn source(s: &Snapshot, n: u32) -> (u64, u64, u64) {
    let c = find(s, n);
    (c.source_in.value(), c.source_out.value(), c.duration.value())
}
fn ids(list: &[u32]) -> Vec<String> {
    list.iter().map(|n| cid(*n)).collect()
}
fn merge(s: &Snapshot, list: &[u32], align: Option<SyncPoint>, timecodes: &dyn SourceTimecodes) -> MergeSyncResult<MergeSyncProposal> {
    let request = MergeClips { clip_ids: ids(list), link_group: "av-1".into(), align };
    with_ctx(None, |ctx| plan_merge_clips(s, SEQ, rev(s), &request, timecodes, ctx))
}
fn sync(s: &Snapshot, list: &[u32], anchor: u32, point: SyncPoint, timecodes: &dyn SourceTimecodes) -> MergeSyncResult<MergeSyncProposal> {
    let request = Synchronize { clip_ids: ids(list), anchor_clip_id: cid(anchor), point, link_group: "sync-1".into() };
    with_ctx(None, |ctx| plan_synchronize(s, SEQ, rev(s), &request, timecodes, ctx))
}
fn refused<T>(r: MergeSyncResult<T>) -> (&'static str, Option<String>) {
    let e = r.err().expect("refused");
    (e.code(), e.clip_id().map(str::to_owned))
}
fn undo_restores(p: &EditProposal, original: &Snapshot) {
    let undo = with_ctx(Some(1000 * F), |ctx| plan_timeline_revert(p.snapshot(), SEQ, rev(p.snapshot()), p.inverse(), ctx)).expect("undo");
    assert_eq!(undo.snapshot().document().sequences, original.document().sequences, "undo restores the sequence");
}
fn trim_out(n: u32, frames: u64) -> TimelineEdit {
    TimelineEdit::TrimOut { clip_id: cid(n), new_duration: Ticks::new(frames * F), ripple: false }
}
fn plan_edit(s: &Snapshot, edit: &TimelineEdit) -> Result<EditProposal> {
    with_ctx(Some(1000 * F), |ctx| plan_timeline_edit(s, SEQ, rev(s), edit, ctx))
}
fn plan_linked(s: &Snapshot, edit: &TimelineEdit, linked: LinkedSelection) -> Result<EditProposal> {
    with_ctx(Some(1000 * F), |ctx| plan_timeline_edit_with_linked_selection(s, SEQ, rev(s), edit, linked, ctx))
}
fn plan_move(s: &Snapshot, list: &[u32], frames: i64, linked: LinkedSelection) -> Result<EditProposal> {
    let request = MoveClips { clip_ids: ids(list), delta: TickDelta::new(i128::from(frames) * i128::from(F)).unwrap(), linked };
    with_ctx(None, |ctx| plan_move_clips(s, SEQ, rev(s), &request, ctx))
}
fn tc(hours: u64, minutes: u8, seconds: u8, frames: u8, rate: u64) -> SourceTimecode {
    SourceTimecode { start: Timecode { hours, minutes, seconds, frames, drop_frame: false }, rate: FrameRate::from_import(rate).expect("table rate").0 }
}

/// V1 [1 linked 0..3F, 2 3F..6F], A1 [3 linked 0..3F, 4 3F..6F], A2 [5 linked to nothing else].
fn linked_av() -> Snapshot {
    snapshot(vec![
        track(1, TrackKind::Video, 0, vec![linked(fclip(1, 0, 3), "lg"), fclip(2, 3, 3)]),
        track(2, TrackKind::Audio, 0, vec![linked(fclip(3, 0, 3), "lg"), fclip(4, 3, 3)]),
        track(3, TrackKind::Audio, 1, vec![linked(fclip(5, 0, 3), "other")]),
    ])
}

// ---- STU-VID-029: unlink, linked-selection toggle, group, sync lock.

#[test]
fn unlink_dissolves_the_group_and_only_the_link() {
    bounded_case("unlink", || {
        let snap = linked_av();
        let before = snap.document().clone();
        let p = with_ctx(None, |ctx| plan_unlink(&snap, SEQ, 3, &cid(3), ctx)).unwrap();
        let s = p.snapshot();
        assert_eq!((group(s, 1), group(s, 3)), (None, None));
        assert_eq!(group(s, 5).as_deref(), Some("other"), "another group stays");
        for n in [1, 3] {
            assert_eq!(source(s, n), source(&snap, n), "clip {n} window stays");
            assert_eq!(find(s, n).timeline_start, find(&snap, n).timeline_start);
        }
        // The exact change list and write footprint name the two clips, nothing else.
        assert_eq!(p.changes().len(), 2);
        for n in [1, 3] {
            let Some(ClipChange::Relink { group_before, group_after, .. }) = p.change_for(&cid(n)) else {
                panic!("clip {n} relinks: {:?}", p.change_for(&cid(n)));
            };
            assert_eq!((group_before.as_deref(), group_after.as_deref()), (Some("lg"), None));
            assert!(p.write_footprint().contains(&cid(n)));
            assert_eq!(p.change_for(&cid(n)).unwrap().before(), p.change_for(&cid(n)).unwrap().after());
        }
        assert_eq!(p.write_footprint().len(), 2, "{:?}", p.write_footprint());
        // After unlink a trim is a one-clip trim; at base it took the partner along.
        let t = plan_edit(s, &trim_out(1, 2)).unwrap();
        assert_eq!(find(t.snapshot(), 3).duration.value(), 3 * F);
        assert_eq!(find(&plan_edit(&snap, &trim_out(1, 2)).unwrap().snapshot().clone(), 3).duration.value(), 2 * F);
        undo_restores(&p, &snap);
        assert_eq!(*snap.document(), before, "input unchanged");
        // Refusals: an unlinked clip has nothing to dissolve, a stale id, a stale revision, a locked member.
        let code = |s: &Snapshot, n: u32, r: u64| with_ctx(None, |ctx| plan_unlink(s, SEQ, r, &cid(n), ctx)).unwrap_err().code();
        assert_eq!(code(&snap, 2, 3), "no_change");
        assert_eq!(code(&snap, 99, 3), "unknown_clip");
        assert_eq!(code(&snap, 1, 2), "stale_revision");
        let mut tracks = before.sequences[0].tracks.clone();
        tracks[1].locked = true;
        assert_eq!(code(&snapshot(tracks), 1, 3), "track_locked");
    });
}

#[test]
fn linked_selection_off_suspends_the_behaviour_without_dissolving() {
    bounded_case("toggle", || {
        let snap = linked_av();
        // On is exactly today's behaviour: trimming one trims all.
        let on = plan_linked(&snap, &trim_out(1, 2), LinkedSelection::On).unwrap();
        assert_eq!(find(on.snapshot(), 3).duration.value(), 2 * F);
        assert_eq!(on.snapshot().document(), plan_edit(&snap, &trim_out(1, 2)).unwrap().snapshot().document());
        // Off: the trim stays on the clip and both clips keep their link id.
        let off = plan_linked(&snap, &trim_out(1, 2), LinkedSelection::Suspended).unwrap();
        let s = off.snapshot();
        assert_eq!(find(s, 1).duration.value(), 2 * F);
        assert_eq!(find(s, 3).duration.value(), 3 * F);
        assert_eq!((group(s, 1).as_deref(), group(s, 3).as_deref()), (Some("lg"), Some("lg")));
        assert!(off.change_for(&cid(3)).is_none());
        undo_restores(&off, &snap);
        // The master toggle back on: the group never dissolved, a trim drives both again by one delta.
        let again = plan_linked(s, &trim_out(1, 1), LinkedSelection::On).unwrap();
        assert_eq!(find(again.snapshot(), 3).duration.value(), 2 * F);
        // Every member-expanding edit honours it: slip, lift, roll stay on the one clip.
        let slip = TimelineEdit::Slip { clip_id: cid(1), delta: TickDelta::new(i128::from(F)).unwrap() };
        let p = plan_linked(&snap, &slip, LinkedSelection::Suspended).unwrap();
        assert_eq!(find(p.snapshot(), 1).source_in.value(), F);
        assert_eq!(find(p.snapshot(), 3).source_in.value(), 0);
        let lift = TimelineEdit::LiftClip { clip_id: cid(1) };
        let p = plan_linked(&snap, &lift, LinkedSelection::Suspended).unwrap();
        assert_eq!(p.snapshot().document().sequences[0].tracks[0].clips.len(), 1);
        assert_eq!(group(p.snapshot(), 3).as_deref(), Some("lg"));
        let roll = TimelineEdit::Roll { left_clip_id: cid(1), delta: TickDelta::new(i128::from(F)).unwrap() };
        let p = plan_linked(&snap, &roll, LinkedSelection::Suspended).unwrap();
        assert_eq!((find(p.snapshot(), 1).duration.value(), find(p.snapshot(), 3).duration.value()), (4 * F, 3 * F));
        // A razor cuts the one clip: the left half keeps the link, the right half has none.
        let razor = |linked| TimelineEdit::Razor { clip_id: cid(1), at: Ticks::new(F), new_right_clip_id: cid(11), linked };
        let p = plan_linked(&snap, &razor(None), LinkedSelection::Suspended).unwrap();
        assert_eq!((group(p.snapshot(), 1).as_deref(), group(p.snapshot(), 11)), (Some("lg"), None));
        assert_eq!(find(p.snapshot(), 3).duration.value(), 3 * F);
        let extra = LinkedRazor { right_link_group: "lg-r".into(), right_clip_ids: vec![] };
        assert_eq!(plan_linked(&snap, &razor(Some(extra)), LinkedSelection::Suspended).unwrap_err(), PulseError::InvalidInput);
        // Refusals stay typed through the toggle.
        assert_eq!(plan_linked(&snap, &trim_out(99, 2), LinkedSelection::Suspended).unwrap_err(), PulseError::UnknownClip);
    });
}

#[test]
fn group_move_takes_an_arbitrary_set_with_no_sync_implication() {
    bounded_case("group", || {
        let fixture = || vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 0, 3), fclip(2, 6, 3)]),
            track(2, TrackKind::Audio, 0, vec![fclip(3, 0, 3), fclip(4, 9, 2)]),
            track(3, TrackKind::Audio, 1, vec![fclip(5, 0, 3)]),
        ];
        let snap = snapshot(fixture());
        let before = snap.document().clone();
        let p = plan_move(&snap, &[1, 4], 1, LinkedSelection::On).unwrap();
        let s = p.snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 4)), (1, 10));
        // Nothing else moves, sync-locked or not; the set moves, it never trims; nothing links.
        assert_eq!((start_f(s, 2), start_f(s, 3), start_f(s, 5)), (6, 0, 0));
        assert_eq!((source(s, 1), source(s, 4)), (source(&snap, 1), source(&snap, 4)));
        for n in 1..=5 {
            assert_eq!(group(s, n), None, "clip {n}");
        }
        assert_eq!(s.document().sequences[0].duration.value(), 12 * F);
        assert_eq!(p.changes().len(), 2);
        assert!(p.write_footprint().contains(&cid(1)) && p.write_footprint().contains(&cid(4)));
        undo_restores(&p, &snap);
        // A move past a neighbour into free space keeps the track ordered; 1 abuts the end of 2.
        let p = plan_move(&snap, &[1], 9, LinkedSelection::On).unwrap();
        let order: Vec<String> = p.snapshot().document().sequences[0].tracks[0].clips.iter().map(|c| c.clip_id.clone()).collect();
        assert_eq!(order, vec![cid(2), cid(1)]);
        assert_eq!(start_f(p.snapshot(), 1), 9);
        // A linked clip brings its partners unless the toggle is off; the group identity stays.
        let mut tracks = fixture();
        tracks[0].clips[0].link_group = Some("lg".into());
        tracks[1].clips[0].link_group = Some("lg".into());
        let linked_snap = snapshot(tracks);
        let p = plan_move(&linked_snap, &[1], 1, LinkedSelection::On).unwrap();
        assert_eq!((start_f(p.snapshot(), 1), start_f(p.snapshot(), 3)), (1, 1));
        let p = plan_move(&linked_snap, &[1], 1, LinkedSelection::Suspended).unwrap();
        assert_eq!((start_f(p.snapshot(), 1), start_f(p.snapshot(), 3)), (1, 0));
        assert_eq!((group(p.snapshot(), 1).as_deref(), group(p.snapshot(), 3).as_deref()), (Some("lg"), Some("lg")));
        // Refusals: time zero, a collision, a duplicate, an unknown id, an empty set, zero delta.
        let code = |list: &[u32], frames: i64| plan_move(&snap, list, frames, LinkedSelection::On).unwrap_err().code();
        assert_eq!(code(&[1], -1), "underflow");
        assert_eq!(code(&[1], 4), "overlap");
        assert_eq!(code(&[1, 1], 1), "duplicate_id");
        assert_eq!(code(&[99], 1), "unknown_clip");
        assert_eq!(code(&[], 1), "invalid_input");
        assert_eq!(code(&[1], 0), "no_change");
        let sub_frame = MoveClips { clip_ids: ids(&[1]), delta: TickDelta::new(1).unwrap(), linked: LinkedSelection::On };
        assert_eq!(with_ctx(None, |ctx| plan_move_clips(&snap, SEQ, 3, &sub_frame, ctx)).unwrap_err().code(), "not_frame_aligned");
        let sub_frame_audio = MoveClips { clip_ids: ids(&[3]), delta: TickDelta::new(1).unwrap(), linked: LinkedSelection::On };
        let p = with_ctx(None, |ctx| plan_move_clips(&snap, SEQ, 3, &sub_frame_audio, ctx)).unwrap();
        assert_eq!(find(p.snapshot(), 3).timeline_start.value(), 1, "audio has no frame grid");
        let mut tracks = fixture();
        tracks[1].locked = true;
        assert_eq!(plan_move(&snapshot(tracks), &[1, 4], 1, LinkedSelection::On).unwrap_err().code(), "track_locked");
        let over = MoveClips { clip_ids: vec![cid(1); MAX_MOVE_SELECTION + 1], delta: TickDelta::ZERO, linked: LinkedSelection::On };
        assert_eq!(with_ctx(None, |ctx| plan_move_clips(&snap, SEQ, 3, &over, ctx)).unwrap_err().code(), "limit_exceeded");
        assert_eq!(with_ctx(None, |ctx| plan_move_clips(&snap, SEQ, 2, &sub_frame_audio, ctx)).unwrap_err().code(), "stale_revision");
        assert_eq!(*snap.document(), before, "input unchanged");
    });
}

/// MT-33726 oracle: link drives trim, a moved set does not trim, the sync lock drives the shift and
/// is not a selection relationship; none of the three writes another's flag.
#[test]
fn link_group_and_sync_lock_stay_three_relations() {
    bounded_case("truth table", || {
        // V1 [1 linked, 2], A1 sync-locked and linked [3 linked, 4], A2 sync-locked unselected [5],
        // A3 not sync-locked unselected [6].
        let fixture = || {
            let mut tracks = vec![
                track(1, TrackKind::Video, 0, vec![linked(fclip(1, 0, 3), "lg"), fclip(2, 3, 3)]),
                track(2, TrackKind::Audio, 0, vec![linked(fclip(3, 0, 3), "lg"), fclip(4, 3, 3)]),
                track(3, TrackKind::Audio, 1, vec![fclip(5, 4, 3)]),
                track(4, TrackKind::Audio, 2, vec![fclip(6, 4, 3)]),
            ];
            tracks[3].sync_locked = false;
            tracks
        };
        let snap = snapshot(fixture());
        let edit = TimelineEdit::TrimOut { clip_id: cid(1), new_duration: Ticks::new(2 * F), ripple: true };
        let p = plan_edit(&snap, &edit).unwrap();
        let s = p.snapshot();
        // Link: the partner trims. Sync lock: the unselected sync-locked track shifts, it does not trim.
        assert_eq!(find(s, 3).duration.value(), 2 * F);
        assert_eq!((start_f(s, 5), find(s, 5).duration.value()), (3, 3 * F));
        assert_eq!(start_f(s, 6), 4, "a track without the sync lock stays");
        // A link partner trims whether its track is sync-locked or not: the two are unrelated.
        let mut tracks = fixture();
        tracks[1].sync_locked = false;
        let p = plan_edit(&snapshot(tracks), &edit).unwrap();
        assert_eq!(find(p.snapshot(), 3).duration.value(), 2 * F);
        // Merge writes the link and nothing about the sync lock; a set move writes neither.
        let plain = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 0, 3)]),
            track(2, TrackKind::Audio, 0, vec![fclip(2, 0, 3)]),
            track(3, TrackKind::Audio, 1, vec![fclip(3, 0, 3)]),
        ]);
        let locks = |s: &Snapshot| s.document().sequences[0].tracks.iter().map(|t| t.sync_locked).collect::<Vec<_>>();
        let merged = merge(&plain, &[1, 2], None, &NoSourceTimecodes).unwrap();
        assert_eq!(locks(merged.proposal().snapshot()), locks(&plain));
        assert_eq!(group(merged.proposal().snapshot(), 3), None);
        let moved = plan_move(&plain, &[1, 3], 1, LinkedSelection::On).unwrap();
        assert_eq!(locks(moved.snapshot()), locks(&plain));
        assert_eq!((group(moved.snapshot(), 1), group(moved.snapshot(), 3)), (None, None));
        // A broken link (two members on one track) refuses a trim; unlink is the way out of it.
        let broken = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![linked(fclip(1, 0, 3), "lg"), linked(fclip(2, 3, 3), "lg")]),
            track(2, TrackKind::Audio, 0, vec![linked(fclip(3, 0, 3), "lg")]),
        ]);
        assert_eq!(plan_edit(&broken, &trim_out(1, 2)).unwrap_err(), PulseError::LinkedEditUnsupported);
        let fixed = with_ctx(None, |ctx| plan_unlink(&broken, SEQ, 3, &cid(1), ctx)).unwrap();
        assert_eq!((group(fixed.snapshot(), 2), group(fixed.snapshot(), 3)), (None, None));
        assert!(plan_edit(fixed.snapshot(), &trim_out(1, 2)).is_ok());
    });
}

// ---- STU-VID-029a: merge clips.

/// V1 [1 at 20F, 10F long], A1 [2 at 0, 4F long], A2 [3 at 40F, 2F long].
fn separate_av() -> Vec<StudioTrack> {
    vec![
        track(1, TrackKind::Video, 0, vec![fclip(1, 20, 10)]),
        track(2, TrackKind::Audio, 0, vec![fclip(2, 0, 4)]),
        track(3, TrackKind::Audio, 1, vec![fclip(3, 40, 2)]),
    ]
}

#[test]
fn merge_clips_links_in_place_as_one_proposal_with_one_inverse() {
    bounded_case("merge in place", || {
        let snap = snapshot(separate_av());
        let before = snap.document().clone();
        let m = merge(&snap, &[1, 2, 3], None, &NoSourceTimecodes).unwrap();
        let p = m.proposal();
        let s = p.snapshot();
        for n in 1..=3 {
            assert_eq!(group(s, n).as_deref(), Some("av-1"), "clip {n}");
            assert_eq!(find(s, n).timeline_start, find(&snap, n).timeline_start);
            assert_eq!(source(s, n), source(&snap, n));
        }
        assert_eq!((p.base_revision(), p.target_revision(), rev(s)), (3, 4, 4));
        // No new primitive: same sequences, same tracks, same clips, an ordinary link group.
        assert_eq!(s.document().sequences.len(), 1);
        assert_eq!(s.document().sequences[0].tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 3);
        assert_eq!(m.link_group(), "av-1");
        assert_eq!(m.basis(), None);
        assert!(m.alignment().iter().all(|a| a.offset == TickDelta::ZERO), "{:?}", m.alignment());
        assert_eq!(m.alignment().len(), 3);
        assert_eq!(p.changes().len(), 3);
        assert_eq!(p.write_footprint().len(), 3, "{:?}", p.write_footprint());
        undo_restores(p, &snap);
        assert_eq!(*snap.document(), before, "input unchanged");
    });
}

#[test]
fn merge_clips_aligns_audio_to_the_video_clip() {
    bounded_case("merge aligned", || {
        let snap = snapshot(separate_av());
        let m = merge(&snap, &[2, 1, 3], Some(SyncPoint::InPoint), &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2), start_f(s, 3)), (20, 20, 20));
        assert_eq!(m.basis(), Some(&SyncPoint::InPoint));
        let offsets: Vec<(String, i128)> = m.alignment().iter().map(|a| (a.clip_id.clone(), a.offset.value())).collect();
        let f = i128::from(F);
        assert_eq!(offsets, vec![(cid(2), 20 * f), (cid(1), 0), (cid(3), -20 * f)]);
        // Out point: the ends meet at the video clip's end, 30F.
        let m = merge(&snap, &[1, 2, 3], Some(SyncPoint::OutPoint), &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2), start_f(s, 3)), (20, 26, 28));
        undo_restores(m.proposal(), &snap);
        // MT-33727 oracle: video marker at 5F, audio marker at 2F: the audio is placed +3F, source
        // windows unchanged.
        let snap = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![marked(fclip(1, 0, 10), "slate", 1, 5 * F)]),
            track(2, TrackKind::Audio, 0, vec![marked(fclip(2, 0, 10), "slate", 2, 2 * F)]),
        ]);
        let m = merge(&snap, &[1, 2], Some(SyncPoint::Marker { name: "slate".into() }), &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2)), (0, 3));
        assert_eq!((source(s, 1), source(s, 2)), (source(&snap, 1), source(&snap, 2)));
        assert_eq!(m.alignment()[1].offset.value(), 3 * i128::from(F));
        // The merged unit is an ordinary link: trimming one trims all, a lift takes both.
        let t = plan_edit(s, &trim_out(1, 4)).unwrap();
        assert_eq!(find(t.snapshot(), 2).duration.value(), 4 * F);
        let l = plan_edit(s, &TimelineEdit::LiftClip { clip_id: cid(2) }).unwrap();
        assert_eq!(l.snapshot().document().sequences[0].tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 0);
    });
}

#[test]
fn merge_clips_refusals_are_typed_and_change_nothing() {
    bounded_case("merge refusals", || {
        let snap = snapshot(separate_av());
        let before = snap.document().clone();
        let no = &NoSourceTimecodes;
        let code = |s: &Snapshot, list: &[u32], align: Option<SyncPoint>| refused(merge(s, list, align, no));
        // No video and audio pair: two audio, one video, one clip, two video, a caption.
        assert_eq!(code(&snap, &[2, 3], None).0, "merge_no_av_pair");
        assert_eq!(code(&snap, &[1], None).0, "merge_no_av_pair");
        assert_eq!(code(&snap, &[], None).0, "merge_no_av_pair");
        let more = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 0, 3)]),
            track(2, TrackKind::Video, 1, vec![fclip(2, 0, 3)]),
            track(3, TrackKind::Audio, 0, vec![fclip(3, 0, 3), fclip(4, 3, 3)]),
            track(4, TrackKind::Caption, 0, vec![fclip(5, 0, 3)]),
        ]);
        assert_eq!(code(&more, &[1, 2, 3], None).0, "merge_no_av_pair");
        assert_eq!(code(&more, &[1, 3, 5], None).0, "merge_no_av_pair");
        // Clips on one track: the second one is named.
        assert_eq!(code(&more, &[1, 3, 4], None), ("link_same_track", Some(cid(4))));
        // A clip already in a group is named; a group id in use is refused.
        let grouped = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 0, 3), linked(fclip(4, 3, 3), "av-1")]),
            track(2, TrackKind::Audio, 0, vec![linked(fclip(2, 0, 3), "lg")]),
            track(3, TrackKind::Audio, 1, vec![fclip(3, 0, 3)]),
        ]);
        assert_eq!(code(&grouped, &[1, 2], None), ("clip_already_linked", Some(cid(2))));
        assert_eq!(code(&grouped, &[1, 3], None).0, "duplicate_id");
        // A locked track, on the video or the audio side, even when nothing would move.
        for t in [0, 1] {
            let mut tracks = separate_av();
            tracks[t].locked = true;
            assert_eq!(code(&snapshot(tracks), &[1, 2], None).0, "track_locked", "track {t}");
        }
        // An overlap the alignment would cause, and alignment before time zero.
        let blocked = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![marked(fclip(1, 20, 10), "m", 1, 0)]),
            track(2, TrackKind::Audio, 0, vec![marked(fclip(2, 0, 10), "m", 2, 2 * F), fclip(3, 25, 2)]),
        ]);
        assert_eq!(code(&blocked, &[1, 2], Some(SyncPoint::InPoint)).0, "overlap");
        assert_eq!(code(&blocked, &[1, 2], Some(SyncPoint::Marker { name: "m".into() })).0, "overlap");
        let before_zero = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![marked(fclip(1, 1, 10), "m", 1, 0)]),
            track(2, TrackKind::Audio, 0, vec![marked(fclip(2, 0, 10), "m", 2, 5 * F)]),
        ]);
        assert_eq!(code(&before_zero, &[1, 2], Some(SyncPoint::Marker { name: "m".into() })).0, "underflow");
        // Hostile selection and ids: budgeted before any work; duplicates; unknown; bad group id.
        let huge = MergeClips { clip_ids: vec![cid(1); MAX_LINKED_SELECTION + 1], link_group: "g".into(), align: None };
        let e = with_ctx(None, |ctx| plan_merge_clips(&snap, SEQ, 3, &huge, no, ctx)).err().unwrap();
        assert_eq!(e.code(), "limit_exceeded");
        let long = MergeClips { clip_ids: ids(&[1, 2]), link_group: "g".repeat(MAX_LINK_GROUP_BYTES + 1), align: None };
        let e = with_ctx(None, |ctx| plan_merge_clips(&snap, SEQ, 3, &long, no, ctx)).err().unwrap();
        assert_eq!(e.code(), "limit_exceeded");
        let bad = MergeClips { clip_ids: ids(&[1, 2]), link_group: "bad group!".into(), align: None };
        let e = with_ctx(None, |ctx| plan_merge_clips(&snap, SEQ, 3, &bad, no, ctx)).err().unwrap();
        assert_eq!(e.code(), "folio_rejected");
        assert_eq!(code(&snap, &[1, 2, 2], None).0, "duplicate_id");
        assert_eq!(code(&snap, &[1, 99], None).0, "unknown_clip");
        // Stale revision, unknown sequence and cancellation.
        let request = MergeClips { clip_ids: ids(&[1, 2]), link_group: "g".into(), align: None };
        let e = with_ctx(None, |ctx| plan_merge_clips(&snap, SEQ, 2, &request, no, ctx)).err().unwrap();
        assert_eq!(e.code(), "stale_revision");
        let e = with_ctx(None, |ctx| plan_merge_clips(&snap, "seq-none", 3, &request, no, ctx)).err().unwrap();
        assert_eq!(e.code(), "unknown_sequence");
        let canceled = CancellationToken::default();
        canceled.cancel();
        let extents = Extent(None);
        let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &extents, cancel: &canceled };
        let e = plan_merge_clips(&snap, SEQ, 3, &request, no, &ctx).err().unwrap();
        assert_eq!(e.code(), "canceled");
        assert_eq!(e.failure_code(), None);
        assert_eq!(*snap.document(), before, "input unchanged");
    });
}

// ---- STU-VID-029a: synchronize.

#[test]
fn synchronize_aligns_by_in_point_and_out_point_around_the_anchor() {
    bounded_case("in out", || {
        let snap = snapshot(separate_av());
        // The anchor stays; here the audio anchor pulls the video clip to it.
        let m = sync(&snap, &[1, 2, 3], 3, SyncPoint::InPoint, &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2), start_f(s, 3)), (40, 40, 40));
        for n in 1..=3 {
            assert_eq!(group(s, n).as_deref(), Some("sync-1"), "clip {n}");
            assert_eq!(source(s, n), source(&snap, n), "clip {n} source window");
        }
        assert_eq!(m.basis(), Some(&SyncPoint::InPoint));
        assert_eq!(s.document().sequences[0].duration.value(), 50 * F);
        assert!(m.proposal().write_footprint().contains(&SEQ.to_owned()), "sequence end moved");
        undo_restores(m.proposal(), &snap);
        // Out point: all ends at the anchor's end (4F); the 10F video clip would pass time zero.
        assert_eq!(refused(sync(&snap, &[1, 2], 2, SyncPoint::OutPoint, &NoSourceTimecodes)).0, "underflow");
        let m = sync(&snap, &[1, 2], 1, SyncPoint::OutPoint, &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(m.proposal().snapshot(), 2), 26);
        // Synchronize takes any kinds: two video clips and a caption, one per track.
        let mixed = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 5, 3)]),
            track(2, TrackKind::Video, 1, vec![fclip(2, 9, 3)]),
            track(3, TrackKind::Caption, 0, vec![fclip(3, 1, 3)]),
        ]);
        let m = sync(&mixed, &[1, 2, 3], 1, SyncPoint::InPoint, &NoSourceTimecodes).unwrap();
        assert_eq!((start_f(m.proposal().snapshot(), 2), start_f(m.proposal().snapshot(), 3)), (5, 5));
        // Sync lock is not touched: an unselected sync-locked track stays where it is.
        let m = sync(&snap, &[1, 2], 1, SyncPoint::InPoint, &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(m.proposal().snapshot(), 3), 40);
    });
}

#[test]
fn synchronize_by_timecode_reads_the_source_label_through_pulse_time() {
    bounded_case("timecode", || {
        // V reads its source from 2F, whose first tick is labelled 01:00:10:00; A reads from 0,
        // first tick 01:00:08:00. The label 01:00:10:02 is A's source tick 2 s + 2F = 52F, so A
        // starts 52F before V: 100F - 52F = 48F.
        let mut v = clip(1, 2 * F, 12 * F, 100 * F, 10 * F);
        v.source_ref = ClipSource::Media {
            artifact_manifest_id: "cam".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        };
        let mut a = fclip(2, 0, 60);
        a.source_ref = ClipSource::Media {
            artifact_manifest_id: "rec".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc124".into() },
            asset_id: None,
        };
        let tracks = vec![track(1, TrackKind::Video, 0, vec![v]), track(2, TrackKind::Audio, 0, vec![a])];
        let snap = snapshot(tracks);
        let ports = Timecodes(vec![("cam", tc(1, 0, 10, 0, F)), ("rec", tc(1, 0, 8, 0, F))]);
        let m = sync(&snap, &[1, 2], 1, SyncPoint::Timecode, &ports).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2)), (100, 48));
        assert_eq!(m.alignment()[1].offset.value(), 48 * i128::from(F));
        assert_eq!(group(s, 2).as_deref(), Some("sync-1"));
        // The same placement holds when the audio clip is the anchor: V moves to 100F from 48F.
        let moved = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![find(&snap, 1)]),
            track(2, TrackKind::Audio, 0, vec![{
                let mut c = find(&snap, 2);
                c.timeline_start = Ticks::new(58 * F);
                c
            }]),
        ]);
        let m = sync(&moved, &[1, 2], 2, SyncPoint::Timecode, &ports).unwrap();
        assert_eq!((start_f(m.proposal().snapshot(), 1), start_f(m.proposal().snapshot(), 2)), (110, 58));
        // Drop-frame: 00:01:00;02 is frame 1800 at 29.97 (frames ;00 and ;01 of minute 1 are skipped),
        // 1800 * 8_475_667_200 = 15_256_200_960_000 ticks after 00:00:00;00.
        let df = |minutes, frames| SourceTimecode {
            start: Timecode { hours: 0, minutes, seconds: 0, frames, drop_frame: true },
            rate: FrameRate::from_import(F2997).unwrap().0,
        };
        let mut long = clip(1, 0, 2000 * F2997, 0, 2000 * F2997);
        long.source_ref = ClipSource::Media {
            artifact_manifest_id: "cam".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        };
        let mut rec = clip(2, 0, 10 * F, 0, 10 * F);
        rec.source_ref = ClipSource::Media {
            artifact_manifest_id: "rec".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc124".into() },
            asset_id: None,
        };
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![long]), track(2, TrackKind::Audio, 0, vec![rec])]);
        let ports = Timecodes(vec![("cam", df(0, 0)), ("rec", df(1, 2))]);
        let m = sync(&snap, &[1, 2], 1, SyncPoint::Timecode, &ports).unwrap();
        assert_eq!(find(m.proposal().snapshot(), 2).timeline_start.value(), 15_256_200_960_000);
    });
}

#[test]
fn synchronize_by_marker_maps_speed_and_reverse_and_refuses_a_hidden_marker() {
    bounded_case("marker", || {
        let m = SyncPoint::Marker { name: "clap".into() };
        // V marker at source 8F, on the timeline at 3F (V reads from 5F, starts at 0).
        let v = marked(clip(1, 5 * F, 15 * F, 0, 10 * F), "clap", 1, 8 * F);
        // A at half speed: source 2F is 4F into the clip; A moves so that lands on 3F: from 10F to -1F.
        let mut slow = marked(clip(2, 0, 4 * F, 10 * F, 8 * F), "clap", 2, 2 * F);
        slow.speed = Speed { numerator: 1, denominator: 2 };
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![v.clone()]), track(2, TrackKind::Audio, 0, vec![slow.clone()])]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes)).0, "underflow");
        // With V at 20F the mark is at 23F, so slow starts at 23F - 4F = 19F.
        let mut late = v.clone();
        late.timeline_start = Ticks::new(20 * F);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![slow])]);
        let p = sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(p.proposal().snapshot(), 2), 19);
        // Reversed: source 2F of a 0..10F clip plays 8F into it: 23F - 8F = 15F. This clip is on an audio
        // track, which shows no frame; the clap is heard on the mark (tests/marker_track_kind.rs).
        let mut back = marked(clip(2, 0, 10 * F, 0, 10 * F), "clap", 2, 2 * F);
        back.speed = Speed { numerator: -1, denominator: 1 };
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![back.clone()])]);
        let p = sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(p.proposal().snapshot(), 2), 15);
        // The same clip on a video track aligns the same source tick, 15F. The NAMED LIMIT, against
        // `frame_layers`: frame 2 is presented in the frame before the mark, 22F..23F; closing it fails here.
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Video, 1, vec![back])]);
        let p = sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(p.proposal().snapshot(), 2), 15);
        let shown = |at: u64| {
            let seq = &p.proposal().snapshot().document().sequences[0];
            let layers = frame_layers(seq, Ticks::new(at * F), &UnknownExtents, &CancellationToken::default()).unwrap();
            layers.iter().find(|l| l.track_id == id("STRK", 2)).expect("the clip is on screen").answer.frame().index
        };
        assert_eq!((shown(22), shown(23)), (2, 1));
        // A marker between frames: audio moves freely, a video clip refuses to leave its frame grid.
        let tick = marked(clip(2, 0, 10 * F, 40 * F, 10 * F), "clap", 2, 7);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![tick])]);
        let p = sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes).unwrap();
        assert_eq!(find(p.proposal().snapshot(), 2).timeline_start.value(), 23 * F - 7);
        assert_eq!(refused(sync(&snap, &[1, 2], 2, m.clone(), &NoSourceTimecodes)).0, "not_frame_aligned");
        // Missing, hidden outside the window, and ambiguous markers name the clip; never a zero.
        let bare = fclip(2, 0, 10);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![bare.clone()])]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes)), ("sync_marker_missing", Some(cid(2))));
        assert_eq!(refused(sync(&snap, &[1, 2], 2, m.clone(), &NoSourceTimecodes)), ("sync_marker_missing", Some(cid(2))));
        let other_name = marked(bare.clone(), "slate", 2, F);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![other_name])]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes)), ("sync_marker_missing", Some(cid(2))));
        let hidden = marked(clip(2, 3 * F, 10 * F, 0, 7 * F), "clap", 2, F);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late.clone()]), track(2, TrackKind::Audio, 0, vec![hidden])]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, m.clone(), &NoSourceTimecodes)), ("sync_marker_missing", Some(cid(2))));
        let tie = marked(marked(bare, "clap", 2, F), "clap", 3, 2 * F);
        let snap = snapshot(vec![track(1, TrackKind::Video, 0, vec![late]), track(2, TrackKind::Audio, 0, vec![tie])]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, m, &NoSourceTimecodes)), ("sync_marker_ambiguous", Some(cid(2))));
    });
}

#[test]
fn synchronize_refusals_name_the_clip_and_never_assume_a_zero() {
    bounded_case("sync refusals", || {
        let snap = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 10, 10)]),
            track(2, TrackKind::Audio, 0, vec![fclip(2, 0, 10)]),
            track(3, TrackKind::Audio, 1, vec![fclip(3, 0, 10), fclip(4, 10, 10)]),
        ]);
        let before = snap.document().clone();
        let good = tc(1, 0, 0, 0, F);
        let code = |s: &Snapshot, list: &[u32], anchor: u32, p: SyncPoint, t: &dyn SourceTimecodes| refused(sync(s, list, anchor, p, t));
        let cam_rec = |cam, rec| Timecodes(vec![("manifest-1", cam), ("manifest-2", rec)]);
        // Timecode missing on either side.
        let only_cam = Timecodes(vec![("manifest-1", good)]);
        let only_rec = Timecodes(vec![("manifest-2", good)]);
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &only_cam), ("sync_timecode_missing", Some(cid(2))));
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &only_rec), ("sync_timecode_missing", Some(cid(1))));
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &NoSourceTimecodes), ("sync_timecode_missing", Some(cid(1))));
        // Nonmatching timecode: another rate, drop against non-drop, or no shared timecode span.
        let t24 = tc(1, 0, 0, 0, 10_584_000_000);
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(good, t24)), ("sync_timecode_rate_mismatch", Some(cid(2))));
        let nd = tc(1, 0, 0, 0, F2997);
        let mut df = nd;
        df.start.drop_frame = true;
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(nd, df)), ("sync_timecode_rate_mismatch", Some(cid(2))));
        let far = tc(5, 0, 0, 0, F);
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(good, far)), ("sync_timecode_no_overlap", Some(cid(2))));
        // A changed speed has no single timecode alignment.
        let mut fast = clip(2, 0, 20 * F, 0, 10 * F);
        fast.speed = Speed { numerator: 2, denominator: 1 };
        let varied = snapshot(vec![track(1, TrackKind::Video, 0, vec![fclip(1, 10, 10)]), track(2, TrackKind::Audio, 0, vec![fast])]);
        assert_eq!(code(&varied, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(good, good)), ("sync_timecode_speed", Some(cid(2))));
        // Hostile port values stay typed.
        let huge = SourceTimecode { start: Timecode { hours: u64::MAX, minutes: 0, seconds: 0, frames: 0, drop_frame: false }, rate: good.rate };
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(good, huge)).0, "overflow");
        let label = SourceTimecode { start: Timecode { hours: 0, minutes: 0, seconds: 0, frames: 99, drop_frame: false }, rate: good.rate };
        assert_eq!(code(&snap, &[1, 2], 1, SyncPoint::Timecode, &cam_rec(good, label)).0, "invalid_timecode");
        // Selection shape.
        assert_eq!(code(&snap, &[1], 1, SyncPoint::InPoint, &NoSourceTimecodes).0, "sync_too_few_clips");
        assert_eq!(code(&snap, &[1, 2], 3, SyncPoint::InPoint, &NoSourceTimecodes).0, "invalid_input");
        assert_eq!(code(&snap, &[1, 2], 99, SyncPoint::InPoint, &NoSourceTimecodes).0, "invalid_input");
        assert_eq!(code(&snap, &[3, 4], 3, SyncPoint::InPoint, &NoSourceTimecodes), ("link_same_track", Some(cid(4))));
        let grouped = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![fclip(1, 10, 10)]),
            track(2, TrackKind::Audio, 0, vec![linked(fclip(2, 0, 10), "lg")]),
        ]);
        assert_eq!(code(&grouped, &[1, 2], 1, SyncPoint::InPoint, &NoSourceTimecodes), ("clip_already_linked", Some(cid(2))));
        let mut tracks = before.sequences[0].tracks.clone();
        tracks[0].locked = true;
        assert_eq!(code(&snapshot(tracks), &[1, 2], 2, SyncPoint::InPoint, &NoSourceTimecodes).0, "track_locked");
        // An overlap the alignment would cause.
        assert_eq!(code(&snap, &[1, 3], 1, SyncPoint::InPoint, &NoSourceTimecodes).0, "overlap");
        // Budget before any work.
        let huge = Synchronize {
            clip_ids: vec![cid(1); MAX_LINKED_SELECTION + 1],
            anchor_clip_id: cid(1),
            point: SyncPoint::InPoint,
            link_group: "g".into(),
        };
        let e = with_ctx(None, |ctx| plan_synchronize(&snap, SEQ, 3, &huge, &NoSourceTimecodes, ctx)).err().unwrap();
        assert_eq!(e.code(), "limit_exceeded");
        assert_eq!(*snap.document(), before, "input unchanged");
    });
}

/// `git grep -iE "correlat|waveform"` at the lane base: no audio correlator in a crate pulse may
/// depend on, so this mode is a typed refusal, never a guessed offset.
#[test]
fn audio_waveform_mode_is_refused_typed() {
    bounded_case("waveform", || {
        let snap = snapshot(separate_av());
        let e = sync(&snap, &[1, 2], 1, SyncPoint::AudioWaveform, &NoSourceTimecodes).err().unwrap();
        assert_eq!(e.code(), "sync_waveform_unavailable");
        assert_eq!(e.failure_code(), Some(hsk_studio_observe::FailureCode::Unsupported));
        let e = merge(&snap, &[1, 2], Some(SyncPoint::AudioWaveform), &NoSourceTimecodes).err().unwrap();
        assert_eq!(e.code(), "sync_waveform_unavailable");
        // The code carries no id and displays as the code.
        assert_eq!(e.clip_id(), None);
        assert_eq!(e.to_string(), "sync_waveform_unavailable");
    });
}

#[test]
fn alignment_lists_transitions_it_ends_and_undo_restores_them() {
    bounded_case("transitions", || {
        let dissolve = TimelineTransition {
            schema_id: TransitionSchema::V1,
            transition_id: "tr-1".into(),
            transition_kind: "cross_dissolve".into(),
            duration: Ticks::new(2 * F),
            alignment: TransitionAlignment::CenteredOnCut,
            center_offset: 0,
            parameters: Vec::new(),
            repeat_end_frame: false,
        };
        let mut outgoing = clip(1, 0, 10 * F, 0, 10 * F);
        outgoing.transitions.tail = Some(dissolve);
        let snap = snapshot(vec![
            track(1, TrackKind::Video, 0, vec![outgoing, clip(2, 2 * F, 12 * F, 10 * F, 10 * F)]),
            track(2, TrackKind::Audio, 0, vec![fclip(3, 30, 5)]),
        ]);
        // The clip after the cut moves away: the transition no longer plays, and it says so.
        let request = Synchronize { clip_ids: ids(&[2, 3]), anchor_clip_id: cid(3), point: SyncPoint::InPoint, link_group: "g".into() };
        let m = with_ctx(Some(1000 * F), |ctx| plan_synchronize(&snap, SEQ, 3, &request, &NoSourceTimecodes, ctx)).unwrap();
        assert_eq!(start_f(m.proposal().snapshot(), 2), 30);
        assert_eq!(m.proposal().dropped_transitions(), ["tr-1".to_owned()]);
        assert!(find(m.proposal().snapshot(), 1).transitions.tail.is_none());
        undo_restores(m.proposal(), &snap);
        // A link written in place moves nothing, so it leaves transitions alone even with unknown
        // source lengths, where a timing edit could not keep one; unlink likewise.
        let merged = merge(&snap, &[1, 3], None, &NoSourceTimecodes).unwrap();
        assert!(merged.proposal().dropped_transitions().is_empty());
        assert!(find(merged.proposal().snapshot(), 1).transitions.tail.is_some());
        let s = merged.proposal().snapshot();
        let unlinked = with_ctx(None, |ctx| plan_unlink(s, SEQ, rev(s), &cid(1), ctx)).unwrap();
        assert!(unlinked.dropped_transitions().is_empty());
        assert!(find(unlinked.snapshot(), 1).transitions.tail.is_some());
    });
}

fn held(mut c: StudioClip, frame: u64) -> StudioClip {
    c.retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(frame * F), anchor: FrameHoldAnchor::Playhead });
    c
}
/// What the program monitor reads at `frame` (`clip_source_at`, the path `frame_layers` uses).
fn picture_at(s: &Snapshot, n: u32, frame: u64) -> u64 {
    clip_source_at(&find(s, n), Ticks::new(frame * F), &Extent(Some(1000 * F)), false).unwrap().0.value()
}

/// STU-VID-031a: a held clip keeps its hold through a link and through a move, because in point and
/// out point read timeline positions. Timecode and marker read a source position, and a held clip
/// plays none: it shows one frame at every tick, so those two bases are refused for it.
#[test]
fn a_held_clip_links_and_moves_but_cannot_align_by_a_source_position() {
    bounded_case("held clip", || {
        let held_av = || vec![
            track(1, TrackKind::Video, 0, vec![held(marked(fclip(1, 20, 10), "clap", 1, 5 * F), 3)]),
            track(2, TrackKind::Audio, 0, vec![marked(fclip(2, 0, 4), "clap", 2, F)]),
        ];
        let snap = snapshot(held_av());
        let hold = find(&snap, 1).retime;
        assert!(hold.frame_hold.is_some());
        assert_eq!(picture_at(&snap, 1, 25), 3 * F, "fixture holds source frame 3");
        let only_records = |p: &EditProposal| !p.changes().iter().any(|c| matches!(c, ClipChange::RetimeRecord { .. }));

        // Link in place: the hold stays and only the link is written.
        let m = merge(&snap, &[1, 2], None, &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((find(s, 1).retime, group(s, 1).as_deref()), (hold, Some("av-1")));
        assert_eq!(picture_at(s, 1, 25), 3 * F);
        assert_eq!(m.proposal().changes().len(), 2);
        assert!(m.proposal().changes().iter().all(|c| matches!(c, ClipChange::Relink { .. })));

        // In point, the held clip as anchor: the audio comes to it and plays its own window.
        let m = merge(&snap, &[1, 2], Some(SyncPoint::InPoint), &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), start_f(s, 2)), (20, 20));
        assert_eq!((find(s, 1).retime, picture_at(s, 1, 25)), (hold, 3 * F));
        assert_eq!(picture_at(s, 2, 21), F, "the linked audio keeps playing");
        // In point, the held clip moving to an audio anchor: the frame goes with it, and undo is exact.
        let m = sync(&snap, &[1, 2], 2, SyncPoint::InPoint, &NoSourceTimecodes).unwrap();
        let s = m.proposal().snapshot();
        assert_eq!((start_f(s, 1), source(s, 1)), (0, source(&snap, 1)));
        assert_eq!((find(s, 1).retime, picture_at(s, 1, 5)), (hold, 3 * F));
        assert!(only_records(m.proposal()), "{:?}", m.proposal().changes());
        undo_restores(m.proposal(), &snap);
        // Out point reads the timeline end the same way.
        let m = sync(&snap, &[1, 2], 1, SyncPoint::OutPoint, &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(m.proposal().snapshot(), 2), 26);
        assert_eq!(find(m.proposal().snapshot(), 1).retime, hold);

        // A group move carries the hold and writes no retime record.
        let p = plan_move(&snap, &[1], 1, LinkedSelection::On).unwrap();
        assert_eq!((start_f(p.snapshot(), 1), find(p.snapshot(), 1).retime), (21, hold));
        assert_eq!(picture_at(p.snapshot(), 1, 21), 3 * F);
        assert!(only_records(&p));

        // Marker and timecode read a source position, which a held clip does not play: refused and
        // named, as the anchor or as a member, whatever the other clip has.
        let clap = || SyncPoint::Marker { name: "clap".into() };
        let held_named = ("sync_hold_unsupported", Some(cid(1)));
        assert_eq!(refused(merge(&snap, &[1, 2], Some(clap()), &NoSourceTimecodes)), held_named);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, clap(), &NoSourceTimecodes)), held_named);
        assert_eq!(refused(sync(&snap, &[1, 2], 2, clap(), &NoSourceTimecodes)), held_named);
        let both = Timecodes(vec![("manifest-1", tc(1, 0, 0, 0, F)), ("manifest-2", tc(1, 0, 0, 0, F))]);
        assert_eq!(refused(sync(&snap, &[1, 2], 1, SyncPoint::Timecode, &both)), held_named);
        assert_eq!(refused(sync(&snap, &[1, 2], 2, SyncPoint::Timecode, &both)), held_named);
        assert_eq!(refused(merge(&snap, &[1, 2], Some(SyncPoint::Timecode), &both)), held_named);
        assert_eq!(
            sync(&snap, &[1, 2], 1, clap(), &NoSourceTimecodes).err().unwrap().failure_code(),
            Some(hsk_studio_observe::FailureCode::Unsupported)
        );
        // The way out is the hold's own planner: cleared, the marker basis works (25F - 1F = 24F).
        let cleared = with_ctx(None, |ctx| plan_clear_frame_hold(&snap, SEQ, 3, &cid(1), ctx)).unwrap();
        let s = cleared.snapshot();
        let m = merge(s, &[1, 2], Some(clap()), &NoSourceTimecodes).unwrap();
        assert_eq!(start_f(m.proposal().snapshot(), 2), 24);

        // Link operations on a linked held clip: unlink and the toggle leave the hold alone.
        let mut tracks = held_av();
        tracks[0].clips[0].link_group = Some("lg".into());
        tracks[1].clips[0].link_group = Some("lg".into());
        let linked_held = snapshot(tracks);
        let p = with_ctx(None, |ctx| plan_unlink(&linked_held, SEQ, 3, &cid(1), ctx)).unwrap();
        assert_eq!((find(p.snapshot(), 1).retime, group(p.snapshot(), 1)), (hold, None));
        let p = plan_linked(&linked_held, &trim_out(1, 2), LinkedSelection::Suspended).unwrap();
        assert_eq!((find(p.snapshot(), 1).duration.value(), find(p.snapshot(), 1).retime), (2 * F, hold));
        assert_eq!((find(p.snapshot(), 2).duration.value(), group(p.snapshot(), 1).as_deref()), (4 * F, Some("lg")));
        let razor = TimelineEdit::Razor { clip_id: cid(1), at: Ticks::new(25 * F), new_right_clip_id: cid(11), linked: None };
        let p = plan_linked(&linked_held, &razor, LinkedSelection::Suspended).unwrap();
        assert_eq!((find(p.snapshot(), 1).retime, find(p.snapshot(), 11).retime), (hold, hold));
        assert_eq!(group(p.snapshot(), 11), None);
        assert_eq!(picture_at(p.snapshot(), 11, 27), 3 * F);
    });
}

#[test]
fn descriptor_names_the_link_operations_and_their_refusals() {
    for token in [
        "plan_merge_clips",
        "plan_synchronize",
        "plan_unlink",
        "plan_move_clips",
        "plan_timeline_edit_with_linked_selection",
        "merge_no_av_pair",
        "sync_waveform_unavailable",
        "sync_hold_unsupported",
        "UNVERIFIED",
    ] {
        assert!(hsk_studio_pulse::DESCRIPTOR.contains(token), "{token}");
    }
}
