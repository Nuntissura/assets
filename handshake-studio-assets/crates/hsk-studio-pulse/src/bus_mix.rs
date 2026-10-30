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
//! - Track automation (lane SMB-AU-MULTITRACK-2, Folio `StudioTrackAutomation`): a track with a
//!   `volume` lane gets the score fader lane `AutomationLane::new(points, Linear, -96..=12 dB)` with
//!   each point at its exact timeline sample and its linear value in dB (floored at -96, capped at
//!   +12), so the fader moves linearly in dB between points and holds outside them; a `pan` lane
//!   likewise (linear). The lane replaces the static `volume` / `pan` (an audio track at volume 0 with
//!   a volume lane plays the lane; a muted track stays silent). Without lanes the strips are built as
//!   before (`AutomationLane::constant`). Points off the sample grid are `not_frame_aligned`.
//!
//! - Clip pan envelopes (lane SMB-AU-MULTITRACK-3, Folio `StudioClipPanEnvelope`): a clip with an
//!   envelope is panned inside its strip by score's pan law for its channel count (mono equal-power,
//!   stereo balance; the strip renders at track pan 0, so the lane is the envelope itself), and the
//!   track's fader and pan then apply to the strip like any other. A panned mono clip therefore
//!   plays in the track's stereo strip (it is two channels after its pan), so the track pan acts on
//!   it as a balance; an unpanned mono clip keeps the mono strip and the equal-power track pan
//!   (Studio reading, Audition UNVERIFIED, D5). An `audio_crossfade` between clips of different
//!   strips (mono/stereo, or panned/unpanned mono) is `audio_unsupported`; [`strip_crossfade_mismatch`]
//!   names the track, the two clips and the case ([`CROSSFADE_MONO_STEREO`],
//!   [`CROSSFADE_PANNED_UNPANNED_MONO`]). Crossfading the two sides as stereo would need both in one
//!   strip, which the per-class strip split does not do (engine gap).
//! - Master volume automation (lane SMB-AU-MULTITRACK-3, Folio `StudioAudioMaster::volume_automation`):
//!   when present the master fader is `AutomationLane::new(points, Linear, -96..=12 dB)` (each point
//!   at its exact timeline sample, its linear value in dB), replacing the static master `volume`.
//!
//! Seeking ([`TimelineBusStream::seek`]) is score's `MixGraph::seek` (exact from 0; later seeks
//! exact for bus effects whose memory is under its warm-up).
#![deny(clippy::arithmetic_side_effects)]

use crate::audio::{AudioSources, StreamLimits, TimelineMixStream};
use crate::error::{PulseError, Result};
use crate::playback::AudioClock;
use crate::time::samples_from_ticks_exact;
use crate::timeline_edit::SourceExtents;
use crate::transition::AUDIO_CROSSFADE;
use hsk_studio_accord::CancellationToken;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{StudioAudioEffectInstance, StudioAudioRouting, StudioSequence, StudioTrack, StudioTrackParam, TrackKind};
use hsk_studio_score::{
    AutomationLane, BusSpec, ChannelLayout, Destination, EffectSlot, GraphMeters, Interpolation, MasterSpec, MixGraph,
    ScoreError, SeekMode, SendSpec, SendTap, StripSource, StripSpec, TRACK_VOLUME_DB,
};

/// Builds one bus or master effect from its stored instance at the mix rate (stereo in and out),
/// with its automation lanes from the instance keyframes (lane SMB-AUDITION-UI; the lanes are in
/// timeline samples of the sequence).
pub type EffectFactory<'a> = dyn FnMut(&StudioAudioEffectInstance, u32) -> Result<EffectSlot> + 'a;

fn db(linear: f64) -> f64 {
    let lo = TRACK_VOLUME_DB.0;
    if linear > 0.0 { (20.0 * linear.log10()).max(lo) } else { lo }
}

/// The fader (dB) and pan lanes of `track`: its automation lanes when the routing has them, else
/// its static values (module docs).
fn track_lanes(routing: &StudioAudioRouting, track: &StudioTrack, rate: u32) -> Result<(AutomationLane, AutomationLane)> {
    let lane = |param: StudioTrackParam, fallback: f64, map: &dyn Fn(f64) -> f64, range: (f64, f64)| -> Result<AutomationLane> {
        let Some(a) = routing.track_lane(&track.track_id, param) else {
            return Ok(AutomationLane::constant(fallback));
        };
        let mut points = Vec::with_capacity(a.points.len());
        for p in &a.points {
            points.push((samples_from_ticks_exact(Ticks::new(p.offset_ticks), rate)?, map(p.value).clamp(range.0, range.1)));
        }
        AutomationLane::new(&points, Interpolation::Linear, range).map_err(score_failure)
    };
    let volume = lane(StudioTrackParam::Volume, db(track.volume), &db, TRACK_VOLUME_DB)?;
    let pan = lane(StudioTrackParam::Pan, track.pan, &|v| v, (-1.0, 1.0))?;
    Ok((volume, pan))
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

/// [`StripCrossfadeMismatch::case`] of a crossfade between a mono and a stereo clip.
pub const CROSSFADE_MONO_STEREO: &str = "mono_stereo_crossfade";
/// [`StripCrossfadeMismatch::case`] of a crossfade between a mono clip with a clip pan envelope and
/// one without (lane SMB-AU-MULTITRACK-3).
pub const CROSSFADE_PANNED_UNPANNED_MONO: &str = "panned_unpanned_mono_crossfade";

/// An `audio_crossfade` the bus mix refuses (`audio_unsupported`): its two clips play in different
/// strips (module docs). The refusal's detail for callers ([`strip_crossfade_mismatch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripCrossfadeMismatch {
    pub track_id: String,
    /// The clip holding the crossfade as its tail.
    pub outgoing: String,
    pub incoming: String,
    /// [`CROSSFADE_MONO_STEREO`] or [`CROSSFADE_PANNED_UNPANNED_MONO`].
    pub case: &'static str,
}

