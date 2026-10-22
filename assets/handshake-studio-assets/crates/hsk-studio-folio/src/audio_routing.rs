//! Audio routing of a sequence (lane SMB-AUDITION-SESSION-2; STU-FX-137 "a sequence declares a master
//! channel configuration, ... submix tracks, sends ... Send: a named routing from a track to a submix
//! with its own level"). The timeline module stores track `volume` and `pan` and the `Submix` track
//! kind but not routing ("track height/routing are not stored yet"); this record stores it beside
//! the document, one entry per sequence:
//!
//! * a route per audio or submix track: its output (a submix track of the same sequence, or the
//!   master when absent) and its sends (target submix track, linear level, pre or post fader, pan);
//! * effects of a submix track (its bus) and of the master, as ordered
//!   [`StudioAudioEffectInstance`]s (the preset instance record, so a stored preset applies as is);
//! * the master volume (linear);
//! * clip gain envelopes: per clip, breakpoints `(offset_ticks from the clip's timeline start,
//!   gain_db)`, linear in dB between points and held outside them (Premiere's clip volume
//!   interpolation UNVERIFIED).
//!
//! A submix track is a bus: its fader and pan are the track's `volume` and `pan`. Folio validates
//! identity, references, the absence of routing cycles, counts and ranges; the mix itself is pulse's
//! and score's. Levels and volumes are linear gains in `0 ..= 10^(12/20)` (score's +12 dB fader top).
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (V4-01), every count is
//! checked before use and charged against the [`Budget`].
#![deny(clippy::arithmetic_side_effects)]
use super::audio_preset::{MAX_AUDIO_PRESET_EFFECTS, instance};
use super::{
    Budget, Code, Result, StudioAudioEffectInstance, StudioDocument, StudioSequence, TrackKind,
    Unresolved, err, key, values::Ctx,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Largest linear level or volume (+12 dB).
pub const MAX_AUDIO_LEVEL: f64 = 3.981_071_705_534_973;
/// Sends one route may hold (score's bound).
pub const MAX_AUDIO_SENDS: usize = 16;
/// Breakpoints one clip gain envelope may hold (score's automation bound).
pub const MAX_CLIP_GAIN_POINTS: usize = 4096;
/// Clip gain range in dB (score's track volume range).
pub const CLIP_GAIN_DB: (f64, f64) = (-96.0, 12.0);
/// Routings one set may hold.
pub const MAX_AUDIO_ROUTINGS: usize = 1024;

/// One send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioSend {
    /// Unique within the route (key grammar).
    pub send_id: String,
    /// A `Submix` track of the sequence.
    pub target: String,
    /// Linear level.
    pub level: f64,
    pub pre_fader: bool,
    /// `-1 ..= 1`.
    pub pan: f64,
}

/// Output and sends of one audio or submix track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackRoute {
    pub track_id: String,
    /// A `Submix` track; absent = the master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<StudioAudioSend>,
}

/// The effect chain of a submix track's bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBusEffects {
    pub track_id: String,
    pub effects: Vec<StudioAudioEffectInstance>,
}

/// The master bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioMaster {
    /// Linear.
    pub volume: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<StudioAudioEffectInstance>,
}

/// One breakpoint of a clip gain envelope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipGainPoint {
    /// Ticks from the clip's `timeline_start`.
    pub offset_ticks: u64,
    pub gain_db: f64,
}

/// The gain envelope of one clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipGainEnvelope {
    pub clip_id: String,
    pub points: Vec<StudioClipGainPoint>,
}

/// The routing of one sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioRouting {
    pub sequence_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<StudioTrackRoute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_effects: Vec<StudioBusEffects>,
    pub master: StudioAudioMaster,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clip_gains: Vec<StudioClipGainEnvelope>,
}

