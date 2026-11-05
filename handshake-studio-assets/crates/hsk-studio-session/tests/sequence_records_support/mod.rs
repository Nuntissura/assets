//! Fixtures shared by the sequence-records test targets (lane SMB-PR-SEQUENCE-SESSION-1).
#![allow(dead_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{AssetSource, Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, sha256_hex_of, write_package_with};
use hsk_studio_session::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 60-second deadline"),
    }
}

/// 24 and 25 fps ticks per frame.
pub const F24: u64 = 10_584_000_000;
pub const F25: u64 = 10_160_640_000;
/// A shipped 25 fps preset and a shipped mode other than `custom`.
pub const SHIPPED_25: &str = "hd-1080p-hd-1080p-25-fps";
pub const SHIPPED_MODE: &str = "quicktime_dv_ntsc";

pub fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn doc_id() -> String {
    identity("SDOC", 1)
}

pub struct Granted;
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

/// Every Observe frame the session sent, as sent.
#[derive(Clone, Default)]
pub struct Frames(pub Arc<Mutex<Vec<Vec<u8>>>>);
impl Frames {
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().concat()
    }
}
struct FrameSink(Frames);
impl SinkPort for FrameSink {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        (self.0).0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}

pub fn token() -> CancellationToken {
    CancellationToken::default()
}
pub fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}
pub fn context(command: &str, revision: u64, correlation: u64) -> CommandContext {
    CommandContext::new(actor(), ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), revision, CorrelationId::new(correlation), None)
}
pub fn ports(frames: &Frames) -> SessionPorts {
    SessionPorts::new(Box::new(FrameSink(frames.clone()))).with_resolver(Box::new(Granted))
}
pub fn try_open(bytes: &[u8], frames: &Frames) -> Result<Session, Failed> {
    Session::open_bytes(bytes, SessionConfig::new(actor()), ports(frames), &token())
}
pub fn open(bytes: &[u8]) -> Session {
    open_counted(bytes).0
}
pub fn open_counted(bytes: &[u8]) -> (Session, Frames) {
    let frames = Frames::default();
    let session = try_open(bytes, &frames).unwrap_or_else(|f| panic!("open refused: {f:?}"));
    (session, frames)
}

fn clip(n: u32, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: identity("SCLP", n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: format!("camera-{n}"),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: format!("ab{n:04}") },
            asset_id: None,
        },
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F24),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F24),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}
pub fn sequence(id: &str, ticks_per_frame: u64, clips: Vec<StudioClip>) -> StudioSequence {
    let track = StudioTrack {
        schema_id: TrackSchema::V1,
        // Unique per sequence id: Folio refuses a track id repeated across a document.
        track_id: identity("STRK", id.bytes().map(u32::from).sum()),
        kind: TrackKind::Video,
        index: 0,
        name: "track".into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    };
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame },
        duration: Ticks::new(0),
        tracks: vec![track],
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
/// `seq-a` at 25 fps, empty; `seq-b` at 24 fps with one clip.
pub fn seq_a() -> StudioSequence {
    sequence("seq-a", F25, vec![])
}
pub fn seq_b() -> StudioSequence {
    sequence("seq-b", F24, vec![clip(1, 24)])
}
pub fn package_of(sequences: &[StudioSequence], held: &[Quarantined]) -> Vec<u8> {
    let sequences: Vec<serde_json::Value> = sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect();
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":doc_id(),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
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
    let mut held = held.to_vec();
    held.sort_by(|a, b| a.name.cmp(&b.name));
    let mut bytes = Vec::new();
    write_package_with(
        &snapshot,
        &assets as &dyn AssetSource,
        &Extras { quarantined: &held, previews: &[] },
        &WriteOptions::default(),
        &Limits::default(),
        &token(),
        &mut bytes,
    )
    .unwrap();
    bytes
}
/// The two-sequence package, with `held` entries.
pub fn package(held: &[Quarantined]) -> Vec<u8> {
    package_of(&[seq_a(), seq_b()], held)
}
/// A held entry named `studio/<token>.<sha256 of the body>`.
pub fn entry(entry_token: &str, body: &[u8]) -> Quarantined {
    let sha256_hex = sha256_hex_of(body);
    Quarantined { name: format!("{DOCUMENT_ENTRY_PREFIX}{entry_token}.{sha256_hex}"), bytes: body.to_vec(), sha256_hex }
}

