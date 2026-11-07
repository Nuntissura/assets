//! The sequence mix graph saved and reopened with the session (lane SMB-AUDITION-SESSION-2; feature
//! `audio-batch`). The routing of every sequence (Folio `StudioAudioRoutingSet`: submix routes and
//! sends, bus and master effects, clip gain envelopes) is the session document entry
//! [`AUDIO_ROUTING_ENTRY`], written through `Run::set_document_entry` (one undoable step, saved
//! with the package) and read back on open; Folio validates it against the open document both
//! ways. Playback and export build pulse's [`TimelineBusStream`] from the sequence and its routing,
//! with bus and master effects built from their stored instances by the batch module's
//! [`EffectSpec`] (with their keyframes as automation lanes, lane SMB-AUDITION-UI); play it
//! through `audio_out::AudioOut::with_buses`.
#![deny(clippy::arithmetic_side_effects)]

use super::audio_batch::EffectSpec;
use super::audio_import::{BlockSource, MixExport, MixFormat, export_from, write_atomically};
use super::monitor::MonitorFail;
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, ClipSource, StudioAudioRouting, StudioAudioRoutingSet, TrackKind};
use hsk_studio_pulse::{AudioSources, NestedAudioSources, NestedExtents, PulseError, SourceExtents, StreamLimits, TimelineBusStream};
use hsk_studio_score as score;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Program audio out hosted by the viewer (feature `audio-device`).
#[cfg(feature = "audio-device")]
pub mod playback;

/// Session document entry holding the routing set (JSON).
pub const AUDIO_ROUTING_ENTRY: &str = "audio_routing";

impl super::Viewer {
    /// Validates `set` against the open document and stores it as the session entry
    /// [`AUDIO_ROUTING_ENTRY`] (saved with the package; undoable like every entry write).
    pub fn set_audio_routing(&mut self, set: &StudioAudioRoutingSet) -> Result<(), String> {
        let snapshot = self.run.snapshot().ok_or("audio routing: no document is open")?;
        folio::validate_audio_routing(set, snapshot.document(), folio::Budget::default(), &CancellationToken::default())
            .map_err(|d| format!("audio routing: {d:?}"))?;
        let bytes = serde_json::to_vec(set).map_err(|e| format!("audio routing: {e}"))?;
        self.run
            .set_document_entry(AUDIO_ROUTING_ENTRY, Some(bytes))
            .map_err(|code| format!("audio routing: {code}"))?;
        self.say(format!("audio routing stored for {} sequence(s)", set.routings.len()));
        Ok(())
    }

    /// The stored routing set (`None` when the document has none), validated against the document.
    pub fn audio_routing(&self) -> Result<Option<StudioAudioRoutingSet>, String> {
        let snapshot = self.run.snapshot().ok_or("audio routing: no document is open")?;
        let Some(bytes) = self.run.document_entry(AUDIO_ROUTING_ENTRY) else {
            return Ok(None);
        };
        folio::parse_audio_routing(bytes, snapshot.document(), folio::Budget::default(), &CancellationToken::default())
            .map(Some)
            .map_err(|d| format!("audio routing: {d:?}"))
    }

