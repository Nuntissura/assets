//! Timeline audio through submix buses, sends and the master (lane SMB-AUDITION-SESSION-2): a
//! sequence plus its Folio [`StudioAudioRouting`] becomes a score [`MixGraph`] whose strips are the
//! timeline's audio tracks, so playback (`TimelineBusStream` is an [`AudioClock`] and renders blocks
//! like [`TimelineMixStream`]) and export hear the buses.
//!
//! - Strips: each audible audio track (enabled, unmuted, solo honoured over the whole sequence) is
//!   rendered at unity by a [`TimelineMixStream`] of that track alone (fades, crossfades, speeds,
//!   clip gain envelopes and grouping as there). Mono and stereo clips of one track are two strips
//!   (score's pan laws differ: mono equal-power, stereo balance) sharing the track's fader
//!   (`20 log10(volume)` dB, floored at -96), pan, output and sends; mixing is linear, so the sum is
//!   the track. An `audio_crossfade` between a mono and a stereo clip is `audio_unsupported`.
//! - Buses: every `Submix` track of the sequence is a bus; its fader and pan are the track's
//!   `volume` and `pan` (a muted submix track gets the -96 dB floor), its effects the routing's
//!   `bus_effects`, its output and sends its route. The master volume and effects come from the
//!   routing. Effect instances are built by the caller's factory (pulse knows no processor kinds).
//! - Sends: linear `level` -> dB (floored at -96), `pre_fader`, `pan`; routes are Folio-validated
//!   (call `validate_audio_routing` first); unknown ids here are `invalid_input`.
//!
//! Seeking ([`TimelineBusStream::seek`]) is score's `MixGraph::seek` (exact from 0; later seeks
//! exact for bus effects whose memory is under its warm-up).
#![deny(clippy::arithmetic_side_effects)]

use crate::audio::{AudioSources, StreamLimits, TimelineMixStream};
use crate::error::{PulseError, Result};
use crate::playback::AudioClock;
use crate::timeline_edit::SourceExtents;
use crate::transition::AUDIO_CROSSFADE;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioAudioEffectInstance, StudioAudioRouting, StudioSequence, TrackKind};
use hsk_studio_score::{
    AutomationLane, BusSpec, ChannelLayout, Destination, EffectSlot, GraphMeters, MasterSpec, MixGraph, ScoreError,
    SeekMode, SendSpec, SendTap, StripSource, StripSpec, TRACK_VOLUME_DB,
};

/// Builds one bus or master effect from its stored instance at the mix rate (stereo in and out),
/// with its automation lanes from the instance keyframes (lane SMB-AUDITION-UI; the lanes are in
/// timeline samples of the sequence).
pub type EffectFactory<'a> = dyn FnMut(&StudioAudioEffectInstance, u32) -> Result<EffectSlot> + 'a;

fn db(linear: f64) -> f64 {
    let lo = TRACK_VOLUME_DB.0;
    if linear > 0.0 { (20.0 * linear.log10()).max(lo) } else { lo }
}

fn score_failure(e: ScoreError) -> PulseError {
    match e {
        ScoreError::Canceled => PulseError::Canceled,
        ScoreError::CapacityExceeded => PulseError::LimitExceeded,
        ScoreError::InvalidRate => PulseError::UnsupportedSampleRate,
        ScoreError::OutOfRange | ScoreError::ChannelMismatch => PulseError::InvalidInput,
        _ => PulseError::AudioRejected,
    }
}

fn pulse_failure(e: PulseError) -> ScoreError {
    match e {
        PulseError::Canceled => ScoreError::Canceled,
        PulseError::LimitExceeded => ScoreError::CapacityExceeded,
        _ => ScoreError::OutOfRange,
    }
}

/// One timeline track (one channel class) at unity as a score strip source.
struct TrackStrip<S: AudioSources> {
    stream: TimelineMixStream<S>,
    channels: usize,
}

impl<S: AudioSources + Send> StripSource for TrackStrip<S> {
    fn channels(&self) -> usize {
        self.channels
    }
    fn length_samples(&self) -> u64 {
        self.stream.length_samples()
    }
    fn position(&self) -> u64 {
        self.stream.position()
    }
    fn seek(&mut self, sample: u64, _cancel: &CancellationToken) -> std::result::Result<(), ScoreError> {
        self.stream.seek(sample);
        Ok(())
    }
    fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> std::result::Result<(), ScoreError> {
        self.stream.render(left, right, cancel).map_err(pulse_failure)
    }
}

