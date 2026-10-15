//! Viewer mixer and routing editor (lane SMB-AUDITION-UI; feature `audio-batch`). One strip per
//! audio and submix track of a sequence (fader, pan, mute, solo, output, sends, peak meter) and a
//! master strip (volume, peak and loudness meters). Every edit is ONE undo step:
//!
//! - fader (`volume`, linear; the panel shows dB), `pan`, `muted`, `soloed` are Folio track fields:
//!   the edited sequence goes through the session's timeline step (`Run::submit_sequence`), undone
//!   and redone by its inverse;
//! - output, sends (target, level, pre/post, pan) and the master volume live in the routing entry
//!   (module [`super::audio_session`]): an entry-only history step (`Run::set_tiles_and_entry`).
//!
//! Playback / metering: [`super::Viewer::open_mixer`] binds the mixer to a sequence and an audio
//! source port; after every edit, undo and redo the pulse bus stream is rebuilt from the current
//! document and routing and moved back to the play position (exact seek). [`super::Viewer::mixer_render`]
//! renders the next frames (a host feeds them to `audio_out`, tests compare them) and updates the
//! meters: per track the post-fader peak over its strips, per bus the post-fader peak, the master
//! peak and the master loudness (score's BS.1770 meter: momentary, short-term, integrated).
#![deny(clippy::arithmetic_side_effects)]

use super::audio_session::{AUDIO_ROUTING_ENTRY, build_bus_stream};
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
        json!({
            "sequence_id": m.sequence_id,
            "sample_rate": m.rate,
            "position": m.stream.as_ref().map(TimelineBusStream::position),
            "latency": m.stream.as_ref().map(|s| s.latency().total),
            "error": m.error,
            "strips": strips,
            "master": {
                "volume": routing.as_ref().map(|r| r.master.volume),
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
                });
            });
        });
        self.drawn.extend(drawn);
        for edit in requests {
            let _ = self.mixer_edit(edit);
        }
    }
}
