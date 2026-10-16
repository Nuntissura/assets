//! Viewer mixer and routing editor (lane SMB-AUDITION-UI; feature `audio-batch`). One strip per
//! audio and submix track of a sequence (fader, pan, mute, solo, output, sends, peak meter) and a
//! master strip (volume, peak and loudness meters). Every edit is ONE undo step:
//!
//! - fader (`volume`, linear; the panel shows dB), `pan`, `muted`, `soloed` are Folio track fields:
//!   the edited sequence goes through the session's timeline step (`Run::submit_sequence`), undone
//!   and redone by its inverse;
//! - output, sends (target, level, pre/post, pan), the master volume and the bus / master effect
//!   stacks (lane SMB-AUDITION-UI-2: add, remove, reorder, enable, change a parameter) live in the
//!   routing entry (module [`super::audio_session`]): an entry-only history step
//!   (`Run::set_tiles_and_entry`). An effect edit is refused before it is stored when the edited
//!   instance does not build at the mixer rate (`audio_session::bus_effect`).
//!
//! Playback / metering: [`super::Viewer::open_mixer`] binds the mixer to a sequence and an audio
//! source port; after every edit, undo and redo the pulse bus stream is rebuilt from the current
//! document and routing and moved back to the play position (exact seek). [`super::Viewer::mixer_render`]
//! renders the next frames (a host feeds them to `audio_out`, tests compare them) and updates the
//! meters: per track the post-fader peak over its strips, per bus the post-fader peak, the master
//! peak and the master loudness (score's BS.1770 meter: momentary, short-term, integrated).
#![deny(clippy::arithmetic_side_effects)]

use super::audio_batch::EffectSpec;
use super::audio_session::{AUDIO_ROUTING_ENTRY, build_bus_stream, bus_effect};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    self as folio, StudioAudioRouting, StudioAudioRoutingSet, StudioAudioSend, StudioSequence, StudioTrackRoute, TrackKind,
};
use hsk_studio_pulse::{AudioSources, PulseError, SourceExtents, TimelineBusStream};
use hsk_studio_score::GraphMeters;
use serde_json::{Value, json};
use std::sync::Arc;

/// Rebuilds the bus stream of the current sequence and routing.
type Builder = Box<dyn Fn(&StudioSequence, &StudioAudioRouting, &CancellationToken) -> Result<TimelineBusStream, PulseError>>;

/// Widget names of the mixer panel (`mixer.<track_id>.<control>` per strip).
pub mod widget {
    pub const PANEL: &str = "mixer.panel";
    pub const MASTER_VOLUME: &str = "mixer.master.volume";
    pub const MASTER_METER: &str = "mixer.master.meter";
    pub const VOLUME: &str = "volume";
    pub const PAN: &str = "pan";
    pub const MUTE: &str = "mute";
    pub const SOLO: &str = "solo";
    pub const OUTPUT: &str = "output";
    pub const ADD_SEND: &str = "add_send";
    pub const METER: &str = "meter";
    pub const ADD_EFFECT: &str = "add_effect";
    pub const MASTER_ADD_EFFECT: &str = "mixer.master.add_effect";
}

/// Effects the panel offers to add, with their starting parameters (score's processor identity
/// tags; see `audio_batch::EffectSpec`).
pub fn effect_templates() -> Vec<(&'static str, EffectSpec)> {
    use hsk_studio_score as score;
    vec![
        ("gain", EffectSpec::Gain { db: 0.0 }),
        (
            "equalizer",
            EffectSpec::Equalizer {
                sections: vec![score::BiquadSpec { kind: score::BiquadKind::Peaking, frequency_hz: 1000.0, gain_db: 0.0, q: 1.0 }],
                master_gain_db: 0.0,
            },
        ),
        ("dynamics", EffectSpec::Dynamics(score::DynamicsParams::compressor(-18.0, 3.0))),
        ("lookahead_limiter", EffectSpec::Limiter(score::LimiterParams::default())),
    ]
}

/// The mixer bound to one sequence.
pub struct MixerState {
    pub sequence_id: String,
    rate: u32,
    block: usize,
    builder: Builder,
    stream: Option<TimelineBusStream>,
    meters: Option<GraphMeters>,
    strip_tracks: Vec<Option<String>>,
    bus_tracks: Vec<String>,
    error: Option<String>,
}

