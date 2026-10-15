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
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, StudioAudioRouting, StudioAudioRoutingSet};
use hsk_studio_pulse::{AudioSources, PulseError, SourceExtents, StreamLimits, TimelineBusStream};
use hsk_studio_score as score;

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
    /// master when none is stored), for playback (`AudioOut::with_buses`) or export.
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
        let routing = self
            .audio_routing()?
            .and_then(|set| set.routings.into_iter().find(|r| r.sequence_id == sequence_id))
            .unwrap_or_else(|| StudioAudioRouting::new(sequence_id));
        build_bus_stream(sequence, &routing, sample_rate_hz, max_block_frames, sources, extents, self.cancel)
            .map_err(|e| format!("bus stream: {}", e.code()))
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
