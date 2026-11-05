//! Lane SMB-PR-RETIME-HOLD-1: a clip's `retime` record (frame hold, interpolation choice, `Maintain
//! audio pitch`) survives the real commit path: `submit_timeline`, undo, redo, save to a file, reopen.
//! A retime-only change moves no clip, so every step here differs from the one before it ONLY in that
//! record. Session has no edge to pulse: the sequences are built here in the shape pulse's
//! `EditProposal::inverse` gives (`restore` = `expect`, `expect` = `write`).
use hsk_studio_accord::{ActorContext, CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{
    AssetSource, Extras, Limits, MemoryAssetSource, WriteOptions, write_package_with,
};
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
fn config() -> SessionConfig {
    SessionConfig::new(ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap())
}
fn ports() -> SessionPorts {
    SessionPorts::new(Box::new(Sink)).with_resolver(Box::new(Granted))
}
fn open(bytes: &[u8]) -> Session {
    Session::open_bytes(bytes, config(), ports(), &token()).unwrap()
}

/// Clip `n`: source frames `[si, si + frames)` at timeline frame `start`.
fn clip(n: u32, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: identity("SCLP", n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "manifest-a".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}

/// V1: clip 1 src 0..48 @0, clip 2 src 100..148 @48; clip 2 carries `retime`.
fn sequence(retime: ClipRetime) -> StudioSequence {
    let mut held = clip(2, 100, 48, 48);
    held.retime = retime;
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: identity("STRK", 1),
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
            clips: vec![clip(1, 0, 48, 0), held],
        }],
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}

fn plain() -> ClipRetime {
    ClipRetime::default()
}
fn hold(frame: u64) -> ClipRetime {
    ClipRetime { frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(frame * F), anchor: FrameHoldAnchor::Playhead }), ..ClipRetime::default() }
}
/// The hold plus the two options.
fn full() -> ClipRetime {
    ClipRetime { frame_interpolation: FrameInterpolation::OpticalFlow, maintain_audio_pitch: true, ..hold(105) }
}
/// The options with the hold cleared.
fn options_only() -> ClipRetime {
    ClipRetime { frame_hold: None, ..full() }
}

fn package_with(seq: StudioSequence) -> Vec<u8> {
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(&seq).unwrap()]
    });
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token(), &Granted).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("fixture became read-only"),
    };
    let assets = MemoryAssetSource::new();
    let mut bytes = Vec::new();
    write_package_with(&snapshot, &assets as &dyn AssetSource, &Extras { quarantined: &[], previews: &[] }, &WriteOptions::default(), &Limits::default(), &token(), &mut bytes).unwrap();
    bytes
}
fn step(command: &str, expect: ClipRetime, write: ClipRetime) -> TimelineStep {
    TimelineStep { command_id: command.into(), correlation_id: 1, sequence_id: "seq-main".into(), expect: sequence(expect), write: sequence(write) }
}
fn live(session: &Session) -> &StudioSequence {
    &session.snapshot().document().sequences[0]
}
/// Applies the template, expecting exactly one revision step and a live sequence holding `want`.
fn commit(session: &mut Session, template: &TimelineStep, want: ClipRetime) -> TimelineStep {
    let revision = session.document_revision();
    let applied = session.submit_timeline(template, revision, &token()).unwrap();
    assert!(applied.changed, "{} changed the document", template.command_id);
    assert_eq!((applied.previous_revision, applied.revision), (revision, revision + 1));
    assert_eq!(applied.provider, TIMELINE_STEP_PROVIDER);
    assert_eq!(live(session), &sequence(want), "{}", template.command_id);
    applied.timeline_inverse.expect("a changing step has an inverse")
}

