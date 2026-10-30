//! Multitrack audio in the timeline panel (lane SMB-AU-MULTITRACK-1, Audition parity row AU-14;
//! feature `audio-batch`). The timeline already draws audio and submix tracks with their clips and
//! moves and trims them through pulse (module `timeline`); this module adds, in the same panel:
//!
//! - **Waveform overviews.** [`super::super::Viewer::open_multitrack`] binds an audio source port
//!   (pulse [`AudioSources`], the port the mixer plays) at a sample rate; each audio clip block then
//!   shows the min / max overview of its source window, from the waveform editor's peak path
//!   ([`super::super::waveform::peaks_of`] over score's `WaveBuffer`), computed once per source at
//!   [`PEAK_MIN_SAMPLES_PER_COLUMN`] samples or more per column (at most [`MAX_PEAK_COLUMNS`] columns,
//!   [`MAX_PEAK_SOURCES`] sources, sources up to [`MAX_PEAK_SOURCE_FRAMES`] frames) and folded per
//!   drawn column. The toolbar button **Waveforms** hides and shows them.
//! - **Automation lanes.** The small **A** button in an audio or submix track header (or
//!   [`AutomationCommand::CycleLane`]) opens one lane row under the track and cycles what it shows
//!   ([`LaneTarget`]): on an audio track the **clip volume** envelopes of its clips (Folio
//!   `StudioClipGainEnvelope`, dB, offsets from the clip start, linear in dB between points and held
//!   outside them); on a submix track the keyframes of one automatable parameter of a bus effect
//!   (Folio effect-instance keyframes, offsets from the sequence start; the parameters score
//!   automates, see `audio_batch::EffectSpec::automation_index`); on both the track's **volume**
//!   and **pan**, which the document stores as single values only, so those lanes draw a flat line
//!   and refuse point edits with `track_automation_not_stored` (engine gap, lane notes).
//!   In an editable lane a click adds a point at the clicked frame and value, a point drags to a
//!   new frame and value, and a right click deletes it. Every point edit is a typed
//!   [`AutomationCommand`] committed as ONE entry-only history step of the routing entry
//!   (`audio_session::AUDIO_ROUTING_ENTRY`, the path the mixer's routing edits use): EDIT > Undo
//!   undoes it, and the routing is saved with the package.
//! - **Clip gain.** With an audio clip selected, **Clip Gain -1 dB / +1 dB** add the step to every
//!   point of the clip's envelope (a clip without one gets a one-point envelope, a constant gain);
//!   refused when a point would leave the clip gain range.
//! - **One mixdown.** Playback and mixdown of what is shown are the mixer's ([`super::super::mixer`]):
//!   `open_multitrack` opens the mixer on the timeline's sequence with the same source port, and
//!   after every timeline edit, lane edit, undo or redo (document revision or routing entry changed)
//!   the mixer's pulse bus stream is rebuilt at its play position. `Viewer::mixer_render` renders it.
//!
//! Hostile input: values must be finite and in range, typed points must name an existing clip /
//! effect / parameter, bus keyframes must land on whole samples at the bound rate
//! (`not_sample_aligned`), envelopes and keyframe lists are bounded by Folio's validation (refused
//! before anything is stored), and drawing is bounded ([`MAX_DRAWN_POINTS`] per lane, peak columns
//! per frame [`MAX_PEAK_COLUMNS_PER_FRAME`]). Audition's own point shapes, lane colours, default
//! values and shortcuts are UNVERIFIED (D5); the choices here are Studio's.

use super::super::Viewer;
use super::super::audio_batch::EffectSpec;
use super::super::audio_session::{AUDIO_ROUTING_ENTRY, bus_effect};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::{
    self as folio, CLIP_GAIN_DB, StudioAudioPresetKeyframe, StudioAudioRouting, StudioClipGainEnvelope, StudioClipGainPoint, StudioSequence, TrackKind,
};
use hsk_studio_pulse::{AudioSources, SourceExtents, samples_from_ticks_exact};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Sample rate the lanes check bus keyframes at when no source port is bound.
pub const DEFAULT_RATE: u32 = 48_000;
/// Clip gain button step (dB).
pub const CLIP_GAIN_STEP_DB: f64 = 1.0;
/// Points drawn per lane and frame.
pub const MAX_DRAWN_POINTS: usize = 4096;
/// Sources whose overview is cached.
pub const MAX_PEAK_SOURCES: usize = 256;
/// Overview columns per source (the waveform editor's peak bound).
pub const MAX_PEAK_COLUMNS: usize = 65_536;
/// Smallest overview column in samples.
pub const PEAK_MIN_SAMPLES_PER_COLUMN: u64 = 256;
/// Longest source given an overview (frames per channel; about 46 minutes at 48 kHz).
pub const MAX_PEAK_SOURCE_FRAMES: u64 = 1 << 27;
/// Waveform columns drawn per frame over all clips.
pub const MAX_PEAK_COLUMNS_PER_FRAME: usize = 16_384;

