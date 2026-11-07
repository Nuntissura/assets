//! `audio.*` (lane SMB-AUDIO-OUT-MIX-1): the sound port, program audio out, the routed audio-only
//! mixdown, stems and a timeline clip in the waveform editor, through the Viewer methods of
//! `audio_session` and `audio_session::playback`. Mixdown, stems, sound and waveform need feature
//! `audio-batch`; audio out also `audio-device`. Without them every row refuses `feature_unavailable`.
//!
//! Audio out opens score-device's `SoftwareProvider` (`software-clock-out`: a clocked thread, no
//! operating-system audio, so a process never takes the Operator's device), under a grant of the
//! caller's own actor on the open document. A host that plays to speakers calls
//! `Viewer::audio_out_open` with its own provider instead.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::CommandRefusal;
use super::decode::Fields;
use serde_json::Value;

#[cfg(feature = "audio-batch")]
mod enabled {
    use super::super::super::Viewer;
    use super::super::super::audio_import::{ImportLimits, MixFormat};
    use super::super::super::monitor::MonitorFail;
    use super::super::decode::{Fields, invalid};
    use super::super::{CommandRefusal, read_bounded};
    use hsk_studio_accord::Ticks;
    use hsk_studio_folio::{ClipSource, ContentDigest};
    use hsk_studio_pulse::{AudioSources, SourceExtents};
    use hsk_studio_reel as reel;
    use serde_json::{Value, json};
    use std::sync::Arc;

    /// Most files one `audio.bind_sound` takes.
    pub const MAX_SOUND_FILES: usize = 64;
    /// Largest sound file read.
    pub const MAX_SOUND_FILE_BYTES: u64 = 1 << 30;
    /// Decoded planar f32 bytes all files of one call may hold (frames x channels x 4).
    pub const MAX_SOUND_DECODED_BYTES: u64 = 1 << 31;

    pub(super) fn refusal(e: MonitorFail) -> CommandRefusal {
        CommandRefusal::new(e.code, json!({"message": e.detail}))
    }

