//! Lane SMB-PR-ROUTING-REPAIR-1 (R57 F-R57-2, F-R57-3; lead rulings on SD-64 and SD-67, overrulable):
//! a removed bus leaves every routing readable, a new sequence that takes a stored routing is reported at
//! its commit, and a valid multicam record set cannot be made invalid by any commit that changes
//! sequences. The named limit stays pinned: a file already stale refuses the whole multicam set on read.
mod sequence_records_support;
use hsk_studio_folio::*;
use hsk_studio_session::*;
use sequence_records_support::*;
use std::sync::{Arc, Mutex};

const AUDIO: u32 = 501;
const BUS: u32 = 502;

fn with_end(mut s: StudioSequence) -> StudioSequence {
    s.duration = s.content_end().unwrap();
    s
}
fn kind_track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    let mut t = sequence("x", F24, clips).tracks.remove(0);
    t.track_id = identity("STRK", n);
    t.kind = kind;
    t.index = index;
    t
}
/// `seq-a`: a video clip, an audio clip, a submix bus.
fn seq_a_bus() -> StudioSequence {
    let mut s = sequence("seq-a", F24, vec![clip(10, 24)]);
    s.tracks.push(kind_track(AUDIO, TrackKind::Audio, 0, vec![clip(11, 24)]));
    s.tracks.push(kind_track(BUS, TrackKind::Submix, 0, vec![]));
    with_end(s)
}
fn seq_a_without_bus() -> StudioSequence {
    let mut s = seq_a_bus();
    s.tracks.retain(|t| t.kind != TrackKind::Submix);
    with_end(s)
}
fn seq_other() -> StudioSequence {
    sequence("seq-b", F24, vec![clip(12, 24)])
}
/// A live route of `seq-a` plays into the bus by its output and by a send; `seq-b` has its own routing.
fn bus_routing() -> StudioAudioRoutingSet {
    let mut a = StudioAudioRouting::new("seq-a");
    a.routes.push(StudioTrackRoute {
        track_id: identity("STRK", AUDIO),
        output: Some(identity("STRK", BUS)),
        sends: vec![StudioAudioSend { send_id: "s1".into(), target: identity("STRK", BUS), level: 0.5, pre_fader: false, pan: 0.0 }],
    });
    let mut b = StudioAudioRouting::new("seq-b");
    b.master.volume = 0.5;
    StudioAudioRoutingSet { routings: vec![a, b] }
}
fn routing_entry(set: &StudioAudioRoutingSet) -> hsk_studio_package::Quarantined {
    entry(AUDIO_ROUTING_ENTRY, &serde_json::to_vec(set).unwrap())
}
fn multicam_of(session: &Session, command: &str, sequences: Vec<SequenceEdit>) -> MulticamStep {
    let live: Option<StudioMulticamRecords> = session.document_entry(MULTICAM_ENTRY).map(|b| serde_json::from_slice(b).unwrap());
    MulticamStep { command_id: command.into(), correlation_id: 1, sequences, records_expect: live.clone(), records_write: live }
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Timeline,
    Multicam,
    Records,
}
const PATHS: [Path; 3] = [Path::Timeline, Path::Multicam, Path::Records];
/// An undo of whichever path committed.
enum Undo {
    Timeline(TimelineStep),
    Multicam(MulticamStep),
    Records(SequenceRecordsStep),
}
impl Undo {
    fn submit(&self, session: &mut Session) -> Result<Applied, String> {
        let revision = session.document_revision();
        match self {
            Self::Timeline(s) => session.submit_timeline(s, revision, &token()).map_err(|f| f.code().to_owned()),
            Self::Multicam(s) => session.submit_multicam(s, revision, &token()).map_err(|f| f.code().to_owned()),
            Self::Records(s) => session.submit_sequence_records(s, revision, &token()).map_err(|f| f.code().to_owned()),
        }
    }
    fn of(applied: &Applied) -> Self {
        if let Some(s) = &applied.timeline_inverse {
            return Self::Timeline(s.clone());
        }
        if let Some(s) = &applied.multicam_inverse {
            return Self::Multicam(s.clone());
        }
        Self::Records(applied.sequence_records_inverse.clone().expect("a changed step has an inverse"))
    }
}
/// Replaces the live `expect` with `write` through `path`.
fn replace(session: &mut Session, path: Path, expect: &StudioSequence, write: &StudioSequence) -> Result<Applied, String> {
    let revision = session.document_revision();
    let edit = || SequenceEdit::Replace { expect: expect.clone(), write: write.clone() };
    match path {
        Path::Timeline => {
            let s = TimelineStep { command_id: "cmd.replace".into(), correlation_id: 1, sequence_id: expect.sequence_id.clone(), expect: expect.clone(), write: write.clone() };
            session.submit_timeline(&s, revision, &token()).map_err(|f| f.code().to_owned())
        }
        Path::Multicam => {
            let s = multicam_of(session, "cmd.replace", vec![edit()]);
            session.submit_multicam(&s, revision, &token()).map_err(|f| f.code().to_owned())
        }
        Path::Records => session.submit_sequence_records(&step(vec![edit()], None, None), revision, &token()).map_err(|f| f.code().to_owned()),
    }
}

