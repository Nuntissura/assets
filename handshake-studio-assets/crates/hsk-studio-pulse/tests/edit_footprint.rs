//! Lane SMB-PR-EDIT-FOOTPRINT-1: every real edit reports what it writes. The oracle here walks the
//! base and the prospective sequence record by record and does not use the planner's own diff: a
//! clip, track, marker or transition that differs must be named, and nothing else may be. Every
//! row is also checked as its own undo through `plan_timeline_revert`.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use std::collections::{BTreeMap, BTreeSet};

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
const SEQ: &str = "seq-main";

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    id("SCLP", n)
}
fn tid(n: u32) -> String {
    id("STRK", n)
}
fn ids(list: &[u32]) -> Vec<String> {
    list.iter().map(|n| cid(*n)).collect()
}
fn at(frames: u64) -> Ticks {
    Ticks::new(frames * F)
}
fn delta(frames: i64) -> TickDelta {
    TickDelta::new(i128::from(frames) * i128::from(F)).unwrap()
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

fn media(n: u32) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: format!("manifest-{n}"),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
        asset_id: None,
    }
}
/// `frames` long at frame `start`, reading its source from frame 10, so both handles are free.
fn clip(n: u32, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: media(n),
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
fn marker(marker_id: &str, name: &str, start: Ticks) -> StudioMarker {
    StudioMarker {
        marker_id: marker_id.into(),
        name: name.into(),
        comment: String::new(),
        color: LabelColor::Rose,
        start,
        duration: Ticks::new(0),
        role: None,
    }
}
fn dissolve(transition_id: &str, kind: &str) -> TimelineTransition {
    TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: transition_id.into(),
        transition_kind: kind.into(),
        duration: at(2),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    }
}
fn track(n: u32, kind: TrackKind, sync_locked: bool, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index: 0,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    }
}

/// V1 [1 0..4 with mk-c1, 2 4..8 with mk-c2 and the tail transition tr-1, 3 8..12, gap, 6 14..16],
/// A1 [4 0..2, 5 4..8]; the sequence marker mk-s1 at frame 2. Frames of 25 fps.
fn fixture(audio_sync_locked: bool) -> Snapshot {
    let mut one = clip(1, 0, 4);
    one.markers.push(marker("mk-c1", "first", at(11)));
    let mut two = clip(2, 4, 4);
    two.markers.push(marker("mk-c2", "second", at(13)));
    two.transitions.tail = Some(dissolve("tr-1", "cross_dissolve"));
    let tracks = vec![
        track(1, TrackKind::Video, true, vec![one, two, clip(3, 8, 4), clip(6, 14, 2)]),
        track(2, TrackKind::Audio, audio_sync_locked, vec![clip(4, 0, 2), clip(5, 4, 4)]),
    ];
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
            duration: at(16),
            tracks,
            markers: vec![marker("mk-s1", "sequence", at(2))],
            work_area: None,
        }],
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}

fn rev(s: &Snapshot) -> u64 {
    s.document().revision
}
fn seq(s: &Snapshot) -> &StudioSequence {
    &s.document().sequences[0]
}

// ---- The oracle: walks the records, knows nothing of the planner's diff.

fn clips_of(s: &StudioSequence) -> BTreeMap<&str, (&str, &StudioClip)> {
    s.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (c.clip_id.as_str(), (t.track_id.as_str(), c)))).collect()
}
fn marker_ids_differing(a: &[StudioMarker], b: &[StudioMarker]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for m in a {
        if b.iter().find(|x| x.marker_id == m.marker_id) != Some(m) {
            out.insert(m.marker_id.clone());
        }
    }
    for m in b {
        if !a.iter().any(|x| x.marker_id == m.marker_id) {
            out.insert(m.marker_id.clone());
        }
    }
    out
}
fn slots(c: &StudioClip) -> [&Option<TimelineTransition>; 2] {
    [&c.transitions.head, &c.transitions.tail]
}
fn transition_ids_differing(a: &StudioClip, b: &StudioClip) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (x, y) in slots(a).into_iter().zip(slots(b)) {
        if x != y {
            out.extend(x.iter().chain(y.iter()).map(|t| t.transition_id.clone()));
        }
    }
    out
}
fn nested_ids(c: &StudioClip) -> Vec<String> {
    let markers = c.markers.iter().map(|m| m.marker_id.clone());
    let transitions = slots(c).into_iter().flatten().map(|t| t.transition_id.clone());
    markers.chain(transitions).collect()
}

