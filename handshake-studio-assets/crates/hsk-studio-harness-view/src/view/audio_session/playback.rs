//! Program audio out hosted by the viewer (lane SMB-AUDIO-OUT-MIX-1; features `audio-batch` and
//! `audio-device`; STU-ARC-013, STU-VID-023). The written [`AudioOut`] gets its caller: the program
//! monitor's sequence plays through the bus stream the mixer and the mixdown use
//! ([`super::SequenceSound`]: the bound sound port with repaired audio resolved; an open audio file
//! plays its own import) into a score-device output stream, and the stream's consumed-frame counter
//! is the transport's master clock.
//!
//! # Manual (no-context operator or model)
//!
//! - **Device**: [`super::super::Viewer::audio_out_devices`] lists what a [`DeviceProvider`] offers
//!   under the caller's [`DeviceGrant`]; [`super::super::Viewer::audio_out_open`] opens one by id with
//!   the caller's grant (no ambient device access). Tests and the drive process use score-device's
//!   `SoftwareProvider` (`software-clock-out`: a clocked thread, no operating-system audio, nothing
//!   reaches the speakers); a host passes score-device's `CpalProvider` (feature `cpal-backend`) for
//!   the real device. Real device output is NOT_PROVEN by any test here.
//! - **Transport**: [`super::super::Viewer::audio_out_transport`] runs play, pause, toggle, scrub and
//!   loop on the program monitor with this output as its clock; audio plays only at 1x forward
//!   (`audio_out` module policy).
//! - **Host loop**: [`super::super::Viewer::audio_out_pump`] fills the device ring off the realtime
//!   path and refreshes the monitor on the audio clock; [`super::super::Viewer::audio_out_run`] pumps
//!   for a bounded time (the drive surface's form, at most [`MAX_RUN_MS`]).
//! - **State**: [`super::super::Viewer::audio_out_json`] (also `state_json()["audio_out"]`): device,
//!   provider, sequence, audible, clock, consumed frames, dropouts (`underruns`, `underrun_frames`:
//!   blocks the device played as silence because the ring ran dry), restarts, loop wraps, and meters
//!   of what was rendered for the device (score graph peaks since the output opened: master, each bus
//!   with its submix track id, each strip with its track id; linear and dBFS).
//! - **Close**: [`super::super::Viewer::audio_out_close`] stops the stream and returns score-device's
//!   close report (frames submitted, consumed, residual, underruns, loss).
//!
//! Refusals (codes): `no_document`, `unknown_sequence`, `audio_sources_missing`, `mixdown`,
//! `repair_unresolved`, `audio_config`, `audio_out_closed`, `run_too_long`, `device_lost`, and
//! score-device's own (`unauthorized`, `unknown_device`, `invalid_device_id`, `invalid_rate`,
//! `invalid_block_frames`, `admission_denied`, ...).
#![deny(clippy::arithmetic_side_effects)]

use super::super::audio_out::{AudioOut, NoTimeline};
use super::super::monitor::{MonitorFail, Transport};
use super::{build_bus_stream, fail};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::StudioAudioRouting;
use hsk_studio_pulse::TimelineBusStream;
use hsk_studio_score_device::{CloseReport, DeviceGrant, DeviceId, DeviceProvider, StreamConfig};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Longest [`super::super::Viewer::audio_out_run`] in one call.
pub const MAX_RUN_MS: u64 = 10_000;
/// Ring depth of the output stream, in blocks.
pub const RING_BLOCKS: u32 = 4;
/// Largest block a caller may ask for (score-device's default limit).
pub const MAX_BLOCK_FRAMES: u32 = 16_384;
/// Monitor side when the output opens a monitor of its own (audio needs no picture).
const MONITOR_SIDE: u32 = 16;

/// A transport command run with the audio output as the clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioTransport {
    Play,
    Pause,
    Toggle,
    Scrub(Ticks),
    Looping(bool),
}

impl AudioTransport {
    /// The token form: `play`, `pause`, `toggle`, `scrub` (with `tick`), `loop_on`, `loop_off`.
    pub fn parse(token: &str, tick: Option<u64>) -> Option<Self> {
        Some(match token {
            "play" => Self::Play,
            "pause" => Self::Pause,
            "toggle" => Self::Toggle,
            "scrub" => Self::Scrub(Ticks::new(tick?)),
            "loop_on" => Self::Looping(true),
            "loop_off" => Self::Looping(false),
            _ => return None,
        })
    }
}

/// The open program audio output.
pub struct ProgramAudio {
    out: AudioOut<NoTimeline>,
    provider: &'static str,
    device: String,
    sequence_id: String,
    pumps: u64,
}