// ----- Item 1, F-R57-2: a removed bus -----

/// The whole table: three commit paths, nothing refused, every sequence reads, the listing names both
/// targets, the bytes never move, an undo empties the listing, a reopen holds it.
#[test]
fn a_removed_bus_commits_and_every_sequence_still_reads() {
    bounded_case("removed_bus", || {
        use StudioInertRoutingKind as K;
        let row = |kind| StudioInertRoutingReference { sequence_id: "seq-a".into(), kind, id: identity("STRK", BUS) };
        for path in PATHS {
            let mut session = open(&package_of(&[seq_a_bus(), seq_other()], &[routing_entry(&bus_routing())]));
            let body = session.document_entry(AUDIO_ROUTING_ENTRY).unwrap().to_vec();
            assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![], "{path:?}");
            let applied = replace(&mut session, path, &seq_a_bus(), &seq_a_without_bus()).unwrap_or_else(|code| panic!("{path:?}: {code}"));
            assert_eq!(session.audio_routing_records(&token()).unwrap(), Some(bus_routing()), "{path:?}: seq-b's routing too");
            assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![row(K::RouteSend), row(K::RouteOutput)], "{path:?}");
            assert_eq!(session.document_entry(AUDIO_ROUTING_ENTRY), Some(body.as_slice()), "{path:?}: left alone");
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert!(reopened.loss().is_empty(), "{path:?}: inert is not a loss");
            assert_eq!(reopened.audio_routing_records(&token()).unwrap(), Some(bus_routing()), "{path:?}: reopened");
            assert_eq!(reopened.inert_audio_routing_references(&token()).unwrap().len(), 2, "{path:?}: reopened listing");
            Undo::of(&applied).submit(&mut session).unwrap_or_else(|code| panic!("{path:?}: undo {code}"));
            assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![], "{path:?}: live again");
            assert_eq!(session.document_entry(AUDIO_ROUTING_ENTRY), Some(body.as_slice()), "{path:?}: undo, bytes");
        }
    });
}

/// The reason nothing is refused at commit: a host removes the bus first and repoints after
/// (harness-core `submit_sequence_and_entry`, sequence then entry).
#[test]
fn a_host_removes_the_bus_and_then_repoints_the_routing() {
    bounded_case("repoint", || {
        let mut session = open(&package_of(&[seq_a_bus(), seq_other()], &[routing_entry(&bus_routing())]));
        replace(&mut session, Path::Timeline, &seq_a_bus(), &seq_a_without_bus()).unwrap();
        let mut repointed = bus_routing();
        repointed.routings[0].routes[0].output = None;
        repointed.routings[0].routes[0].sends.clear();
        session.set_document_entry(AUDIO_ROUTING_ENTRY, Some(serde_json::to_vec(&repointed).unwrap()), &token()).unwrap();
        assert_eq!(session.audio_routing_records(&token()).unwrap(), Some(repointed));
        assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![]);
    });
}

// ----- Item 3, F-R57-3: a takeover -----

