//! Lane SMB-PR-ENTRY-REFERENCES-1 (lead decision on F-R56-3): an audio routing that names a clip, a
//! track or a sequence the document does not hold is inert, because nothing that plays looks it up. The
//! oracle is the mix itself: with the stale ids it is the mix without them, sample for sample.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use hsk_studio_score::EffectSlot;

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;
const STALE_CLIP: &str = "SCLP-019abcde-0000-7000-8000-0000000000ee";
const STALE_TRACK: &str = "STRK-019abcde-0000-7000-8000-0000000000ee";

fn source(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}
fn clip(n: u32, source_ref: ClipSource, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref,
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}
fn track(n: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind: TrackKind::Audio,
        index: 0,
        name: format!("T{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 0.75,
        pan: 0.25,
    }
}
fn sequence(id: &str, clips: Vec<StudioClip>) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(4 * F),
        tracks: vec![track(if id == "seq-main" { 1 } else { 2 }, clips)],
        markers: Vec::new(),
        work_area: None,
    }
}

/// "ramp": mono, a different value on every sample, so a stale gain or pan could not hide.
#[derive(Clone)]
struct Sources(std::sync::Arc<Vec<Vec<f32>>>);
impl Sources {
    fn new() -> Self {
        Self(std::sync::Arc::new(vec![(0..8 * SPF).map(|k| (k as f32 / (8 * SPF) as f32) * 0.5).collect()]))
    }
}
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        matches!(s, ClipSource::Media { .. }).then_some(self.0.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(8 * F))
    }
}

/// A live gain envelope, so the routing is doing something a stale id could disturb.
fn live_routing(sequence_id: &str) -> StudioAudioRouting {
    let mut r = StudioAudioRouting::new(sequence_id);
    r.clip_gains.push(StudioClipGainEnvelope {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }, StudioClipGainPoint { offset_ticks: 2 * F, gain_db: -12.0 }],
    });
    r
}
/// The same routing with one stale id of every kind that is inert.
fn with_stale_ids(mut r: StudioAudioRouting) -> StudioAudioRouting {
    r.clip_gains.push(StudioClipGainEnvelope { clip_id: STALE_CLIP.into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -48.0 }] });
    r.clip_pans.push(StudioClipPanEnvelope { clip_id: STALE_CLIP.into(), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: -1.0 }] });
    r.routes.push(StudioTrackRoute { track_id: STALE_TRACK.into(), output: None, sends: vec![] });
    r.bus_effects.push(StudioBusEffects {
        track_id: STALE_TRACK.into(),
        effects: vec![StudioAudioEffectInstance { instance_id: "stale".into(), filter_kind: "lookahead_limiter".into(), enabled: true, params: Default::default(), keyframes: vec![] }],
    });
    r.track_automation.push(StudioTrackAutomation { track_id: STALE_TRACK.into(), param: StudioTrackParam::Volume, points: vec![StudioAutomationPoint { offset_ticks: 0, value: 0.0 }] });
    r
}

/// Nothing here has a live bus, so a factory call means a stale chain was built.
fn no_effects(_: &StudioAudioEffectInstance, _: u32) -> hsk_studio_pulse::Result<EffectSlot> {
    panic!("no effect is live in this mix")
}

fn render(stream: &mut TimelineBusStream, frames: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    let mut at = 0;
    while at < frames {
        let n = 960.min(frames - at);
        stream.render(&mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
        at += n;
    }
    [l, r]
}
fn bus_mix<S: AudioSources + Clone + Send + 'static>(sequence: &StudioSequence, routing: &StudioAudioRouting, sources: S, extents: &dyn SourceExtents) -> [Vec<f32>; 2] {
    let mut factory = no_effects;
    let mut stream = TimelineBusStream::new(sequence, routing, RATE, 960, sources, extents, StreamLimits::default(), &mut factory, &CancellationToken::default()).unwrap();
    render(&mut stream, 4 * SPF)
}