fn db(linear: f64) -> f64 {
    if linear > 0.0 { 20.0 * linear.log10() } else { f64::NEG_INFINITY }
}

/// A requested mixer edit (UI and API share it).
#[derive(Clone, Debug, PartialEq)]
pub enum MixerEdit {
    Volume { track_id: String, linear: f64 },
    Pan { track_id: String, pan: f64 },
    Mute { track_id: String, muted: bool },
    Solo { track_id: String, soloed: bool },
    Output { track_id: String, bus: Option<String> },
    AddSend { track_id: String, target: String, level: f64, pre_fader: bool },
    Send { track_id: String, send_id: String, level: f64, pre_fader: bool, pan: f64 },
    RemoveSend { track_id: String, send_id: String },
    MasterVolume { linear: f64 },
    /// Bus (`Some(submix track id)`) or master (`None`) effect stack edits (lane SMB-AUDITION-UI-2).
    AddEffect { bus: Option<String>, instance: folio::StudioAudioEffectInstance, at: Option<usize> },
    RemoveEffect { bus: Option<String>, instance_id: String },
    MoveEffect { bus: Option<String>, instance_id: String, to: usize },
    EffectEnabled { bus: Option<String>, instance_id: String, enabled: bool },
    EffectParam { bus: Option<String>, instance_id: String, key: String, value: f64 },
}

/// A fresh instance id `fx-<n>` not used in `list`.
pub fn fresh_effect_id(list: &[folio::StudioAudioEffectInstance]) -> String {
    let mut n = list.len();
    loop {
        let id = format!("fx-{n}");
        if list.iter().all(|e| e.instance_id != id) {
            return id;
        }
        n = n.saturating_add(1);
    }
}