/// A brand-new sequence holding the removed one's id: other clips, the same track id grammar.
fn brand_new_a() -> StudioSequence {
    sequence("seq-a", F24, vec![clip(77, 24)])
}
fn insert(at: usize, write: StudioSequence) -> SequenceEdit {
    SequenceEdit::Insert { at, write }
}
fn taken(id: &str) -> Vec<RoutingTakeover> {
    vec![RoutingTakeover { sequence_id: id.into() }]
}
fn opened_routed() -> Session {
    open(&package_of(&[seq_a_bus(), seq_other()], &[routing_entry(&bus_routing())]))
}
fn remove_a(session: &mut Session, via_multicam: bool) -> Applied {
    let revision = session.document_revision();
    let edit = SequenceEdit::Remove { expect: seq_a_bus() };
    if via_multicam {
        let s = multicam_of(session, "cmd.remove", vec![edit]);
        session.submit_multicam(&s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"))
    } else {
        session.submit_sequence_records(&step(vec![edit], None, None), revision, &token()).unwrap_or_else(|f| panic!("{f:?}"))
    }
}
fn submit_edits(session: &mut Session, via_multicam: bool, edits: Vec<SequenceEdit>) -> Applied {
    let revision = session.document_revision();
    if via_multicam {
        let s = multicam_of(session, "cmd.insert", edits);
        session.submit_multicam(&s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"))
    } else {
        session.submit_sequence_records(&step(edits, None, None), revision, &token()).unwrap_or_else(|f| panic!("{f:?}"))
    }
}

/// The measured finding: a clean commit said nothing. Now the commit says so, and the routing still
/// applies, the ruling reports and does not refuse.
#[test]
fn a_new_sequence_on_a_stored_routing_is_reported_by_its_commit() {
    bounded_case("takeover", || {
        for via_multicam in [false, true] {
            let mut session = opened_routed();
            let removed = remove_a(&mut session, via_multicam);
            assert_eq!(removed.routing_takeovers, vec![], "a removal takes nothing");
            let applied = submit_edits(&mut session, via_multicam, vec![insert(0, brand_new_a())]);
            assert_eq!(applied.routing_takeovers, taken("seq-a"), "via_multicam {via_multicam}");
            assert_eq!(session.audio_routing_records(&token()).unwrap(), Some(bus_routing()), "reported, not removed");
        }
    });
}

/// An undo of the removal puts the same value back: live again by id, no report. Repeating it, too.
#[test]
fn an_undo_of_the_removal_is_not_a_takeover() {
    bounded_case("undo_not_takeover", || {
        for via_multicam in [false, true] {
            let mut session = opened_routed();
            let removed = remove_a(&mut session, via_multicam);
            let undo = Undo::of(&removed);
            let undone = undo.submit(&mut session).unwrap();
            assert_eq!(undone.routing_takeovers, vec![], "undo, via_multicam {via_multicam}");
            let redone = Undo::of(&undone).submit(&mut session).unwrap();
            assert_eq!(redone.routing_takeovers, vec![], "redo of the removal");
            let again = Undo::of(&redone).submit(&mut session).unwrap();
            assert_eq!(again.routing_takeovers, vec![], "undo again");
        }
    });
}

/// The takeover's own history: its undo removes the new sequence, its redo is the value that step
/// already reported. A later brand-new value is reported again.
#[test]
fn a_takeovers_redo_is_not_reported_twice_and_another_new_value_is() {
    bounded_case("takeover_redo", || {
        let mut session = opened_routed();
        remove_a(&mut session, false);
        let applied = submit_edits(&mut session, false, vec![insert(0, brand_new_a())]);
        let undone = Undo::of(&applied).submit(&mut session).unwrap();
        let redone = Undo::of(&undone).submit(&mut session).unwrap();
        assert_eq!(redone.routing_takeovers, vec![]);
        let undone = Undo::of(&redone).submit(&mut session).unwrap();
        assert_eq!(undone.routing_takeovers, vec![]);
        let other = sequence("seq-a", F24, vec![clip(78, 24)]);
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, other)]).routing_takeovers, taken("seq-a"));
    });
}