/// A file under the test run's temp root, removed when dropped.
struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("hsk-studio-retime-commit-{name}-{}.handshake", std::process::id())))
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn a_hold_and_retime_options_survive_commit_undo_redo_save_and_reopen() {
    bounded_case("retime_commit", || {
        let mut session = open(&package_with(sequence(plain())));
        assert_eq!(session.document_revision(), 3);

        // Set the hold, then the options: one revision each, and the live document holds the record.
        let undo_hold = commit(&mut session, &step("cmd-hold", plain(), hold(105)), hold(105));
        let undo_options = commit(&mut session, &step("cmd-options", hold(105), full()), full());
        assert_eq!(session.document_revision(), 5);

        // Undo both in order, then redo both: each returns the exact record.
        let redo_options = commit(&mut session, &undo_options, hold(105));
        let redo_hold = commit(&mut session, &undo_hold, plain());
        assert_eq!(live(&session).tracks[0].clips[1].retime, plain());
        commit(&mut session, &redo_hold, hold(105));
        commit(&mut session, &redo_options, full());
        assert_eq!(session.document_revision(), 9);
        assert_eq!(live(&session).tracks[0].clips[1].retime, full());
        assert_eq!(live(&session).tracks[0].clips[0].retime, plain(), "only that clip carries it");

        // Save to a file and reopen: the record comes back exactly, with no loss note.
        let file = TempFile::new("full");
        let saved = session.save_to(&file.0, &token()).unwrap();
        assert_eq!(saved.revision, 9);
        let reopened = Session::open_path(&file.0, config(), ports(), &token()).unwrap();
        assert_eq!(live(&reopened), &sequence(full()));
        assert_eq!(reopened.document_revision(), 9);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(reopened.snapshot().encoded_bytes(), session.snapshot().encoded_bytes(), "the document comes back byte-identical");

        // The reopened session is live: clearing the hold commits, keeps the options and saves.
        let mut reopened = reopened;
        commit(&mut reopened, &step("cmd-clear", full(), options_only()), options_only());
        let checkpoint = reopened.export_checkpoint(&token()).unwrap();
        let again = open(&checkpoint.bytes);
        assert_eq!(live(&again), &sequence(options_only()));
        assert!(again.loss().is_empty(), "{:?}", again.loss());
        let wire = String::from_utf8_lossy(again.snapshot().encoded_bytes()).into_owned();
        assert!(wire.contains("\"maintain_audio_pitch\":true") && !wire.contains("frame_hold"), "{wire}");

        // Everything undone: an old-shape clip again, with no retime key in the saved document.
        let mut session = open(&package_with(sequence(plain())));
        let undo = commit(&mut session, &step("cmd-hold", plain(), hold(105)), hold(105));
        commit(&mut session, &undo, plain());
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let old_shape = open(&checkpoint.bytes);
        assert_eq!(live(&old_shape), &sequence(plain()));
        assert!(!String::from_utf8_lossy(old_shape.snapshot().encoded_bytes()).contains("retime"));
    });
}

/// A difference in the record alone is a real difference to every gate of the commit path.
#[test]
fn a_retime_only_difference_reaches_every_gate_of_the_commit_path() {
    bounded_case("retime_gates", || {
        let code = |s: &mut Session, st: &TimelineStep, rev: u64| s.submit_timeline(st, rev, &token()).unwrap_err().error.code();
        let mut session = open(&package_with(sequence(plain())));
        commit(&mut session, &step("cmd-hold", plain(), hold(105)), hold(105));
        // Preconditions: a step computed from the unheld sequence never overwrites the hold.
        assert_eq!(code(&mut session, &step("cmd-stale", plain(), options_only()), 4), "value_conflict");
        // Another frame, or the same frame from another record, is another value.
        assert_eq!(code(&mut session, &step("cmd-stale", hold(106), plain()), 4), "value_conflict");
        // Folio on the commit path: the hostile record is refused and nothing changes.
        let mut at_max = sequence(hold(105));
        at_max.tracks[0].clips[1].retime.frame_hold = Some(ClipFrameHold { source_tick: Ticks::new(u64::MAX), anchor: FrameHoldAnchor::InPoint });
        let hostile = TimelineStep { command_id: "cmd-max".into(), correlation_id: 1, sequence_id: "seq-main".into(), expect: sequence(hold(105)), write: at_max };
        assert_eq!(code(&mut session, &hostile, 4), "folio_rejected");
        assert_eq!(session.document_revision(), 4);
        assert_eq!(live(&session), &sequence(hold(105)));
        // Writing the value the document holds is a no-change step.
        let same = session.submit_timeline(&step("cmd-same", hold(105), hold(105)), 4, &token()).unwrap();
        assert!(!same.changed && same.timeline_inverse.is_none());
        assert_eq!(session.document_revision(), 4);
        // The idempotency binding sees the record: another hold under the same key is a conflict,
        // the same hold replays and applies nothing.
        let mut keyed = open(&package_with(sequence(plain())));
        let key = IdempotencyKey::parse("retime-key-1").unwrap();
        let first = keyed.submit_timeline_keyed(&key, &step("cmd-k", plain(), hold(105)), 3, &token()).unwrap();
        let replay = keyed.submit_timeline_keyed(&key, &step("cmd-k", plain(), hold(105)), 3, &token()).unwrap();
        assert!(!first.replayed && replay.replayed);
        let other = keyed.submit_timeline_keyed(&key, &step("cmd-k", plain(), hold(106)), 3, &token()).unwrap_err();
        assert_eq!(other.error.code(), "idempotency_conflict");
        assert_eq!(keyed.document_revision(), 4);
        assert_eq!(live(&keyed), &sequence(hold(105)));
    });
}