    /// Pulse's bus stream of sequence `sequence_id` with its stored routing (or everything to the
    /// master when none is stored), for playback (`AudioOut::with_buses`) or export. Nested sequence
    /// clips play their nested sequence's mix (lane SMB-PR-NEST-1, pulse `NestedAudioSources`); a
    /// nested sequence that cannot be rendered refuses the stream (`nested audio unresolved`), so an
    /// export never carries silence in its place.
    pub fn bus_stream<S: AudioSources + Clone + Send + 'static>(
        &self,
        sequence_id: &str,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
    ) -> Result<TimelineBusStream, String> {
        let snapshot = self.run.snapshot().ok_or("bus stream: no document is open")?;
        let sequence = snapshot
            .document()
            .sequences
            .iter()
            .find(|s| s.sequence_id == sequence_id)
            .ok_or_else(|| format!("bus stream: unknown sequence {sequence_id}"))?;
        let set = self.audio_routing()?.unwrap_or_default();
        let routing = set.routings.iter().find(|r| r.sequence_id == sequence_id).cloned().unwrap_or_else(|| StudioAudioRouting::new(sequence_id));
        self.bus_stream_with(sequence, &set, &routing, sample_rate_hz, max_block_frames, sources, extents)
    }

    /// [`super::Viewer::bus_stream`] of `sequence` with `routing` given (a stem's copy, lane
    /// SMB-AUDIO-OUT-MIX-1); nested clips resolve through the document's sequences and `set`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bus_stream_with<S: AudioSources + Clone + Send + 'static>(
        &self,
        sequence: &folio::StudioSequence,
        set: &StudioAudioRoutingSet,
        routing: &StudioAudioRouting,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
    ) -> Result<TimelineBusStream, String> {
        let snapshot = self.run.snapshot().ok_or("bus stream: no document is open")?;
        let sequences = &snapshot.document().sequences;
        // Lane SMB-PR-NEST-2: the nested mixes apply their own clip gain and pan envelopes.
        let nested = NestedAudioSources::with_routing(sequences, sequence, sources, set, sample_rate_hz, max_block_frames, extents, StreamLimits::default(), self.cancel)
            .map_err(|e| format!("bus stream: {}", e.code()))?;
        if let Some(r) = nested.reports().first() {
            return Err(format!("bus stream: nested audio unresolved: sequence {} ({}) used by {:?}", r.sequence_id, r.code, r.clip_ids));
        }
        let extents = NestedExtents::new(sequences, extents);
        build_bus_stream(sequence, routing, sample_rate_hz, max_block_frames, nested, &extents, self.cancel)
            .map_err(|e| format!("bus stream: {}", e.code()))
    }
}

// ----- the sound of a document sequence (lane SMB-AUDIO-OUT-MIX-1) ------------------------------

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// Block every mixdown, stem and sum-check render takes.
const BLOCK: usize = 4096;
/// Most stems one call prints (each is one render and one file).
pub const MAX_STEMS: usize = 32;
/// The residual a stem set must stay at or under to sum to the print (the audio-post scenario's step
/// 90 gate; no spec clause names a value).
pub const STEM_SUM_LIMIT_DBFS: f64 = -90.0;

/// A repaired clip source: its edited samples and their length.
type Repaired = (ClipSource, Arc<Vec<Vec<f32>>>, Ticks);

/// The export's sound port ([`super::Viewer::set_export_audio`]) with repaired audio resolved: a
/// clip source whose samples are the source of the stored waveform edit list
/// ([`super::waveform::WAVEFORM_EDIT_ENTRY`], one undo step per edit, saved with the package) plays
/// and exports as that list replayed, at the edited length. Playback, the audio-only mixdown, stems
/// and Export Media all read it, so what the waveform editor repaired is what the mix holds.
#[derive(Clone)]
pub struct SequenceSound {
    base: Arc<dyn AudioSources + Send + Sync>,
    /// Port extents of the document's clip sources, read once.
    extents: Vec<(ClipSource, Ticks)>,
    /// Sources the edit list applies to: the edited samples and their length.
    repaired: Vec<Repaired>,
    rate: u32,
}

impl SequenceSound {
    pub fn rate(&self) -> u32 {
        self.rate
    }
    /// The clip sources that play repaired audio.
    pub fn repaired_sources(&self) -> Vec<&ClipSource> {
        self.repaired.iter().map(|(s, _, _)| s).collect()
    }
}

impl AudioSources for SequenceSound {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        match self.repaired.iter().find(|(s, _, _)| s == source) {
            Some((_, samples, _)) => Some(samples.as_slice()),
            None => self.base.audio(source),
        }
    }
}

impl SourceExtents for SequenceSound {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        self.repaired
            .iter()
            .find(|(s, _, _)| s == source)
            .map(|(_, _, t)| *t)
            .or_else(|| self.extents.iter().find(|(s, _)| s == source).map(|(_, t)| *t))
    }
}

/// A bus stream as encoder input, from its start to its end.
struct BusSource {
    stream: TimelineBusStream,
    frames: u64,
}