/// What an automation lane shows.
#[derive(Clone, Debug, PartialEq)]
pub enum LaneTarget {
    /// The clip gain envelopes of the audio track's clips (dB).
    ClipVolume,
    /// The track fader (one stored value; drawn in dB).
    TrackVolume,
    /// The track pan (one stored value, -1 ..= 1).
    TrackPan,
    /// One automatable parameter of an effect of the submix track's bus.
    BusParam { instance_id: String, key: String },
}

impl LaneTarget {
    /// Stable key (`clip_volume`, `track_volume`, `track_pan`, `bus.<instance>.<param>`).
    pub fn key(&self) -> String {
        match self {
            Self::ClipVolume => "clip_volume".into(),
            Self::TrackVolume => "track_volume".into(),
            Self::TrackPan => "track_pan".into(),
            Self::BusParam { instance_id, key } => format!("bus.{instance_id}.{key}"),
        }
    }
    /// Header label.
    pub fn label(&self) -> String {
        match self {
            Self::ClipVolume => "Clip Volume".into(),
            Self::TrackVolume => "Volume".into(),
            Self::TrackPan => "Pan".into(),
            Self::BusParam { instance_id, key } => format!("{instance_id} {key}"),
        }
    }
    /// Points can be edited (the document stores time-varying values for it).
    pub fn editable(&self) -> bool {
        matches!(self, Self::ClipVolume | Self::BusParam { .. })
    }
}

/// A point of a lane: a clip envelope point (`clip_id`, offset from the clip start) or a bus
/// keyframe (`clip_id` `None`, offset from the sequence start).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PointId {
    pub clip_id: Option<String>,
    pub offset_ticks: u64,
}

/// One point as drawn: its id, timeline tick and value.
#[derive(Clone, Debug, PartialEq)]
pub struct LanePoint {
    pub id: PointId,
    pub at: Ticks,
    pub value: f64,
}

/// One continuous stretch of a lane: a clip span (clip volume) or the whole sequence. `points` are
/// the stretch's points in time order; with none the stretch holds `rest`.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneSpan {
    pub clip_id: Option<String>,
    pub start: Ticks,
    pub end: Ticks,
    pub rest: f64,
    pub points: Vec<LanePoint>,
}

/// A lane as the panel draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneView {
    pub track_id: String,
    pub target: LaneTarget,
    /// Value range of the lane's vertical axis.
    pub range: (f64, f64),
    pub unit: &'static str,
    pub spans: Vec<LaneSpan>,
    /// Why the lane cannot be shown or edited (e.g. an invalid routing entry).
    pub error: Option<String>,
}

impl LaneView {
    /// Every point of the lane in time order.
    pub fn points(&self) -> impl Iterator<Item = &LanePoint> {
        self.spans.iter().flat_map(|s| s.points.iter())
    }
}

/// Value at `t` of `points` (`(tick, value)` sorted): linear between points, held outside them.
pub fn value_at(points: &[(u64, f64)], t: u64, rest: f64) -> f64 {
    let Some(first) = points.first() else { return rest };
    if t <= first.0 {
        return first.1;
    }
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let span = b.0.saturating_sub(a.0).max(1) as f64;
            let k = t.saturating_sub(a.0) as f64 / span;
            return a.1 + (b.1 - a.1) * k;
        }
    }
    points.last().map_or(rest, |p| p.1)
}

/// A typed multitrack action (buttons, header toggles, lane clicks and drags run exactly these).
#[derive(Clone, Debug, PartialEq)]
pub enum AutomationCommand {
    /// Shows `lane` under the track (`None` hides it); a view setting, no history step.
    ShowLane { track_id: String, lane: Option<LaneTarget> },
    /// Shows the track's next lane choice ([`Viewer::timeline_lane_choices`]), after the last none.
    CycleLane { track_id: String },
    /// Adds a point at timeline tick `at` with `value` (replacing a point at the same tick).
    AddPoint { track_id: String, lane: LaneTarget, at: Ticks, value: f64 },
    /// Moves a point to timeline tick `at` and `value`.
    MovePoint { track_id: String, lane: LaneTarget, point: PointId, at: Ticks, value: f64 },
    DeletePoint { track_id: String, lane: LaneTarget, point: PointId },
    /// Adds `delta_db` to every point of the clip's gain envelope (or creates a constant one).
    ClipGain { clip_id: String, delta_db: f64 },
    /// Waveform overviews on / off (a view setting).
    ToggleWaveforms,
}

