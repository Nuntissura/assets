//! Lane SMB-R60-FOLLOWUPS-1 items 3a and 3b (validation round R60 attempt 2 probes a1, a2) and review F3 of
//! c43f2d90 `export_audio` (features `audio-batch`, `audio-device`): the routed mixdown, the stem export and
//! the open audio file's mixdown admit through the viewer's bound JobPort before rendering. A saturated port
//! refuses each typed `overloaded`, is asked exactly once, and no file is written. The sequence fixture is
//! tests/audio_out_mix.rs's. Oracles: a port double counting every admit; the output folder's listing.
#![cfg(all(feature = "audio-batch", feature = "audio-device"))]

mod support_audio;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::audio_import::MixFormat;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

/// Port double: refuses every admit; counts asks.
#[derive(Default)]
struct Saturated {
    asks: AtomicU64,
}
impl JobPort for Saturated {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Err(JobRefused { retry_after_ms: 25 })
    }
    fn release(&self, _ticket: JobTicket) {}
}
impl Saturated {
    fn asks(&self) -> u64 {
        self.asks.load(Ordering::SeqCst)
    }
}

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
        retime: Default::default(),
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
/// Track 1 (dc, fader 0.5) and track 2 (an impulse) over the submix track 3.
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(10 * F),
        tracks: vec![track(1, TrackKind::Audio, vec![clip(1, "dc", 10)], 0.5), track(2, TrackKind::Audio, vec![clip(2, "imp", 10)], 1.0), track(3, TrackKind::Submix, vec![], 1.0)],
        markers: Vec::new(),
        work_area: None,
    }
}

#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let mut imp = vec![0.0; 12 * SPF];
    imp[1000] = 0.5;
    Sources(Arc::new(vec![("dc".into(), vec![vec![0.25; 12 * SPF], vec![0.25; 12 * SPF]]), ("imp".into(), vec![imp])]))
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

/// Track 1 to the submix, track 2 straight to the master.
fn routing() -> StudioAudioRoutingSet {
    let mut r = StudioAudioRouting::new("seq-main");
    r.routes = vec![StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] }, StudioTrackRoute { track_id: id(2), output: None, sends: vec![] }];
    StudioAudioRoutingSet { routings: vec![r] }
}

fn package(dir: &Path) -> PathBuf {
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

/// A viewer on the sequence session, sources bound, on a saturated port; the empty output folder.
fn session(label: &str) -> (Viewer, Arc<Saturated>, PathBuf) {
    let dir = support_audio::scratch(label);
    let path = package(&dir);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.set_audio_routing(&routing()).unwrap();
    v.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    let port = Arc::new(Saturated::default());
    v.set_job_port(port.clone());
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    (v, port, out)
}

fn listing(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

/// Item 3a (probe a1).
#[test]
fn r60_a1_mixdown_export_is_admitted_through_the_bound_job_port() {
    let (mut v, port, out) = session("r60-a1");
    let refused = v.export_sequence_mix("seq-main", MixFormat::WavFloat, &out.join("mix.wav"), false).expect_err("a saturated port refuses the mixdown");
    assert_eq!(refused.code, "overloaded", "{}", refused.detail);
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}

/// Item 3b (probe a2).
#[test]
fn r60_a2_stem_export_is_admitted_through_the_bound_job_port() {
    let (mut v, port, out) = session("r60-a2");
    let refused = v.export_sequence_stems("seq-main", &[id(3)], true, MixFormat::WavFloat, &out, false).expect_err("a saturated port refuses the stems");
    assert_eq!(refused.code, "overloaded", "{}", refused.detail);
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}

/// Review F3 of c43f2d90: the open audio file's mixdown (`export_audio`).
#[test]
fn r60_f3_audio_file_export_is_admitted_through_the_bound_job_port() {
    let dir = support_audio::scratch("r60-f3-audio");
    let samples: Vec<f32> = (0..9600).map(|i| if i % 2 == 0 { 0.25 } else { -0.25 }).collect();
    let wav = dir.join("tone.wav");
    std::fs::write(&wav, support_audio::wav(2, RATE, support_audio::WavFormat::Float32, None, &samples)).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&wav).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    let port = Arc::new(Saturated::default());
    v.set_job_port(port.clone());
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let refused = v.export_audio(&out.join("tone-mix.wav"), MixFormat::WavFloat, false).expect_err("a saturated port refuses the export");
    assert!(refused.contains("overloaded"), "typed: {refused}");
    assert_eq!(port.asks(), 1, "asked exactly once");
    assert!(listing(&out).is_empty(), "no file: {:?}", listing(&out));
}