/// Every id the edit wrote: a clip whose stored value differs, a marker or transition that differs
/// and the clip or sequence holding it, a track whose clip list gained or lost a clip, the sequence
/// when a stored sequence member differs.
fn expected_writes(old: &StudioSequence, new: &StudioSequence) -> BTreeSet<String> {
    let (before, after) = (clips_of(old), clips_of(new));
    let mut out = BTreeSet::new();
    for (clip_id, (track_id, clip)) in &before {
        match after.get(clip_id) {
            None => {
                out.insert((*clip_id).to_owned());
                out.insert((*track_id).to_owned());
                out.extend(nested_ids(clip));
            }
            Some((new_track, new_clip)) => {
                if clip != new_clip {
                    out.insert((*clip_id).to_owned());
                }
                if track_id != new_track {
                    out.insert((*track_id).to_owned());
                    out.insert((*new_track).to_owned());
                }
                out.extend(marker_ids_differing(&clip.markers, &new_clip.markers));
                out.extend(transition_ids_differing(clip, new_clip));
            }
        }
    }
    for (clip_id, (track_id, clip)) in &after {
        if !before.contains_key(clip_id) {
            out.insert((*clip_id).to_owned());
            out.insert((*track_id).to_owned());
            out.extend(nested_ids(clip));
        }
    }
    let sequence_markers = marker_ids_differing(&old.markers, &new.markers);
    if old.duration != new.duration || old.work_area != new.work_area || !sequence_markers.is_empty() {
        out.insert(old.sequence_id.clone());
    }
    out.extend(sequence_markers);
    out
}

/// What the change list must hold for this successor, as `(kind, record id)`. A clip that was
/// inserted carries its markers and transitions inside its `Insert`.
fn expected_entries(old: &StudioSequence, new: &StudioSequence) -> BTreeSet<(&'static str, String)> {
    let (before, after) = (clips_of(old), clips_of(new));
    let mut out = BTreeSet::new();
    for (clip_id, (_, clip)) in &before {
        let id = (*clip_id).to_owned();
        match after.get(clip_id) {
            None => {
                out.insert(("remove", id));
                out.extend(clip.markers.iter().map(|m| ("clip_marker", m.marker_id.clone())));
                out.extend(slots(clip).into_iter().flatten().map(|t| ("clip_transition", t.transition_id.clone())));
            }
            Some((_, new_clip)) => {
                let window = |c: &StudioClip| (c.source_in, c.source_out, c.timeline_start, c.duration);
                if window(clip) != window(new_clip) {
                    out.insert(("retime", id.clone()));
                }
                if clip.speed != new_clip.speed {
                    out.insert(("speed", id.clone()));
                }
                if clip.retime != new_clip.retime {
                    out.insert(("retime_record", id.clone()));
                }
                if clip.link_group != new_clip.link_group {
                    out.insert(("relink", id));
                }
                out.extend(marker_ids_differing(&clip.markers, &new_clip.markers).into_iter().map(|m| ("clip_marker", m)));
                out.extend(transition_ids_differing(clip, new_clip).into_iter().map(|t| ("clip_transition", t)));
            }
        }
    }
    for clip_id in after.keys() {
        if !before.contains_key(clip_id) {
            out.insert(("insert", (*clip_id).to_owned()));
        }
    }
    out.extend(marker_ids_differing(&old.markers, &new.markers).into_iter().map(|m| ("sequence_marker", m)));
    if old.work_area != new.work_area {
        out.insert(("work_area", old.sequence_id.clone()));
    }
    out
}

/// The record each entry names, as `(kind, id)`: the sub-record's own id where it has one.
fn entries_of(p: &EditProposal) -> BTreeSet<(&'static str, String)> {
    p.changes()
        .iter()
        .map(|c| {
            let name = match c {
                ClipChange::ClipMarker { marker_id, .. } | ClipChange::SequenceMarker { marker_id, .. } => marker_id.clone(),
                ClipChange::ClipTransition { before, after, .. } => {
                    after.as_ref().or(before.as_ref()).map(|t| t.transition_id.clone()).unwrap_or_default()
                }
                ClipChange::WorkArea { sequence_id, .. } => sequence_id.clone(),
                other => other.clip_id().unwrap_or_default().to_owned(),
            };
            (c.kind(), name)
        })
        .collect()
}

