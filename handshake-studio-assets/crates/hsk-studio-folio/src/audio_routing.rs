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
//!   interpolation UNVERIFIED);
//! * track automation (lane SMB-AU-MULTITRACK-2, STU-FX-137 "Track | Carries ... volume (default 1.0,
//!   linear gain), pan (default 0) ... keyframe_mode"): per audio or submix track and parameter
//!   ([`StudioTrackParam`] `volume` | `pan`) breakpoints `(offset_ticks from the sequence start,
//!   value)`, volume a linear gain in `0 ..= MAX_AUDIO_LEVEL` (interpolated linearly in dB by the
//!   mix, as the fader), pan in `-1 ..= 1` (linear); held outside the points. Where a track has a
//!   lane, the lane replaces the track's static `volume` / `pan` in the mix. Absent (no entry) the
//!   routing serializes as before, byte for byte;
//! * clip pan envelopes (lane SMB-AU-MULTITRACK-3): per clip, breakpoints `(offset_ticks from the
//!   clip's timeline start, pan)` with pan in `-1 ..= 1`, linear between points and held outside
//!   them, beside the clip gain envelopes and kept like them;
//! * master volume automation (lane SMB-AU-MULTITRACK-3, [`StudioAudioMaster::volume_automation`]):
//!   breakpoints `(offset_ticks from the sequence start, linear volume)` like a track volume lane;
//!   where present it replaces the static master `volume` in the mix.
//!
//! Absent members (no clip pan envelope, no master lane) are not encoded, so earlier routings
//! serialize byte for byte.
//!
//! A submix track is a bus: its fader and pan are the track's `volume` and `pan`. Folio validates
//! identity, references, the absence of routing cycles, counts and ranges; the mix itself is pulse's
//! and score's. Levels and volumes are linear gains in `0 ..= 10^(12/20)` (score's +12 dB fader top).
//!
//! An id the document does not hold is inert, not invalid (lead decision on R56 F-R56-3, the rule of
//! the track display records; vendor parity UNVERIFIED, D5): a routing of an absent sequence, and in
//! any routing a route, bus effect chain or automation lane of an absent track and an envelope of an
//! absent clip. Only a well-formed id is inert: an absent sequence id keeps the key grammar, an absent
//! track or clip id the domain id grammar with its own prefix, and so do the output and send targets of
//! an inert route; a malformed one is `InvalidId` (tests/audio_routing_records.rs). An inert member is
//! kept, still checked for what it holds (uniqueness, counts, ranges, its budget), plays nothing because
//! the mix looks these up from the tracks and clips it plays (pulse tests/inert_routing_mix.rs), and is
//! live again when the id comes back (an_inert_member_is_live_again_when_its_track_or_clip_exists). A stale
//! id does not make the set unreadable for another sequence. Listed by [`inert_audio_routing_references`].
//! The output or send target of a live route naming an absent bus is inert too (lead reading on R57
//! F-R57-2, SD-64, spec owner decides; tests/audio_routing_records.rs
//! a_live_route_naming_an_absent_bus_is_inert_and_listed): the route plays to the master, a missing send
//! is skipped (pulse tests/inert_routing_mix.rs). It is not refused at a commit, because a host commits
//! the sequence before its entry and a refusal would block a step that removes a bus and repoints the
//! routing together. What stays refused is an id held with the wrong kind, a clip on a video track or an
//! output to an audio track, and a cycle among held buses, also when the id only now came back: that
//! read still refuses the whole set.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (V4-01), every count is
//! checked before use and charged against the [`Budget`].
#![deny(clippy::arithmetic_side_effects)]
use super::audio_preset::{MAX_AUDIO_PRESET_EFFECTS, instance};
use super::{
    Budget, Code, Result, StudioAudioEffectInstance, StudioDocument, StudioSequence, TrackKind,
    Unresolved, err, id, key, values::Ctx,
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
/// Breakpoints one track automation lane may hold (score's automation bound).
pub const MAX_TRACK_AUTOMATION_POINTS: usize = 4096;
/// Breakpoints one clip pan envelope may hold (score's automation bound).
pub const MAX_CLIP_PAN_POINTS: usize = 4096;

/// One send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioSend {
    /// Unique within the route (key grammar).
    pub send_id: String,
    /// A `Submix` track of the sequence; one it does not hold is inert and the send is skipped.
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
    /// A `Submix` track; absent = the master, and a track the sequence does not hold plays to the master.
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
    /// Master volume automation (lane SMB-AU-MULTITRACK-3): offsets from the sequence start, linear
    /// volumes in `0 ..= MAX_AUDIO_LEVEL`, strictly increasing; empty = none (the static `volume`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_automation: Vec<StudioAutomationPoint>,
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

/// One breakpoint of a clip pan envelope (lane SMB-AU-MULTITRACK-3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipPanPoint {
    /// Ticks from the clip's `timeline_start`.
    pub offset_ticks: u64,
    /// `-1 ..= 1` (left to right).
    pub pan: f64,
}