/// A file's inert routing: nothing was removed here, so an insert is a takeover. A sequence no routing
/// names, and a session with no routing or a damaged one, report nothing.
#[test]
fn only_a_stored_routing_of_that_id_is_taken() {
    bounded_case("takeover_scope", || {
        let mut set = StudioAudioRoutingSet { routings: vec![StudioAudioRouting::new("seq-gone")] };
        set.routings[0].master.volume = 0.1;
        let mut session = open(&package_of(&[seq_other()], &[routing_entry(&set)]));
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, sequence("seq-free", F24, vec![clip(80, 24)]))]).routing_takeovers, vec![]);
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, sequence("seq-gone", F24, vec![clip(81, 24)]))]).routing_takeovers, taken("seq-gone"));

        let mut session = open(&package_of(&[seq_other()], &[]));
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, sequence("seq-gone", F24, vec![clip(81, 24)]))]).routing_takeovers, vec![]);

        let mut session = open(&package_of(&[seq_other()], &[entry(AUDIO_ROUTING_ENTRY, b"{not json")]));
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, sequence("seq-gone", F24, vec![clip(81, 24)]))]).routing_takeovers, vec![], "a damaged entry reads as its refusal, and no routing of it applies");
    });
}

/// Value, not provenance: the same value put back by hand is the sequence that was removed. And what a
/// reopen forgets (named limit): the session knows what it removed, not what an earlier session did, so
/// an inverse carried across a reopen reports, while the sequence comes back live either way.
#[test]
fn what_counts_as_an_undo_is_the_value_this_session_removed() {
    bounded_case("takeover_value", || {
        let mut session = opened_routed();
        remove_a(&mut session, false);
        assert_eq!(submit_edits(&mut session, false, vec![insert(0, seq_a_bus())]).routing_takeovers, vec![], "the same value");

        let mut session = opened_routed();
        let removed = remove_a(&mut session, false);
        let mut reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
        let undone = Undo::of(&removed).submit(&mut reopened).unwrap();
        assert_eq!(undone.routing_takeovers, taken("seq-a"), "named limit: a report, nothing refused");
        assert_eq!(reopened.inert_audio_routing_references(&token()).unwrap(), vec![], "live again by id");
    });
}

