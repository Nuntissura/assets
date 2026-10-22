//! Lane SMB-FOLIO-TIMELINE-RECORDS: a timeline edit (an insert) plus its marker and transition
//! changes is ONE session step with one undo and one redo, and the records survive the package.
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
fn open(bytes: &[u8]) -> Session {
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .unwrap();
    Session::open_bytes(
        bytes,
        SessionConfig::new(actor),
        SessionPorts::new(Box::new(Sink)).with_resolver(Box::new(Granted)),
        &token(),
    )
    .unwrap()
}

/// Clip `n`: source frames `[si, si + frames)` at timeline frame `start`.
fn clip(n: u32, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: identity("SCLP", n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "manifest-a".into(),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "abc123".into(),
            },
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
    }
}
fn sequence(clips: Vec<StudioClip>, markers: Vec<StudioMarker>) -> StudioSequence {
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
            clips,
        }],
        markers,
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
fn marker(id: &str, start: u64, frames: u64, role: Option<MarkerRole>) -> StudioMarker {
    StudioMarker {
        marker_id: id.into(),
        name: format!("marker {id}"),
        comment: String::new(),
        color: LabelColor::Teal,
        start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        role,
    }
}

/// V1: A src 0..48 @0, B src 100..148 @48.
fn original() -> StudioSequence {
    sequence(vec![clip(1, 0, 48, 0), clip(2, 100, 48, 48)], vec![])
}
/// The insert of 12 frames (source 500..512) at frame 24 on V1, as Pulse plans it (A split at the
/// in point, A's right half 3 takes the rest, B ripples to 60), PLUS the records that follow it: a
/// chapter marker, a clip marker on the incoming clip and an 8-frame dissolve on the 3|B cut.
fn edited() -> StudioSequence {
    let mut incoming = clip(9, 500, 12, 24);
    incoming.markers = vec![marker("m-in", 504, 0, Some(MarkerRole::CuePoint))];
    let mut right = clip(3, 24, 24, 36);
    right.transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "t-3b".into(),
        transition_kind: "cross_dissolve".into(),
        duration: Ticks::new(8 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    });
    sequence(
        vec![clip(1, 0, 24, 0), incoming, right, clip(2, 100, 48, 60)],
        vec![marker("m-chapter", 10, 4, Some(MarkerRole::Chapter))],
    )
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
    let snapshot = match folio::inspect_bytes(
        &serde_json::to_vec(&doc).unwrap(),
        Budget::default(),
        &token(),
        &Granted,
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("fixture became read-only"),
    };
    let assets = MemoryAssetSource::new();
    let mut bytes = Vec::new();
    write_package_with(
        &snapshot,
        &assets as &dyn AssetSource,
        &Extras {
            quarantined: &[],
            previews: &[],
        },
        &WriteOptions::default(),
        &Limits::default(),
        &token(),
        &mut bytes,
    )
    .unwrap();
    bytes
}
fn step(command: &str, expect: StudioSequence, write: StudioSequence) -> TimelineStep {
    TimelineStep {
        command_id: command.into(),
        correlation_id: 1,
        sequence_id: "seq-main".into(),
        expect,
        write,
    }
}
fn live(session: &Session) -> &StudioSequence {
    &session.snapshot().document().sequences[0]
}