impl StudioAudioRouting {
    /// Everything to the master at unity, no effects, no envelopes.
    pub fn new(sequence_id: impl Into<String>) -> Self {
        Self {
            sequence_id: sequence_id.into(),
            routes: Vec::new(),
            bus_effects: Vec::new(),
            master: StudioAudioMaster { volume: 1.0, effects: Vec::new() },
            clip_gains: Vec::new(),
        }
    }
}

/// The routings of a document (one per sequence at most).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioRoutingSet {
    pub routings: Vec<StudioAudioRouting>,
}

fn invalid(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

fn level(v: f64) -> bool {
    v.is_finite() && (0.0..=MAX_AUDIO_LEVEL).contains(&v)
}

fn effects(list: &[StudioAudioEffectInstance], target: &str, cx: &mut Ctx) -> Result<()> {
    if list.len() > MAX_AUDIO_PRESET_EFFECTS {
        return Err(err(Code::BudgetExceeded, target));
    }
    cx.nodes(list.len(), target)?;
    let mut ids = BTreeSet::new();
    for e in list {
        if !key(&e.instance_id) || !ids.insert(e.instance_id.as_str()) {
            return Err(invalid(target, "effects/instance_id"));
        }
        instance(e, &format!("{target}/{}", e.instance_id), cx)?;
    }
    Ok(())
}

fn routing(r: &StudioAudioRouting, seq: &StudioSequence, cx: &mut Ctx) -> Result<()> {
    let target = format!("audio_routing/{}", r.sequence_id);
    let kinds: BTreeMap<&str, TrackKind> = seq.tracks.iter().map(|t| (t.track_id.as_str(), t.kind)).collect();
    let submix = |id: &str| kinds.get(id) == Some(&TrackKind::Submix);
    cx.nodes(r.routes.len(), &target)?;
    let mut routed = BTreeSet::new();
    // Bus -> bus edges for the cycle check.
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for route in &r.routes {
        cx.tick()?;
        let t = format!("{target}/routes/{}", route.track_id);
        match kinds.get(route.track_id.as_str()) {
            Some(TrackKind::Audio | TrackKind::Submix) => {}
            _ => return Err(err(Code::InvalidId, t)),
        }
        if !routed.insert(route.track_id.as_str()) {
            return Err(err(Code::DuplicateField, t));
        }
        if route.sends.len() > MAX_AUDIO_SENDS {
            return Err(err(Code::BudgetExceeded, t));
        }
        cx.nodes(route.sends.len(), &t)?;
        let mut targets: Vec<&str> = Vec::new();
        if let Some(o) = &route.output {
            if !submix(o) || *o == route.track_id {
                return Err(invalid(&t, "output"));
            }
            targets.push(o);
        }
        let mut send_ids = BTreeSet::new();
        for s in &route.sends {
            if !key(&s.send_id) || !send_ids.insert(s.send_id.as_str()) {
                return Err(invalid(&t, "sends/send_id"));
            }
            if !submix(&s.target) || s.target == route.track_id {
                return Err(invalid(&t, "sends/target"));
            }
            if !level(s.level) {
                return Err(invalid(&t, "sends/level"));
            }
            if !s.pan.is_finite() || !(-1.0..=1.0).contains(&s.pan) {
                return Err(invalid(&t, "sends/pan"));
            }
            targets.push(&s.target);
        }
        if submix(&route.track_id) {
            edges.insert(route.track_id.as_str(), targets);
        }
    }
    // Acyclic bus routing (depth-first, iterative colouring).
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    for start in edges.keys().copied() {
        if state.contains_key(start) {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        state.insert(start, 1);
        while let Some((node, at)) = stack.pop() {
            let next = edges.get(node).and_then(|v| v.get(at)).copied();
            match next {
                None => {
                    state.insert(node, 2);
                }
                Some(n) => {
                    stack.push((node, at.saturating_add(1)));
                    match state.get(n) {
                        Some(1) => return Err(invalid(&target, "routes/cycle")),
                        Some(_) => {}
                        None => {
                            state.insert(n, 1);
                            stack.push((n, 0));
                        }
                    }
                }
            }
        }
    }
    cx.nodes(r.bus_effects.len(), &target)?;
    let mut buses = BTreeSet::new();
    for b in &r.bus_effects {
        let t = format!("{target}/bus_effects/{}", b.track_id);
        if !submix(&b.track_id) {
            return Err(err(Code::InvalidId, t));
        }
        if !buses.insert(b.track_id.as_str()) {
            return Err(err(Code::DuplicateField, t));
        }
        effects(&b.effects, &t, cx)?;
    }
    if !level(r.master.volume) {
        return Err(invalid(&target, "master/volume"));
    }
    effects(&r.master.effects, &format!("{target}/master"), cx)?;
    let clips: BTreeSet<&str> = seq
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .flat_map(|t| t.clips.iter().map(|c| c.clip_id.as_str()))
        .collect();
    cx.nodes(r.clip_gains.len(), &target)?;
    let mut enveloped = BTreeSet::new();
    for g in &r.clip_gains {
        cx.tick()?;
        let t = format!("{target}/clip_gains/{}", g.clip_id);
        if !clips.contains(g.clip_id.as_str()) {
            return Err(err(Code::InvalidId, t));
        }
        if !enveloped.insert(g.clip_id.as_str()) {
            return Err(err(Code::DuplicateField, t));
        }
        if g.points.is_empty() || g.points.len() > MAX_CLIP_GAIN_POINTS {
            return Err(invalid(&t, "points"));
        }
        cx.nodes(g.points.len(), &t)?;
        if g.points.windows(2).any(|w| w[0].offset_ticks >= w[1].offset_ticks) {
            return Err(invalid(&t, "points/offset_ticks"));
        }
        let (lo, hi) = CLIP_GAIN_DB;
        if g.points.iter().any(|p| !p.gain_db.is_finite() || p.gain_db < lo || p.gain_db > hi) {
            return Err(invalid(&t, "points/gain_db"));
        }
    }
    Ok(())
}

/// Validates every routing against `document`: one routing per existing sequence; routes of audio or
/// submix tracks of that sequence, at most one per track, outputs and send targets submix tracks
/// other than the track itself, acyclic bus routing, unique key-grammar send ids, levels and the
/// master volume in `0 ..= MAX_AUDIO_LEVEL`, pans in `-1 ..= 1`; bus effects of submix tracks (one
/// entry each) and master effects validated as preset instances; clip gain envelopes of clips on
/// audio tracks of the sequence, one each, 1..=`MAX_CLIP_GAIN_POINTS` points strictly increasing in
/// `offset_ticks` with `gain_db` in [`CLIP_GAIN_DB`].
pub fn validate_audio_routing(
    set: &StudioAudioRoutingSet,
    document: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    if set.routings.len() > MAX_AUDIO_ROUTINGS {
        return Err(err(Code::BudgetExceeded, "audio_routing"));
    }
    cx.nodes(set.routings.len(), "audio_routing")?;
    let mut seen = BTreeSet::new();
    for r in &set.routings {
        let seq = document
            .sequences
            .iter()
            .find(|s| s.sequence_id == r.sequence_id)
            .ok_or_else(|| err(Code::InvalidId, format!("audio_routing/{}", r.sequence_id)))?;
        if !seen.insert(r.sequence_id.as_str()) {
            return Err(err(Code::DuplicateField, format!("audio_routing/{}", r.sequence_id)));
        }
        routing(r, seq, &mut cx)?;
    }
    Ok(())
}

/// Parses and validates a routing set from wire JSON (shared Folio preflight; duplicate keys are
/// `DuplicateField`, unknown fields and wrong types `InvalidJson`).
pub fn parse_audio_routing(
    input: &[u8],
    document: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioAudioRoutingSet> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "audio_routing",
            )
        })?
        .0;
    let set: StudioAudioRoutingSet = serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "audio_routing"))?;
    validate_audio_routing(&set, document, b, token)?;
    Ok(set)
}