/// The pan envelope of one clip (lane SMB-AU-MULTITRACK-3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipPanEnvelope {
    pub clip_id: String,
    pub points: Vec<StudioClipPanPoint>,
}

/// The track parameter a [`StudioTrackAutomation`] lane automates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTrackParam {
    /// The fader: values are linear gains like `StudioTrack::volume`.
    Volume,
    /// `-1 ..= 1` like `StudioTrack::pan`.
    Pan,
}

impl StudioTrackParam {
    /// Inclusive value range.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::Volume => (0.0, MAX_AUDIO_LEVEL),
            Self::Pan => (-1.0, 1.0),
        }
    }
}

/// One breakpoint of a track automation lane.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAutomationPoint {
    /// Ticks from the sequence start.
    pub offset_ticks: u64,
    pub value: f64,
}

/// The automation of one parameter of one audio or submix track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackAutomation {
    pub track_id: String,
    pub param: StudioTrackParam,
    pub points: Vec<StudioAutomationPoint>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_automation: Vec<StudioTrackAutomation>,
    /// Clip pan envelopes (lane SMB-AU-MULTITRACK-3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clip_pans: Vec<StudioClipPanEnvelope>,
}

impl StudioAudioRouting {
    /// Everything to the master at unity, no effects, no envelopes.
    pub fn new(sequence_id: impl Into<String>) -> Self {
        Self {
            sequence_id: sequence_id.into(),
            routes: Vec::new(),
            bus_effects: Vec::new(),
            master: StudioAudioMaster { volume: 1.0, effects: Vec::new(), volume_automation: Vec::new() },
            clip_gains: Vec::new(),
            track_automation: Vec::new(),
            clip_pans: Vec::new(),
        }
    }