#[test]
fn a_keyed_replay_repeats_the_report() {
    bounded_case("takeover_replay", || {
        let mut session = opened_routed();
        remove_a(&mut session, true);
        let key = IdempotencyKey::parse("takeover-1").unwrap();
        let s = multicam_of(&session, "cmd.insert", vec![insert(0, brand_new_a())]);
        let revision = session.document_revision();
        let first = session.submit_multicam_keyed(&key, &s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"));
        assert_eq!(first.routing_takeovers, taken("seq-a"));
        let replay = session.submit_multicam_keyed(&key, &s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"));
        assert!(replay.replayed);
        assert_eq!(replay.routing_takeovers, taken("seq-a"));
    });
}

#[derive(Clone, Default)]
struct Authority(Arc<Mutex<Option<CommitReceipt>>>);
impl CommitPort for Authority {
    fn commit(&self, request: &CommitRequest<'_>) -> CommitOutcome {
        let receipt = CommitReceipt { revision: request.successor_revision, sequence: 1, binding: "someone-else".to_owned() };
        *self.0.lock().unwrap() = Some(receipt.clone());
        CommitOutcome::Committed(receipt)
    }
    fn lookup(&self, _: &IdempotencyKey) -> Lookup {
        match &*self.0.lock().unwrap() {
            Some(receipt) => Lookup::Found(receipt.clone()),
            None => Lookup::Absent,
        }
    }
}

/// A resync can give a new sequence an id too: the report carries it.
#[test]
fn a_resync_reports_a_takeover() {
    bounded_case("takeover_resync", || {
        let mut set = StudioAudioRoutingSet { routings: vec![StudioAudioRouting::new("seq-gone")] };
        set.routings[0].master.volume = 0.1;
        let bytes = package_of(&[seq_other()], &[routing_entry(&set)]);
        let ports = ports(&Frames::default()).with_commits(Box::new(Authority::default()));
        let mut session = Session::open_bytes(&bytes, SessionConfig::new(actor()), ports, &token()).unwrap_or_else(|f| panic!("{f:?}"));
        let key = IdempotencyKey::parse("presets-1").unwrap();
        let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
        assert_eq!(session.submit_sequence_records_keyed(&key, &presets, 3, &token()).unwrap_err().code(), "receipt_mismatch");
        let mut document = session.snapshot().document().clone();
        document.sequences.push(sequence("seq-gone", F24, vec![clip(81, 24)]));
        document.revision = 4;
        let report = session.resync_diverged(&key, &serde_json::to_vec(&document).unwrap(), &token()).unwrap_or_else(|f| panic!("{f:?}"));
        assert_eq!(report.routing_takeovers, taken("seq-gone"));
    });
}

// ----- Item 4: what can still make a whole routing set unreadable -----

/// Measured for the lane notes (V4-06): an id that comes BACK held with the wrong kind, or a bus that
/// comes back closing a cycle, still refuses the whole set, other sequences included. The rule's
/// "stays refused" half (Folio module docs). The Viewer's minting skips every id a stored routing names,
/// so this reach is a direct commit or a file. Pinned so a change here is seen.
#[test]
fn a_stale_id_coming_back_as_the_wrong_kind_still_refuses_the_whole_set() {
    bounded_case("wrong_kind", || {
        let refused = |set: &StudioAudioRoutingSet, write: StudioSequence| {
            let mut session = open(&package_of(&[seq_a_without_bus(), seq_other()], &[routing_entry(set)]));
            session.audio_routing_records(&token()).unwrap();
            replace(&mut session, Path::Timeline, &seq_a_without_bus(), &write).unwrap();
            session.audio_routing_records(&token()).unwrap_err().code().to_owned()
        };
        // The output's bus comes back as an audio track.
        let mut as_audio = seq_a_without_bus();
        as_audio.tracks.push(kind_track(BUS, TrackKind::Audio, 1, vec![]));
        assert_eq!(refused(&bus_routing(), with_end(as_audio)), "document_rejected");
        // A stale route's track comes back as video.
        let mut set = bus_routing();
        set.routings[0].routes.push(StudioTrackRoute { track_id: identity("STRK", 600), output: None, sends: vec![] });
        let mut as_video = seq_a_without_bus();
        as_video.tracks.push(kind_track(600, TrackKind::Video, 1, vec![]));
        assert_eq!(refused(&set, with_end(as_video)), "document_rejected");
        // A stale envelope's clip comes back on a video track.
        let mut set = bus_routing();
        set.routings[0].clip_gains.push(StudioClipGainEnvelope { clip_id: identity("SCLP", 700), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }] });
        let mut on_video = seq_a_without_bus();
        on_video.tracks[0].clips.push({
            let mut c = clip(700, 24);
            c.timeline_start = hsk_studio_accord::Ticks::new(24 * F24);
            c
        });
        assert_eq!(refused(&set, with_end(on_video)), "document_rejected");
        // The bus comes back and closes a cycle its route held while it was absent.
        let mut set = bus_routing();
        set.routings[0].routes[0].sends.clear();
        set.routings[0].routes.push(StudioTrackRoute { track_id: identity("STRK", BUS), output: Some(identity("STRK", 601)), sends: vec![] });
        set.routings[0].routes.push(StudioTrackRoute { track_id: identity("STRK", 601), output: Some(identity("STRK", BUS)), sends: vec![] });
        let mut both = seq_a_bus();
        both.tracks.push(kind_track(601, TrackKind::Submix, 1, vec![]));
        assert_eq!(refused(&set, with_end(both)), "document_rejected");
    });
}

// ----- Angle order, option A: no commit may make a valid record set invalid -----

const SRC_V1: u32 = 801;
const SRC_V2: u32 = 802;
const NESTED: u32 = 810;

/// A declared multicam source of two video tracks and an audio track.
fn source_seq() -> StudioSequence {
    let mut s = sequence("seq-src", F24, vec![]);
    s.tracks.clear();
    s.tracks.push(kind_track(SRC_V1, TrackKind::Video, 0, vec![clip(820, 24)]));
    s.tracks.push(kind_track(SRC_V2, TrackKind::Video, 1, vec![clip(821, 24)]));
    s.tracks.push(kind_track(803, TrackKind::Audio, 0, vec![clip(822, 24)]));
    with_end(s)
}
fn parent_seq() -> StudioSequence {
    let mut c = clip(NESTED, 12);
    c.source_ref = ClipSource::Sequence { sequence_id: "seq-src".into() };
    sequence("seq-p", F24, vec![c])
}
fn mode(n: u32, angle: u32) -> StudioMulticamClip {
    StudioMulticamClip { clip_id: identity("SCLP", n), active_angle: angle, audio_angle: angle, audio_follows_video: true, follows_nest_setting: false, enabled: true }
}
fn declared() -> StudioMulticamRecords {
    let mut r = StudioMulticamRecords::new(doc_id());
    r.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-src".into(),
        selection_order: MulticamSelectionOrder::SourceOrder,
        angle_order: multicam_angle_order(&source_seq(), MulticamSelectionOrder::SourceOrder),
    });
    r.clips.push(mode(NESTED, 2));
    r
}
fn multicam_entry(r: &StudioMulticamRecords) -> hsk_studio_package::Quarantined {
    entry(MULTICAM_ENTRY, &serde_json::to_vec(r).unwrap())
}
fn opened_multicam() -> Session {
    open(&package_of(&[source_seq(), parent_seq(), seq_other()], &[multicam_entry(&declared())]))
}
fn sequences_of(session: &Session) -> Vec<StudioSequence> {
    session.snapshot().document().sequences.clone()
}