#[test]
fn a_mix_with_stale_ids_equals_the_mix_without_them() {
    let seq = sequence("seq-main", vec![clip(1, source("ramp"), 4)]);
    let clean = live_routing("seq-main");
    let without = bus_mix(&seq, &clean, Sources::new(), &Ext);
    assert!(without[0].iter().any(|v| v.abs() > 0.01) && without[1].iter().any(|v| v.abs() > 0.01), "audible on both channels");
    assert_eq!(bus_mix(&seq, &with_stale_ids(clean.clone()), Sources::new(), &Ext), without, "sample for sample");
    // The live envelope is what the routing does, so this compares a routing that matters.
    assert_ne!(bus_mix(&seq, &StudioAudioRouting::new("seq-main"), Sources::new(), &Ext), without);
}

/// The plain mix takes the envelopes directly: the same answer there.
#[test]
fn the_plain_mix_ignores_a_stale_envelope_too() {
    let token = CancellationToken::default();
    let seq = sequence("seq-main", vec![clip(1, source("ramp"), 4)]);
    let clean = live_routing("seq-main");
    let stale = with_stale_ids(clean.clone());
    let mix = |routing: &StudioAudioRouting| {
        let mut stream = TimelineMixStream::with_clip_envelopes(&seq, RATE, 960, Sources::new(), &Ext, StreamLimits::default(), &routing.clip_gains, &routing.clip_pans, &token).unwrap();
        let (mut l, mut r) = (vec![0.0; 4 * SPF], vec![0.0; 4 * SPF]);
        for at in (0..4 * SPF).step_by(960) {
            stream.render(&mut l[at..at + 960], &mut r[at..at + 960], &token).unwrap();
        }
        [l, r]
    };
    assert_eq!(mix(&stale), mix(&clean));
}

/// A routing of a sequence the document does not hold changes no nested mix, and neither do stale ids
/// in the nested sequence's own routing.
#[test]
fn a_routing_of_an_absent_sequence_changes_no_nested_mix() {
    let token = CancellationToken::default();
    let inner = sequence("seq-inner", vec![clip(1, source("ramp"), 4)]);
    let parent = sequence("seq-main", vec![clip(9, ClipSource::Sequence { sequence_id: "seq-inner".into() }, 4)]);
    let sequences = vec![parent.clone(), inner];
    let clean = StudioAudioRoutingSet { routings: vec![live_routing("seq-inner")] };
    let stale = StudioAudioRoutingSet { routings: vec![with_stale_ids(live_routing("seq-inner")), StudioAudioRouting::new("seq-gone")] };
    let mix = |set: &StudioAudioRoutingSet| {
        let nested = NestedAudioSources::with_routing(&sequences, &parent, Sources::new(), set, RATE, 960, &Ext, StreamLimits::default(), &token).unwrap();
        assert!(nested.reports().is_empty(), "{:?}", nested.reports());
        let extents = NestedExtents::new(&sequences, &Ext);
        bus_mix(&parent, &StudioAudioRouting::new("seq-main"), nested, &extents)
    };
    let without = mix(&clean);
    assert!(without[0].iter().any(|v| v.abs() > 0.01), "audible");
    assert_eq!(mix(&stale), without, "sample for sample");
}

// ----- F-R57-2: a removed bus, against the rendered mix -----