impl BlockSource for BusSource {
    fn channels(&self) -> u16 {
        2
    }
    fn rate(&self) -> u32 {
        hsk_studio_pulse::AudioClock::sample_rate_hz(&self.stream)
    }
    fn frames(&self) -> u64 {
        self.frames
    }
    fn render(&mut self, planes: &mut [Vec<f32>], n: usize, cancel: &CancellationToken) -> Result<(), MonitorFail> {
        let [l, r] = planes else {
            return Err(fail("audio_mix", "stereo planes"));
        };
        self.stream.render(&mut l[..n], &mut r[..n], cancel).map_err(|e| fail("audio_mix", e.code()))
    }
}

/// One printed stem.
#[derive(Clone, Debug, PartialEq)]
pub struct StemPrint {
    /// The submix track the stem carries; `None`: the rest (everything no listed bus carries).
    pub bus_track_id: Option<String>,
    pub output: PathBuf,
    pub receipt: MixExport,
}

/// A stem set and its sum check against the print.
#[derive(Clone, Debug, PartialEq)]
pub struct StemReport {
    pub stems: Vec<StemPrint>,
    pub frames: u64,
    /// Peak of `print - sum(stems)` over every sample of both channels (linear).
    pub residual_peak: f64,
    /// Peak of the print (linear).
    pub print_peak: f64,
}

impl StemReport {
    /// The residual in dBFS; `None` for an exact null.
    pub fn residual_dbfs(&self) -> Option<f64> {
        (self.residual_peak > 0.0).then(|| 20.0 * self.residual_peak.log10())
    }
    /// The stems sum to the print within [`STEM_SUM_LIMIT_DBFS`].
    pub fn sums_to_print(&self) -> bool {
        self.residual_dbfs().is_none_or(|db| db <= STEM_SUM_LIMIT_DBFS)
    }
    pub fn to_json(&self) -> Value {
        json!({
            "frames": self.frames,
            "stems": self.stems.iter().map(|s| json!({"bus_track_id": s.bus_track_id, "output": s.output.to_string_lossy(), "receipt": s.receipt.to_json()})).collect::<Vec<_>>(),
            "sum_check": {"residual_dbfs": self.residual_dbfs(), "print_peak": self.print_peak, "limit_dbfs": STEM_SUM_LIMIT_DBFS, "sums_to_print": self.sums_to_print()},
        })
    }
}

/// The submix track `track_id`'s output chain ends at `target` (a bus id), following route outputs; an
/// output naming no held bus goes to the master (pulse's rule, F-R57-2).
fn reaches(sequence: &folio::StudioSequence, routing: &StudioAudioRouting, track_id: &str, target: &str) -> bool {
    let is_bus = |id: &str| sequence.tracks.iter().any(|t| t.track_id == id && t.kind == TrackKind::Submix);
    let mut at = track_id.to_string();
    for _ in 0..=sequence.tracks.len() {
        if at == target {
            return true;
        }
        match routing.routes.iter().find(|r| r.track_id == at).and_then(|r| r.output.clone()) {
            Some(next) if is_bus(&next) => at = next,
            _ => return false,
        }
    }
    false
}

/// The sequence and routing of one stem: audio tracks whose output chain does not reach the stem's
/// bus are muted, and every send whose target chain does not reach it is dropped, so the stem holds
/// exactly the signal the print routes through that bus. `None` stands for the rest: what reaches
/// none of `listed`.
fn stem_of(sequence: &folio::StudioSequence, routing: &StudioAudioRouting, bus: Option<&str>, listed: &[String]) -> (folio::StudioSequence, StudioAudioRouting) {
    let keep = |id: &str| match bus {
        Some(b) => reaches(sequence, routing, id, b),
        None => !listed.iter().any(|b| reaches(sequence, routing, id, b)),
    };
    let mut seq = sequence.clone();
    for t in seq.tracks.iter_mut().filter(|t| t.kind == TrackKind::Audio) {
        if !keep(&t.track_id) {
            t.muted = true;
        }
    }
    let mut r = routing.clone();
    for route in &mut r.routes {
        route.sends.retain(|s| keep(&s.target));
    }
    (seq, r)
}

/// File-name form of a bus name: letters, digits, `-` and `_` kept, anything else `_`.
fn file_stem(name: &str) -> String {
    let s: String = name.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.is_empty() { "bus".into() } else { s }
}

fn peak(a: f32, b: f32) -> f64 {
    (f64::from(a) - f64::from(b)).abs()
}