    /// The decoded files of `audio.bind_sound`, each keyed as the clip source `sha256:<hex>` of its
    /// bytes (as `Viewer::bind_program_media` and the CLI's `--audio` key them).
    #[derive(Clone)]
    struct CommandSound(Arc<Vec<(ClipSource, Ticks, Vec<Vec<f32>>)>>);
    impl AudioSources for CommandSound {
        fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
            self.0.iter().find(|(s, _, _)| s == source).map(|(_, _, p)| p.as_slice())
        }
    }
    impl SourceExtents for CommandSound {
        fn extent(&self, source: &ClipSource) -> Option<Ticks> {
            self.0.iter().find(|(s, _, _)| s == source).map(|(_, t, _)| *t)
        }
    }

    /// Reads and decodes every file, then binds them as the sound port. Refused before the port
    /// changes: `sound_too_many`, `resource_unreadable`, `resource_too_large`, `audio_decode`,
    /// `audio_empty`, `audio_rate_mismatch` (no resampler), `audio_too_large` (the decoded budget).
    fn bind_sound(v: &mut Viewer, files: &[String]) -> Result<Value, CommandRefusal> {
        if files.is_empty() || files.len() > MAX_SOUND_FILES {
            return Err(CommandRefusal::new("sound_too_many", json!({"files": files.len(), "limit": MAX_SOUND_FILES})));
        }
        let cancel = v.cancel;
        let max_frames = ImportLimits::default().max_frames;
        let mut port = Vec::with_capacity(files.len());
        let mut rate: Option<u32> = None;
        let mut charged = 0u64;
        for name in files {
            let bytes = read_bounded(std::path::Path::new(name), MAX_SOUND_FILE_BYTES)?;
            let decode_fail = |message: String| CommandRefusal::new("audio_decode", json!({"path": name, "message": message}));
            let too_large = || CommandRefusal::new("audio_too_large", json!({"path": name, "decoded_limit": MAX_SOUND_DECODED_BYTES}));
            let digest = reel::full_digest(&mut std::io::Cursor::new(&bytes[..]), 1 << 20, cancel).map_err(|e| decode_fail(e.to_string()))?;
            let remaining = MAX_SOUND_DECODED_BYTES.saturating_sub(charged);
            let samples = remaining.checked_div(4).unwrap_or(0);
            let decoded = reel::audio::decode_audio_file_within(&mut std::io::Cursor::new(&bytes[..]), &reel::Limits::default(), max_frames.min(samples), samples, cancel).map_err(|e| match e {
                reel::ReelError::LimitExceeded { what: "audio-sample-budget" | "audio-frame-budget" } => too_large(),
                e => decode_fail(e.to_string()),
            })?;
            let frames = u64::try_from(decoded.frames()).map_err(|_| decode_fail("frame count".into()))?;
            if frames == 0 || decoded.channels == 0 {
                return Err(CommandRefusal::new("audio_empty", json!({"path": name})));
            }
            let cost = frames.checked_mul(u64::from(decoded.channels)).and_then(|n| n.checked_mul(4)).ok_or_else(too_large)?;
            if cost > remaining {
                return Err(too_large());
            }
            charged = charged.saturating_add(cost);
            match rate {
                Some(r) if r != decoded.sample_rate => return Err(CommandRefusal::new("audio_rate_mismatch", json!({"path": name, "rate": decoded.sample_rate, "expected": r}))),
                _ => rate = Some(decoded.sample_rate),
            }
            let extent = hsk_studio_pulse::ticks_from_samples(frames, decoded.sample_rate).map_err(|e| decode_fail(e.code().to_string()))?;
            let hex = reel::hex(&digest);
            let source = ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None };
            port.push((source, extent, decoded.planar()));
        }
        let rate = rate.ok_or_else(|| CommandRefusal::new("sound_too_many", json!({"files": 0})))?;
        let listed: Vec<String> = port.iter().map(|(s, _, _)| match s {
            ClipSource::Media { artifact_manifest_id, .. } => artifact_manifest_id.clone(),
            _ => String::new(),
        }).collect();
        let sound = CommandSound(Arc::new(port));
        v.set_export_audio(sound.clone(), Arc::new(sound), rate).map_err(|message| CommandRefusal::new("sound_port_refused", json!({"message": message})))?;
        Ok(json!({"sample_rate_hz": rate, "sources": listed}))
    }

    const FORMATS: &[(&str, &str)] = &[("wav16", "wav16"), ("wav24", "wav24"), ("wav_float", "wav_float"), ("flac16", "flac16"), ("flac24", "flac24"), ("aiff16", "aiff16"), ("aiff24", "aiff24"), ("aiff_float", "aiff_float")];

    fn format(f: &Fields<'_>) -> Result<MixFormat, CommandRefusal> {
        let token = f.token("format", FORMATS)?;
        MixFormat::parse(token).ok_or_else(|| invalid("format", "a mix format"))
    }

    pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
        match id {
            "audio.bind_sound" => bind_sound(v, &f.list("files")?),
            "audio.export_mix" => {
                let format = format(&f)?;
                let r = v.export_sequence_mix(&f.text("sequence_id")?, format, &f.path("output")?, f.flag_or("overwrite", false)?).map_err(refusal)?;
                Ok(r.to_json())
            }
            "audio.export_stems" => {
                let format = format(&f)?;
                let r = v.export_sequence_stems(&f.text("sequence_id")?, &f.list("buses")?, f.flag_or("rest", false)?, format, &f.path("dir")?, f.flag_or("overwrite", false)?).map_err(refusal)?;
                Ok(r.to_json())
            }
            "audio.waveform_open_clip" => v.open_waveform_from_clip(&f.text("clip_id")?).map_err(refusal),
            _ => out(v, id, f),
        }
    }

    #[cfg(feature = "audio-device")]
    fn out(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
        use super::super::super::audio_session::playback::{AudioTransport, close_json};
        use hsk_studio_score_device::{DeviceGrant, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareProvider};
        static PROVIDER: std::sync::OnceLock<SoftwareProvider> = std::sync::OnceLock::new();
        let provider = PROVIDER.get_or_init(|| SoftwareProvider::new(Limits::default()));
        let grant = |v: &Viewer| -> Result<DeviceGrant, CommandRefusal> {
            let snapshot = v.run().snapshot().ok_or_else(|| CommandRefusal::new("no_document", json!({"message": "nothing is open"})))?;
            let resource = hsk_studio_accord::DomainId::parse(&snapshot.document().document_id).map_err(|_| CommandRefusal::new("no_document", json!({"message": "document id"})))?;
            Ok(DeviceGrant::new(v.run().actor().clone(), resource, &[Direction::Output], None))
        };
        match id {
            "audio.out_devices" => v.audio_out_devices(provider, &grant(v)?).map_err(refusal),
            "audio.out_open" => {
                let device = f.opt_text("device")?.unwrap_or_else(|| SOFTWARE_OUTPUT_DEVICE.to_string());
                let block = if f.has("block_frames") { u32::try_from(f.unsigned("block_frames")?).map_err(|_| invalid("block_frames", "1..=16384"))? } else { 480 };
                let sequence = f.opt_text("sequence_id")?;
                let g = grant(v)?;
                v.audio_out_open(provider, &g, &device, block, sequence.as_deref()).map_err(refusal)
            }
            "audio.out_transport" => {
                let tick = if f.has("tick") { Some(f.unsigned("tick")?) } else { None };
                let action = f.text("action")?;
                let command = AudioTransport::parse(&action, tick).ok_or_else(|| invalid(if action == "scrub" { "tick" } else { "action" }, "play|pause|toggle|scrub (with tick)|loop_on|loop_off"))?;
                v.audio_out_transport(command).map_err(refusal)
            }
            "audio.out_run" => v.audio_out_run(f.unsigned("ms")?).map_err(refusal),
            "audio.out_state" => Ok(v.audio_out_json()),
            "audio.out_close" => Ok(v.audio_out_close().as_ref().map_or(Value::Null, close_json)),
            other => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
        }
    }

    #[cfg(not(feature = "audio-device"))]
    fn out(_: &mut Viewer, id: &str, _: Fields<'_>) -> Result<Value, CommandRefusal> {
        Err(CommandRefusal::new("feature_unavailable", json!({"command_id": id, "feature": "audio-device"})))
    }
}

/// One `audio.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    #[cfg(feature = "audio-batch")]
    {
        enabled::run(v, id, f)
    }
    #[cfg(not(feature = "audio-batch"))]
    {
        let _ = (v, f);
        Err(CommandRefusal::new("feature_unavailable", serde_json::json!({"command_id": id, "feature": "audio-batch"})))
    }
}