    /// The lane of `param` on `track_id`, if any.
    pub fn track_lane(&self, track_id: &str, param: StudioTrackParam) -> Option<&StudioTrackAutomation> {
        self.track_automation.iter().find(|a| a.track_id == track_id && a.param == param)
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

/// `seq` is `None` for the routing of an absent sequence: every member is then inert.
fn routing(r: &StudioAudioRouting, seq: Option<&StudioSequence>, cx: &mut Ctx) -> Result<()> {
    let target = format!("audio_routing/{}", r.sequence_id);
    let tracks = || seq.iter().flat_map(|s| s.tracks.iter());
    let kinds: BTreeMap<&str, TrackKind> = tracks().map(|t| (t.track_id.as_str(), t.kind)).collect();
    let submix = |id: &str| kinds.get(id) == Some(&TrackKind::Submix);
    cx.nodes(r.routes.len(), &target)?;
    let mut routed = BTreeSet::new();
    // Bus -> bus edges for the cycle check.
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for route in &r.routes {
        cx.tick()?;
        let t = format!("{target}/routes/{}", route.track_id);
        let live = match kinds.get(route.track_id.as_str()) {
            Some(TrackKind::Audio | TrackKind::Submix) => true,
            Some(_) => return Err(err(Code::InvalidId, t)),
            None => {
                id(&route.track_id, "STRK", &t)?;
                false
            }
        };
        if !routed.insert(route.track_id.as_str()) {
            return Err(err(Code::DuplicateField, t));
        }
        if route.sends.len() > MAX_AUDIO_SENDS {
            return Err(err(Code::BudgetExceeded, t));
        }
        cx.nodes(route.sends.len(), &t)?;
        let mut targets: Vec<&str> = Vec::new();
        if let Some(o) = &route.output {
            if !live || !kinds.contains_key(o.as_str()) {
                id(o, "STRK", &format!("{t}/output"))?;
            } else if !submix(o) || *o == route.track_id {
                return Err(invalid(&t, "output"));
            } else {
                targets.push(o);
            }
        }
        let mut send_ids = BTreeSet::new();
        for s in &route.sends {
            if !key(&s.send_id) || !send_ids.insert(s.send_id.as_str()) {
                return Err(invalid(&t, "sends/send_id"));
            }
            let held_target = live && kinds.contains_key(s.target.as_str());
            if !held_target {
                id(&s.target, "STRK", &format!("{t}/sends/target"))?;
            } else if !submix(&s.target) || s.target == route.track_id {
                return Err(invalid(&t, "sends/target"));
            }
            if !level(s.level) {
                return Err(invalid(&t, "sends/level"));
            }
            if !s.pan.is_finite() || !(-1.0..=1.0).contains(&s.pan) {
                return Err(invalid(&t, "sends/pan"));
            }
            if held_target {
                targets.push(&s.target);
            }
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
        if !kinds.contains_key(b.track_id.as_str()) {
            id(&b.track_id, "STRK", &t)?;
        } else if !submix(&b.track_id) {
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
    let m = &r.master.volume_automation;
    if m.len() > MAX_TRACK_AUTOMATION_POINTS {
        return Err(invalid(&target, "master/volume_automation"));
    }
    cx.nodes(m.len(), &target)?;
    if m.windows(2).any(|w| w[0].offset_ticks >= w[1].offset_ticks) {
        return Err(invalid(&target, "master/volume_automation/offset_ticks"));
    }
    if m.iter().any(|p| !level(p.value)) {
        return Err(invalid(&target, "master/volume_automation/value"));
    }
    let not_audio: BTreeSet<&str> = tracks()
        .filter(|t| t.kind != TrackKind::Audio)
        .flat_map(|t| t.clips.iter().map(|c| c.clip_id.as_str()))
        .collect();
    let held_clips: BTreeSet<&str> = tracks().flat_map(|t| t.clips.iter().map(|c| c.clip_id.as_str())).collect();
    cx.nodes(r.clip_gains.len(), &target)?;
    let mut enveloped = BTreeSet::new();
    for g in &r.clip_gains {
        cx.tick()?;
        let t = format!("{target}/clip_gains/{}", g.clip_id);
        if !held_clips.contains(g.clip_id.as_str()) {
            id(&g.clip_id, "SCLP", &t)?;
        }
        if not_audio.contains(g.clip_id.as_str()) {
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
    cx.nodes(r.track_automation.len(), &target)?;
    let mut automated = BTreeSet::new();
    for a in &r.track_automation {
        cx.tick()?;
        let t = format!("{target}/track_automation/{}", a.track_id);
        match kinds.get(a.track_id.as_str()) {
            None => id(&a.track_id, "STRK", &t)?,
            Some(TrackKind::Audio | TrackKind::Submix) => {}
            Some(_) => return Err(err(Code::InvalidId, t)),
        }
        if !automated.insert((a.track_id.as_str(), a.param)) {
            return Err(err(Code::DuplicateField, t));
        }
        if a.points.is_empty() || a.points.len() > MAX_TRACK_AUTOMATION_POINTS {
            return Err(invalid(&t, "points"));
        }
        cx.nodes(a.points.len(), &t)?;
        if a.points.windows(2).any(|w| w[0].offset_ticks >= w[1].offset_ticks) {
            return Err(invalid(&t, "points/offset_ticks"));
        }
        let (lo, hi) = a.param.range();
        if a.points.iter().any(|p| !p.value.is_finite() || p.value < lo || p.value > hi) {
            return Err(invalid(&t, "points/value"));
        }
    }
    cx.nodes(r.clip_pans.len(), &target)?;
    let mut panned = BTreeSet::new();
    for g in &r.clip_pans {
        cx.tick()?;
        let t = format!("{target}/clip_pans/{}", g.clip_id);
        if !held_clips.contains(g.clip_id.as_str()) {
            id(&g.clip_id, "SCLP", &t)?;
        }
        if not_audio.contains(g.clip_id.as_str()) {
            return Err(err(Code::InvalidId, t));
        }
        if !panned.insert(g.clip_id.as_str()) {
            return Err(err(Code::DuplicateField, t));
        }
        if g.points.is_empty() || g.points.len() > MAX_CLIP_PAN_POINTS {
            return Err(invalid(&t, "points"));
        }
        cx.nodes(g.points.len(), &t)?;
        if g.points.windows(2).any(|w| w[0].offset_ticks >= w[1].offset_ticks) {
            return Err(invalid(&t, "points/offset_ticks"));
        }
        if g.points.iter().any(|p| !p.pan.is_finite() || !(-1.0..=1.0).contains(&p.pan)) {
            return Err(invalid(&t, "points/pan"));
        }
    }
    Ok(())
}

/// What a [`StudioInertRoutingReference`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StudioInertRoutingKind {
    /// A whole routing: its sequence is not in the document.
    Routing,
    /// A route of a track its sequence does not hold.
    Route,
    /// The output of a live route names a bus its sequence does not hold: the route plays to the master.
    RouteOutput,
    /// A send of a live route names a bus its sequence does not hold: the send is skipped.
    RouteSend,
    /// A bus effect chain of a track its sequence does not hold.
    BusEffects,
    /// An automation lane of a track its sequence does not hold.
    TrackAutomation,
    /// A gain envelope of a clip its sequence does not hold.
    ClipGain,
    /// A pan envelope of a clip its sequence does not hold.
    ClipPan,
}

/// One id a routing names that the document does not hold: an opaque id and a kind, no values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudioInertRoutingReference {
    /// The routing's sequence.
    pub sequence_id: String,
    pub kind: StudioInertRoutingKind,
    /// The absent id: the sequence, track or clip the kind names.
    pub id: String,
}

/// The inert references of `set` against `document`, in stored order.
pub fn inert_audio_routing_references(set: &StudioAudioRoutingSet, document: &StudioDocument) -> Vec<StudioInertRoutingReference> {
    use StudioInertRoutingKind as K;
    let mut out = Vec::new();
    for r in &set.routings {
        let mut push = |kind, id: &str| out.push(StudioInertRoutingReference { sequence_id: r.sequence_id.clone(), kind, id: id.to_owned() });
        let Some(seq) = document.sequences.iter().find(|s| s.sequence_id == r.sequence_id) else {
            push(K::Routing, &r.sequence_id);
            continue;
        };
        // Sets: a hostile routing holds as many ids as the node budget allows.
        let tracks: BTreeSet<&str> = seq.tracks.iter().map(|t| t.track_id.as_str()).collect();
        let clips: BTreeSet<&str> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.clip_id.as_str()).collect();
        let track = |id: &str| tracks.contains(id);
        let clip = |id: &str| clips.contains(id);
        r.routes.iter().filter(|x| !track(&x.track_id)).for_each(|x| push(K::Route, &x.track_id));
        for route in r.routes.iter().filter(|x| track(&x.track_id)) {
            route.sends.iter().filter(|s| !track(&s.target)).for_each(|s| push(K::RouteSend, &s.target));
            route.output.iter().filter(|o| !track(o)).for_each(|o| push(K::RouteOutput, o));
        }
        r.bus_effects.iter().filter(|x| !track(&x.track_id)).for_each(|x| push(K::BusEffects, &x.track_id));
        r.track_automation.iter().filter(|x| !track(&x.track_id)).for_each(|x| push(K::TrackAutomation, &x.track_id));
        r.clip_gains.iter().filter(|x| !clip(&x.clip_id)).for_each(|x| push(K::ClipGain, &x.clip_id));
        r.clip_pans.iter().filter(|x| !clip(&x.clip_id)).for_each(|x| push(K::ClipPan, &x.clip_id));
    }
    out
}

/// Validates every routing against `document`: one routing per sequence id, key grammar; routes of
/// audio or submix tracks of that sequence, at most one per track, held outputs and send targets of a live
/// route submix tracks other than the track itself, acyclic bus routing, unique key-grammar send ids,
/// levels and the master volume in `0 ..= MAX_AUDIO_LEVEL`, pans in `-1 ..= 1`; bus effects of submix
/// tracks (one entry each) and master effects validated as preset instances; clip gain envelopes of
/// clips on audio tracks of the sequence, one each, 1..=`MAX_CLIP_GAIN_POINTS` points strictly increasing
/// in `offset_ticks` with `gain_db` in [`CLIP_GAIN_DB`]; track automation lanes of audio or submix
/// tracks of the sequence (other kinds of track `InvalidId`), one per track and parameter
/// (`DuplicateField`), 1..=`MAX_TRACK_AUTOMATION_POINTS` points strictly increasing in `offset_ticks`
/// with values in [`StudioTrackParam::range`] (`InvalidField`); master volume automation at most
/// `MAX_TRACK_AUTOMATION_POINTS` points strictly increasing with values in `0 ..= MAX_AUDIO_LEVEL`
/// (`InvalidField`); clip pan envelopes like clip gain envelopes (clips on audio tracks, one each,
/// 1..=`MAX_CLIP_PAN_POINTS` points strictly increasing, pan in `-1 ..= 1`). An id the document does not
/// hold is inert (module docs), an output or send target of a live route included: it must be well-formed
/// (`InvalidId`), the checks above that need the id's owner are skipped, the rest apply.
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
        let seq = document.sequences.iter().find(|s| s.sequence_id == r.sequence_id);
        if seq.is_none() && !key(&r.sequence_id) {
            return Err(err(Code::InvalidId, format!("audio_routing/{}", r.sequence_id)));
        }
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