impl AutomationCommand {
    pub fn key(&self) -> &'static str {
        match self {
            Self::ShowLane { .. } => "lane_show",
            Self::CycleLane { .. } => "lane_cycle",
            Self::AddPoint { .. } => "lane_point_add",
            Self::MovePoint { .. } => "lane_point_move",
            Self::DeletePoint { .. } => "lane_point_delete",
            Self::ClipGain { .. } => "clip_gain",
            Self::ToggleWaveforms => "waveforms",
        }
    }
}

/// The min / max overview of one source (per channel, columns of equal sample spans).
struct Overview {
    frames: u64,
    columns: Vec<Vec<(f32, f32)>>,
}

impl Overview {
    fn column_count(&self) -> u64 {
        self.columns.first().map_or(0, |c| c.len() as u64)
    }
    /// The column holding sample `s` (columns split `0..frames` as `peaks_of` does).
    fn column_of(&self, s: u64) -> usize {
        let n = self.column_count();
        let c = u128::from(s.min(self.frames.saturating_sub(1))).saturating_mul(u128::from(n)).checked_div(u128::from(self.frames.max(1))).unwrap_or(0);
        usize::try_from(c).unwrap_or(usize::MAX).min(usize::try_from(n.saturating_sub(1)).unwrap_or(0))
    }
    /// `(min, max)` over every channel of samples `a .. b`.
    fn fold(&self, a: u64, b: u64) -> (f32, f32) {
        if self.frames == 0 || a >= self.frames || b <= a {
            return (0.0, 0.0);
        }
        let (c0, c1) = (self.column_of(a), self.column_of(b.saturating_sub(1)));
        let mut out = (f32::INFINITY, f32::NEG_INFINITY);
        for ch in &self.columns {
            for &(lo, hi) in ch.get(c0..=c1).unwrap_or(&[]) {
                out = (out.0.min(lo), out.1.max(hi));
            }
        }
        if out.0 > out.1 { (0.0, 0.0) } else { out }
    }
}

/// The bound source port.
struct Binding {
    rate: u32,
    sources: Box<dyn AudioSources + Send>,
}

/// Multitrack state of the timeline panel (inside `TimelineState`).
pub struct AudioLanes {
    pub(in crate::view) lanes: BTreeMap<String, LaneTarget>,
    pub waveforms: bool,
    binding: Option<Binding>,
    peaks: BTreeMap<String, Result<Overview, String>>,
    /// (document revision, routing entry hash) the mixer was last rebuilt for.
    followed: Option<(u64, u64)>,
}

impl Default for AudioLanes {
    fn default() -> Self {
        Self { lanes: BTreeMap::new(), waveforms: true, binding: None, peaks: BTreeMap::new(), followed: None }
    }
}

impl AudioLanes {
    /// The lane shown under `track_id`.
    pub fn lane(&self, track_id: &str) -> Option<&LaneTarget> {
        self.lanes.get(track_id)
    }
    /// Track ids with a lane, in id order.
    pub fn lane_tracks(&self) -> Vec<String> {
        self.lanes.keys().cloned().collect()
    }
    /// The bound sample rate (`None`: no source port bound).
    pub fn rate(&self) -> Option<u32> {
        self.binding.as_ref().map(|b| b.rate)
    }

    /// `(min, max)` per column over every channel of source samples `a .. b` of `source`, split into
    /// `columns` equal spans (the overview computed on first use).
    pub fn peaks(&mut self, source: &folio::ClipSource, a: u64, b: u64, columns: usize) -> Result<Vec<(f32, f32)>, String> {
        let binding = self.binding.as_ref().ok_or("no_audio_sources")?;
        if columns == 0 || columns > MAX_PEAK_COLUMNS || b <= a {
            return Err("waveform_out_of_range".into());
        }
        let key = serde_json::to_string(source).map_err(|e| e.to_string())?;
        if !self.peaks.contains_key(&key) {
            if self.peaks.len() >= MAX_PEAK_SOURCES {
                return Err("waveform_cache_full".into());
            }
            let built = overview(binding, source);
            self.peaks.insert(key.clone(), built);
        }
        let o = match self.peaks.get(&key) {
            Some(Ok(o)) => o,
            Some(Err(e)) => return Err(e.clone()),
            None => return Err("waveform_missing".into()),
        };
        let span = b.saturating_sub(a);
        let n = columns as u64;
        Ok((0..n)
            .map(|j| {
                let x = a.saturating_add(u64::try_from(u128::from(span).saturating_mul(u128::from(j)).checked_div(u128::from(n)).unwrap_or(0)).unwrap_or(u64::MAX));
                let y = a.saturating_add(u64::try_from(u128::from(span).saturating_mul(u128::from(j.saturating_add(1))).checked_div(u128::from(n)).unwrap_or(0)).unwrap_or(u64::MAX));
                o.fold(x, y.max(x.saturating_add(1)))
            })
            .collect())
    }
}

