//! Lane SMB-PR-MULTICAM-2 (STU-VID-040 "Every operation in this sub-section is one reversible
//! `StudioHistoryEntry`"; STU-VID-033 flatten "is one reversible history step"): a multicam step
//! (sequence edits incl. a CREATED source sequence, plus the multicam record entry) commits as ONE
//! revision, ONE receipt and ONE inverse. "One step" is proven by submitting the inverse and
//! comparing the exact prior document sequences and records; refusals change nothing.
use hsk_studio_accord::{ActorContext, CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{AssetSource, Extras, Limits, MemoryAssetSource, WriteOptions, write_package_with};
use hsk_studio_session::*;
use serde_json::json;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

/// 24 fps ticks per frame.
const F: u64 = 10_584_000_000;

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

struct Granted;
impl folio::Resolver for Granted {
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
        false
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}
struct Sink;
impl SinkPort for Sink {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}
fn token() -> CancellationToken {
    CancellationToken::default()
}
fn open(bytes: &[u8]) -> Session {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    Session::open_bytes(bytes, SessionConfig::new(actor), SessionPorts::new(Box::new(Sink)).with_resolver(Box::new(Granted)), &token())
        .unwrap()
}

/// Clip `n` of `source`: source frames `[si, si + frames)` at timeline frame `start`.
fn clip(n: u32, source: ClipSource, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: identity("SCLP", n),
        source_ref: source,
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn media(n: u32) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: format!("camera-{n}"),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: format!("ab{n:04}") },
        asset_id: None,
    }
}
fn nest() -> ClipSource {
    ClipSource::Sequence { sequence_id: "seq-mc".into() }
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: identity("STRK", n),
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
fn sequence(id: &str, tracks: Vec<StudioTrack>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks,
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
/// `seq-mc`: three camera angles (video tracks 100..102, clips 100..102, 24 frames each).
fn source_sequence() -> StudioSequence {
    sequence("seq-mc", (0..3).map(|i| track(100 + i, TrackKind::Video, i, vec![clip(100 + i, media(i), 0, 24, 0)])).collect())
}
/// `seq-main`: V1 holds `clips`; V2 holds media clip 3.
fn main_sequence(clips: Vec<StudioClip>) -> StudioSequence {
    sequence("seq-main", vec![track(1, TrackKind::Video, 0, clips), track(2, TrackKind::Video, 1, vec![clip(3, media(7), 0, 10, 0)])])
}
fn package(sequences: &[StudioSequence]) -> Vec<u8> {
    let sequences: Vec<serde_json::Value> = sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect();
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":sequences
    });
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token(), &Granted).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("fixture became read-only"),
    };
    let assets = MemoryAssetSource::new();
    let mut bytes = Vec::new();
    write_package_with(
        &snapshot,
        &assets as &dyn AssetSource,
        &Extras { quarantined: &[], previews: &[] },
        &WriteOptions::default(),
        &Limits::default(),
        &token(),
        &mut bytes,
    )
    .unwrap();
    bytes
}
fn declared() -> StudioMulticamRecords {
    let mut r = StudioMulticamRecords::new(identity("SDOC", 1));
    r.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-mc".into(),
        selection_order: MulticamSelectionOrder::SourceOrder,
        angle_order: multicam_angle_order(&source_sequence(), MulticamSelectionOrder::SourceOrder),
    });
    r
}
fn mode(n: u32, angle: u32) -> StudioMulticamClip {
    StudioMulticamClip {
        clip_id: identity("SCLP", n),
        active_angle: angle,
        audio_angle: angle,
        audio_follows_video: true,
        follows_nest_setting: false,
        enabled: true,
    }
}
fn step(command: &str, sequences: Vec<SequenceEdit>, records_expect: Option<StudioMulticamRecords>, records_write: Option<StudioMulticamRecords>) -> MulticamStep {
    MulticamStep { command_id: command.into(), correlation_id: 7, sequences, records_expect, records_write }
}
fn fresh_inverse(applied: &Applied, command: &str) -> MulticamStep {
    let mut inverse = applied.multicam_inverse.clone().expect("a changed step has an inverse");
    inverse.command_id = command.into();
    inverse.correlation_id = 8;
    inverse
}
/// What a session holds for the multicam contract: its sequences and its multicam entry.
fn state(s: &Session) -> (Vec<StudioSequence>, Option<Vec<u8>>, u64) {
    (s.snapshot().document().sequences.clone(), s.document_entry(MULTICAM_ENTRY).map(<[u8]>::to_vec), s.document_revision())
}