impl super::Viewer {
    /// The sequence sound ([`SequenceSound`]) from the bound port. Refusals: `not_open`,
    /// `audio_sources_missing` (no port), `routing_unreadable`, `repair_unresolved` (a stored edit
    /// list that cannot be replayed over its source: never the unrepaired audio in its place).
    pub fn sequence_sound(&self) -> Result<SequenceSound, MonitorFail> {
        let snapshot = self.run.snapshot().ok_or_else(|| fail("not_open", "nothing is open"))?;
        let (base, port_extents, rate) = self.sound_port().ok_or_else(|| fail("audio_sources_missing", "no sound port is bound (Viewer::set_export_audio)"))?;
        let mut sources: Vec<ClipSource> = Vec::new();
        for c in snapshot.document().sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()) {
            if !matches!(c.source_ref, ClipSource::Sequence { .. }) && !sources.contains(&c.source_ref) {
                sources.push(c.source_ref.clone());
            }
        }
        let extents = sources.iter().filter_map(|s| port_extents.extent(s).map(|t| (s.clone(), t))).collect();
        let mut repaired = Vec::new();
        if let Some(doc) = self.waveform_document().map_err(|e| fail("repair_unresolved", e))?.filter(|d| !d.ops.is_empty()) {
            let plugins = self.waveform.as_ref().map(|w| &w.plugins);
            let fallback = super::waveform::PluginRegistry::default();
            for s in &sources {
                let Some(samples) = base.audio(s) else { continue };
                let frames = samples.first().map_or(0, |c| c.len() as u64);
                if rate != doc.source.sample_rate_hz || samples.len() != doc.source.channels || frames != doc.source.frames {
                    continue;
                }
                let buffer = score::WaveBuffer::new(rate, samples.to_vec()).map_err(|e| fail("repair_unresolved", format!("{e:?}")))?;
                if super::waveform::SourceIdentity::of(&buffer) != doc.source {
                    continue;
                }
                let edited = super::waveform::replay(&buffer, &doc.ops, plugins.unwrap_or(&fallback), self.run.actor(), self.cancel).map_err(|e| fail("repair_unresolved", e))?;
                let length = hsk_studio_pulse::ticks_from_samples(edited.frames(), rate).map_err(|e| fail("repair_unresolved", e.code()))?;
                repaired.push((s.clone(), Arc::new(edited.into_channels()), length));
            }
        }
        Ok(SequenceSound { base, extents, repaired, rate })
    }

    /// Opens the waveform editor on the source of timeline clip `clip_id` (any sequence of the
    /// document; STU-OVR-015, the audio-post scenario's step 32): the source's samples from the sound
    /// port at its rate (the original: a stored edit list of this source replays over it), with the
    /// clip's source window selected on every channel. Edits there are the waveform editor's (one undo
    /// step each), and the repaired audio reaches the mix through [`SequenceSound`]. Refusals:
    /// `not_open`, `unknown_clip`, `clip_not_media` (a nested sequence clip), `audio_sources_missing`
    /// (no port, or the port lacks the source), `waveform_open` (the editor's refusal).
    pub fn open_waveform_from_clip(&mut self, clip_id: &str) -> Result<Value, MonitorFail> {
        let snapshot = self.run.snapshot().ok_or_else(|| fail("not_open", "nothing is open"))?;
        let clip = snapshot
            .document()
            .sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.clips.iter())
            .find(|c| c.clip_id == clip_id)
            .cloned()
            .ok_or_else(|| fail("unknown_clip", clip_id))?;
        if matches!(clip.source_ref, ClipSource::Sequence { .. }) {
            return Err(fail("clip_not_media", clip_id));
        }
        let (base, _, rate) = self.sound_port().ok_or_else(|| fail("audio_sources_missing", "no sound port is bound (Viewer::set_export_audio)"))?;
        let samples = base.audio(&clip.source_ref).ok_or_else(|| fail("audio_sources_missing", format!("{clip_id}: the sound port lacks its source")))?.to_vec();
        let frames = samples.first().map_or(0, |c| c.len() as u64);
        let at = |t: Ticks| hsk_studio_pulse::samples_floor(t, rate).map(|s| s.min(frames)).map_err(|e| fail("waveform_open", e.code()));
        let (start, end) = (at(clip.source_in)?, at(clip.source_out)?);
        self.open_waveform(rate, samples).map_err(|e| fail("waveform_open", e))?;
        if start < end {
            self.waveform_select(Some(score::WaveSelection { start, end, channels: score::ALL_CHANNELS })).map_err(|e| fail("waveform_open", e))?;
        }
        Ok(json!({"clip_id": clip_id, "sample_rate_hz": rate, "frames": frames, "selection": [start, end]}))
    }

    /// The bus stream of `sequence_id` over the sequence sound (`mixdown` for pulse's refusal).
    fn sound_stream(&self, sequence_id: &str, sound: &SequenceSound, block: usize) -> Result<TimelineBusStream, MonitorFail> {
        self.bus_stream(sequence_id, sound.rate, block, sound.clone(), sound).map_err(|e| fail("mixdown", e))
    }

    /// The audio-only routed mixdown of `sequence_id` (STU-FX-137 routing): the stream the mixer
    /// and playback play (stored routing, buses, sends, effects, automation, nested clips, repaired
    /// audio) from the sequence start to its end, written to `output` as `format` through reel's
    /// encoders (WAV, FLAC, AIFF), atomically, never over the opened file. Needs no picture. Refusals:
    /// as [`super::Viewer::sequence_sound`], `unknown_sequence`, `mixdown`, `export_over_source`, then
    /// [`super::audio_import::write_atomically`]'s and `export_from`'s.
    pub fn export_sequence_mix(&mut self, sequence_id: &str, format: MixFormat, output: &Path, overwrite: bool) -> Result<MixExport, MonitorFail> {
        let result = self.export_sequence_mix_inner(sequence_id, format, output, overwrite);
        match &result {
            Ok(r) => self.say(format!("mixdown {sequence_id}: {} ({} frames, {} clipped)", output.display(), r.frames, r.clipped_samples)),
            Err(e) => self.say(format!("mixdown {sequence_id} refused {} {}", e.code, e.detail)),
        }
        result
    }

    fn check_output(&self, output: &Path) -> Result<(), MonitorFail> {
        if self.source.as_deref() == Some(output) {
            return Err(fail("export_over_source", output.display().to_string()));
        }
        Ok(())
    }

    fn known_sequence(&self, sequence_id: &str) -> Result<folio::StudioSequence, MonitorFail> {
        let snapshot = self.run.snapshot().ok_or_else(|| fail("not_open", "nothing is open"))?;
        snapshot.document().sequences.iter().find(|s| s.sequence_id == sequence_id).cloned().ok_or_else(|| fail("unknown_sequence", sequence_id))
    }

    fn export_sequence_mix_inner(&self, sequence_id: &str, format: MixFormat, output: &Path, overwrite: bool) -> Result<MixExport, MonitorFail> {
        self.known_sequence(sequence_id)?;
        self.check_output(output)?;
        let sound = self.sequence_sound()?;
        let mut stream = self.sound_stream(sequence_id, &sound, BLOCK)?;
        stream.seek(0, self.cancel).map_err(|e| fail("mixdown", e.code()))?;
        let frames = stream.length_samples();
        let cancel = self.cancel;
        write_atomically(output, overwrite, |w| export_from(&mut BusSource { stream, frames }, BLOCK, format, w, None, cancel))
    }

    /// Stems of `sequence_id` (STU-FX-137 buses; the audio-post scenario's steps 90 and 91): one
    /// file per submix bus in `buses` (each a top-level bus: its output chain ends at the master),
    /// plus with `rest` one file of everything no listed bus carries, all from the sequence start to
    /// its end (equal length and start), written into `dir` as `<bus name>.<ext>` (`rest.<ext>`).
    /// Each stem holds the print's own signal through its bus ([`stem_of`]: unrouted tracks muted,
    /// sends that do not reach the bus dropped), and the report's sum check renders the print and
    /// every stem again in lockstep and measures `print - sum(stems)`: a send from a track into a bus
    /// it does not output to, or a non-linear master effect, shows there instead of passing silently.
    /// Refusals before anything is written: as [`super::Viewer::sequence_sound`], `unknown_sequence`,
    /// `stem_none` (no bus and no rest), `stem_count` (over [`MAX_STEMS`]), `unknown_bus` (not a
    /// submix track of the sequence), `stem_duplicate`, `stem_not_top_level`, `stem_name_collision`,
    /// `output_exists` (any stem file, without `overwrite`), `export_over_source`, `mixdown`. A
    /// failure while writing removes the stems this call wrote.
    #[allow(clippy::too_many_arguments)]
    pub fn export_sequence_stems(&mut self, sequence_id: &str, buses: &[String], rest: bool, format: MixFormat, dir: &Path, overwrite: bool) -> Result<StemReport, MonitorFail> {
        let result = self.export_sequence_stems_inner(sequence_id, buses, rest, format, dir, overwrite);
        match &result {
            Ok(r) => self.say(format!("stems {sequence_id}: {} files, residual {:?} dBFS", r.stems.len(), r.residual_dbfs())),
            Err(e) => self.say(format!("stems {sequence_id} refused {} {}", e.code, e.detail)),
        }
        result
    }

    fn export_sequence_stems_inner(&self, sequence_id: &str, buses: &[String], rest: bool, format: MixFormat, dir: &Path, overwrite: bool) -> Result<StemReport, MonitorFail> {
        let sequence = self.known_sequence(sequence_id)?;
        if buses.is_empty() && !rest {
            return Err(fail("stem_none", "no bus and no rest"));
        }
        if buses.len() > MAX_STEMS {
            return Err(fail("stem_count", format!("{} buses, at most {MAX_STEMS}", buses.len())));
        }
        let set = self.audio_routing().map_err(|e| fail("routing_unreadable", e))?.unwrap_or_default();
        let routing = set.routings.iter().find(|r| r.sequence_id == sequence_id).cloned().unwrap_or_else(|| StudioAudioRouting::new(sequence_id));
        let mut names: Vec<String> = Vec::new();
        for (i, b) in buses.iter().enumerate() {
            let track = sequence.tracks.iter().find(|t| t.track_id == *b && t.kind == TrackKind::Submix).ok_or_else(|| fail("unknown_bus", b.clone()))?;
            if buses[..i].contains(b) {
                return Err(fail("stem_duplicate", b.clone()));
            }
            let output = routing.routes.iter().find(|r| r.track_id == *b).and_then(|r| r.output.clone());
            if output.is_some_and(|o| sequence.tracks.iter().any(|t| t.track_id == o && t.kind == TrackKind::Submix)) {
                return Err(fail("stem_not_top_level", b.clone()));
            }
            names.push(file_stem(&track.name));
        }
        if rest {
            names.push("rest".into());
        }
        let ext = format.extension();
        let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(format!("{n}.{ext}"))).collect();
        for (i, p) in paths.iter().enumerate() {
            if paths[..i].contains(p) {
                return Err(fail("stem_name_collision", p.display().to_string()));
            }
            self.check_output(p)?;
            if p.exists() && !overwrite {
                return Err(fail("output_exists", p.display().to_string()));
            }
        }
        let sound = self.sequence_sound()?;
        let mut plan: Vec<(Option<String>, folio::StudioSequence, StudioAudioRouting)> = buses.iter().map(|b| {
            let (s, r) = stem_of(&sequence, &routing, Some(b), buses);
            (Some(b.clone()), s, r)
        }).collect();
        if rest {
            let (s, r) = stem_of(&sequence, &routing, None, buses);
            plan.push((None, s, r));
        }
        let stream = |v: &Self, s: &folio::StudioSequence, r: &StudioAudioRouting| -> Result<TimelineBusStream, MonitorFail> {
            let mut st = v.bus_stream_with(s, &set, r, sound.rate, BLOCK, sound.clone(), &sound).map_err(|e| fail("mixdown", e))?;
            st.seek(0, v.cancel).map_err(|e| fail("mixdown", e.code()))?;
            Ok(st)
        };
        // Every stream is built (and refused) before the first file is written.
        let mut streams = Vec::with_capacity(plan.len());
        for (_, s, r) in &plan {
            streams.push(stream(self, s, r)?);
        }
        let mut print = stream(self, &sequence, &routing)?;
        let frames = print.length_samples();
        let cancel = self.cancel;
        let mut stems: Vec<StemPrint> = Vec::with_capacity(plan.len());
        for ((bus, _, _), (st, path)) in plan.iter().zip(streams.into_iter().zip(&paths)) {
            match write_atomically(path, overwrite, |w| export_from(&mut BusSource { stream: st, frames }, BLOCK, format, w, None, cancel)) {
                Ok(receipt) => stems.push(StemPrint { bus_track_id: bus.clone(), output: path.clone(), receipt }),
                Err(e) => {
                    for s in &stems {
                        let _ = std::fs::remove_file(&s.output);
                    }
                    return Err(e);
                }
            }
        }
        // Sum check: the print and every stem rendered in lockstep.
        let mut parts = Vec::with_capacity(plan.len());
        for (_, s, r) in &plan {
            parts.push(stream(self, s, r)?);
        }
        let (mut pl, mut pr) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let (mut sl, mut sr) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let (mut acc_l, mut acc_r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let (mut residual_peak, mut print_peak) = (0.0f64, 0.0f64);
        let mut done = 0u64;
        while done < frames {
            let n = BLOCK.min(usize::try_from(frames.saturating_sub(done)).unwrap_or(BLOCK));
            print.render(&mut pl[..n], &mut pr[..n], cancel).map_err(|e| fail("mixdown", e.code()))?;
            acc_l[..n].fill(0.0);
            acc_r[..n].fill(0.0);
            for p in &mut parts {
                p.render(&mut sl[..n], &mut sr[..n], cancel).map_err(|e| fail("mixdown", e.code()))?;
                for i in 0..n {
                    acc_l[i] += sl[i];
                    acc_r[i] += sr[i];
                }
            }
            for i in 0..n {
                residual_peak = residual_peak.max(peak(pl[i], acc_l[i])).max(peak(pr[i], acc_r[i]));
                print_peak = print_peak.max(f64::from(pl[i].abs())).max(f64::from(pr[i].abs()));
            }
            done = done.saturating_add(n as u64);
        }
        Ok(StemReport { stems, frames, residual_peak, print_peak })
    }
}