fn overview(binding: &Binding, source: &folio::ClipSource) -> Result<Overview, String> {
    let channels = binding.sources.audio(source).ok_or("waveform_source_missing")?;
    let frames = channels.first().map_or(0, Vec::len) as u64;
    if channels.is_empty() || frames == 0 {
        return Ok(Overview { frames: 0, columns: Vec::new() });
    }
    if frames > MAX_PEAK_SOURCE_FRAMES {
        return Err("waveform_source_too_long".into());
    }
    let per = PEAK_MIN_SAMPLES_PER_COLUMN.max(frames.div_ceil(MAX_PEAK_COLUMNS as u64));
    let columns = usize::try_from(frames.div_ceil(per)).unwrap_or(MAX_PEAK_COLUMNS).clamp(1, MAX_PEAK_COLUMNS);
    let buffer = hsk_studio_score::WaveBuffer::new(binding.rate, channels.to_vec()).map_err(|e| format!("waveform_source_rejected {e:?}"))?;
    let columns = super::super::waveform::peaks_of(&buffer, 0, frames, columns)?;
    Ok(Overview { frames, columns })
}

/// Waveform columns of `clip`'s source window part `from .. to` (fractions 0..=1 of the window).
pub(in crate::view) fn clip_peaks(audio: &mut AudioLanes, clip: &folio::StudioClip, from: f64, to: f64, columns: usize) -> Result<Vec<(f32, f32)>, String> {
    let rate = audio.rate().ok_or("no_audio_sources")?;
    let (si, so) = (sample_of(clip.source_in, rate), sample_of(clip.source_out, rate));
    let len = so.saturating_sub(si) as f64;
    let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
    let a = si.saturating_add((len * from) as u64);
    let b = si.saturating_add((len * to) as u64).min(so);
    audio.peaks(&clip.source_ref, a, b, columns)
}

/// Sample index (floor) of `t` at `rate`.
pub fn sample_of(t: Ticks, rate: u32) -> u64 {
    let v = u128::from(t.value()).saturating_mul(u128::from(rate)).checked_div(u128::from(TICKS_PER_SECOND)).unwrap_or(0);
    u64::try_from(v).unwrap_or(u64::MAX)
}

fn db(linear: f64) -> f64 {
    if linear > 0.0 { (20.0 * linear.log10()).max(CLIP_GAIN_DB.0) } else { CLIP_GAIN_DB.0 }
}

fn checked_value(value: f64, (lo, hi): (f64, f64)) -> Result<f64, String> {
    if !value.is_finite() {
        return Err("value_not_finite".into());
    }
    if value < lo || value > hi {
        return Err(format!("value_out_of_range {value} outside {lo}..={hi}"));
    }
    Ok(value)
}

/// Range, current static value and score automation index of a bus parameter.
fn bus_param(instance: &folio::StudioAudioEffectInstance, key: &str, rate: u32) -> Result<((f64, f64), f64), String> {
    let mut stat = instance.clone();
    stat.keyframes.clear();
    let spec = EffectSpec::from_preset_instance(&stat).map_err(|_| "effect_unsupported".to_owned())?.ok_or("effect_unsupported")?;
    let index = spec.automation_index(key).ok_or("not_automatable")?;
    let processor = spec.build_automatable(rate, 2).map_err(|e| format!("effect_refused {e:?}"))?;
    let p = processor.automation_parameters().into_iter().nth(index).ok_or("not_automatable")?;
    let value = instance.params.iter().find(|p| p.key == key).map(|p| p.value).ok_or("unknown_parameter")?;
    Ok(((p.min, p.max), value))
}