impl super::Viewer {
    /// Binds the mixer to sequence `sequence_id`: `sources` and `extents` feed pulse's bus stream at
    /// `sample_rate_hz` in blocks of `max_block_frames`.
    pub fn open_mixer<S: AudioSources + Clone + Send + 'static>(
        &mut self,
        sequence_id: &str,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: Arc<dyn SourceExtents>,
    ) -> Result<(), String> {
        let builder: Builder = Box::new(move |seq, routing, cancel| {
            build_bus_stream(seq, routing, sample_rate_hz, max_block_frames, sources.clone(), extents.as_ref(), cancel)
        });
        self.mixer = Some(MixerState {
            sequence_id: sequence_id.to_owned(),
            rate: sample_rate_hz,
            block: max_block_frames,
            builder,
            stream: None,
            meters: None,
            strip_tracks: Vec::new(),
            bus_tracks: Vec::new(),
            error: None,
        });
        self.mixer_rebuild()
    }

    fn mixer_sequence(&self) -> Result<StudioSequence, String> {
        let m = self.mixer.as_ref().ok_or("mixer: not open")?;
        self.run
            .snapshot()
            .and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == m.sequence_id).cloned())
            .ok_or_else(|| format!("mixer: unknown sequence {}", m.sequence_id))
    }

    fn mixer_routing_set(&self) -> Result<StudioAudioRoutingSet, String> {
        Ok(self.audio_routing()?.unwrap_or_default())
    }

    /// The routing of the mixer's sequence (everything to the master when none is stored).
    pub fn mixer_routing(&self) -> Result<StudioAudioRouting, String> {
        let id = self.mixer.as_ref().ok_or("mixer: not open")?.sequence_id.clone();
        Ok(self
            .mixer_routing_set()?
            .routings
            .into_iter()
            .find(|r| r.sequence_id == id)
            .unwrap_or_else(|| StudioAudioRouting::new(id)))
    }

    /// Rebuilds the bus stream from the current document and routing at the previous position.
    fn mixer_rebuild(&mut self) -> Result<(), String> {
        let sequence = self.mixer_sequence()?;
        let routing = self.mixer_routing()?;
        let cancel = self.cancel;
        let m = self.mixer.as_mut().ok_or("mixer: not open")?;
        let position = m.stream.as_ref().map_or(0, TimelineBusStream::position);
        let built = (m.builder)(&sequence, &routing, cancel).and_then(|mut s| {
            if position > 0 {
                s.seek(position, cancel)?;
            }
            Ok(s)
        });
        match built {
            Ok(s) => {
                m.strip_tracks = s.strip_tracks().to_vec();
                m.bus_tracks = s.bus_tracks().to_vec();
                m.stream = Some(s);
                m.error = None;
                Ok(())
            }
            Err(e) => {
                let text = format!("mixer: {}", e.code());
                m.stream = None;
                m.error = Some(text.clone());
                Err(text)
            }
        }
    }

    /// Renders the next `frames` (in stream blocks) through the buses and updates the meters.
    pub fn mixer_render(&mut self, frames: usize) -> Result<[Vec<f32>; 2], String> {
        let cancel = self.cancel;
        let m = self.mixer.as_mut().ok_or("mixer: not open")?;
        let block = m.block;
        let stream = m.stream.as_mut().ok_or_else(|| m.error.clone().unwrap_or_else(|| "mixer: no stream".into()))?;
        let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let mut at = 0usize;
        while at < frames {
            let n = block.min(frames.saturating_sub(at));
            let end = at.saturating_add(n);
            stream.render(&mut l[at..end], &mut r[at..end], cancel).map_err(|e| format!("mixer: {}", e.code()))?;
            at = end;
        }
        m.meters = Some(stream.meters());
        Ok([l, r])
    }

    /// Applies one edit as one undo step and rebuilds the stream.
    pub fn mixer_edit(&mut self, edit: MixerEdit) -> Result<(), String> {
        let result = self.mixer_edit_inner(edit);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    fn mixer_edit_inner(&mut self, edit: MixerEdit) -> Result<(), String> {
        let track_field = |seq: &mut StudioSequence, id: &str| -> Result<usize, String> {
            seq.tracks
                .iter()
                .position(|t| t.track_id == id && matches!(t.kind, TrackKind::Audio | TrackKind::Submix))
                .ok_or_else(|| format!("mixer: no audio or submix track {id}"))
        };
        match edit {
            MixerEdit::Volume { .. } | MixerEdit::Pan { .. } | MixerEdit::Mute { .. } | MixerEdit::Solo { .. } => {
                let mut seq = self.mixer_sequence()?;
                let (op, i) = match &edit {
                    MixerEdit::Volume { track_id, linear } => {
                        let i = track_field(&mut seq, track_id)?;
                        if !linear.is_finite() || *linear < 0.0 || *linear > folio::MAX_AUDIO_LEVEL {
                            return Err(format!("mixer: volume {linear} outside 0..={}", folio::MAX_AUDIO_LEVEL));
                        }
                        seq.tracks[i].volume = *linear;
                        ("mixer_volume", i)
                    }
                    MixerEdit::Pan { track_id, pan } => {
                        let i = track_field(&mut seq, track_id)?;
                        if !pan.is_finite() || !(-1.0..=1.0).contains(pan) {
                            return Err(format!("mixer: pan {pan} outside -1..=1"));
                        }
                        seq.tracks[i].pan = *pan;
                        ("mixer_pan", i)
                    }
                    MixerEdit::Mute { track_id, muted } => {
                        let i = track_field(&mut seq, track_id)?;
                        seq.tracks[i].muted = *muted;
                        ("mixer_mute", i)
                    }
                    MixerEdit::Solo { track_id, soloed } => {
                        let i = track_field(&mut seq, track_id)?;
                        seq.tracks[i].soloed = *soloed;
                        ("mixer_solo", i)
                    }
                    _ => return Err("mixer: unreachable".into()),
                };
                let _ = i;
                if !self.run.submit_sequence(op, seq) {
                    return Err(format!("mixer: {op} refused: {}", self.op_error()));
                }
            }
            other => {
                let mut set = self.mixer_routing_set()?;
                let id = self.mixer.as_ref().ok_or("mixer: not open")?.sequence_id.clone();
                let at = match set.routings.iter().position(|r| r.sequence_id == id) {
                    Some(at) => at,
                    None => {
                        set.routings.push(StudioAudioRouting::new(id.clone()));
                        set.routings.len().saturating_sub(1)
                    }
                };
                let routing = &mut set.routings[at];
                let route = |routing: &mut StudioAudioRouting, track_id: &str| -> usize {
                    match routing.routes.iter().position(|r| r.track_id == track_id) {
                        Some(i) => i,
                        None => {
                            routing.routes.push(StudioTrackRoute { track_id: track_id.to_owned(), output: None, sends: Vec::new() });
                            routing.routes.len().saturating_sub(1)
                        }
                    }
                };
                let op = match other {
                    MixerEdit::Output { track_id, bus } => {
                        let i = route(routing, &track_id);
                        routing.routes[i].output = bus;
                        "mixer_output"
                    }
                    MixerEdit::AddSend { track_id, target, level, pre_fader } => {
                        let i = route(routing, &track_id);
                        let sends = &mut routing.routes[i].sends;
                        let mut n = sends.len();
                        let send_id = loop {
                            let candidate = format!("send-{n}");
                            if sends.iter().all(|s| s.send_id != candidate) {
                                break candidate;
                            }
                            n = n.saturating_add(1);
                        };
                        sends.push(StudioAudioSend { send_id, target, level, pre_fader, pan: 0.0 });
                        "mixer_add_send"
                    }
                    MixerEdit::Send { track_id, send_id, level, pre_fader, pan } => {
                        let i = route(routing, &track_id);
                        let send = routing.routes[i]
                            .sends
                            .iter_mut()
                            .find(|s| s.send_id == send_id)
                            .ok_or_else(|| format!("mixer: no send {send_id} on {track_id}"))?;
                        send.level = level;
                        send.pre_fader = pre_fader;
                        send.pan = pan;
                        "mixer_send"
                    }
                    MixerEdit::RemoveSend { track_id, send_id } => {
                        let i = route(routing, &track_id);
                        let before = routing.routes[i].sends.len();
                        routing.routes[i].sends.retain(|s| s.send_id != send_id);
                        if routing.routes[i].sends.len() == before {
                            return Err(format!("mixer: no send {send_id} on {track_id}"));
                        }
                        "mixer_remove_send"
                    }
                    MixerEdit::MasterVolume { linear } => {
                        routing.master.volume = linear;
                        "mixer_master_volume"
                    }
                    MixerEdit::AddEffect { bus, instance, at } => {
                        let rate = self.mixer.as_ref().map_or(48_000, |m| m.rate);
                        if instance.enabled {
                            bus_effect(&instance, rate).map_err(|e| format!("mixer: effect refused: {}", e.code()))?;
                        }
                        let list = effects_of(routing, bus.as_deref());
                        if list.iter().any(|e| e.instance_id == instance.instance_id) {
                            return Err(format!("mixer: effect id {} exists", instance.instance_id));
                        }
                        let at = at.unwrap_or(list.len()).min(list.len());
                        list.insert(at, instance);
                        "mixer_add_effect"
                    }
                    MixerEdit::RemoveEffect { bus, instance_id } => {
                        let list = effects_of(routing, bus.as_deref());
                        let i = list.iter().position(|e| e.instance_id == instance_id).ok_or_else(|| format!("mixer: no effect {instance_id}"))?;
                        list.remove(i);
                        "mixer_remove_effect"
                    }
                    MixerEdit::MoveEffect { bus, instance_id, to } => {
                        let list = effects_of(routing, bus.as_deref());
                        let i = list.iter().position(|e| e.instance_id == instance_id).ok_or_else(|| format!("mixer: no effect {instance_id}"))?;
                        let e = list.remove(i);
                        let to = to.min(list.len());
                        list.insert(to, e);
                        "mixer_move_effect"
                    }
                    MixerEdit::EffectEnabled { bus, instance_id, enabled } => {
                        let rate = self.mixer.as_ref().map_or(48_000, |m| m.rate);
                        let list = effects_of(routing, bus.as_deref());
                        let e = list.iter_mut().find(|e| e.instance_id == instance_id).ok_or_else(|| format!("mixer: no effect {instance_id}"))?;
                        e.enabled = enabled;
                        if enabled {
                            bus_effect(e, rate).map_err(|x| format!("mixer: effect refused: {}", x.code()))?;
                        }
                        "mixer_effect_enabled"
                    }
                    MixerEdit::EffectParam { bus, instance_id, key, value } => {
                        let rate = self.mixer.as_ref().map_or(48_000, |m| m.rate);
                        let list = effects_of(routing, bus.as_deref());
                        let e = list.iter_mut().find(|e| e.instance_id == instance_id).ok_or_else(|| format!("mixer: no effect {instance_id}"))?;
                        let param = e.params.iter_mut().find(|p| p.key == key).ok_or_else(|| format!("mixer: no parameter {key}"))?;
                        param.value = value;
                        if e.enabled {
                            bus_effect(e, rate).map_err(|x| format!("mixer: effect refused: {}", x.code()))?;
                        }
                        "mixer_effect_param"
                    }
                    _ => return Err("mixer: unreachable".into()),
                };
                let snapshot = self.run.snapshot().ok_or("mixer: no document is open")?;
                folio::validate_audio_routing(&set, snapshot.document(), folio::Budget::default(), &CancellationToken::default())
                    .map_err(|d| format!("mixer: {op} refused: {d:?}"))?;
                let bytes = serde_json::to_vec(&set).map_err(|e| format!("mixer: {e}"))?;
                if !self.run.set_tiles_and_entry(op, "", None, AUDIO_ROUTING_ENTRY, bytes) {
                    return Err(format!("mixer: {op} refused: {}", self.op_error()));
                }
            }
        }
        self.mixer_rebuild()
    }

    /// Undo / redo of the run, followed by a stream rebuild when a mixer is open.
    pub fn mixer_undo(&mut self, undo: bool) -> Result<(), String> {
        let ok = if undo { self.run.undo() } else { self.run.redo() };
        if !ok {
            return Err(format!("mixer: {}", self.op_error()));
        }
        if self.mixer.is_some() { self.mixer_rebuild() } else { Ok(()) }
    }

    /// Structured projection of the mixer (`Null` when closed).
    pub fn mixer_json(&self) -> Value {
        let Some(m) = &self.mixer else { return Value::Null };
        let sequence = self.mixer_sequence().ok();
        let routing = self.mixer_routing().ok();
        let peak = |id: &str| -> Option<[f32; 2]> {
            let meters = m.meters.as_ref()?;
            let from_strips = m
                .strip_tracks
                .iter()
                .zip(&meters.strips)
                .filter(|(t, _)| t.as_deref() == Some(id))
                .map(|(_, p)| *p);
            let from_buses = m.bus_tracks.iter().zip(&meters.buses).filter(|(t, _)| *t == id).map(|(_, p)| *p);
            from_strips.chain(from_buses).reduce(|a, b| [a[0].max(b[0]), a[1].max(b[1])])
        };
        let strips: Vec<Value> = sequence
            .iter()
            .flat_map(|s| s.tracks.iter())
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Submix))
            .map(|t| {
                let route = routing.as_ref().and_then(|r| r.routes.iter().find(|x| x.track_id == t.track_id));
                json!({
                    "track_id": t.track_id,
                    "name": t.name,
                    "kind": if t.kind == TrackKind::Submix { "submix" } else { "audio" },
                    "volume": t.volume,
                    "volume_db": db(t.volume),
                    "pan": t.pan,
                    "muted": t.muted,
                    "soloed": t.soloed,
                    "output": route.and_then(|r| r.output.clone()),
                    "sends": route.map(|r| r.sends.iter().map(|s| json!({"send_id": s.send_id, "target": s.target, "level": s.level, "pre_fader": s.pre_fader, "pan": s.pan})).collect::<Vec<_>>()).unwrap_or_default(),
                    "peak": peak(&t.track_id),
                })
            })
            .collect();
        let meters = m.meters.as_ref();
        let fx_json = |list: &[folio::StudioAudioEffectInstance]| -> Vec<Value> {
            list.iter()
                .map(|e| {
                    json!({
                        "instance_id": e.instance_id,
                        "filter_kind": e.filter_kind,
                        "enabled": e.enabled,
                        "params": e.params.iter().map(|p| json!({"key": p.key, "value": p.value})).collect::<Vec<_>>(),
                        "keyframes": e.keyframes.len(),
                    })
                })
                .collect()
        };
        let strips: Vec<Value> = strips
            .into_iter()
            .map(|mut s| {
                if s["kind"] == "submix" {
                    let id = s["track_id"].as_str().unwrap_or_default().to_owned();
                    let list = routing
                        .as_ref()
                        .and_then(|r| r.bus_effects.iter().find(|b| b.track_id == id))
                        .map(|b| fx_json(&b.effects))
                        .unwrap_or_default();
                    s["effects"] = Value::from(list);
                }
                s
            })
            .collect();
        json!({
            "sequence_id": m.sequence_id,
            "sample_rate": m.rate,
            "position": m.stream.as_ref().map(TimelineBusStream::position),
            "latency": m.stream.as_ref().map(|s| s.latency().total),
            "error": m.error,
            "strips": strips,
            "master": {
                "volume": routing.as_ref().map(|r| r.master.volume),
                "effects": routing.as_ref().map(|r| fx_json(&r.master.effects)).unwrap_or_default(),
                "peak": meters.map(|x| x.master),
                "momentary_lufs": meters.and_then(|x| x.loudness.momentary_lufs),
                "short_term_lufs": meters.and_then(|x| x.loudness.short_term_lufs),
                "integrated_lufs": meters.and_then(|x| x.loudness.integrated_lufs),
            },
        })
    }

    /// The mixer panel (bottom): one strip per audio / submix track and the master strip.
    pub(crate) fn mixer_ui(&mut self, ctx: &egui::Context) {
        if self.mixer.is_none() {
            return;
        }
        let state = self.mixer_json();
        let buses: Vec<String> = state["strips"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["kind"] == "submix")
            .filter_map(|s| s["track_id"].as_str().map(str::to_owned))
            .collect();
        let mut requests: Vec<MixerEdit> = Vec::new();
        let mut drawn: Vec<String> = vec![widget::PANEL.into()];
        egui::TopBottomPanel::bottom(super::widget::id(widget::PANEL)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for strip in state["strips"].as_array().into_iter().flatten() {
                    let id = strip["track_id"].as_str().unwrap_or_default().to_owned();
                    let name = |c: &str| format!("mixer.{id}.{c}");
                    ui.vertical(|ui| {
                        ui.label(strip["name"].as_str().unwrap_or_default());
                        let mut vol_db = strip["volume_db"].as_f64().filter(|v| v.is_finite()).unwrap_or(-96.0);
                        if ui.push_id(super::widget::id(&name(widget::VOLUME)), |ui| ui.add(egui::Slider::new(&mut vol_db, -96.0..=12.0).vertical().suffix(" dB"))).inner.changed() {
                            requests.push(MixerEdit::Volume { track_id: id.clone(), linear: 10f64.powf(vol_db / 20.0) });
                        }
                        let mut pan = strip["pan"].as_f64().unwrap_or(0.0);
                        if ui.push_id(super::widget::id(&name(widget::PAN)), |ui| ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).text("pan"))).inner.changed() {
                            requests.push(MixerEdit::Pan { track_id: id.clone(), pan });
                        }
                        let mut muted = strip["muted"].as_bool().unwrap_or(false);
                        if ui.push_id(super::widget::id(&name(widget::MUTE)), |ui| ui.checkbox(&mut muted, "M")).inner.changed() {
                            requests.push(MixerEdit::Mute { track_id: id.clone(), muted });
                        }
                        let mut soloed = strip["soloed"].as_bool().unwrap_or(false);
                        if strip["kind"] == "audio"
                            && ui.push_id(super::widget::id(&name(widget::SOLO)), |ui| ui.checkbox(&mut soloed, "S")).inner.changed()
                        {
                            requests.push(MixerEdit::Solo { track_id: id.clone(), soloed });
                        }
                        let current = strip["output"].as_str().map(str::to_owned);
                        egui::ComboBox::from_id_salt(super::widget::id(&name(widget::OUTPUT)))
                            .selected_text(current.clone().unwrap_or_else(|| "master".into()))
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(current.is_none(), "master").clicked() && current.is_some() {
                                    requests.push(MixerEdit::Output { track_id: id.clone(), bus: None });
                                }
                                for b in buses.iter().filter(|b| **b != id) {
                                    if ui.selectable_label(current.as_deref() == Some(b.as_str()), b.as_str()).clicked() && current.as_deref() != Some(b.as_str()) {
                                        requests.push(MixerEdit::Output { track_id: id.clone(), bus: Some(b.clone()) });
                                    }
                                }
                            });
                        for send in strip["sends"].as_array().into_iter().flatten() {
                            let send_id = send["send_id"].as_str().unwrap_or_default().to_owned();
                            let mut level = send["level"].as_f64().unwrap_or(0.0);
                            let mut pre = send["pre_fader"].as_bool().unwrap_or(false);
                            let pan = send["pan"].as_f64().unwrap_or(0.0);
                            let key = name(&format!("send.{send_id}"));
                            let changed = ui
                                .push_id(super::widget::id(&key), |ui| {
                                    let a = ui.add(egui::Slider::new(&mut level, 0.0..=folio::MAX_AUDIO_LEVEL).text(send["target"].as_str().unwrap_or_default())).changed();
                                    let b = ui.checkbox(&mut pre, "pre").changed();
                                    a || b
                                })
                                .inner;
                            if changed {
                                requests.push(MixerEdit::Send { track_id: id.clone(), send_id: send_id.clone(), level, pre_fader: pre, pan });
                            }
                            drawn.push(key);
                        }
                        if let Some(first) = buses.iter().find(|b| **b != id)
                            && ui.push_id(super::widget::id(&name(widget::ADD_SEND)), |ui| ui.button("+ send")).inner.clicked()
                        {
                            requests.push(MixerEdit::AddSend { track_id: id.clone(), target: first.clone(), level: 1.0, pre_fader: false });
                        }
                        if strip["kind"] == "submix" {
                            effects_ui(ui, Some(&id), &strip["effects"], &mut requests, &mut drawn);
                        }
                        let p = strip["peak"].as_array().map_or(0.0, |a| a.iter().filter_map(Value::as_f64).fold(0.0, f64::max));
                        ui.push_id(super::widget::id(&name(widget::METER)), |ui| ui.add(egui::ProgressBar::new(p.clamp(0.0, 1.0) as f32).text(format!("{:.1} dBFS", db(p)))));
                        for c in [widget::VOLUME, widget::PAN, widget::MUTE, widget::OUTPUT, widget::METER] {
                            drawn.push(name(c));
                        }
                        if strip["kind"] == "audio" {
                            drawn.push(name(widget::SOLO));
                        }
                        if buses.iter().any(|b| *b != id) {
                            drawn.push(name(widget::ADD_SEND));
                        }
                    });
                    ui.separator();
                }
                ui.vertical(|ui| {
                    ui.label("Master");
                    let master = &state["master"];
                    let mut vol_db = db(master["volume"].as_f64().unwrap_or(1.0)).max(-96.0);
                    if ui.push_id(super::widget::id(widget::MASTER_VOLUME), |ui| ui.add(egui::Slider::new(&mut vol_db, -96.0..=12.0).vertical().suffix(" dB"))).inner.changed() {
                        requests.push(MixerEdit::MasterVolume { linear: 10f64.powf(vol_db / 20.0) });
                    }
                    let p = master["peak"].as_array().map_or(0.0, |a| a.iter().filter_map(Value::as_f64).fold(0.0, f64::max));
                    let lufs = |k: &str| master[k].as_f64().map_or_else(|| "--".to_owned(), |v| format!("{v:.1}"));
                    ui.push_id(super::widget::id(widget::MASTER_METER), |ui| {
                        ui.add(egui::ProgressBar::new(p.clamp(0.0, 1.0) as f32).text(format!("{:.1} dBFS", db(p))));
                        ui.label(format!("M {} S {} I {} LUFS", lufs("momentary_lufs"), lufs("short_term_lufs"), lufs("integrated_lufs")));
                    });
                    drawn.push(widget::MASTER_VOLUME.into());
                    drawn.push(widget::MASTER_METER.into());
                    effects_ui(ui, None, &master["effects"], &mut requests, &mut drawn);
                });
            });
        });
        self.drawn.extend(drawn);
        for edit in requests {
            let _ = self.mixer_edit(edit);
        }
    }
}

