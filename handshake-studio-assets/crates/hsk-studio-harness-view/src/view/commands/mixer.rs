//! `mixer.*` and `multitrack.*` (lane SMB-DRIVE-TYPED-2): every `MixerEdit` member through
//! `Viewer::mixer_edit` and every `AutomationCommand` through `Viewer::multitrack_command`, as the mixer
//! panel and the timeline's lanes run them: one entry-only undo step per edit, named by the engine
//! (`mixer_volume`, `timeline_lane_point_add`, ...). The sound is the bound port read through
//! `Viewer::sequence_sound`, so repaired audio plays. Before each row the mixer follows the history
//! when the timeline is open. Feature `audio-batch`; elsewhere every row refuses `feature_unavailable`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::CommandRefusal;
use super::decode::Fields;
use serde_json::Value;

#[cfg(feature = "audio-batch")]
pub(super) mod enabled {
    use super::super::super::Viewer;
    use super::super::super::mixer::{MixerEdit, effect_templates, fresh_effect_id};
    use super::super::super::monitor::MonitorFail;
    use super::super::super::timeline::{AutomationCommand, LaneTarget, PointId};
    use super::super::decode::{Fields, invalid, missing};
    use super::super::{CommandRefusal, engine_refusal};
    use hsk_studio_accord::Ticks;
    use hsk_studio_folio as folio;
    use serde_json::{Value, json};
    use std::sync::Arc;

    /// Most frames one `mixer.render` holds (60 s at 48 kHz, two f32 planes).
    pub const MAX_RENDER_FRAMES: u64 = 2_880_000;
    /// Largest stream block a row asks for.
    pub const MAX_BLOCK_FRAMES: u64 = 16_384;
    /// The block of the window's multitrack binding.
    const DEFAULT_BLOCK: usize = 960;
    /// The refusal text of an unknown template: the names of `effect_templates()` (lane
    /// SMB-SCORE-EFFECTS-6; harness-core's row declares the same list).
    const TEMPLATES: &str = "gain|equalizer|dynamics|lookahead_limiter|graphic_eq_10_bands|graphic_eq_20_bands|graphic_eq_30_bands|hard_limiter|flanger|phaser|chorus|click_repair|reverb|feedback_delay|hum_remover|multiband_compressor";
    const EFFECT: &str = "a Folio StudioAudioEffectInstance record";
    const LANES: &str = "clip_volume|track_volume|track_pan|clip_pan|bus.<instance_id>.<key>";

    pub(in crate::view::commands) fn monitor_refusal(e: MonitorFail) -> CommandRefusal {
        CommandRefusal::new(e.code, json!({"message": e.detail}))
    }

    fn block(f: &Fields<'_>) -> Result<usize, CommandRefusal> {
        if !f.has("block_frames") {
            return Ok(DEFAULT_BLOCK);
        }
        let n = f.unsigned("block_frames")?;
        if n == 0 || n > MAX_BLOCK_FRAMES {
            return Err(invalid("block_frames", "1..=16384"));
        }
        usize::try_from(n).map_err(|_| invalid("block_frames", "1..=16384"))
    }

    /// `template` (a panel template with a fresh `fx-<n>` id in `existing`) or `effect` (a whole
    /// record), exactly one of them.
    pub(in crate::view::commands) fn effect_instance(f: &Fields<'_>, existing: &[folio::StudioAudioEffectInstance]) -> Result<folio::StudioAudioEffectInstance, CommandRefusal> {
        match (f.has("template"), f.has("effect")) {
            (true, true) => Err(CommandRefusal::new("parameter_conflict", json!({"keys": ["template", "effect"]}))),
            (false, false) => Err(missing("template")),
            (false, true) => serde_json::from_value::<folio::StudioAudioEffectInstance>(f.raw("effect", EFFECT)?).map_err(|_| invalid("effect", EFFECT)),
            (true, false) => {
                let name = f.text("template")?;
                let (_, spec) = effect_templates().into_iter().find(|(n, _)| *n == name).ok_or_else(|| invalid("template", TEMPLATES))?;
                Ok(spec.to_preset_instance(fresh_effect_id(existing)))
            }
        }
    }