const BUS: u32 = 7;
const BUS_2: u32 = 8;
fn bus_id(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn bus_track(n: u32, volume: f64) -> StudioTrack {
    let mut t = track(n, Vec::new());
    t.kind = TrackKind::Submix;
    t.volume = volume;
    t.pan = 0.0;
    t
}
/// `seq-main` with one audio track of one clip, plus `buses`.
fn with_buses(buses: Vec<StudioTrack>) -> StudioSequence {
    let mut s = sequence("seq-main", vec![clip(1, source("ramp"), 4)]);
    s.tracks.extend(buses);
    s
}
fn audio_track_id() -> String {
    bus_id(1)
}
fn assert_audible(mix: &[Vec<f32>; 2]) {
    assert!(mix[0].iter().any(|v| v.abs() > 0.01) && mix[1].iter().any(|v| v.abs() > 0.01), "audible on both channels");
}

/// A live route's output to a removed bus plays to the master: sample for sample the mix of no route.
#[test]
fn an_output_to_a_removed_bus_plays_to_the_master() {
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes.push(StudioTrackRoute { track_id: audio_track_id(), output: Some(bus_id(BUS)), sends: vec![] });
    let through_bus = bus_mix(&with_buses(vec![bus_track(BUS, 0.25)]), &routing, Sources::new(), &Ext);
    let removed = with_buses(vec![]);
    let to_master = bus_mix(&removed, &StudioAudioRouting::new("seq-main"), Sources::new(), &Ext);
    assert_audible(&to_master);
    assert_ne!(through_bus, to_master, "the bus's fader is what the route was doing");
    assert_eq!(bus_mix(&removed, &routing, Sources::new(), &Ext), to_master, "sample for sample");
}

/// A send to a removed bus is skipped; the track's own output is untouched.
#[test]
fn a_send_to_a_removed_bus_is_skipped() {
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes.push(StudioTrackRoute {
        track_id: audio_track_id(),
        output: None,
        sends: vec![StudioAudioSend { send_id: "s1".into(), target: bus_id(BUS), level: 1.0, pre_fader: false, pan: 0.0 }],
    });
    let with_send = bus_mix(&with_buses(vec![bus_track(BUS, 1.0)]), &routing, Sources::new(), &Ext);
    let removed = with_buses(vec![]);
    let without = bus_mix(&removed, &StudioAudioRouting::new("seq-main"), Sources::new(), &Ext);
    assert_audible(&without);
    assert_ne!(with_send, without, "the send is what the route was doing");
    assert_eq!(bus_mix(&removed, &routing, Sources::new(), &Ext), without, "sample for sample");
}

/// A live bus whose own output names a removed bus plays to the master through its own fader.
#[test]
fn a_bus_routed_to_a_removed_bus_plays_to_the_master() {
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes.push(StudioTrackRoute { track_id: audio_track_id(), output: Some(bus_id(BUS)), sends: vec![] });
    let mut direct = routing.clone();
    routing.routes.push(StudioTrackRoute { track_id: bus_id(BUS), output: Some(bus_id(BUS_2)), sends: vec![] });
    direct.routes.push(StudioTrackRoute { track_id: bus_id(BUS), output: None, sends: vec![] });
    let before = bus_mix(&with_buses(vec![bus_track(BUS, 0.5), bus_track(BUS_2, 0.25)]), &routing, Sources::new(), &Ext);
    let removed = with_buses(vec![bus_track(BUS, 0.5)]);
    let expected = bus_mix(&removed, &direct, Sources::new(), &Ext);
    assert_audible(&expected);
    assert_ne!(before, expected, "the second bus's fader is what the route was doing");
    assert_eq!(bus_mix(&removed, &routing, Sources::new(), &Ext), expected, "sample for sample");
}

/// The bus coming back is live again, so the routing that went inert does what it did.
#[test]
fn a_bus_that_comes_back_is_heard_again() {
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes.push(StudioTrackRoute { track_id: audio_track_id(), output: Some(bus_id(BUS)), sends: vec![] });
    let held = with_buses(vec![bus_track(BUS, 0.25)]);
    let first = bus_mix(&held, &routing, Sources::new(), &Ext);
    let _ = bus_mix(&with_buses(vec![]), &routing, Sources::new(), &Ext);
    assert_eq!(bus_mix(&held, &routing, Sources::new(), &Ext), first);
}

/// A held track that is not a bus is not absent: refused, as before this lane.
#[test]
fn a_held_track_that_is_not_a_bus_stays_refused() {
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes.push(StudioTrackRoute {
        track_id: audio_track_id(),
        output: None,
        sends: vec![StudioAudioSend { send_id: "s1".into(), target: audio_track_id(), level: 1.0, pre_fader: false, pan: 0.0 }],
    });
    let mut factory = no_effects;
    let stream = TimelineBusStream::new(&with_buses(vec![]), &routing, RATE, 960, Sources::new(), &Ext, StreamLimits::default(), &mut factory, &CancellationToken::default());
    assert_eq!(stream.err(), Some(PulseError::InvalidInput));
}
