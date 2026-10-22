//! Lane SMB-AUDITION-SESSION-2 (features `audio-batch`, `audio-device`): the sequence mix graph is
//! stored with the session (Folio audio routing entry), survives save and reopen, builds the bus
//! stream (submix buses with a stored effect, a pre-fader send, a clip gain envelope) and plays it
//! through `audio_out` on score-device's software device (no OS audio; a real device stays
//! NOT_PROVEN for the Operator's attended session). Oracles: hand-computed sample values of constant
//! and impulse clips, and the device loopback against the stream's own render.
#![cfg(all(feature = "audio-batch", feature = "audio-device"))]

mod support_audio;

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::audio_out::AudioOut;
use hsk_studio_harness::view::audio_session::{AUDIO_ROUTING_ENTRY, build_bus_stream};
use hsk_studio_harness::view::monitor::{DropPolicy, FrameSource, ProgramMonitor, SourceFrame};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_score_device::{
    DeviceGrant, DeviceId, DeviceProvider, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions, SoftwareProvider,
    StreamConfig,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

fn id(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn source(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}
fn clip(n: u32, src: &str, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source(src),
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>, volume: f64) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id(n),
        kind,
        index: if kind == TrackKind::Submix { 0 } else { n - 1 },
        name: format!("T{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume,
        pan: 0.0,
    }
}
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(10 * F),
        tracks: vec![
            track(1, TrackKind::Audio, vec![clip(1, "dc", 10)], 0.5),
            track(2, TrackKind::Audio, vec![clip(2, "imp", 10)], 1.0),
            track(3, TrackKind::Submix, vec![], 1.0),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}

#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let n = 12 * SPF;
    let mut imp = vec![0.0; n];
    imp[1000] = 0.5;
    Sources(Arc::new(vec![("dc".into(), vec![vec![0.25; n], vec![0.25; n]]), ("imp".into(), vec![imp])]))
}
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else { return None };
        self.0.iter().find(|(n, _)| n == id).map(|(_, v)| v.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(12 * F))
    }
}

fn routing() -> StudioAudioRoutingSet {
    let mut r = StudioAudioRouting::new("seq-main");
    r.routes = vec![
        StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] },
        StudioTrackRoute { track_id: id(2), output: None, sends: vec![StudioAudioSend { send_id: "s1".into(), target: id(3), level: 0.5, pre_fader: true, pan: 0.0 }] },
    ];
    // A stored limiter preset instance far above the signal: a pure, latent delay on the bus.
    r.bus_effects = vec![StudioBusEffects {
        track_id: id(3),
        effects: vec![StudioAudioEffectInstance {
            instance_id: "fx-0".into(),
            filter_kind: "lookahead_limiter".into(),
            enabled: true,
            params: vec![
                StudioAudioPresetParam { key: "ceiling_db".into(), value: 0.0 },
                StudioAudioPresetParam { key: "lookahead_ms".into(), value: 3.0 },
                StudioAudioPresetParam { key: "release_ms".into(), value: 50.0 },
                StudioAudioPresetParam { key: "true_peak".into(), value: 0.0 },
            ],
            keyframes: vec![],
        }],
    }];
    r.clip_gains = vec![StudioClipGainEnvelope {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }, StudioClipGainPoint { offset_ticks: 5 * F, gain_db: -12.0 }],
    }];
    StudioAudioRoutingSet { routings: vec![r] }
}