#[test]
fn timeline_step_round_trips_and_undoes_as_one_step() {
    bounded_case("timeline_undo", || {
        let mut session = open(&package_with(original()));
        assert_eq!(session.document_revision(), 3);

        // The insert and its marker and transition records: one step, one revision.
        let applied = session
            .submit_timeline(&step("cmd-insert", original(), edited()), 3, &token())
            .unwrap();
        assert!(applied.changed);
        assert_eq!((applied.previous_revision, applied.revision), (3, 4));
        assert_eq!(applied.provider, TIMELINE_STEP_PROVIDER);
        assert!(applied.revision_updates.is_empty() && applied.inverse.is_none());
        assert_eq!(live(&session), &edited());

        // Undo restores clips, markers and the transition together.
        let mut undo = applied.timeline_inverse.clone().unwrap();
        undo.command_id = "cmd-undo".into();
        let undone = session.submit_timeline(&undo, 4, &token()).unwrap();
        assert_eq!(undone.revision, 5);
        assert_eq!(live(&session), &original());
        // Redo is the undo's own inverse.
        let mut redo = undone.timeline_inverse.clone().unwrap();
        redo.command_id = "cmd-redo".into();
        session.submit_timeline(&redo, 5, &token()).unwrap();
        assert_eq!(live(&session), &edited());

        // Package round trip: the records come back exactly, with no loss note.
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let reopened = open(&checkpoint.bytes);
        assert_eq!(live(&reopened), &edited());
        assert_eq!(reopened.document_revision(), 6);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        let seq = live(&reopened);
        assert_eq!(seq.markers[0].role, Some(MarkerRole::Chapter));
        assert_eq!(seq.tracks[0].clips[1].markers[0].marker_id, "m-in");
        assert_eq!(
            seq.tracks[0].clips[2]
                .transitions
                .tail
                .as_ref()
                .unwrap()
                .transition_id,
            "t-3b"
        );

        // Idempotent under a key: an identical retry replays the receipt and applies nothing.
        let mut keyed = open(&package_with(original()));
        let key = IdempotencyKey::parse("timeline-key-1").unwrap();
        let first = keyed
            .submit_timeline_keyed(&key, &step("cmd-k", original(), edited()), 3, &token())
            .unwrap();
        let again = keyed
            .submit_timeline_keyed(&key, &step("cmd-k", original(), edited()), 3, &token())
            .unwrap();
        assert!(!first.replayed && again.replayed);
        assert_eq!(keyed.document_revision(), 4);
    });
}

#[test]
fn hostile_timeline_steps_change_nothing() {
    bounded_case("timeline_hostile", || {
        let mut session = open(&package_with(original()));
        let code = |s: &mut Session, st: &TimelineStep, rev: u64| {
            s.submit_timeline(st, rev, &token())
                .unwrap_err()
                .error
                .code()
        };

        // The precondition: a step computed from another sequence value never overwrites it.
        assert_eq!(
            code(&mut session, &step("c", edited(), original()), 3),
            "value_conflict"
        );
        let mut other = step("c", original(), edited());
        other.sequence_id = "seq-other".into();
        assert_eq!(code(&mut session, &other, 3), "invalid_input");
        let mut missing = step("c", original(), edited());
        for s in [&mut missing.expect, &mut missing.write] {
            s.sequence_id = "seq-other".into();
        }
        missing.sequence_id = "seq-other".into();
        assert_eq!(code(&mut session, &missing, 3), "invalid_target");
        assert_eq!(
            code(&mut session, &step("", original(), edited()), 3),
            "invalid_command"
        );
        assert_eq!(
            code(
                &mut session,
                &step(&"x".repeat(MAX_COMMAND_ID_BYTES + 1), original(), edited()),
                3
            ),
            "invalid_command"
        );
        assert_eq!(
            code(&mut session, &step("c", original(), edited()), 2),
            "stale_revision"
        );

        // Results Folio refuses: overlapping clips, two transitions at one cut, wrong duration.
        let mut overlap = edited();
        overlap.tracks[0].clips[3].timeline_start = Ticks::new(50 * F);
        overlap.duration = overlap.content_end().unwrap();
        assert_eq!(
            code(&mut session, &step("c", original(), overlap), 3),
            "folio_rejected"
        );
        let mut two = edited();
        two.tracks[0].clips[3].transitions.head = two.tracks[0].clips[2]
            .transitions
            .tail
            .clone()
            .map(|mut t| {
                t.transition_id = "t-other".into();
                t
            });
        assert_eq!(
            code(&mut session, &step("c", original(), two), 3),
            "folio_rejected"
        );
        let mut stale_duration = edited();
        stale_duration.duration = Ticks::new(1);
        assert_eq!(
            code(&mut session, &step("c", original(), stale_duration), 3),
            "folio_rejected"
        );
        // Canceled before anything is published.
        let canceled = token();
        canceled.cancel();
        let err = session
            .submit_timeline(&step("c", original(), edited()), 3, &canceled)
            .unwrap_err();
        assert_eq!(err.error.code(), "canceled");

        // Nothing above changed the session.
        assert_eq!(session.document_revision(), 3);
        assert_eq!(live(&session), &original());
        // Writing the value the document holds is a no-change step without an inverse.
        let same = session
            .submit_timeline(&step("c", original(), original()), 3, &token())
            .unwrap();
        assert!(!same.changed && same.timeline_inverse.is_none());
        assert_eq!(session.document_revision(), 3);
    });
}