/// A silent strip (a sequence with nothing audible still clocks playback).
struct Silence(u64);
impl StripSource for Silence {
    fn channels(&self) -> usize {
        2
    }
    fn length_samples(&self) -> u64 {
        0
    }
    fn position(&self) -> u64 {
        self.0
    }
    fn seek(&mut self, sample: u64, _: &CancellationToken) -> std::result::Result<(), ScoreError> {
        self.0 = sample;
        Ok(())
    }
    fn render(&mut self, left: &mut [f32], right: &mut [f32], _: &CancellationToken) -> std::result::Result<(), ScoreError> {
        left.fill(0.0);
        right.fill(0.0);
        self.0 = self.0.saturating_add(u64::try_from(left.len()).unwrap_or(u64::MAX));
        Ok(())
    }
}

/// The timeline mixed through its buses (module docs); also the playback [`AudioClock`].
pub struct TimelineBusStream {
    graph: MixGraph,
    rate: u32,
    strip_tracks: Vec<Option<String>>,
    bus_tracks: Vec<String>,
}

impl TimelineBusStream {
    /// Refusals: as [`TimelineMixStream::new`] per track; `invalid_input` for a routing of another
    /// sequence, unknown track ids or an invalid graph; `audio_unsupported` for a mono/stereo
    /// crossfade; the factory's error for an effect; score's graph refusals mapped (`limit_exceeded`
    /// for more than its strip, bus or latency bounds).
    #[allow(clippy::too_many_arguments)]
    pub fn new<S: AudioSources + Clone + Send + 'static>(
        sequence: &StudioSequence,
        routing: &StudioAudioRouting,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        effects: &mut EffectFactory<'_>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        cancel.check()?;
        if routing.sequence_id != sequence.sequence_id {
            return Err(PulseError::InvalidInput);
        }
        let buses: Vec<usize> = (0..sequence.tracks.len())
            .filter(|&t| sequence.tracks[t].kind == TrackKind::Submix)
            .collect();
        let bus_index = |id: &str| buses.iter().position(|&t| sequence.tracks[t].track_id == id);
        let destination = |output: &Option<String>| -> Result<Destination> {
            match output {
                None => Ok(Destination::Master),
                Some(id) => bus_index(id).map(Destination::Bus).ok_or(PulseError::InvalidInput),
            }
        };
        let route_of = |track_id: &str| routing.routes.iter().find(|r| r.track_id == track_id);
        let sends_of = |track_id: &str| -> Result<(Destination, Vec<SendSpec>)> {
            let Some(route) = route_of(track_id) else {
                return Ok((Destination::Master, Vec::new()));
            };
            let mut sends = Vec::with_capacity(route.sends.len());
            for s in &route.sends {
                sends.push(SendSpec {
                    bus: bus_index(&s.target).ok_or(PulseError::InvalidInput)?,
                    tap: if s.pre_fader { SendTap::PreFader } else { SendTap::PostFader },
                    level_db: AutomationLane::constant(db(s.level)),
                    pan: s.pan,
                });
            }
            Ok((destination(&route.output)?, sends))
        };
        // Audible audio tracks over the whole sequence (solo is sequence-wide).
        let solo = sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.soloed);
        let mut strips = Vec::new();
        let mut strip_tracks = Vec::new();
        for track in &sequence.tracks {
            cancel.check()?;
            if track.kind != TrackKind::Audio || !track.enabled || track.muted || (solo && !track.soloed) || track.volume <= 0.0 {
                continue;
            }
            let channels_of = |c: &hsk_studio_folio::StudioClip| sources.audio(&c.source_ref).map(<[Vec<f32>]>::len);
            for (i, clip) in track.clips.iter().enumerate() {
                if clip.enabled
                    && clip.transitions.tail.as_ref().is_some_and(|t| t.transition_kind == AUDIO_CROSSFADE)
                    && track.clips.get(i.saturating_add(1)).is_some_and(|n| n.enabled && channels_of(n) != channels_of(clip))
                {
                    return Err(PulseError::AudioUnsupported);
                }
            }
            let (output, sends) = sends_of(&track.track_id)?;
            for class in [1usize, 2] {
                let mut only = track.clone();
                for c in &mut only.clips {
                    c.enabled = c.enabled && channels_of(c) == Some(class);
                }
                if !only.clips.iter().any(|c| c.enabled) {
                    continue;
                }
                only.volume = 1.0;
                only.pan = if class == 1 { -1.0 } else { 0.0 };
                only.muted = false;
                only.soloed = false;
                let mut seq = sequence.clone();
                seq.tracks = vec![only];
                let stream = TimelineMixStream::with_clip_gains(
                    &seq,
                    sample_rate_hz,
                    max_block_frames,
                    sources.clone(),
                    extents,
                    limits,
                    &routing.clip_gains,
                    cancel,
                )?;
                strip_tracks.push(Some(track.track_id.clone()));
                strips.push(StripSpec::source(
                    Box::new(TrackStrip { stream, channels: class }),
                    AutomationLane::constant(db(track.volume)),
                    AutomationLane::constant(track.pan),
                    output,
                    sends.clone(),
                ));
            }
        }
        if strips.is_empty() {
            strip_tracks.push(None);
            strips.push(StripSpec::source(
                Box::new(Silence(0)),
                AutomationLane::constant(0.0),
                AutomationLane::constant(0.0),
                Destination::Master,
                Vec::new(),
            ));
        }
        let mut bus_specs = Vec::with_capacity(buses.len());
        for &t in &buses {
            let track = &sequence.tracks[t];
            let mut bus = BusSpec::new(track.name.clone());
            bus.volume_db = AutomationLane::constant(if track.muted { TRACK_VOLUME_DB.0 } else { db(track.volume) });
            bus.pan = AutomationLane::constant(track.pan);
            let (output, sends) = sends_of(&track.track_id)?;
            bus.output = output;
            bus.sends = sends;
            if let Some(fx) = routing.bus_effects.iter().find(|b| b.track_id == track.track_id) {
                for e in fx.effects.iter().filter(|e| e.enabled) {
                    bus.effects.push(effects(e, sample_rate_hz)?);
                }
            }
            bus_specs.push(bus);
        }
        let mut master = MasterSpec { effects: Vec::new(), volume_db: AutomationLane::constant(db(routing.master.volume)) };
        for e in routing.master.effects.iter().filter(|e| e.enabled) {
            master.effects.push(effects(e, sample_rate_hz)?);
        }
        let graph = MixGraph::new(sample_rate_hz, max_block_frames, strips, bus_specs, master).map_err(score_failure)?;
        let bus_tracks = buses.iter().map(|&t| sequence.tracks[t].track_id.clone()).collect();
        Ok(Self { graph, rate: sample_rate_hz, strip_tracks, bus_tracks })
    }

    pub fn length_samples(&self) -> u64 {
        self.graph.length_samples()
    }
    pub fn position(&self) -> u64 {
        self.graph.position()
    }
    pub fn max_block_frames(&self) -> usize {
        self.graph.max_block_frames()
    }
    /// Latency the graph compensates (score's report).
    pub fn latency(&self) -> &hsk_studio_score::GraphLatency {
        self.graph.latency()
    }
    /// Output layout (stereo), for the device stream configuration.
    pub fn layout(&self) -> ChannelLayout {
        ChannelLayout::stereo()
    }
    /// Seek behaviour (score [`SeekMode`]; `Exact` by default: effect tails of any length match the
    /// continuous render, a backward seek re-renders from the start).
    pub fn set_seek_mode(&mut self, mode: SeekMode) {
        self.graph.set_seek_mode(mode);
    }
    /// Strip, bus and master meters since the last reset (score [`GraphMeters`]); strips are in
    /// build order (audible audio tracks, mono strip before stereo strip), buses in submix order.
    pub fn meters(&self) -> GraphMeters {
        self.graph.meters()
    }
    /// Track id of each strip of [`TimelineBusStream::meters`] (`None`: the silent strip).
    pub fn strip_tracks(&self) -> &[Option<String>] {
        &self.strip_tracks
    }
    /// Submix track id of each bus of [`TimelineBusStream::meters`].
    pub fn bus_tracks(&self) -> &[String] {
        &self.bus_tracks
    }
    pub fn reset_meters(&mut self) {
        self.graph.reset_meters();
    }

    /// Moves to `sample` (score `MixGraph::seek`).
    pub fn seek(&mut self, sample: u64, cancel: &CancellationToken) -> Result<()> {
        self.graph.seek(sample, cancel).map_err(score_failure)
    }
    /// Renders the next `left.len()` frames (at most `max_block_frames`) and advances.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<()> {
        let at = self.graph.position();
        self.graph.render(at, left, right, cancel).map_err(score_failure)
    }
}

impl AudioClock for TimelineBusStream {
    fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
    fn sample_position(&self) -> u64 {
        self.graph.position()
    }
}