fn package(dir: &std::path::Path) -> std::path::PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence()).unwrap()]
    });
    struct Any;
    impl folio::Resolver for Any {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("session.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn render(stream: &mut hsk_studio_pulse::TimelineBusStream, frames: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    for at in (0..frames).step_by(480) {
        let n = 480.min(frames - at);
        stream.render(&mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
    }
    [l, r]
}

#[test]
fn routing_is_saved_with_the_session_reopens_and_plays_through_the_buses() {
    let dir = support_audio::scratch("session");
    let path = package(&dir);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    assert_eq!(v.audio_routing().unwrap(), None);
    // A routing Folio refuses is not stored (send to an audio track).
    let mut bad = routing();
    bad.routings[0].routes[1].sends[0].target = id(1);
    assert!(v.set_audio_routing(&bad).unwrap_err().contains("audio routing"));
    v.set_audio_routing(&routing()).unwrap();
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();

    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    assert_eq!(w.audio_routing().unwrap(), Some(routing()), "the routing reopens with the session");
    let mut stream = w.bus_stream("seq-main", RATE, 480, sources(), &Ext).unwrap();
    let frames = 10 * SPF;
    let [l, r] = render(&mut stream, frames);
    // Hand values: track 1 (0.25, fader 0.5, clip gain 0 -> -12 dB over 5 frames) via the latent
    // bus; track 2 impulse direct (mono centre equal-power) plus its pre-fader send at 0.5.
    let c = std::f64::consts::FRAC_1_SQRT_2;
    for k in [0usize, 1000, 4800, 9600, 15_000] {
        let env = if k < 5 * SPF { -12.0 * k as f64 / (5 * SPF) as f64 } else { -12.0 };
        let a = 0.25 * 0.5 * 10f64.powf(env / 20.0);
        let b = if k == 1000 { 0.5 * c * 1.5 } else { 0.0 };
        assert!((f64::from(l[k]) - (a + b)).abs() < 2e-6, "{k}: {} vs {}", l[k], a + b);
        assert!((f64::from(r[k]) - (a + b)).abs() < 2e-6);
    }
    // The viewer's stream is the one built from the same records directly.
    let seq = sequence();
    let mut direct = build_bus_stream(&seq, &routing().routings[0], RATE, 480, sources(), &Ext, &CancellationToken::default()).unwrap();
    let [dl, _] = render(&mut direct, frames);
    assert_eq!(dl, l);

    // Playback through audio_out on the software device: the device receives the bus mix.
    struct NoFrames;
    impl FrameSource for NoFrames {
        fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
            None
        }
    }
    let token = CancellationToken::default();
    let bus = w.bus_stream("seq-main", RATE, 480, sources(), &Ext).unwrap();
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let grant = DeviceGrant::new(
        ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap(),
        DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap(),
        &[Direction::Output],
        None,
    );
    let config = StreamConfig { sample_rate_hz: RATE, layout: bus.layout(), block_frames: 480, ring_blocks: 4 };
    let device = provider.open_output(&DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap(), &config, Some(&grant), &token).unwrap();
    let mut out = AudioOut::with_buses(bus, device).unwrap();
    assert!(out.mix().buses().is_some() && out.mix().timeline().is_none());
    let mut m = ProgramMonitor::new(seq, 2, 2, DropPolicy::DropLate).unwrap();
    out.command(&mut m, |m, c| m.play(c)).unwrap().unwrap();
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        out.pump(&token).unwrap();
        let _ = m.refresh(&out, &NoFrames, &Ext, &token).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    out.command(&mut m, |m, c| m.pause(c)).unwrap().unwrap();
    let report = out.close();
    assert!(report.reconciles() && !report.device_lost, "{report:?}");
    let played: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[0]).collect();
    let first = played.iter().position(|s| *s != 0.0).expect("audio reached the device");
    let n = played[first..].iter().take_while(|s| **s != 0.0).count().min(frames);
    assert!(n >= 480 * 10, "only {n} samples played");
    for i in 0..n {
        assert_eq!(played[first + i].to_bits(), l[i].to_bits(), "device sample {i}");
    }
    assert_eq!(AUDIO_ROUTING_ENTRY, "audio_routing");
}

/// Lane SMB-AUDITION-UI: keyframes of a stored bus effect instance become automation lanes of the
/// bus stage (offsets in ticks from the sequence start; linear between keyframes): a gain keyed from
/// 0 dB to -12 dB over 5 frames on the submix gives the hand-computed ramp; a keyed parameter score
/// cannot automate is refused.
#[test]
fn bus_effect_keyframes_from_stored_instances_automate_the_bus() {
    let mut set = routing();
    let r = &mut set.routings[0];
    r.clip_gains.clear();
    r.routes[1].sends.clear();
    r.bus_effects[0].effects = vec![StudioAudioEffectInstance {
        instance_id: "fx-gain".into(),
        filter_kind: "gain".into(),
        enabled: true,
        params: vec![StudioAudioPresetParam { key: "db".into(), value: 0.0 }],
        keyframes: vec![
            StudioAudioPresetKeyframe { key: "db".into(), offset_ticks: 0, value: 0.0 },
            StudioAudioPresetKeyframe { key: "db".into(), offset_ticks: 5 * F, value: -12.0 },
        ],
    }];
    let seq = sequence();
    let token = CancellationToken::default();
    let mut stream = build_bus_stream(&seq, &set.routings[0], RATE, 480, sources(), &Ext, &token).unwrap();
    let [l, _] = render(&mut stream, 10 * SPF);
    for k in [0usize, 4800, 9599, 9600, 15_000] {
        let db = if k < 5 * SPF { -12.0 * k as f64 / (5 * SPF) as f64 } else { -12.0 };
        let a = 0.25 * 0.5 * 10f64.powf(db / 20.0);
        let b = if k == 1000 { 0.5 * std::f64::consts::FRAC_1_SQRT_2 } else { 0.0 };
        assert!((f64::from(l[k]) - (a + b)).abs() < 2e-6, "{k}: {} vs {}", l[k], a + b);
    }
    let mut bad = set.routings[0].clone();
    bad.bus_effects[0].effects = vec![StudioAudioEffectInstance {
        instance_id: "fx-lim".into(),
        filter_kind: "lookahead_limiter".into(),
        enabled: true,
        params: vec![
            StudioAudioPresetParam { key: "ceiling_db".into(), value: -1.0 },
            StudioAudioPresetParam { key: "lookahead_ms".into(), value: 3.0 },
            StudioAudioPresetParam { key: "release_ms".into(), value: 50.0 },
            StudioAudioPresetParam { key: "true_peak".into(), value: 0.0 },
        ],
        keyframes: vec![StudioAudioPresetKeyframe { key: "lookahead_ms".into(), offset_ticks: 0, value: 4.0 }],
    }];
    assert!(build_bus_stream(&seq, &bad, RATE, 480, sources(), &Ext, &token).is_err());
}