/// A track [`TimelineBusStream::new`] renders (enabled, unmuted, solo honoured, volume above 0 or a
/// volume lane).
fn audible(track: &StudioTrack, solo: bool, routing: &StudioAudioRouting) -> bool {
    let automated = routing.track_lane(&track.track_id, StudioTrackParam::Volume).is_some();
    track.kind == TrackKind::Audio && track.enabled && !track.muted && (!solo || track.soloed) && (track.volume > 0.0 || automated)
}

/// Strip class of a clip: its channel count, except a mono clip with a clip pan envelope, which
/// plays in the stereo strip (module docs); `None` when the source is missing.
fn strip_class(c: &hsk_studio_folio::StudioClip, routing: &StudioAudioRouting, sources: &dyn AudioSources) -> Option<usize> {
    match sources.audio(&c.source_ref).map(<[Vec<f32>]>::len) {
        Some(1) if routing.clip_pans.iter().any(|p| p.clip_id == c.clip_id) => Some(2),
        other => other,
    }
}

fn track_crossfade_mismatch(track: &StudioTrack, routing: &StudioAudioRouting, sources: &dyn AudioSources) -> Option<StripCrossfadeMismatch> {
    for (i, clip) in track.clips.iter().enumerate() {
        if !clip.enabled || !clip.transitions.tail.as_ref().is_some_and(|t| t.transition_kind == AUDIO_CROSSFADE) {
            continue;
        }
        let Some(next) = track.clips.get(i.saturating_add(1)).filter(|n| n.enabled) else { continue };
        if strip_class(clip, routing, sources) == strip_class(next, routing, sources) {
            continue;
        }
        let channels = |c: &hsk_studio_folio::StudioClip| sources.audio(&c.source_ref).map(<[Vec<f32>]>::len);
        let case = if channels(clip) == channels(next) { CROSSFADE_PANNED_UNPANNED_MONO } else { CROSSFADE_MONO_STEREO };
        return Some(StripCrossfadeMismatch { track_id: track.track_id.clone(), outgoing: clip.clip_id.clone(), incoming: next.clip_id.clone(), case });
    }
    None
}

/// The first crossfade of `sequence` that [`TimelineBusStream::new`] refuses as `audio_unsupported`
/// because its clips play in different strips (audible tracks only, in track order); `None` when there
/// is none. Rendering such a pair would need both sides as stereo in one strip, which the strip split
/// does not do (engine gap, lane SMB-AU-MULTITRACK-3).
pub fn strip_crossfade_mismatch(sequence: &StudioSequence, routing: &StudioAudioRouting, sources: &dyn AudioSources) -> Option<StripCrossfadeMismatch> {
    let solo = sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.soloed);
    sequence.tracks.iter().filter(|t| audible(t, solo, routing)).find_map(|t| track_crossfade_mismatch(t, routing, sources))
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
            if !audible(track, solo, routing) {
                continue;
            }
            let (volume, pan) = track_lanes(routing, track, sample_rate_hz)?;
            let class_of = |c: &hsk_studio_folio::StudioClip| strip_class(c, routing, &sources);
            if track_crossfade_mismatch(track, routing, &sources).is_some() {
                return Err(PulseError::AudioUnsupported);
            }
            let (output, sends) = sends_of(&track.track_id)?;
            for class in [1usize, 2] {
                let mut only = track.clone();
                for c in &mut only.clips {
                    c.enabled = c.enabled && class_of(c) == Some(class);
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
                let stream = TimelineMixStream::with_clip_envelopes(
                    &seq,
                    sample_rate_hz,
                    max_block_frames,
                    sources.clone(),
                    extents,
                    limits,
                    &routing.clip_gains,
                    &routing.clip_pans,
                    cancel,
                )?;
                strip_tracks.push(Some(track.track_id.clone()));
                strips.push(StripSpec::source(
                    Box::new(TrackStrip { stream, channels: class }),
                    volume.clone(),
                    pan.clone(),
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
            let (volume, pan) = track_lanes(routing, track, sample_rate_hz)?;
            bus.volume_db = if track.muted { AutomationLane::constant(TRACK_VOLUME_DB.0) } else { volume };
            bus.pan = pan;
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
        let master_volume = if routing.master.volume_automation.is_empty() {
            AutomationLane::constant(db(routing.master.volume))
        } else {
            let mut points = Vec::with_capacity(routing.master.volume_automation.len());
            for p in &routing.master.volume_automation {
                points.push((samples_from_ticks_exact(Ticks::new(p.offset_ticks), sample_rate_hz)?, db(p.value).clamp(TRACK_VOLUME_DB.0, TRACK_VOLUME_DB.1)));
            }
            AutomationLane::new(&points, Interpolation::Linear, TRACK_VOLUME_DB).map_err(score_failure)?
        };
        let mut master = MasterSpec { effects: Vec::new(), volume_db: master_volume };
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