/// STU-VID-018: all nine immersive fields, none at a default.
pub fn immersive() -> StudioImmersiveSettings {
    StudioImmersiveSettings {
        projection_type: IMMERSIVE_PROJECTION_EQUIRECTANGULAR,
        stereoscopic_type: IMMERSIVE_STEREO_OVER_UNDER,
        stereoscopic_eye: 1,
        captured_horizontal_view: 360.0,
        captured_vertical_view: 180.0,
        field_of_horizontal_view: 108.0,
        field_of_vertical_view: 108.0,
        ambisonics_monitoring_type: IMMERSIVE_AMBISONICS_NONE,
        ambisonics_hrir: Some("hrir-1".into()),
    }
}
/// A complete settings record: immersive, and a preview format whose frame size differs from the
/// sequence's (STU-VID-018, STU-VID-019).
pub fn settings(ticks_per_frame: u64) -> StudioSequenceSettings {
    StudioSequenceSettings {
        frame_size: SequenceFrameSize { width: 3840, height: 1920 },
        pixel_aspect_ratio: SequencePixelAspectRatio::SQUARE,
        frame_rate: SequenceFrameRate { ticks_per_frame },
        field_order: SequenceFieldOrder::Progressive,
        video_time_display: StudioTimeDisplay::Timecode25,
        audio_time_display: StudioTimeDisplay::AudioSamples,
        audio_sample_rate_hz: 48_000,
        audio_master_channel_type: AudioMasterChannelType::Stereo,
        working_color_space: SequenceWorkingColorSpace::STUDIO_DEFAULT,
        auto_tone_map: false,
        use_maximum_bit_depth: true,
        use_maximum_render_quality: false,
        allow_linear_compositing: true,
        initial_video_track_count: 3,
        initial_audio_track_count: 4,
        preview_render_format: StudioPreviewRenderFormat {
            codec: Some("apcs".into()),
            frame_size: SequenceFrameSize { width: 1920, height: 960 },
        },
        immersive: Some(immersive()),
    }
}
pub fn settings_entry(sequence_id: &str, mode: &str, ticks_per_frame: u64) -> StudioSequenceSettingsEntry {
    StudioSequenceSettingsEntry { sequence_id: sequence_id.into(), editing_mode: mode.into(), settings: settings(ticks_per_frame) }
}
pub fn records(entries: Vec<StudioSequenceSettingsEntry>) -> StudioSequenceSettingsRecords {
    let mut r = StudioSequenceSettingsRecords::new(doc_id());
    r.sequences = entries;
    r
}
pub fn preset(preset_id: &str, ticks_per_frame: u64) -> StudioSequencePreset {
    StudioSequencePreset {
        preset_id: preset_id.into(),
        name: format!("Preset {preset_id}"),
        category_path: vec!["Custom".into()],
        settings: settings(ticks_per_frame),
        defaulted_fields: Vec::new(),
        warnings: Vec::new(),
    }
}
pub fn library(presets: Vec<StudioSequencePreset>) -> StudioSequencePresetSet {
    let mut set = StudioSequencePresetSet::new();
    set.presets = presets;
    set
}

pub fn apply(preset_id: &str, sequence_id: &str) -> StudioSequenceSettingsCommand {
    StudioSequenceSettingsCommand::ApplyPreset { preset_id: preset_id.into(), sequence_id: sequence_id.into() }
}
pub fn set_mode(sequence_id: &str, mode_id: &str) -> StudioSequenceSettingsCommand {
    StudioSequenceSettingsCommand::SetEditingMode { sequence_id: sequence_id.into(), mode_id: mode_id.into() }
}
pub fn save(sequence_id: &str, preset_id: &str) -> StudioSequenceSettingsCommand {
    StudioSequenceSettingsCommand::SavePresetFromSequence {
        sequence_id: sequence_id.into(),
        preset_id: preset_id.into(),
        name: "Saved preset".into(),
        category_path: vec!["Saved".into()],
    }
}
/// Runs `command` under a context built for it, at the session's revision.
pub fn run(session: &mut Session, command: &StudioSequenceSettingsCommand, correlation: u64) -> Result<Applied, Failed> {
    let revision = session.document_revision();
    session.submit_sequence_command_as(&context(command.command_id(), revision, correlation), command, revision, &token())
}
pub fn step(sequences: Vec<SequenceEdit>, settings: Option<RecordEdit<StudioSequenceSettingsRecords>>, presets: Option<RecordEdit<StudioSequencePresetSet>>) -> SequenceRecordsStep {
    SequenceRecordsStep { command_id: "cmd.test".into(), correlation_id: 7, sequences, settings, presets }
}
pub fn fresh_inverse(applied: &Applied, command: &str) -> SequenceRecordsStep {
    let mut inverse = applied.sequence_records_inverse.clone().expect("a changed step has an inverse");
    inverse.command_id = command.into();
    inverse.correlation_id = 8;
    inverse
}

/// What a session holds for this contract: sequences, both entries' bytes, revision.
pub type State = (Vec<StudioSequence>, Option<Vec<u8>>, Option<Vec<u8>>, u64);
pub fn state(s: &Session) -> State {
    (
        s.snapshot().document().sequences.clone(),
        s.document_entry(SEQUENCE_SETTINGS_ENTRY).map(<[u8]>::to_vec),
        s.document_entry(SEQUENCE_PRESETS_ENTRY).map(<[u8]>::to_vec),
        s.document_revision(),
    )
}
/// [`State`] without the revision (an undo restores values at a new revision).
pub fn values(s: &Session) -> (Vec<StudioSequence>, Option<Vec<u8>>, Option<Vec<u8>>) {
    let (a, b, c, _) = state(s);
    (a, b, c)
}
