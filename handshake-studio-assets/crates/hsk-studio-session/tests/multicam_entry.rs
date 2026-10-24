//! Lane SMB-PR-MULTICAM-1: the Folio multicam records (STU-VID-033) persist as the session document
//! entry `studio/multicam.<sha256>`, validated on every set and read, and survive save and reopen
//! byte-identically; a stored entry the document no longer supports reads as the Folio diagnostic.
use hsk_studio_accord::{ActorContext, CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{AssetSource, Extras, Limits, MemoryAssetSource, PackageError, WriteOptions, write_package_with};
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

fn clip(n: u32, source: ClipSource, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: identity("SCLP", n),
        source_ref: source,
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
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
/// `seq-mc`: three camera angles; `seq-main`: V1 multicam clip 1 nesting `seq-mc`.
fn source_sequence() -> StudioSequence {
    sequence("seq-mc", (0..3).map(|i| track(100 + i, TrackKind::Video, i, vec![clip(100 + i, media(i), 24)])).collect())
}
fn main_sequence(with_multicam_clip: bool) -> StudioSequence {
    let clips = if with_multicam_clip { vec![clip(1, ClipSource::Sequence { sequence_id: "seq-mc".into() }, 12)] } else { vec![] };
    sequence("seq-main", vec![track(1, TrackKind::Video, 0, clips)])
}
fn package() -> Vec<u8> {
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(source_sequence()).unwrap(), serde_json::to_value(main_sequence(true)).unwrap()]
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
fn records(angle: u32) -> StudioMulticamRecords {
    let mut r = StudioMulticamRecords::new(identity("SDOC", 1));
    r.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-mc".into(),
        selection_order: MulticamSelectionOrder::TopDown,
        angle_order: multicam_angle_order(&source_sequence(), MulticamSelectionOrder::TopDown),
    });
    r.clips.push(StudioMulticamClip {
        clip_id: identity("SCLP", 1),
        active_angle: angle,
        audio_angle: angle,
        audio_follows_video: true,
        follows_nest_setting: true,
        enabled: true,
    });
    r
}
fn folio_code(e: &SessionError) -> Option<Code> {
    match e {
        SessionError::Package(PackageError::Document(d)) => Some(d.code),
        _ => None,
    }
}

#[test]
fn multicam_records_survive_save_and_reopen() {
    bounded_case("multicam_save_reopen", || {
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&MULTICAM_ENTRY) && MULTICAM_ENTRY == "multicam");
        let mut session = open(&package());
        assert_eq!(session.multicam_records(&token()).unwrap(), None);
        let r = records(3);
        session.set_multicam_records(Some(&r), &token()).unwrap();
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(r.clone()));
        let stored = session.document_entry(MULTICAM_ENTRY).unwrap().to_vec();
        assert_eq!(stored, serde_json::to_vec(&r).unwrap());

        // A refused set (angle past the angle count, another document) changes nothing.
        let err = session.set_multicam_records(Some(&records(4)), &token()).unwrap_err();
        assert_eq!(folio_code(&err), Some(Code::InvalidField));
        let mut foreign = r.clone();
        foreign.document_id = identity("SDOC", 2);
        assert_eq!(folio_code(&session.set_multicam_records(Some(&foreign), &token()).unwrap_err()), Some(Code::InvalidField));
        assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(stored.as_slice()));
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(session.set_multicam_records(None, &canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.multicam_records(&canceled).unwrap_err().code(), "canceled");

        // Save and reopen: the entry comes back byte-identical, with no loss note.
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let reopened = open(&checkpoint.bytes);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(reopened.document_entry(MULTICAM_ENTRY), Some(stored.as_slice()));
        assert_eq!(reopened.multicam_records(&token()).unwrap(), Some(r));
        // Removing the entry removes it from the next save.
        let mut cleared = open(&checkpoint.bytes);
        cleared.set_multicam_records(None, &token()).unwrap();
        let again = open(&cleared.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(again.multicam_records(&token()).unwrap(), None);
    });
}

#[test]
fn stale_multicam_entry_reads_as_a_diagnostic() {
    bounded_case("multicam_stale_entry", || {
        let mut session = open(&package());
        session.set_multicam_records(Some(&records(1)), &token()).unwrap();
        let stored = session.document_entry(MULTICAM_ENTRY).unwrap().to_vec();
        // The multicam clip leaves the document (one timeline step); the entry no longer validates.
        let step = TimelineStep {
            command_id: "cmd-lift".into(),
            correlation_id: 1,
            sequence_id: "seq-main".into(),
            expect: main_sequence(true),
            write: main_sequence(false),
        };
        session.submit_timeline(&step, 3, &token()).unwrap();
        let err = session.multicam_records(&token()).unwrap_err();
        assert_eq!(folio_code(&err), Some(Code::DanglingReference), "{err:?}");
        // The stored bytes are kept as they are (never rewritten into partial records).
        assert_eq!(session.document_entry(MULTICAM_ENTRY), Some(stored.as_slice()));
        // Corrupt bytes read as invalid JSON, typed.
        session.set_document_entry(MULTICAM_ENTRY, Some(b"{not json".to_vec()), &token()).unwrap();
        assert_eq!(folio_code(&session.multicam_records(&token()).unwrap_err()), Some(Code::InvalidJson));
    });
}