/// The three ways an order goes stale, on the three commit paths.
#[test]
fn no_commit_path_changes_the_video_tracks_of_a_declared_source() {
    bounded_case("guard_refuses", || {
        let mut removed = source_seq();
        removed.tracks.retain(|t| t.track_id != identity("STRK", SRC_V2));
        let mut added = source_seq();
        added.tracks.push(kind_track(804, TrackKind::Video, 2, vec![]));
        let mut reindexed = source_seq();
        reindexed.tracks[0].index = 1;
        reindexed.tracks[1].index = 0;
        for (what, write) in [("removed", removed), ("added", added), ("reindexed", reindexed)] {
            let write = with_end(write);
            for path in PATHS {
                let mut session = opened_multicam();
                let (held, body, revision) = (sequences_of(&session), session.document_entry(MULTICAM_ENTRY).unwrap().to_vec(), session.document_revision());
                assert_eq!(replace(&mut session, path, &source_seq(), &write).unwrap_err(), "folio_rejected", "{what} {path:?}");
                assert_eq!(sequences_of(&session), held, "{what} {path:?}");
                assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(body.as_slice()), "{what} {path:?}");
                assert_eq!(session.document_revision(), revision, "{what} {path:?}");
                assert_eq!(session.multicam_records(&token()).unwrap(), Some(declared()), "{what} {path:?}");
            }
        }
    });
}