    fn edit_of(v: &Viewer, id: &str, f: &Fields<'_>) -> Result<MixerEdit, CommandRefusal> {
        let track = || f.text("track_id");
        let bus = f.opt_text("bus")?;
        let instance = || f.text("instance_id");
        Ok(match id {
            "mixer.volume" => MixerEdit::Volume { track_id: track()?, linear: f.number("linear")? },
            "mixer.pan" => MixerEdit::Pan { track_id: track()?, pan: f.number("pan")? },
            "mixer.mute" => MixerEdit::Mute { track_id: track()?, muted: f.flag("muted")? },
            "mixer.solo" => MixerEdit::Solo { track_id: track()?, soloed: f.flag("soloed")? },
            "mixer.output" => MixerEdit::Output { track_id: track()?, bus },
            "mixer.add_send" => MixerEdit::AddSend { track_id: track()?, target: f.text("target")?, level: f.number("level")?, pre_fader: f.flag_or("pre_fader", false)? },
            "mixer.send" => MixerEdit::Send { track_id: track()?, send_id: f.text("send_id")?, level: f.number("level")?, pre_fader: f.flag("pre_fader")?, pan: f.number("pan")? },
            "mixer.remove_send" => MixerEdit::RemoveSend { track_id: track()?, send_id: f.text("send_id")? },
            "mixer.master_volume" => MixerEdit::MasterVolume { linear: f.number("linear")? },
            "mixer.add_effect" => {
                let routing = v.mixer_routing().map_err(|e| CommandRefusal::new("routing_unreadable", json!({"message": e})))?;
                let existing = match bus.as_deref() {
                    Some(b) => routing.bus_effects.iter().find(|x| x.track_id == b).map(|x| x.effects.clone()).unwrap_or_default(),
                    None => routing.master.effects.clone(),
                };
                let at = if f.has("at") { Some(f.index("at")?) } else { None };
                MixerEdit::AddEffect { bus, instance: effect_instance(f, &existing)?, at }
            }
            "mixer.remove_effect" => MixerEdit::RemoveEffect { bus, instance_id: instance()? },
            "mixer.move_effect" => MixerEdit::MoveEffect { bus, instance_id: instance()?, to: f.index("to")? },
            "mixer.effect_enabled" => MixerEdit::EffectEnabled { bus, instance_id: instance()?, enabled: f.flag("enabled")? },
            "mixer.effect_param" => MixerEdit::EffectParam { bus, instance_id: instance()?, key: f.text("key")?, value: f.number("value")? },
            other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
        })
    }

    fn mixer(v: &mut Viewer, id: &str, f: &Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if id == "mixer.open" {
            let block = block(f)?;
            let sequence = f.text("sequence_id")?;
            let sound = v.sequence_sound().map_err(monitor_refusal)?;
            let rate = sound.rate();
            // A refused open keeps the mixer that was open (review F2 on 2bbe2207).
            let open = v.mixer.take();
            if let Err(e) = v.open_mixer(&sequence, rate, block, sound.clone(), Arc::new(sound)) {
                let refusal = engine_refusal(v, before, e);
                v.mixer = open;
                return Err(refusal);
            }
            return Ok(v.mixer_json());
        }
        if v.mixer.is_none() {
            return Err(CommandRefusal::new("mixer_closed", json!({"hint": "mixer.open or multitrack.open first"})));
        }
        v.timeline_audio_follow();
        match id {
            "mixer.state" => {}
            "mixer.render" => {
                let frames = f.unsigned("frames")?;
                if frames == 0 || frames > MAX_RENDER_FRAMES {
                    return Err(invalid("frames", "1..=2880000"));
                }
                let frames = usize::try_from(frames).map_err(|_| invalid("frames", "1..=2880000"))?;
                v.mixer_render(frames).map_err(|e| engine_refusal(v, before, e))?;
            }
            _ => {
                let edit = edit_of(v, id, f)?;
                v.mixer_edit(edit).map_err(|e| engine_refusal(v, before, e))?;
            }
        }
        Ok(v.mixer_json())
    }