/// The effect stack of bus `bus` (`None` = master) in `routing`, created empty when absent.
fn effects_of<'a>(routing: &'a mut StudioAudioRouting, bus: Option<&str>) -> &'a mut Vec<folio::StudioAudioEffectInstance> {
    match bus {
        None => &mut routing.master.effects,
        Some(id) => {
            let i = match routing.bus_effects.iter().position(|b| b.track_id == id) {
                Some(i) => i,
                None => {
                    routing.bus_effects.push(folio::StudioBusEffects { track_id: id.to_owned(), effects: Vec::new() });
                    routing.bus_effects.len().saturating_sub(1)
                }
            };
            &mut routing.bus_effects[i].effects
        }
    }
}

/// One effect stack in the panel: per effect enable, up, down, remove and a drag value per
/// parameter; an add combo with [`effect_templates`]. Widget ids
/// `mixer.<bus|master>.fx.<instance_id>.{enabled,up,down,remove,<param key>}` and the add combo.
fn effects_ui(ui: &mut egui::Ui, bus: Option<&str>, effects: &Value, requests: &mut Vec<MixerEdit>, drawn: &mut Vec<String>) {
    let owner = bus.unwrap_or("master").to_owned();
    let list: Vec<&Value> = effects.as_array().into_iter().flatten().collect();
    let count = list.len();
    for (i, e) in list.iter().enumerate() {
        let iid = e["instance_id"].as_str().unwrap_or_default().to_owned();
        let key = |c: &str| format!("mixer.{owner}.fx.{iid}.{c}");
        ui.push_id(super::widget::id(&key("row")), |ui| {
            ui.horizontal(|ui| {
                let mut enabled = e["enabled"].as_bool().unwrap_or(true);
                if ui.checkbox(&mut enabled, e["filter_kind"].as_str().unwrap_or_default()).changed() {
                    requests.push(MixerEdit::EffectEnabled { bus: bus.map(str::to_owned), instance_id: iid.clone(), enabled });
                }
                if ui.add_enabled(i > 0, egui::Button::new("up")).clicked() {
                    requests.push(MixerEdit::MoveEffect { bus: bus.map(str::to_owned), instance_id: iid.clone(), to: i.saturating_sub(1) });
                }
                if ui.add_enabled(i.saturating_add(1) < count, egui::Button::new("down")).clicked() {
                    requests.push(MixerEdit::MoveEffect { bus: bus.map(str::to_owned), instance_id: iid.clone(), to: i.saturating_add(1) });
                }
                if ui.button("x").clicked() {
                    requests.push(MixerEdit::RemoveEffect { bus: bus.map(str::to_owned), instance_id: iid.clone() });
                }
            });
            for p in e["params"].as_array().into_iter().flatten() {
                let pk = p["key"].as_str().unwrap_or_default().to_owned();
                let mut v = p["value"].as_f64().unwrap_or(0.0);
                if ui.push_id(super::widget::id(&key(&pk)), |ui| ui.add(egui::DragValue::new(&mut v).speed(0.1).prefix(format!("{pk} ")))).inner.changed() {
                    requests.push(MixerEdit::EffectParam { bus: bus.map(str::to_owned), instance_id: iid.clone(), key: pk.clone(), value: v });
                }
                drawn.push(key(&pk));
            }
        });
        for c in ["enabled", "up", "down", "remove"] {
            drawn.push(key(c));
        }
    }
    let add_id = match bus {
        Some(b) => format!("mixer.{b}.{}", widget::ADD_EFFECT),
        None => widget::MASTER_ADD_EFFECT.to_owned(),
    };
    let existing: Vec<folio::StudioAudioEffectInstance> = list
        .iter()
        .map(|e| folio::StudioAudioEffectInstance {
            instance_id: e["instance_id"].as_str().unwrap_or_default().to_owned(),
            filter_kind: String::new(),
            enabled: true,
            params: Vec::new(),
            keyframes: Vec::new(),
        })
        .collect();
    egui::ComboBox::from_id_salt(super::widget::id(&add_id)).selected_text("+ effect").show_ui(ui, |ui| {
        for (name, spec) in effect_templates() {
            if ui.selectable_label(false, name).clicked() {
                requests.push(MixerEdit::AddEffect { bus: bus.map(str::to_owned), instance: spec.to_preset_instance(fresh_effect_id(&existing)), at: None });
            }
        }
    });
    drawn.push(add_id);
}