impl Viewer {
    /// Binds the timeline to an audio source port (opening the timeline when it is not open): the
    /// waveform overviews read `sources`, and the mixer opens on the timeline's sequence with the
    /// same port at `sample_rate_hz` in blocks of `max_block_frames` (its bus stream is the one
    /// playback and mixdown path; module docs).
    pub fn open_multitrack<S: AudioSources + Clone + Send + 'static>(
        &mut self,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: Arc<dyn SourceExtents>,
    ) -> Result<(), String> {
        if self.timeline.is_none() {
            self.open_timeline()?;
        }
        let sequence_id = self.timeline.as_ref().map(|t| t.sequence_id.clone()).ok_or("no_timeline")?;
        self.open_mixer(&sequence_id, sample_rate_hz, max_block_frames, sources.clone(), extents)?;
        let t = self.timeline.as_mut().ok_or("no_timeline")?;
        t.audio.binding = Some(Binding { rate: sample_rate_hz, sources: Box::new(sources) });
        t.audio.peaks.clear();
        t.audio.followed = None;
        self.timeline_audio_follow();
        self.say(format!("multitrack: sequence {sequence_id} bound at {sample_rate_hz} Hz (mixer open)"));
        Ok(())
    }

    /// Runs one multitrack action; refusals are typed codes (also on the status line) and store
    /// nothing.
    pub fn multitrack_command(&mut self, command: AutomationCommand) -> Result<(), String> {
        let result = self.multitrack_inner(&command);
        if let Err(e) = &result {
            self.say(format!("timeline {}: {e}", command.key()));
        }
        result
    }

    /// The lanes a track offers: audio tracks clip volume, volume, pan; submix tracks volume, pan
    /// and every automatable parameter of its bus effects.
    pub fn timeline_lane_choices(&self, track_id: &str) -> Vec<LaneTarget> {
        let Some(seq) = self.timeline_sequence() else { return Vec::new() };
        let Some(track) = seq.tracks.iter().find(|t| t.track_id == track_id) else { return Vec::new() };
        match track.kind {
            TrackKind::Audio => vec![LaneTarget::ClipVolume, LaneTarget::TrackVolume, LaneTarget::TrackPan],
            TrackKind::Submix => {
                let mut out = vec![LaneTarget::TrackVolume, LaneTarget::TrackPan];
                let rate = self.lanes_rate();
                if let Ok(routing) = self.timeline_routing() {
                    for e in routing.bus_effects.iter().filter(|b| b.track_id == track_id).flat_map(|b| b.effects.iter()) {
                        for p in &e.params {
                            if bus_param(e, &p.key, rate).is_ok() {
                                out.push(LaneTarget::BusParam { instance_id: e.instance_id.clone(), key: p.key.clone() });
                            }
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// The lane `target` of `track_id` as drawn (`None`: no such audio or submix track).
    pub fn timeline_lane(&self, track_id: &str, target: &LaneTarget) -> Option<LaneView> {
        let seq = self.timeline_sequence()?;
        let track = seq.tracks.iter().find(|t| t.track_id == track_id && matches!(t.kind, TrackKind::Audio | TrackKind::Submix))?;
        let whole = |rest: f64, points: Vec<LanePoint>| vec![LaneSpan { clip_id: None, start: Ticks::new(0), end: seq.duration, rest, points }];
        let mut view = LaneView { track_id: track_id.to_owned(), target: target.clone(), range: CLIP_GAIN_DB, unit: "dB", spans: Vec::new(), error: None };
        match target {
            LaneTarget::TrackVolume => view.spans = whole(db(track.volume), Vec::new()),
            LaneTarget::TrackPan => {
                view.range = (-1.0, 1.0);
                view.unit = "pan";
                view.spans = whole(track.pan, Vec::new());
            }
            LaneTarget::ClipVolume => {
                let routing = self.timeline_routing();
                if let Err(e) = &routing {
                    view.error = Some(e.clone());
                }
                let gains = routing.map(|r| r.clip_gains).unwrap_or_default();
                for c in track.clips.iter().take(super::MAX_SCANNED_CLIPS) {
                    let Some(end) = c.timeline_end() else { continue };
                    let points = gains
                        .iter()
                        .find(|g| g.clip_id == c.clip_id)
                        .map(|g| {
                            g.points
                                .iter()
                                .map(|p| LanePoint { id: PointId { clip_id: Some(c.clip_id.clone()), offset_ticks: p.offset_ticks }, at: Ticks::new(c.timeline_start.value().saturating_add(p.offset_ticks)), value: p.gain_db })
                                .collect()
                        })
                        .unwrap_or_default();
                    view.spans.push(LaneSpan { clip_id: Some(c.clip_id.clone()), start: c.timeline_start, end, rest: 0.0, points });
                }
            }
            LaneTarget::BusParam { instance_id, key } => {
                view.unit = "";
                let found = self.timeline_routing().and_then(|r| {
                    r.bus_effects.iter().filter(|b| b.track_id == track_id).flat_map(|b| b.effects.iter()).find(|e| &e.instance_id == instance_id).cloned().ok_or_else(|| "unknown_effect".to_owned())
                });
                match found.and_then(|e| bus_param(&e, key, self.lanes_rate()).map(|r| (e, r))) {
                    Ok((e, (range, value))) => {
                        view.range = range;
                        let points = e
                            .keyframes
                            .iter()
                            .filter(|k| &k.key == key)
                            .map(|k| LanePoint { id: PointId { clip_id: None, offset_ticks: k.offset_ticks }, at: Ticks::new(k.offset_ticks), value: k.value })
                            .collect();
                        view.spans = whole(value, points);
                    }
                    Err(e) => view.error = Some(e),
                }
            }
        }
        Some(view)
    }

    /// Waveform columns of clip `clip_id`'s source window (or of its part `from .. to` in 0..=1).
    pub fn timeline_clip_peaks(&mut self, clip_id: &str, columns: usize) -> Result<Vec<(f32, f32)>, String> {
        self.timeline_clip_peaks_part(clip_id, 0.0, 1.0, columns)
    }

    pub(in crate::view) fn timeline_clip_peaks_part(&mut self, clip_id: &str, from: f64, to: f64, columns: usize) -> Result<Vec<(f32, f32)>, String> {
        let clip = self.timeline_sequence().and_then(|s| s.tracks.iter().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| t.clips.iter()).find(|c| c.clip_id == clip_id)).cloned().ok_or("unknown_audio_clip")?;
        let t = self.timeline.as_mut().ok_or("no_timeline")?;
        clip_peaks(&mut t.audio, &clip, from, to, columns)
    }

    /// Rebuilds the mixer's bus stream when the document revision or the routing entry changed
    /// since the last rebuild (an edit, undo or redo anywhere), so what plays is what is shown.
    pub(in crate::view) fn timeline_audio_follow(&mut self) {
        use std::hash::{Hash, Hasher};
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.run.document_entry(AUDIO_ROUTING_ENTRY).hash(&mut h);
        let now = (revision, h.finish());
        let Some(t) = self.timeline.as_mut() else { return };
        if t.audio.followed == Some(now) {
            return;
        }
        t.audio.followed = Some(now);
        if self.mixer.is_some()
            && let Err(e) = self.mixer_rebuild()
        {
            self.say(format!("timeline: mixer not rebuilt ({e})"));
        }
    }

    /// `state_json()["timeline"]["audio"]`.
    pub(in crate::view) fn timeline_audio_json(&self) -> Value {
        let Some(t) = self.timeline.as_ref() else { return Value::Null };
        let lanes: Vec<Value> = t
            .audio
            .lanes
            .iter()
            .map(|(track, target)| match self.timeline_lane(track, target) {
                Some(v) => json!({
                    "track_id": track,
                    "lane": target.key(),
                    "editable": target.editable(),
                    "range": [v.range.0, v.range.1],
                    "points": v.points().map(|p| json!({"clip_id": p.id.clip_id, "offset_ticks": p.id.offset_ticks, "at": p.at.value(), "value": p.value})).collect::<Vec<_>>(),
                    "error": v.error,
                }),
                None => json!({"track_id": track, "lane": target.key(), "error": "unknown_track"}),
            })
            .collect();
        json!({
            "bound_rate": t.audio.rate(),
            "waveforms": t.audio.waveforms,
            "lanes": lanes,
            "mixer_open": self.mixer.is_some(),
        })
    }

    fn lanes_rate(&self) -> u32 {
        self.timeline.as_ref().and_then(|t| t.audio.rate()).unwrap_or(DEFAULT_RATE)
    }

    /// The stored routing of the timeline's sequence (everything to the master when none).
    fn timeline_routing(&self) -> Result<StudioAudioRouting, String> {
        let id = self.timeline.as_ref().map(|t| t.sequence_id.clone()).ok_or("no_timeline")?;
        Ok(self.audio_routing()?.unwrap_or_default().routings.into_iter().find(|r| r.sequence_id == id).unwrap_or_else(|| StudioAudioRouting::new(id)))
    }

    fn multitrack_inner(&mut self, command: &AutomationCommand) -> Result<(), String> {
        self.timeline_sync_monitor();
        if self.timeline.is_none() {
            return Err("no_timeline".into());
        }
        match command {
            AutomationCommand::ToggleWaveforms => {
                let t = self.timeline.as_mut().ok_or("no_timeline")?;
                t.audio.waveforms = !t.audio.waveforms;
                Ok(())
            }
            AutomationCommand::ShowLane { track_id, lane } => {
                if let Some(target) = lane
                    && !self.timeline_lane_choices(track_id).contains(target)
                {
                    return Err(format!("lane_unavailable {}", target.key()));
                }
                let t = self.timeline.as_mut().ok_or("no_timeline")?;
                match lane {
                    Some(target) => t.audio.lanes.insert(track_id.clone(), target.clone()),
                    None => t.audio.lanes.remove(track_id),
                };
                Ok(())
            }
            AutomationCommand::CycleLane { track_id } => {
                let choices = self.timeline_lane_choices(track_id);
                if choices.is_empty() {
                    return Err("no_audio_track".into());
                }
                let t = self.timeline.as_mut().ok_or("no_timeline")?;
                let next = match t.audio.lanes.get(track_id).and_then(|c| choices.iter().position(|x| x == c)) {
                    None => choices.first().cloned(),
                    Some(i) => choices.get(i.saturating_add(1)).cloned(),
                };
                match next {
                    Some(n) => t.audio.lanes.insert(track_id.clone(), n),
                    None => t.audio.lanes.remove(track_id),
                };
                Ok(())
            }
            AutomationCommand::AddPoint { track_id, lane, at, value } => self.lane_edit("timeline_lane_point_add", track_id, lane, |e| e.put(None, *at, *value)),
            AutomationCommand::MovePoint { track_id, lane, point, at, value } => self.lane_edit("timeline_lane_point_move", track_id, lane, |e| e.put(Some(point), *at, *value)),
            AutomationCommand::DeletePoint { track_id, lane, point } => self.lane_edit("timeline_lane_point_delete", track_id, lane, |e| e.delete(point)),
            AutomationCommand::ClipGain { clip_id, delta_db } => {
                if !delta_db.is_finite() {
                    return Err("value_not_finite".into());
                }
                if *delta_db == 0.0 {
                    return Err("no_change".into());
                }
                let delta = *delta_db;
                self.routing_commit("timeline_clip_gain", |routing, seq| {
                    let found = seq.tracks.iter().find_map(|t| t.clips.iter().find(|c| &c.clip_id == clip_id).map(|_| t.kind)).ok_or("unknown_clip")?;
                    if found != TrackKind::Audio {
                        return Err("not_an_audio_clip".into());
                    }
                    match routing.clip_gains.iter_mut().find(|g| &g.clip_id == clip_id) {
                        Some(g) => {
                            for p in &mut g.points {
                                p.gain_db = checked_value(p.gain_db + delta, CLIP_GAIN_DB).map_err(|_| "clip_gain_out_of_range".to_owned())?;
                            }
                        }
                        None => {
                            let gain_db = checked_value(delta, CLIP_GAIN_DB).map_err(|_| "clip_gain_out_of_range".to_owned())?;
                            routing.clip_gains.push(StudioClipGainEnvelope { clip_id: clip_id.clone(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db }] });
                        }
                    }
                    Ok(())
                })
            }
        }
    }

    /// One point edit of lane `lane` on `track_id`.
    fn lane_edit(&mut self, op: &str, track_id: &str, lane: &LaneTarget, f: impl FnOnce(&mut LaneEdit<'_>) -> Result<(), String>) -> Result<(), String> {
        if !lane.editable() {
            return Err("track_automation_not_stored".into());
        }
        let rate = self.lanes_rate();
        self.routing_commit(op, |routing, seq| {
            let track = seq.tracks.iter().find(|t| t.track_id == track_id).ok_or("unknown_track")?;
            let mut edit = match lane {
                LaneTarget::ClipVolume if track.kind == TrackKind::Audio => LaneEdit::Clip { routing, track },
                LaneTarget::BusParam { instance_id, key } if track.kind == TrackKind::Submix => {
                    let instance = routing
                        .bus_effects
                        .iter_mut()
                        .filter(|b| b.track_id == track_id)
                        .flat_map(|b| b.effects.iter_mut())
                        .find(|e| &e.instance_id == instance_id)
                        .ok_or("unknown_effect")?;
                    let (range, _) = bus_param(instance, key, rate)?;
                    LaneEdit::Bus { instance, key, range, rate }
                }
                _ => return Err(format!("lane_unavailable {}", lane.key())),
            };
            f(&mut edit)?;
            if let LaneEdit::Bus { instance, rate, .. } = &edit
                && instance.enabled
            {
                bus_effect(instance, *rate).map_err(|e| format!("effect_refused {}", e.code()))?;
            }
            Ok(())
        })
    }

    /// Edits the timeline sequence's routing, validates the whole set against the document and
    /// stores it as ONE entry-only history step; the mixer follows.
    fn routing_commit(&mut self, op: &str, edit: impl FnOnce(&mut StudioAudioRouting, &StudioSequence) -> Result<(), String>) -> Result<(), String> {
        let seq = self.timeline_sequence().cloned().ok_or("unknown_sequence")?;
        let mut set = self.audio_routing()?.unwrap_or_default();
        let at = match set.routings.iter().position(|r| r.sequence_id == seq.sequence_id) {
            Some(i) => i,
            None => {
                set.routings.push(StudioAudioRouting::new(seq.sequence_id.clone()));
                set.routings.len().saturating_sub(1)
            }
        };
        edit(&mut set.routings[at], &seq)?;
        let snapshot = self.run.snapshot().ok_or("not_open")?;
        folio::validate_audio_routing(&set, snapshot.document(), folio::Budget::default(), &CancellationToken::default()).map_err(|d| format!("routing_invalid {:?} {}", d.code, d.target))?;
        let bytes = serde_json::to_vec(&set).map_err(|e| e.to_string())?;
        if !self.run.set_tiles_and_entry(op, "", None, AUDIO_ROUTING_ENTRY, bytes) {
            return Err(format!("submit_failed {}", self.op_error()));
        }
        self.timeline_audio_follow();
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        self.say(format!("timeline: {op} (revision {revision})"));
        Ok(())
    }
}

/// The record one lane edit changes.
enum LaneEdit<'a> {
    Clip { routing: &'a mut StudioAudioRouting, track: &'a folio::StudioTrack },
    Bus { instance: &'a mut folio::StudioAudioEffectInstance, key: &'a str, range: (f64, f64), rate: u32 },
}

impl LaneEdit<'_> {
    /// Puts a point at `at` / `value`, removing `from` first (a move); a point already at `at`
    /// is replaced by an add and refuses a move (`point_exists`).
    fn put(&mut self, from: Option<&PointId>, at: Ticks, value: f64) -> Result<(), String> {
        match self {
            Self::Clip { routing, track } => {
                let value = checked_value(value, CLIP_GAIN_DB)?;
                let clip = match from {
                    Some(p) => {
                        let id = p.clip_id.as_deref().ok_or("unknown_point")?;
                        track.clips.iter().find(|c| c.clip_id == id).ok_or("unknown_clip")?
                    }
                    None => track.clips.iter().find(|c| c.timeline_start <= at && c.timeline_end().is_some_and(|e| at < e)).ok_or("no_clip_at")?,
                };
                let end = clip.timeline_end().ok_or("overflow")?;
                if at < clip.timeline_start || at > end {
                    return Err("outside_clip".into());
                }
                let offset = at.value().saturating_sub(clip.timeline_start.value());
                let i = match routing.clip_gains.iter().position(|g| g.clip_id == clip.clip_id) {
                    Some(i) => i,
                    None if from.is_none() => {
                        routing.clip_gains.push(StudioClipGainEnvelope { clip_id: clip.clip_id.clone(), points: Vec::new() });
                        routing.clip_gains.len().saturating_sub(1)
                    }
                    None => return Err("unknown_point".into()),
                };
                let points = &mut routing.clip_gains[i].points;
                if let Some(p) = from {
                    let j = points.iter().position(|q| q.offset_ticks == p.offset_ticks).ok_or("unknown_point")?;
                    points.remove(j);
                    if points.iter().any(|q| q.offset_ticks == offset) {
                        return Err("point_exists".into());
                    }
                }
                points.retain(|q| q.offset_ticks != offset);
                let k = points.partition_point(|q| q.offset_ticks < offset);
                points.insert(k, StudioClipGainPoint { offset_ticks: offset, gain_db: value });
                Ok(())
            }
            Self::Bus { instance, key, range, rate } => {
                let value = checked_value(value, *range)?;
                samples_from_ticks_exact(at, *rate).map_err(|_| "not_sample_aligned".to_owned())?;
                let list = &mut instance.keyframes;
                if let Some(p) = from {
                    let j = list.iter().position(|k| k.key == *key && k.offset_ticks == p.offset_ticks).ok_or("unknown_point")?;
                    list.remove(j);
                    if list.iter().any(|k| k.key == *key && k.offset_ticks == at.value()) {
                        return Err("point_exists".into());
                    }
                }
                list.retain(|k| !(k.key == *key && k.offset_ticks == at.value()));
                list.push(StudioAudioPresetKeyframe { key: (*key).to_owned(), offset_ticks: at.value(), value });
                list.sort_by(|a, b| a.key.cmp(&b.key).then(a.offset_ticks.cmp(&b.offset_ticks)));
                Ok(())
            }
        }
    }

    fn delete(&mut self, point: &PointId) -> Result<(), String> {
        match self {
            Self::Clip { routing, .. } => {
                let id = point.clip_id.as_deref().ok_or("unknown_point")?;
                let i = routing.clip_gains.iter().position(|g| g.clip_id == id).ok_or("unknown_point")?;
                let points = &mut routing.clip_gains[i].points;
                let j = points.iter().position(|q| q.offset_ticks == point.offset_ticks).ok_or("unknown_point")?;
                points.remove(j);
                if points.is_empty() {
                    routing.clip_gains.remove(i);
                }
                Ok(())
            }
            Self::Bus { instance, key, .. } => {
                let j = instance.keyframes.iter().position(|k| k.key == *key && k.offset_ticks == point.offset_ticks).ok_or("unknown_point")?;
                instance.keyframes.remove(j);
                Ok(())
            }
        }
    }
}