/// An Insert in a records step: a new sequence holding a clip an inert mode names, as media, would make
/// the set refuse. So would a Replace of a moded clip's source. Both are refused for the same reason.
#[test]
fn an_insert_or_replace_that_would_invalidate_a_clip_mode_is_refused() {
    bounded_case("guard_insert", || {
        let mut records = declared();
        records.clips.push(mode(830, 1));
        let mut session = open(&package_of(&[source_seq(), parent_seq(), seq_other()], &[multicam_entry(&records)]));
        assert_eq!(session.inert_multicam_clips(&token()).unwrap().len(), 1);
        let held = sequences_of(&session);
        let revision = session.document_revision();
        let s = step(vec![insert(0, sequence("seq-new", F24, vec![clip(830, 24)]))], None, None);
        assert_eq!(session.submit_sequence_records(&s, revision, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(sequences_of(&session), held);
        // A clip that keeps a mode cannot become media on any path.
        let mut media = parent_seq();
        media.tracks[0].clips[0] = clip(NESTED, 12);
        for path in PATHS {
            let mut session = opened_multicam();
            assert_eq!(replace(&mut session, path, &parent_seq(), &media).unwrap_err(), "folio_rejected", "{path:?}");
        }
        // An insert that touches nothing the records name commits.
        let mut session = opened_multicam();
        let revision = session.document_revision();
        let s = step(vec![insert(0, sequence("seq-new", F24, vec![clip(831, 24)]))], None, None);
        session.submit_sequence_records(&s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"));
    });
}

/// A step that changes no video track of a declared source commits on every path, undo and redo pass
/// the guard, and the records never move.
#[test]
fn a_step_that_leaves_the_declared_video_tracks_alone_commits_and_undoes() {
    bounded_case("guard_passes", || {
        let mut audio_added = source_seq();
        audio_added.tracks.push(kind_track(805, TrackKind::Audio, 1, vec![]));
        let mut clip_moved = source_seq();
        clip_moved.tracks[0].clips[0].timeline_start = hsk_studio_accord::Ticks::new(24 * F24);
        let mut renamed = source_seq();
        renamed.name = "renamed".into();
        let mut muted = source_seq();
        muted.tracks[0].muted = true;
        let mut clip_gone = parent_seq();
        clip_gone.tracks[0].clips.clear();
        let mut other_video = seq_other();
        other_video.tracks.push(kind_track(806, TrackKind::Video, 1, vec![]));
        let cases: [(&str, StudioSequence, StudioSequence); 6] = [
            ("source audio track added", source_seq(), with_end(audio_added)),
            ("source clip moved", source_seq(), with_end(clip_moved)),
            ("source renamed", source_seq(), renamed),
            ("source track muted", source_seq(), muted),
            ("the moded clip removed: its mode goes inert", parent_seq(), with_end(clip_gone)),
            ("a video track on an undeclared sequence", seq_other(), with_end(other_video)),
        ];
        for (what, expect, write) in cases {
            for path in PATHS {
                let mut session = opened_multicam();
                let before = sequences_of(&session);
                let body = session.document_entry(MULTICAM_ENTRY).unwrap().to_vec();
                let applied = replace(&mut session, path, &expect, &write).unwrap_or_else(|code| panic!("{what} {path:?}: {code}"));
                let after = sequences_of(&session);
                let undone = Undo::of(&applied).submit(&mut session).unwrap_or_else(|code| panic!("{what} {path:?}: undo {code}"));
                assert_eq!(sequences_of(&session), before, "{what} {path:?}: undo");
                Undo::of(&undone).submit(&mut session).unwrap_or_else(|code| panic!("{what} {path:?}: redo {code}"));
                assert_eq!(sequences_of(&session), after, "{what} {path:?}: redo");
                assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(body.as_slice()), "{what} {path:?}: bytes");
                assert!(session.multicam_records(&token()).is_ok(), "{what} {path:?}: reads");
            }
        }
    });
}

/// The one path a track change on a multicam source has: a multicam step carrying rewritten records.
/// No planner produces one (named in the lane notes). Undo and redo pass.
#[test]
fn a_multicam_step_carrying_rewritten_records_can_change_the_tracks() {
    bounded_case("guard_multicam_rewrite", || {
        let mut session = opened_multicam();
        let mut write = source_seq();
        write.tracks.retain(|t| t.track_id != identity("STRK", SRC_V2));
        let write = with_end(write);
        let mut rewritten = declared();
        rewritten.multicam_sequences[0].angle_order = multicam_angle_order(&write, MulticamSelectionOrder::SourceOrder);
        rewritten.clips[0] = mode(NESTED, 1);
        let s = MulticamStep {
            command_id: "cmd.rewrite".into(),
            correlation_id: 1,
            sequences: vec![SequenceEdit::Replace { expect: source_seq(), write }],
            records_expect: Some(declared()),
            records_write: Some(rewritten.clone()),
        };
        let revision = session.document_revision();
        let applied = session.submit_multicam(&s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}"));
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(rewritten.clone()));
        let undone = Undo::of(&applied).submit(&mut session).unwrap();
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(declared()));
        Undo::of(&undone).submit(&mut session).unwrap();
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(rewritten));
    });
}

fn stale_records() -> StudioMulticamRecords {
    let mut r = declared();
    r.multicam_sequences[0].angle_order.push(identity("STRK", 899));
    r
}