/// The create-multicam-source result: `seq-mc` inserted after `seq-main`, declared in new records.
fn create_step() -> MulticamStep {
    step("cmd.clip.create.multicam", vec![SequenceEdit::Insert { at: 1, write: source_sequence() }], None, Some(declared()))
}

#[test]
fn created_source_sequence_and_records_commit_as_one_step() {
    bounded_case("created_source_commit", || {
        let mut session = open(&package(&[main_sequence(vec![])]));
        let receipts_before = session.receipt_count();
        let key = IdempotencyKey::parse("multicam-create-1").unwrap();
        let applied = session.submit_multicam_keyed(&key, &create_step(), 3, &token()).unwrap();
        assert_eq!((applied.provider.as_str(), applied.command_id.as_str()), (MULTICAM_STEP_PROVIDER, "cmd.clip.create.multicam"));
        assert_eq!((applied.previous_revision, applied.revision, applied.changed), (3, 4, true), "one revision");
        assert_eq!(session.receipt_count(), receipts_before + 1, "one receipt");
        let ids: Vec<&str> = session.snapshot().document().sequences.iter().map(|s| s.sequence_id.as_str()).collect();
        assert_eq!(ids, vec!["seq-main", "seq-mc"], "the created source sequence is in the document");
        assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(serde_json::to_vec(&declared()).unwrap().as_slice()));
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(declared()), "the entry validates against the new document");
        let inverse = applied.multicam_inverse.clone().unwrap();
        assert_eq!(inverse.sequences, vec![SequenceEdit::Remove { expect: source_sequence() }]);
        assert_eq!((inverse.records_expect, inverse.records_write), (Some(declared()), None));
        // A replay of the key applies nothing a second time.
        let replay = session.submit_multicam_keyed(&key, &create_step(), 3, &token()).unwrap();
        assert!(replay.replayed && replay.revision == 4);
        assert_eq!(session.document_revision(), 4);

        // Create plus a clip nesting it in seq-main, with its mode: still one revision.
        let mut both = open(&package(&[main_sequence(vec![])]));
        let mut records = declared();
        records.clips.push(mode(1, 2));
        let combined = step(
            "cmd.clip.create.multicam",
            vec![
                SequenceEdit::Replace { expect: main_sequence(vec![]), write: main_sequence(vec![clip(1, nest(), 0, 12, 0)]) },
                SequenceEdit::Insert { at: 0, write: source_sequence() },
            ],
            None,
            Some(records.clone()),
        );
        let applied = both.submit_multicam(&combined, 3, &token()).unwrap();
        assert_eq!(applied.revision, 4);
        assert_eq!(both.snapshot().document().sequences, vec![source_sequence(), main_sequence(vec![clip(1, nest(), 0, 12, 0)])]);
        assert_eq!(both.multicam_records(&token()).unwrap(), Some(records));
        // A step that changes nothing is a no-change result (revision unchanged, no inverse).
        let same = step("cmd.noop", vec![], both.multicam_records(&token()).unwrap(), both.multicam_records(&token()).unwrap());
        let noop = both.submit_multicam(&same, 4, &token()).unwrap();
        assert!(!noop.changed && noop.revision == 4 && noop.multicam_inverse.is_none());
    });
}