    fn lane(text: &str) -> Option<LaneTarget> {
        Some(match text {
            "clip_volume" => LaneTarget::ClipVolume,
            "track_volume" => LaneTarget::TrackVolume,
            "track_pan" => LaneTarget::TrackPan,
            "clip_pan" => LaneTarget::ClipPan,
            other => {
                let (instance_id, key) = other.strip_prefix("bus.")?.split_once('.')?;
                if instance_id.is_empty() || key.is_empty() {
                    return None;
                }
                LaneTarget::BusParam { instance_id: instance_id.to_owned(), key: key.to_owned() }
            }
        })
    }

    fn lane_of(f: &Fields<'_>, key: &str) -> Result<LaneTarget, CommandRefusal> {
        lane(&f.text(key)?).ok_or_else(|| invalid(key, LANES))
    }

    fn point(f: &Fields<'_>) -> Result<PointId, CommandRefusal> {
        Ok(PointId { clip_id: f.opt_text("point_clip_id")?, offset_ticks: f.unsigned("point_offset_ticks")? })
    }

    fn multitrack(v: &mut Viewer, id: &str, f: &Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if id == "multitrack.open" {
            let block = block(f)?;
            let sound = v.sequence_sound().map_err(monitor_refusal)?;
            let rate = sound.rate();
            v.open_multitrack(rate, block, sound.clone(), Arc::new(sound)).map_err(|e| engine_refusal(v, before, e))?;
            return Ok(v.timeline_audio_json());
        }
        if v.timeline.is_none() {
            return Err(CommandRefusal::new("no_timeline", json!({"hint": "multitrack.open first"})));
        }
        let track = || f.text("track_id");
        let command = match id {
            "multitrack.lane" => {
                let (track_id, target) = (track()?, lane_of(f, "lane")?);
                let view = v.timeline_lane(&track_id, &target).ok_or_else(|| CommandRefusal::new("unknown_track", json!({"track_id": track_id})))?;
                return Ok(json!({
                    "track_id": track_id, "lane": target.key(), "editable": target.editable(), "range": [view.range.0, view.range.1], "unit": view.unit,
                    "points": view.points().map(|p| json!({"clip_id": p.id.clip_id, "offset_ticks": p.id.offset_ticks, "at": p.at.value(), "value": p.value})).collect::<Vec<_>>(),
                    "error": view.error,
                }));
            }
            "multitrack.lane_show" => AutomationCommand::ShowLane { track_id: track()?, lane: if f.has("lane") { Some(lane_of(f, "lane")?) } else { None } },
            "multitrack.lane_cycle" => AutomationCommand::CycleLane { track_id: track()? },
            "multitrack.lane_point_add" => AutomationCommand::AddPoint { track_id: track()?, lane: lane_of(f, "lane")?, at: Ticks::new(f.unsigned("at")?), value: f.number("value")? },
            "multitrack.lane_point_move" => AutomationCommand::MovePoint { track_id: track()?, lane: lane_of(f, "lane")?, point: point(f)?, at: Ticks::new(f.unsigned("at")?), value: f.number("value")? },
            "multitrack.lane_point_delete" => AutomationCommand::DeletePoint { track_id: track()?, lane: lane_of(f, "lane")?, point: point(f)? },
            "multitrack.clip_gain" => AutomationCommand::ClipGain { clip_id: f.text("clip_id")?, delta_db: f.number("delta_db")? },
            "multitrack.waveforms" => AutomationCommand::ToggleWaveforms,
            other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
        };
        v.multitrack_command(command).map_err(|e| engine_refusal(v, before, e))?;
        Ok(v.timeline_audio_json())
    }

    pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if id.starts_with("multitrack.") { multitrack(v, id, &f, before) } else { mixer(v, id, &f, before) }
    }
}

/// One `mixer.*` or `multitrack.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    #[cfg(feature = "audio-batch")]
    {
        enabled::run(v, id, f, before)
    }
    #[cfg(not(feature = "audio-batch"))]
    {
        let _ = (v, f, before);
        Err(CommandRefusal::new("feature_unavailable", serde_json::json!({"command_id": id, "feature": "audio-batch"})))
    }
}