/// A replaced slot names both ids in the footprint; the entry names the one it now holds.
fn entries_match(row: &str, old: &StudioSequence, new: &StudioSequence, p: &EditProposal, failures: &mut Vec<String>) {
    let (expected, actual) = (expected_entries(old, new), entries_of(p));
    let missing: Vec<_> = expected.iter().filter(|e| !actual.contains(e) && !replaced_id(e, old, new)).collect();
    let extra: Vec<_> = actual.difference(&expected).collect();
    if !missing.is_empty() || !extra.is_empty() {
        failures.push(format!("{row}: changes() missing {missing:?}, extra {extra:?}"));
    }
}
fn replaced_id(entry: &(&'static str, String), old: &StudioSequence, new: &StudioSequence) -> bool {
    // The old id of a slot whose transition took another id.
    entry.0 == "clip_transition"
        && clips_of(old).iter().any(|(clip_id, (_, c))| {
            clips_of(new).get(clip_id).is_some_and(|(_, n)| {
                slots(c).into_iter().zip(slots(n)).any(|(x, y)| {
                    matches!((x, y), (Some(a), Some(b)) if a.transition_id == entry.1 && b.transition_id != entry.1)
                })
            })
        })
}

fn check_proposal(row: &str, base: &Snapshot, p: &EditProposal, failures: &mut Vec<String>) {
    let (old, new) = (seq(base), seq(p.snapshot()));
    if old == new {
        failures.push(format!("{row}: the fixture edit changed nothing"));
        return;
    }
    if p.changes().is_empty() {
        failures.push(format!("{row}: changes() is empty"));
    }
    if p.write_footprint().is_empty() {
        failures.push(format!("{row}: write_footprint() is empty"));
    }
    let written: BTreeSet<String> = p.write_footprint().iter().cloned().collect();
    if written.len() != p.write_footprint().len() {
        failures.push(format!("{row}: write_footprint() repeats an id: {:?}", p.write_footprint()));
    }
    let expected = expected_writes(old, new);
    if written != expected {
        let missing: Vec<_> = expected.difference(&written).collect();
        let extra: Vec<_> = written.difference(&expected).collect();
        failures.push(format!("{row}: write_footprint() missing {missing:?}, extra {extra:?}"));
    }
    entries_match(row, old, new, p, failures);
}

struct Row {
    name: &'static str,
    base: Snapshot,
    proposal: EditProposal,
}

fn timeline_row(name: &'static str, base: &Snapshot, edit: TimelineEdit) -> Row {
    let proposal = with_ctx(|ctx| plan_timeline_edit(base, SEQ, rev(base), &edit, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn record_row(name: &'static str, base: &Snapshot, edit: RecordEdit) -> Row {
    let proposal = with_ctx(|ctx| plan_record_edit(base, SEQ, rev(base), &edit, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn assembly_row(name: &'static str, base: &Snapshot, mode: AssemblyMode, start: u64, frames: u64, new: u32, fresh: &[u32]) -> Row {
    let edit = AssemblyEdit {
        mode,
        source: media(new),
        speed: Speed::UNIT,
        points: AssemblyPoints { source_in: Some(at(0)), source_out: Some(at(frames)), sequence_in: Some(at(start)), sequence_out: None },
        fit: None,
        patches: vec![SourcePatch { track_id: tid(1), clip_id: cid(new) }],
        link_group: None,
        fresh_clip_ids: ids(fresh),
    };
    let proposal = with_ctx(|ctx| plan_assembly_edit(base, SEQ, rev(base), &edit, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn speed_row(name: &'static str, base: &Snapshot, clip_no: u32, target: SpeedDurationTarget, reverse: bool) -> Row {
    let edit = SpeedDuration { clip_id: cid(clip_no), target, reverse };
    let proposal = with_ctx(|ctx| plan_speed_duration(base, SEQ, rev(base), &edit, None, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn grid() -> SourceFrameGrid {
    SourceFrameGrid::new(F).unwrap()
}
fn hold_row(name: &'static str, base: &Snapshot, clip_no: u32) -> Row {
    let edit = SetFrameHold { clip_id: cid(clip_no), at: HoldAt::InPoint, grid: grid() };
    let proposal = with_ctx(|ctx| plan_set_frame_hold(base, SEQ, rev(base), &edit, None, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn merge_row(name: &'static str, base: &Snapshot, list: &[u32], align: Option<SyncPoint>) -> Row {
    let edit = MergeClips { clip_ids: ids(list), link_group: "av-1".into(), align };
    let proposal = with_ctx(|ctx| plan_merge_clips(base, SEQ, rev(base), &edit, &NoSourceTimecodes, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal: proposal.proposal().clone() }
}
fn move_row(name: &'static str, base: &Snapshot, list: &[u32], frames: i64) -> Row {
    let edit = MoveClips { clip_ids: ids(list), delta: delta(frames), linked: LinkedSelection::On };
    let proposal = with_ctx(|ctx| plan_move_clips(base, SEQ, rev(base), &edit, ctx)).unwrap_or_else(|e| panic!("{name}: {}", e.code()));
    Row { name, base: base.clone(), proposal }
}
fn renamed(marker_id: &str, start: Ticks) -> StudioMarker {
    marker(marker_id, "renamed", start)
}
fn trim_out(n: u32, frames: u64, ripple: bool) -> TimelineEdit {
    TimelineEdit::TrimOut { clip_id: cid(n), new_duration: at(frames), ripple }
}

/// One row per command of every planner that returns an `EditProposal` (the lane file lists the
/// sites and why the others do not apply).
fn rows() -> Vec<Row> {
    let s = fixture(true);
    let unsynced = fixture(false);
    let mut rows = vec![
        // plan_timeline_edit, every TimelineEdit.
        timeline_row("trim_out", &s, trim_out(1, 3, false)),
        timeline_row("trim_out_ripple", &s, trim_out(1, 3, true)),
        timeline_row("trim_in", &s, TimelineEdit::TrimIn { clip_id: cid(6), new_head: at(15), ripple: false }),
        timeline_row("razor_hands_over_marker_and_transition", &s, TimelineEdit::Razor { clip_id: cid(2), at: at(6), new_right_clip_id: cid(20), linked: None }),
        timeline_row("roll", &s, TimelineEdit::Roll { left_clip_id: cid(1), delta: delta(1) }),
        timeline_row("lift_clip", &s, TimelineEdit::LiftClip { clip_id: cid(6) }),
        timeline_row("lift_clip_with_marker_and_transition", &s, TimelineEdit::LiftClip { clip_id: cid(2) }),
        timeline_row("extract_clip", &s, TimelineEdit::ExtractClip { clip_id: cid(3) }),
        timeline_row("lift_range", &s, TimelineEdit::LiftRange { span: TickRange::new(at(1), at(2)).unwrap(), fresh_clip_ids: ids(&[21, 22]) }),
        timeline_row("extract_range", &s, TimelineEdit::ExtractRange { span: TickRange::new(at(1), at(2)).unwrap(), fresh_clip_ids: ids(&[21, 22]) }),
        timeline_row("close_gap", &s, TimelineEdit::CloseGap { track_id: tid(1), at: at(13) }),
        timeline_row("slip", &s, TimelineEdit::Slip { clip_id: cid(1), delta: delta(1) }),
        timeline_row("slide", &s, TimelineEdit::Slide { clip_id: cid(2), delta: delta(1) }),
        // plan_assembly_edit.
        assembly_row("insert_after_the_end", &s, AssemblyMode::Insert, 20, 2, 30, &[]),
        assembly_row("insert_splits_a_clip", &unsynced, AssemblyMode::Insert, 6, 2, 30, &[31]),
        assembly_row("overwrite_splits_a_clip", &s, AssemblyMode::Overwrite, 9, 1, 30, &[31]),
        // plan_record_edit, every RecordEdit in both scopes.
        record_row("add_sequence_marker", &s, RecordEdit::AddMarker { scope: MarkerScope::Sequence, marker: marker("mk-new", "new", at(1)) }),
        record_row("add_clip_marker", &s, RecordEdit::AddMarker { scope: MarkerScope::Clip { clip_id: cid(3) }, marker: marker("mk-new", "new", at(11)) }),
        record_row("replace_sequence_marker", &s, RecordEdit::ReplaceMarker { marker: renamed("mk-s1", at(2)) }),
        record_row("replace_clip_marker", &s, RecordEdit::ReplaceMarker { marker: renamed("mk-c1", at(11)) }),
        record_row("remove_sequence_marker", &s, RecordEdit::RemoveMarker { marker_id: "mk-s1".into() }),
        record_row("remove_clip_marker", &s, RecordEdit::RemoveMarker { marker_id: "mk-c1".into() }),
        record_row("set_transition", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(1), transition: Some(Box::new(dissolve("tr-2", "cross_dissolve"))) }),
        record_row("replace_transition_same_id", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: Some(Box::new(dissolve("tr-1", "wipe"))) }),
        record_row("replace_transition_new_id", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: Some(Box::new(dissolve("tr-3", "cross_dissolve"))) }),
        record_row("clear_transition", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: None }),
        record_row("set_work_area", &s, RecordEdit::SetWorkArea(Some(WorkArea { start: at(1), end: at(5) }))),
        // plan_rate_stretch and plan_speed_duration.
        {
            let edit = RateStretch { clip_id: cid(6), edge: RateEdge::Tail, new_edge: at(18) };
            let proposal = with_ctx(|ctx| plan_rate_stretch(&s, SEQ, rev(&s), &edit, None, ctx)).unwrap();
            Row { name: "rate_stretch", base: s.clone(), proposal }
        },
        speed_row("speed_duration_reverse_only", &s, 6, SpeedDurationTarget::Speed(Speed::UNIT), true),
        speed_row("speed_duration_new_duration", &s, 6, SpeedDurationTarget::Duration(at(1)), false),
        // The retime planners.
        hold_row("set_frame_hold", &s, 1),
        {
            let edit = InsertFrameHoldSegment { clip_id: cid(3), span: TickRange::new(at(9), at(10)).unwrap(), at: HoldAt::InPoint, grid: grid(), fresh_clip_ids: ids(&[40, 41]), fresh_link_groups: Vec::new() };
            let proposal = with_ctx(|ctx| plan_insert_frame_hold_segment(&s, SEQ, rev(&s), &edit, None, ctx)).unwrap();
            Row { name: "insert_frame_hold_segment", base: s.clone(), proposal }
        },
        {
            let edit = RetimeOptions { clip_id: cid(1), frame_interpolation: Some(FrameInterpolation::FrameBlending), maintain_audio_pitch: Some(true) };
            let proposal = with_ctx(|ctx| plan_set_retime_options(&s, SEQ, rev(&s), &edit, ctx)).unwrap();
            Row { name: "set_retime_options", base: s.clone(), proposal }
        },
        // The merge and sync planners.
        merge_row("merge_clips_in_place", &s, &[1, 4], None),
        merge_row("merge_clips_aligned", &s, &[6, 5], Some(SyncPoint::InPoint)),
        {
            let edit = Synchronize { clip_ids: ids(&[3, 5]), anchor_clip_id: cid(3), point: SyncPoint::InPoint, link_group: "sync-1".into() };
            let proposal = with_ctx(|ctx| plan_synchronize(&s, SEQ, rev(&s), &edit, &NoSourceTimecodes, ctx)).unwrap();
            Row { name: "synchronize", base: s.clone(), proposal: proposal.proposal().clone() }
        },
        move_row("move_clips", &s, &[6], 2),
        move_row("move_drops_the_transition_of_a_clip_that_stays", &s, &[3], 1),
    ];
    // Planners that need an earlier step's result.
    let after = |rows: &[Row], name: &str| rows.iter().find(|r| r.name == name).expect("earlier row").proposal.snapshot().clone();
    let held = after(&rows, "set_frame_hold");
    let proposal = with_ctx(|ctx| plan_clear_frame_hold(&held, SEQ, rev(&held), &cid(1), ctx)).unwrap();
    rows.push(Row { name: "clear_frame_hold", base: held, proposal });
    let linked = after(&rows, "merge_clips_in_place");
    let proposal = with_ctx(|ctx| plan_unlink(&linked, SEQ, rev(&linked), &cid(1), ctx)).unwrap();
    rows.push(Row { name: "unlink", base: linked.clone(), proposal });
    let proposal = with_ctx(|ctx| plan_timeline_edit_with_linked_selection(&linked, SEQ, rev(&linked), &trim_out(1, 3, false), LinkedSelection::Suspended, ctx)).unwrap();
    rows.push(Row { name: "linked_selection_suspended_trim", base: linked, proposal });
    let area = after(&rows, "set_work_area");
    rows.push(record_row("move_work_area", &area, RecordEdit::SetWorkArea(Some(WorkArea { start: at(2), end: at(5) }))));
    rows.push(record_row("clear_work_area", &area, RecordEdit::SetWorkArea(None)));
    rows
}

#[test]
fn every_planner_names_every_changed_record() {
    bounded_case("every_planner_names_every_changed_record", || {
        let mut failures = Vec::new();
        let rows = rows();
        for row in &rows {
            check_proposal(row.name, &row.base, &row.proposal, &mut failures);
            // plan_timeline_revert: the undo of every row reports the same records going back.
            let done = row.proposal.snapshot();
            let undo = with_ctx(|ctx| plan_timeline_revert(done, SEQ, rev(done), row.proposal.inverse(), ctx)).unwrap_or_else(|e| panic!("{} undo: {}", row.name, e.code()));
            check_proposal(&format!("undo {}", row.name), done, &undo, &mut failures);
        }
        assert_eq!(rows.len(), 43, "rows");
        assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    });
}

/// `(row, what the planner answered)`: a planner given an edit with no effect refuses `no_change`,
/// so there is no proposal and nothing is reported.
#[test]
fn an_edit_that_changes_nothing_reports_nothing() {
    bounded_case("an_edit_that_changes_nothing_reports_nothing", || {
        let s = fixture(true);
        let area = with_ctx(|ctx| plan_record_edit(&s, SEQ, rev(&s), &RecordEdit::SetWorkArea(Some(WorkArea { start: at(1), end: at(5) })), ctx)).unwrap();
        let area = area.snapshot();
        fn answer<E>(r: std::result::Result<EditProposal, E>, code: impl Fn(&E) -> String) -> String {
            match r {
                Ok(p) => format!("planned changes={:?} writes={:?}", p.changes(), p.write_footprint()),
                Err(e) => code(&e),
            }
        }
        let timeline = |edit: TimelineEdit| answer(with_ctx(|ctx| plan_timeline_edit(&s, SEQ, rev(&s), &edit, ctx)), |e| e.code().to_owned());
        let record = |base: &Snapshot, edit: RecordEdit| answer(with_ctx(|ctx| plan_record_edit(base, SEQ, rev(base), &edit, ctx)), |e| e.code().to_owned());
        let unit_speed = SpeedDuration { clip_id: cid(6), target: SpeedDurationTarget::Speed(Speed::UNIT), reverse: false };
        let same_edge = RateStretch { clip_id: cid(6), edge: RateEdge::Tail, new_edge: at(16) };
        let no_options = RetimeOptions { clip_id: cid(1), frame_interpolation: None, maintain_audio_pitch: None };
        let no_move = MoveClips { clip_ids: ids(&[6]), delta: TickDelta::ZERO, linked: LinkedSelection::On };
        let table: Vec<(&str, String)> = vec![
            ("trim_out", timeline(trim_out(1, 4, false))),
            ("trim_in", timeline(TimelineEdit::TrimIn { clip_id: cid(6), new_head: at(14), ripple: false })),
            ("roll", timeline(TimelineEdit::Roll { left_clip_id: cid(1), delta: TickDelta::ZERO })),
            ("slip", timeline(TimelineEdit::Slip { clip_id: cid(1), delta: TickDelta::ZERO })),
            ("slide", timeline(TimelineEdit::Slide { clip_id: cid(2), delta: TickDelta::ZERO })),
            ("replace_sequence_marker", record(&s, RecordEdit::ReplaceMarker { marker: marker("mk-s1", "sequence", at(2)) })),
            ("replace_clip_marker", record(&s, RecordEdit::ReplaceMarker { marker: marker("mk-c1", "first", at(11)) })),
            ("clear_empty_transition", record(&s, RecordEdit::SetTransition { outgoing_clip_id: cid(1), transition: None })),
            ("same_transition", record(&s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: Some(Box::new(dissolve("tr-1", "cross_dissolve"))) })),
            ("clear_empty_work_area", record(&s, RecordEdit::SetWorkArea(None))),
            ("same_work_area", record(area, RecordEdit::SetWorkArea(Some(WorkArea { start: at(1), end: at(5) })))),
            ("rate_stretch", answer(with_ctx(|ctx| plan_rate_stretch(&s, SEQ, rev(&s), &same_edge, None, ctx)), |e| e.code().to_owned())),
            ("speed_duration", answer(with_ctx(|ctx| plan_speed_duration(&s, SEQ, rev(&s), &unit_speed, None, ctx)), |e| e.code().to_owned())),
            ("clear_frame_hold", answer(with_ctx(|ctx| plan_clear_frame_hold(&s, SEQ, rev(&s), &cid(1), ctx)), |e| e.code().to_owned())),
            ("retime_options", answer(with_ctx(|ctx| plan_set_retime_options(&s, SEQ, rev(&s), &no_options, ctx)), |e| e.code().to_owned())),
            ("unlink", answer(with_ctx(|ctx| plan_unlink(&s, SEQ, rev(&s), &cid(1), ctx)), |e| e.code().to_owned())),
            ("move_clips", answer(with_ctx(|ctx| plan_move_clips(&s, SEQ, rev(&s), &no_move, ctx)), |e| e.code().to_owned())),
        ];
        for (row, got) in &table {
            assert_eq!(got, "no_change", "{row}");
        }
    });
}

#[test]
fn a_reverse_only_speed_change_names_the_clip_and_both_speeds() {
    bounded_case("a_reverse_only_speed_change_names_the_clip_and_both_speeds", || {
        let s = fixture(true);
        let row = speed_row("reverse", &s, 6, SpeedDurationTarget::Speed(Speed::UNIT), true);
        let window = ClipWindow { source_in: at(10), source_out: at(12), timeline_start: at(14), duration: at(2) };
        let expected = ClipChange::Speed { clip_id: cid(6), track_id: tid(1), window, before: Speed::UNIT, after: Speed { numerator: -1, denominator: 1 } };
        assert_eq!(row.proposal.changes(), [expected]);
        assert_eq!(row.proposal.write_footprint(), [cid(6)], "the clip is written, its track is not");
    });
}

#[test]
fn a_window_change_keeps_its_retime_entry_and_adds_the_speed() {
    bounded_case("a_window_change_keeps_its_retime_entry_and_adds_the_speed", || {
        let s = fixture(true);
        let row = speed_row("duration", &s, 6, SpeedDurationTarget::Duration(at(1)), false);
        assert!(matches!(row.proposal.change_for(&cid(6)), Some(ClipChange::Retime { .. })));
        assert_eq!(row.proposal.changes().len(), 2);
        let ClipChange::Speed { before, after, .. } = &row.proposal.changes()[1] else {
            panic!("{:?}", row.proposal.changes());
        };
        assert_eq!((*before, *after), (Speed::UNIT, Speed { numerator: 2, denominator: 1 }));
    });
}

#[test]
fn marker_entries_carry_the_record_before_and_after() {
    bounded_case("marker_entries_carry_the_record_before_and_after", || {
        let s = fixture(true);
        let new = marker("mk-new", "new", at(1));
        let added = record_row("add", &s, RecordEdit::AddMarker { scope: MarkerScope::Sequence, marker: new.clone() }).proposal;
        assert_eq!(added.changes(), [ClipChange::SequenceMarker { sequence_id: SEQ.into(), marker_id: "mk-new".into(), before: None, after: Some(Box::new(new)) }]);
        assert_eq!(added.write_footprint(), [SEQ.to_owned(), "mk-new".to_owned()]);
        assert_eq!(added.changes()[0].clip_id(), None);
        assert_eq!(added.changes()[0].track_id(), None);

        let old = marker("mk-c1", "first", at(11));
        let new = renamed("mk-c1", at(11));
        let replaced = record_row("replace", &s, RecordEdit::ReplaceMarker { marker: new.clone() }).proposal;
        let [ClipChange::ClipMarker { clip_id, track_id, marker_id, before, after, .. }] = replaced.changes() else {
            panic!("{:?}", replaced.changes());
        };
        assert_eq!((clip_id, track_id, marker_id.as_str()), (&cid(1), &tid(1), "mk-c1"));
        assert_eq!((before.as_deref(), after.as_deref()), (Some(&old), Some(&new)));
        assert_eq!(replaced.write_footprint(), [cid(1), "mk-c1".to_owned()]);

        let removed = record_row("remove", &s, RecordEdit::RemoveMarker { marker_id: "mk-c1".into() }).proposal;
        let [ClipChange::ClipMarker { before, after: None, .. }] = removed.changes() else {
            panic!("{:?}", removed.changes());
        };
        assert_eq!(before.as_deref(), Some(&old));
        assert_eq!(removed.removed_markers(), ["mk-c1"]);
    });
}

#[test]
fn transition_entries_name_the_slot_and_both_ids() {
    bounded_case("transition_entries_name_the_slot_and_both_ids", || {
        let s = fixture(true);
        let replaced = record_row("replace", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: Some(Box::new(dissolve("tr-3", "cross_dissolve"))) }).proposal;
        let [ClipChange::ClipTransition { clip_id, slot: TransitionSlot::Tail, before, after, .. }] = replaced.changes() else {
            panic!("{:?}", replaced.changes());
        };
        assert_eq!(clip_id, &cid(2));
        assert_eq!(before.as_ref().map(|t| t.transition_id.as_str()), Some("tr-1"));
        assert_eq!(after.as_ref().map(|t| t.transition_id.as_str()), Some("tr-3"));
        assert_eq!(replaced.write_footprint(), [cid(2), "tr-1".to_owned(), "tr-3".to_owned()]);
    });
}

#[test]
fn the_work_area_is_written_through_its_sequence() {
    bounded_case("the_work_area_is_written_through_its_sequence", || {
        let s = fixture(true);
        let area = WorkArea { start: at(1), end: at(5) };
        let set = record_row("set", &s, RecordEdit::SetWorkArea(Some(area))).proposal;
        assert_eq!(set.changes(), [ClipChange::WorkArea { sequence_id: SEQ.into(), before: None, after: Some(area) }]);
        assert_eq!(set.write_footprint(), [SEQ.to_owned()]);
        let cleared = record_row("clear", set.snapshot(), RecordEdit::SetWorkArea(None)).proposal;
        assert_eq!(cleared.changes(), [ClipChange::WorkArea { sequence_id: SEQ.into(), before: Some(area), after: None }]);
    });
}

/// The clip that keeps its window but loses its tail transition is written, and is named.
#[test]
fn a_pruned_transition_names_the_clip_that_stays() {
    bounded_case("a_pruned_transition_names_the_clip_that_stays", || {
        let s = fixture(true);
        let moved = move_row("move", &s, &[3], 1).proposal;
        assert_eq!(moved.dropped_transitions(), ["tr-1"]);
        let Some(ClipChange::ClipTransition { before, after: None, slot: TransitionSlot::Tail, .. }) = moved.change_for(&cid(2)) else {
            panic!("{:?}", moved.changes());
        };
        assert_eq!(before.as_ref().map(|t| t.transition_id.as_str()), Some("tr-1"));
        assert!(moved.write_footprint().contains(&cid(2)));
        assert!(moved.write_footprint().contains(&"tr-1".to_owned()));
    });
}

/// What leaves with a removed clip follows its `Remove`.
#[test]
fn a_removed_clip_lists_what_left_with_it() {
    bounded_case("a_removed_clip_lists_what_left_with_it", || {
        let s = fixture(true);
        let lifted = timeline_row("lift", &s, TimelineEdit::LiftClip { clip_id: cid(2) }).proposal;
        let kinds: Vec<&str> = lifted.changes().iter().map(ClipChange::kind).collect();
        assert_eq!(kinds, ["remove", "clip_marker", "clip_transition"]);
        assert!(matches!(lifted.change_for(&cid(2)), Some(ClipChange::Remove { .. })));
        assert_eq!(lifted.write_footprint(), [cid(2), tid(1), "mk-c2".to_owned(), "tr-1".to_owned()]);
        assert_eq!(lifted.removed_markers(), ["mk-c2"]);
    });
}

/// What a wrong answer costs a host that admits disjoint proposals by their footprints (STU-ARC-011):
/// two proposals on one base that write the same record must share an entry, or that host applies
/// both and loses one. Two that touch nothing in common must stay disjoint.
#[test]
fn proposals_that_write_one_record_share_a_footprint_entry() {
    bounded_case("proposals_that_write_one_record_share_a_footprint_entry", || {
        let s = fixture(true);
        let overlap = |a: &EditProposal, b: &EditProposal| a.write_footprint().iter().any(|w| b.write_footprint().contains(w));
        let reverse = speed_row("reverse", &s, 6, SpeedDurationTarget::Speed(Speed::UNIT), true).proposal;
        let slip = timeline_row("slip", &s, TimelineEdit::Slip { clip_id: cid(6), delta: delta(1) }).proposal;
        assert!(overlap(&reverse, &slip), "both write clip 6");
        let area_a = record_row("a", &s, RecordEdit::SetWorkArea(Some(WorkArea { start: at(1), end: at(5) }))).proposal;
        let area_b = record_row("b", &s, RecordEdit::SetWorkArea(Some(WorkArea { start: at(2), end: at(5) }))).proposal;
        assert!(overlap(&area_a, &area_b), "both write the work area");
        let marker_a = record_row("a", &s, RecordEdit::ReplaceMarker { marker: renamed("mk-c1", at(11)) }).proposal;
        let marker_b = record_row("b", &s, RecordEdit::RemoveMarker { marker_id: "mk-c1".into() }).proposal;
        assert!(overlap(&marker_a, &marker_b), "both write the marker");
        let clear = record_row("clear", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: None }).proposal;
        let same = record_row("replace", &s, RecordEdit::SetTransition { outgoing_clip_id: cid(2), transition: Some(Box::new(dissolve("tr-1", "wipe"))) }).proposal;
        assert!(overlap(&clear, &same), "both write the transition");
        assert!(!overlap(&marker_a, &slip), "marker on clip 1 and a slip of clip 6");
        assert!(!overlap(&marker_a, &area_a), "marker on clip 1 and the work area");
    });
}

#[test]
fn the_descriptor_states_the_report() {
    for word in ["clip_marker", "sequence_marker", "clip_transition", "work_area", "every_planner_names_every_changed_record"] {
        assert!(hsk_studio_pulse::DESCRIPTOR.contains(word), "{word}");
    }
}