#[test]
fn undo_removes_the_created_sequence_and_redo_restores_it() {
    bounded_case("created_source_undo_redo", || {
        let mut session = open(&package(&[main_sequence(vec![])]));
        let before = state(&session);
        let applied = session.submit_multicam(&create_step(), 3, &token()).unwrap();
        let after = state(&session);
        let undo = session.submit_multicam(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(undo.revision, 5, "undo is one revision");
        assert_eq!((state(&session).0, state(&session).1), (before.0.clone(), before.1.clone()), "undo removes the sequence and the entry");
        let redo = session.submit_multicam(&fresh_inverse(&undo, "cmd.redo"), 5, &token()).unwrap();
        assert_eq!(redo.revision, 6);
        assert_eq!((state(&session).0, state(&session).1), (after.0.clone(), after.1.clone()), "redo restores both");
        // The undo of the redo is bound to what the redo left: replaying the first undo now conflicts.
        let stale = session.submit_multicam(&fresh_inverse(&redo, "cmd.undo2"), 6, &token()).unwrap();
        assert_eq!(stale.revision, 7);
        let old_undo = fresh_inverse(&applied, "cmd.undo3");
        assert_eq!(session.submit_multicam(&old_undo, 7, &token()).unwrap_err().error.code(), "invalid_target");

        // Removing a source sequence a clip still nests is refused by Folio validation.
        let mut nested = open(&package(&[source_sequence(), main_sequence(vec![clip(1, nest(), 0, 12, 0)])]));
        let held = state(&nested);
        let remove = step("cmd.remove", vec![SequenceEdit::Remove { expect: source_sequence() }], None, None);
        assert_eq!(nested.submit_multicam(&remove, 3, &token()).unwrap_err().error.code(), "folio_rejected");
        assert_eq!(state(&nested), held, "the refusal changes nothing");
    });
}

#[test]
fn flatten_and_record_pass_commit_as_one_step_with_their_records() {
    bounded_case("flatten_record_pass_commit", || {
        let main0 = main_sequence(vec![clip(1, nest(), 0, 12, 0)]);
        let mut session = open(&package(&[source_sequence(), main0.clone()]));
        let mut r0 = declared();
        r0.clips.push(mode(1, 1));
        session.set_multicam_records(Some(&r0), &token()).unwrap();
        let start = state(&session);

        // Record pass: a cut at frame 6, the later part on angle 3 (one sequence + records).
        let main1 = main_sequence(vec![clip(1, nest(), 0, 6, 0), clip(11, nest(), 6, 6, 6)]);
        let mut r1 = r0.clone();
        r1.clips.push(mode(11, 3));
        let pass = step("cmd.multicam.toggle.record", vec![SequenceEdit::Replace { expect: main0.clone(), write: main1.clone() }], Some(r0.clone()), Some(r1.clone()));
        let recorded = session.submit_multicam(&pass, 3, &token()).unwrap();
        assert_eq!(recorded.revision, 4, "a record pass is one step (STU-VID-040)");
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(r1.clone()));
        let after_pass = state(&session);

        // Flatten: the cut list becomes ordinary media clips and the modes are discarded.
        let main2 = main_sequence(vec![clip(500, media(0), 0, 6, 0), clip(501, media(2), 6, 6, 6)]);
        let flatten = step("cmd.clip.multicam.flatten", vec![SequenceEdit::Replace { expect: main1.clone(), write: main2.clone() }], Some(r1.clone()), Some(declared()));
        let flattened = session.submit_multicam(&flatten, 4, &token()).unwrap();
        assert_eq!(flattened.revision, 5, "flatten is one step (STU-VID-033)");
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(declared()));

        // Undo flatten, then undo the pass: exact prior sequences and records each time.
        session.submit_multicam(&fresh_inverse(&flattened, "cmd.undo.flatten"), 5, &token()).unwrap();
        assert_eq!((state(&session).0, state(&session).1), (after_pass.0, after_pass.1));
        session.submit_multicam(&fresh_inverse(&recorded, "cmd.undo.pass"), 6, &token()).unwrap();
        assert_eq!((state(&session).0, state(&session).1), (start.0.clone(), start.1));

        // A records-only step (switch without a cut) is one revision with one inverse too.
        let mut r_switch = r0.clone();
        r_switch.clips[0] = mode(1, 2);
        let switch = step("cmd.multicam.choosenocut.camera2", vec![], Some(r0.clone()), Some(r_switch.clone()));
        let switched = session.submit_multicam(&switch, 7, &token()).unwrap();
        assert_eq!(switched.revision, 8, "a records-only step is one revision");
        assert_eq!(state(&session).0, start.0, "it changes no sequence");
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(r_switch));
        session.submit_multicam(&fresh_inverse(&switched, "cmd.undo.switch"), 8, &token()).unwrap();
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(r0));
    });
}