/// NAMED LIMIT, F-R57-2's angle_order cell stays open for files (lead ruling, STOP AFTER A): a file with a
/// stale order refuses the whole multicam set on read, typed, and no angle moves. The guard does not hold
/// such a file up: it refuses only when records validate now and fail on the result.
#[test]
fn a_file_with_a_stale_order_refuses_the_whole_set_on_read_and_can_still_be_edited() {
    bounded_case("stale_file", || {
        let mut records = stale_records();
        records.multicam_sequences.push(StudioMulticamSequence {
            sequence_id: "seq-b".into(),
            selection_order: MulticamSelectionOrder::SourceOrder,
            angle_order: multicam_angle_order(&seq_other(), MulticamSelectionOrder::SourceOrder),
        });
        for path in PATHS {
            let mut session = open(&package_of(&[source_seq(), parent_seq(), seq_other()], &[multicam_entry(&records)]));
            let body = session.document_entry(MULTICAM_ENTRY).unwrap().to_vec();
            assert_eq!(session.multicam_records(&token()).unwrap_err().code(), "document_rejected", "{path:?}: the healthy seq-b declaration with it");
            let mut renamed = source_seq();
            renamed.name = "renamed".into();
            match (path, replace(&mut session, path, &source_seq(), &renamed)) {
                // The multicam step validates the records it carries, and always has.
                (Path::Multicam, outcome) => assert_eq!(outcome.unwrap_err(), "folio_rejected"),
                (_, outcome) => {
                    outcome.unwrap_or_else(|code| panic!("{path:?}: {code}"));
                }
            }
            assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(body.as_slice()), "{path:?}: bytes");
            assert_eq!(session.multicam_records(&token()).unwrap_err().code(), "document_rejected", "{path:?}");
        }
    });
}

/// THE UNDO CORNER (named, lead ruling): a stale file repaired by a step goes live, and the undo of that
/// repair is refused `folio_rejected`, because it would make a valid set invalid and the session cannot
/// tell it is an undo. Nothing changes; the file's stale state is not put back.
#[test]
fn an_undo_of_a_repair_of_a_stale_file_is_refused() {
    bounded_case("undo_corner", || {
        let mut records = declared();
        records.multicam_sequences[0].angle_order.push(identity("STRK", 804));
        let session = open(&package_of(&[source_seq(), parent_seq(), seq_other()], &[multicam_entry(&records)]));
        assert_eq!(session.multicam_records(&token()).unwrap_err().code(), "document_rejected");
        let mut repaired = source_seq();
        repaired.tracks.push(kind_track(804, TrackKind::Video, 2, vec![]));
        let repaired = with_end(repaired);
        for path in [Path::Timeline, Path::Records] {
            let mut session_here = open(&package_of(&[source_seq(), parent_seq(), seq_other()], &[multicam_entry(&records)]));
            let applied = replace(&mut session_here, path, &source_seq(), &repaired).unwrap_or_else(|code| panic!("{path:?}: {code}"));
            assert_eq!(session_here.multicam_records(&token()).unwrap(), Some(records.clone()), "{path:?}: live");
            let held = sequences_of(&session_here);
            assert_eq!(Undo::of(&applied).submit(&mut session_here).unwrap_err(), "folio_rejected", "{path:?}");
            assert_eq!(sequences_of(&session_here), held, "{path:?}: nothing moved");
            assert!(session_here.multicam_records(&token()).is_ok(), "{path:?}");
        }
    });
}

/// Not a bound, a measurement for the lane notes: what the guard adds to one sequence commit on a large
/// record set, in this build's profile. Prints `MEASURED`; asserts only that both commits happen.
#[test]
fn measured_guard_cost_on_a_large_record_set() {
    bounded_case("guard_cost", || {
        const MODES: u32 = 1800;
        let clips: Vec<StudioClip> = (0..MODES)
            .map(|i| {
                let mut c = clip(1000 + i, 1);
                c.source_ref = ClipSource::Sequence { sequence_id: "seq-src".into() };
                c.timeline_start = hsk_studio_accord::Ticks::new(u64::from(i) * F24);
                c
            })
            .collect();
        let parent = sequence("seq-p", F24, clips);
        let mut records = declared();
        records.clips = (0..MODES).map(|i| mode(1000 + i, 1)).collect();
        let time = |held: &[hsk_studio_package::Quarantined]| {
            let mut session = open(&package_of(&[source_seq(), parent.clone(), seq_other()], held));
            let mut best = std::time::Duration::MAX;
            let (mut from, mut to) = (seq_other(), seq_other());
            for k in 0..7 {
                to.name = format!("name-{k}");
                let started = std::time::Instant::now();
                replace(&mut session, Path::Timeline, &from, &to).unwrap();
                best = best.min(started.elapsed());
                from.clone_from(&to);
            }
            best
        };
        let with = time(&[multicam_entry(&records)]);
        let without = time(&[]);
        println!("MEASURED guard_cost modes={MODES} commit_with_entry={with:?} commit_without_entry={without:?} (fastest of 7, this build's profile)");
    });
}