/// Builds a bus or master effect from its stored instance (lane SMB-AUDITION-UI: with keyframes).
/// Keyframe offsets are ticks from the sequence start, converted to samples exactly
/// (`not_frame_aligned` otherwise); keyed parameters must be automatable in score (see
/// [`EffectSpec::automation_index`]); lanes are linear between keyframes and held outside them.
pub fn bus_effect(instance: &folio::StudioAudioEffectInstance, rate: u32) -> Result<score::EffectSlot, PulseError> {
    let mut stat = instance.clone();
    stat.keyframes.clear();
    let spec = EffectSpec::from_preset_instance(&stat)
        .map_err(|_| PulseError::InvalidInput)?
        .ok_or(PulseError::InvalidInput)?;
    let processor = spec.build_automatable(rate, 2).map_err(|_| PulseError::InvalidInput)?;
    let params = processor.automation_parameters();
    let mut slot = score::EffectSlot::new(processor);
    let mut keys: Vec<&str> = instance.keyframes.iter().map(|k| k.key.as_str()).collect();
    keys.dedup();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        let index = spec.automation_index(key).ok_or(PulseError::AudioUnsupported)?;
        let range = params.get(index).map(|p| (p.min, p.max)).ok_or(PulseError::InvalidInput)?;
        let mut points = Vec::new();
        for k in instance.keyframes.iter().filter(|k| k.key == key) {
            let at = hsk_studio_pulse::samples_from_ticks_exact(hsk_studio_accord::Ticks::new(k.offset_ticks), rate)?;
            points.push((at, k.value));
        }
        let lane = score::AutomationLane::new(&points, score::Interpolation::Linear, range).map_err(|_| PulseError::InvalidInput)?;
        slot = slot.with_lane(index, lane);
    }
    Ok(slot)
}

/// [`TimelineBusStream::new`] with [`bus_effect`] as the effect factory.
pub fn build_bus_stream<S: AudioSources + Clone + Send + 'static>(
    sequence: &folio::StudioSequence,
    routing: &StudioAudioRouting,
    sample_rate_hz: u32,
    max_block_frames: usize,
    sources: S,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<TimelineBusStream, PulseError> {
    let mut factory = bus_effect;
    TimelineBusStream::new(
        sequence,
        routing,
        sample_rate_hz,
        max_block_frames,
        sources,
        extents,
        StreamLimits::default(),
        &mut factory,
        cancel,
    )
}