fn db(linear: f32) -> Option<f64> {
    (linear > 0.0).then(|| 20.0 * f64::from(linear).log10())
}

fn pair(p: [f32; 2]) -> Value {
    json!({"peak": p, "dbfs": [db(p[0]), db(p[1])]})
}

impl ProgramAudio {
    fn buses(&self) -> Option<&TimelineBusStream> {
        self.out.mix().buses()
    }

    /// Structured projection (module docs, State).
    pub fn to_json(&self) -> Value {
        let mut v = self.out.state_json();
        let c = self.out.stream().counters();
        v["provider"] = json!(self.provider);
        v["device"] = json!(self.device);
        v["sequence_id"] = json!(self.sequence_id);
        v["pumps"] = json!(self.pumps);
        v["underrun_frames"] = json!(c.underrun_frames());
        v["played_sequence_sample"] = json!(self.out.played_sequence_sample());
        if let Some(b) = self.buses() {
            let m = b.meters();
            v["meters"] = json!({
                "master": pair(m.master),
                "buses": b.bus_tracks().iter().zip(&m.buses).map(|(id, p)| json!({"track_id": id, "level": pair(*p)})).collect::<Vec<_>>(),
                "strips": b.strip_tracks().iter().zip(&m.strips).map(|(id, p)| json!({"track_id": id, "level": pair(*p)})).collect::<Vec<_>>(),
            });
        }
        v
    }
}

impl super::super::Viewer {
    /// The devices `provider` lists under `grant` (score-device's refusal code otherwise).
    pub fn audio_out_devices(&self, provider: &dyn DeviceProvider, grant: &DeviceGrant) -> Result<Value, MonitorFail> {
        let list = provider.enumerate(Some(grant)).map_err(|e| fail(e.code(), provider.id()))?;
        Ok(json!(list
            .iter()
            .map(|d| json!({"id": d.id.as_str(), "name": d.name, "max_output_channels": d.max_output_channels, "min_sample_rate_hz": d.min_sample_rate_hz, "max_sample_rate_hz": d.max_sample_rate_hz}))
            .collect::<Vec<_>>()))
    }

    /// The bus stream the program monitor's sequence plays: the open audio file's own import when the
    /// monitor shows it, else the document sequence over [`super::super::Viewer::sequence_sound`].
    fn program_stream(&self, sequence_id: &str, block: usize) -> Result<TimelineBusStream, MonitorFail> {
        if let Some(a) = &self.audio
            && a.import.sequence.sequence_id == sequence_id
        {
            let audio = a.import.audio.clone();
            return build_bus_stream(&a.import.sequence, &StudioAudioRouting::new(sequence_id), audio.sample_rate, block, audio.clone(), &audio, self.cancel)
                .map_err(|e| fail("mixdown", e.code()));
        }
        let sound = self.sequence_sound()?;
        self.sound_stream(sequence_id, &sound, block)
    }

    /// Opens the program audio output on `device` of `provider` with the caller's `grant`, at the sound
    /// port's rate, `block_frames` per device block (module docs). `sequence_id`: the program monitor
    /// opens on it when it shows another sequence or none; `None` keeps the monitor's (else the
    /// document's first sequence). An output already open is closed first. The audio follows the
    /// transport at once (playing: audio from the playhead).
    pub fn audio_out_open(&mut self, provider: &dyn DeviceProvider, grant: &DeviceGrant, device: &str, block_frames: u32, sequence_id: Option<&str>) -> Result<Value, MonitorFail> {
        let device_id = DeviceId::parse(device).map_err(|e| fail(e.code(), device.to_string()))?;
        if block_frames == 0 || block_frames > MAX_BLOCK_FRAMES {
            return Err(fail("audio_config", format!("block_frames {block_frames}: 1..={MAX_BLOCK_FRAMES}")));
        }
        let shown = self.monitor.as_ref().map(|m| m.monitor.sequence().sequence_id.clone());
        let wanted = match (sequence_id, &shown) {
            (Some(s), _) => s.to_string(),
            (None, Some(s)) => s.clone(),
            (None, None) => {
                let snapshot = self.run.snapshot().ok_or_else(|| fail("no_document", "nothing is open"))?;
                snapshot.document().sequences.first().map(|s| s.sequence_id.clone()).ok_or_else(|| fail("unknown_sequence", "the document has no sequence"))?
            }
        };
        let block = usize::try_from(block_frames).map_err(|_| fail("audio_config", "block_frames"))?;
        let bus = self.program_stream(&wanted, block)?;
        if shown.as_deref() != Some(wanted.as_str()) {
            self.open_monitor(&wanted, MONITOR_SIDE, MONITOR_SIDE)?;
        }
        self.audio_out_close();
        let config = StreamConfig { sample_rate_hz: hsk_studio_pulse::AudioClock::sample_rate_hz(&bus), layout: bus.layout(), block_frames, ring_blocks: RING_BLOCKS };
        let stream = provider.open_output(&device_id, &config, Some(grant), self.cancel).map_err(|e| fail(e.code(), format!("{} {device}", provider.id())))?;
        let mut out = AudioOut::with_buses(bus, stream)?;
        if let Some(m) = &self.monitor {
            out.follow(m.monitor.scheduler())?;
        }
        let audio = ProgramAudio { out, provider: provider.id(), device: device.to_string(), sequence_id: wanted.clone(), pumps: 0 };
        let state = audio.to_json();
        self.program_audio = Some(audio);
        self.say(format!("audio out: {} {device} on {wanted}", provider.id()));
        Ok(state)
    }