#[test]
fn stale_expectation_is_value_conflict_and_changes_nothing() {
    bounded_case("stale_expectation", || {
        let main0 = main_sequence(vec![clip(1, nest(), 0, 12, 0)]);
        let mut session = open(&package(&[source_sequence(), main0.clone()]));
        let mut r0 = declared();
        r0.clips.push(mode(1, 1));
        session.set_multicam_records(Some(&r0), &token()).unwrap();
        let held = state(&session);
        let receipts = session.receipt_count();
        let main1 = main_sequence(vec![clip(1, nest(), 0, 6, 0), clip(11, nest(), 6, 6, 6)]);
        let mut r1 = r0.clone();
        r1.clips.push(mode(11, 3));
        let good = step("cmd.multicam.toggle.record", vec![SequenceEdit::Replace { expect: main0.clone(), write: main1.clone() }], Some(r0.clone()), Some(r1.clone()));
        let code = |session: &mut Session, s: &MulticamStep, rev: u64| session.submit_multicam(s, rev, &token()).unwrap_err().error.code();

        let mut stale_records = good.clone();
        stale_records.records_expect = Some(declared());
        assert_eq!(code(&mut session, &stale_records, 3), "value_conflict", "the quoted records are not the live ones");
        let mut absent_records = good.clone();
        absent_records.records_expect = None;
        assert_eq!(code(&mut session, &absent_records, 3), "value_conflict");
        let mut stale_sequence = good.clone();
        stale_sequence.sequences = vec![SequenceEdit::Replace { expect: main_sequence(vec![]), write: main1.clone() }];
        assert_eq!(code(&mut session, &stale_sequence, 3), "value_conflict", "the quoted sequence is not the live one");
        let mut existing = good.clone();
        existing.sequences = vec![SequenceEdit::Insert { at: 0, write: source_sequence() }];
        assert_eq!(code(&mut session, &existing, 3), "value_conflict", "a created id already exists");
        let mut unknown = good.clone();
        unknown.sequences = vec![SequenceEdit::Replace { expect: sequence("seq-x", vec![]), write: sequence("seq-x", vec![]) }];
        assert_eq!(code(&mut session, &unknown, 3), "invalid_target");
        let mut renamed = good.clone();
        renamed.sequences = vec![SequenceEdit::Replace { expect: main0.clone(), write: sequence("seq-other", vec![]) }];
        assert_eq!(code(&mut session, &renamed, 3), "invalid_input");
        let mut twice = good.clone();
        twice.sequences.push(SequenceEdit::Remove { expect: main0.clone() });
        assert_eq!(code(&mut session, &twice, 3), "invalid_input", "one edit per sequence id");
        let mut past_end = good.clone();
        past_end.sequences = vec![SequenceEdit::Insert { at: 3, write: sequence("seq-new", vec![]) }];
        assert_eq!(code(&mut session, &past_end, 3), "invalid_input");
        let mut bad_records = good.clone();
        bad_records.records_write.as_mut().unwrap().clips[1].active_angle = 4;
        assert_eq!(code(&mut session, &bad_records, 3), "folio_rejected", "records must validate against the result");
        let mut dangling = good.clone();
        dangling.sequences.clear();
        assert_eq!(code(&mut session, &dangling, 3), "folio_rejected", "clip 11 is not in the document without the cut");
        let mut no_command = good.clone();
        no_command.command_id = String::new();
        assert_eq!(code(&mut session, &no_command, 3), "invalid_command");
        let mut flood = good.clone();
        flood.sequences = (0..=MAX_MULTICAM_STEP_EDITS as u32).map(|i| SequenceEdit::Insert { at: 0, write: sequence(&format!("seq-{i}"), vec![]) }).collect();
        assert_eq!(code(&mut session, &flood, 3), "budget_exceeded");
        assert_eq!(code(&mut session, &good, 2), "stale_revision");
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(session.submit_multicam(&good, 3, &canceled).unwrap_err().error.code(), "canceled");
        // Unparseable live entry bytes never equal an expectation.
        let mut corrupt = open(&package(&[source_sequence(), main0.clone()]));
        corrupt.set_document_entry(MULTICAM_ENTRY, Some(b"{not json".to_vec()), &token()).unwrap();
        assert_eq!(code(&mut corrupt, &good, 3), "value_conflict");

        assert_eq!(state(&session), held, "every refusal leaves sequences, entry and revision untouched");
        assert_eq!(session.receipt_count(), receipts);
        // The good step still commits afterwards.
        assert_eq!(session.submit_multicam(&good, 3, &token()).unwrap().revision, 4);
    });
}

#[test]
fn committed_multicam_step_survives_save_and_reopen() {
    bounded_case("multicam_commit_save_reopen", || {
        let mut session = open(&package(&[main_sequence(vec![])]));
        let before = state(&session);
        let applied = session.submit_multicam(&create_step(), 3, &token()).unwrap();
        let after = state(&session);
        assert!(session.is_dirty());
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let mut reopened = open(&checkpoint.bytes);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(state(&reopened), after, "sequences, entry and revision come back");
        assert_eq!(reopened.multicam_records(&token()).unwrap(), Some(declared()));
        // The inverse taken before the save still undoes on the reopened document.
        reopened.submit_multicam(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!((state(&reopened).0, state(&reopened).1), (before.0, before.1));
        let again = open(&reopened.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(again.document_entry(MULTICAM_ENTRY), None, "the undone entry is not saved");
        assert_eq!(again.snapshot().document().sequences.len(), 1);
    });
}