    /// Runs `command` on the program monitor with the output as its clock (module docs, Transport).
    pub fn audio_out_transport(&mut self, command: AudioTransport) -> Result<Value, MonitorFail> {
        let audio = self.program_audio.as_mut().ok_or_else(|| fail("audio_out_closed", "no audio output is open"))?;
        let m = self.monitor.as_mut().ok_or_else(|| fail("audio_out_closed", "no program monitor"))?;
        let ran = audio.out.command(&mut m.monitor, |m, c| match command {
            AudioTransport::Play => m.play(c),
            AudioTransport::Pause => m.pause(c),
            AudioTransport::Toggle if m.scheduler().transport() == Transport::Playing => m.pause(c),
            AudioTransport::Toggle => m.play(c),
            AudioTransport::Scrub(t) => m.scrub(t, c),
            AudioTransport::Looping(on) => m.set_looping(on, c),
        })?;
        ran?;
        Ok(audio.to_json())
    }

    /// One host-loop step (module docs): fills the ring, then refreshes the monitor on the audio clock.
    /// Returns the blocks pushed. `device_lost` keeps the output open for its close report.
    pub fn audio_out_pump(&mut self) -> Result<u64, MonitorFail> {
        let audio = self.program_audio.as_mut().ok_or_else(|| fail("audio_out_closed", "no audio output is open"))?;
        let pushed = audio.out.pump(self.cancel)?;
        audio.pumps = audio.pumps.saturating_add(1);
        if let Some(m) = &mut self.monitor
            && let Some(sources) = &m.sources
        {
            let _ = m.monitor.refresh(&audio.out, sources.as_ref(), m.extents.as_ref(), self.cancel);
        }
        // Past the end of the active range the transport stops; the audio follows (silence).
        if let Some(m) = &self.monitor
            && m.monitor.scheduler().transport() != Transport::Playing
            && audio.out.audible()
        {
            audio.out.follow(m.monitor.scheduler())?;
        }
        Ok(pushed)
    }

    /// Pumps every 2 ms for `ms` milliseconds (at most [`MAX_RUN_MS`]), then reports the state.
    pub fn audio_out_run(&mut self, ms: u64) -> Result<Value, MonitorFail> {
        if ms > MAX_RUN_MS {
            return Err(fail("run_too_long", format!("{ms} ms, at most {MAX_RUN_MS}")));
        }
        self.program_audio.as_ref().ok_or_else(|| fail("audio_out_closed", "no audio output is open"))?;
        let end = Instant::now().checked_add(Duration::from_millis(ms)).ok_or_else(|| fail("run_too_long", "deadline"))?;
        loop {
            self.audio_out_pump()?;
            if Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(self.audio_out_json())
    }

    /// The output's state (`Null` when closed).
    pub fn audio_out_json(&self) -> Value {
        self.program_audio.as_ref().map_or(Value::Null, ProgramAudio::to_json)
    }

    /// The open output, if any.
    pub fn audio_out(&self) -> Option<&ProgramAudio> {
        self.program_audio.as_ref()
    }

    /// Stops and closes the output; its close report (`None`: none was open).
    pub fn audio_out_close(&mut self) -> Option<CloseReport> {
        let audio = self.program_audio.take()?;
        let report = audio.out.close();
        self.say(format!("audio out closed: {} frames consumed, {} underruns", report.frames_consumed, report.underruns));
        Some(report)
    }
}

/// The JSON form of a close report (no samples).
pub fn close_json(r: &CloseReport) -> Value {
    json!({
        "frames_submitted": r.frames_submitted, "frames_consumed": r.frames_consumed, "residual_frames": r.residual_frames,
        "underruns": r.underruns, "underrun_frames": r.underrun_frames, "device_lost": r.device_lost, "callbacks": r.callbacks,
        "reconciles": r.reconciles(), "clean": r.is_clean(),
    })
}
