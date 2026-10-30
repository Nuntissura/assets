//! Lane SMB-PR-SEQ-EXPORT-UI-1 slice 3 (feature `media`; round unit hsk-studio-harness `["media"]`):
//! `export-sequence PACKAGE --sequence ID --preset ID [--in T --out T] --output OUT [--overwrite]
//! [--media FILE]... [--width W --height H] [--json]` opens the package headlessly and drives the same
//! `Viewer::export_sequence` the window's Export Media runs. Media: reel's committed DNxHR LB fixture
//! (ffmpeg-made, 264x120, 25 fps, 3 frames; provenance in `hsk-studio-reel/tests/fixtures/dnx/`),
//! read through the crate tree, never a machine path; every file lives under `std::env::temp_dir()`.
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::cli;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::timeline::ExportSequenceRequest;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 25 fps ticks per frame; 48 kHz has 1920 samples per frame.
const F: u64 = 10_160_640_000;
const DNX: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/dnx/dnxhr-lb-264x120.mov");
const PRESET: &str = "studio.dnxhr.hq.mov";

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-cli-export-sequence-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// The fixture's clip source as `Viewer::bind_program_media` names it.
fn dnx_source() -> ClipSource {
    let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(DNX), 1 << 20, &CancellationToken::default()).unwrap();
    let hex = hsk_studio_reel::hex(&digest);
    ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None }
}

/// `seq-main`: V1 plays the fixture's 3 frames at timeline frames [0, 3); no audio clip.
fn package(dir: &Path) -> PathBuf {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: dnx_source(),
        source_in: Ticks::new(0),
        source_out: Ticks::new(3 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(3 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
    let sequence = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(3 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000009","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(&sequence).unwrap()]
    });
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
    let path = dir.join("sequence.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn cli(args: &[&str]) -> (i32, Value) {
    let out = cli::run(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or_else(|_| panic!("JSON: {}", out.stdout)))
}

/// The CLI's result for the saved package equals `Viewer::export_sequence` on the same package
/// (same receipt JSON: counts, recipe, encoder, muxer, the output's SHA-256); the whole sequence
/// [0, 3) is 3 frames and 3 x 1920 = 5760 samples (hand-computed; no audio clip, so the mix is the
/// timeline's silence); exit 0.
#[test]
fn export_sequence_cli_gives_the_receipt_of_the_headless_call() {
    let s = Scratch::new("parity");
    let pkg = package(&s.0);
    let media = s.path("dnxhr-lb.mov");
    std::fs::write(&media, DNX).unwrap();
    // Headless.
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&pkg).unwrap();
    v.bind_program_media("seq-main", vec![DNX.to_vec()], 64, 32).unwrap();
    let mut request = ExportSequenceRequest::new(PRESET, s.0.join("headless.mov"));
    request.width = 64;
    request.height = 32;
    let headless = v.export_sequence("seq-main", request).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    assert_eq!(headless["receipt"]["outcome"], "complete", "{headless}");
    assert_eq!(headless["receipt"]["frames_encoded"].as_u64(), Some(3), "{headless}");
    assert_eq!(headless["receipt"]["audio_frames"].as_u64(), Some(3 * 1920), "{headless}");
    // CLI.
    let out = s.path("cli.mov");
    let (exit, json) = cli(&["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--output", &out, "--media", &media, "--width", "64", "--height", "32", "--json"]);
    assert_eq!(exit, 0, "{json}");
    assert_eq!(json["verdict"], "pass");
    assert_eq!(json["export"]["receipt"], headless["receipt"], "same receipt JSON");
    for key in ["sequence_id", "preset_id", "in_tick", "out_tick", "frames", "audio_frames", "width", "height"] {
        assert_eq!(json["export"][key], headless[key], "{key}");
    }
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(s.0.join("headless.mov")).unwrap(), "byte-identical files");
}

/// Typed errors, exit non-zero: an unknown `--preset` (`unknown_preset`, exit 1), a `--in` that is
/// not a tick count and a lone `--in` (usage, exit 2), `--out` before `--in` (`export_range`, exit 1),
/// a missing `--sequence` (usage, exit 2), no `--media` (no frame decoder: `frame_decoder_missing`,
/// exit 1). No file is written.
#[test]
fn export_sequence_cli_refuses_bad_arguments_typed() {
    let s = Scratch::new("refusals");
    let pkg = package(&s.0);
    let media = s.path("dnxhr-lb.mov");
    std::fs::write(&media, DNX).unwrap();
    let out = s.path("never.mov");
    let base = |extra: &[&str]| -> (i32, Value) {
        let mut args = vec!["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--output", &out, "--media", &media, "--width", "64", "--height", "32"];
        args.extend_from_slice(extra);
        cli(&args)
    };
    let (exit, json) = base(&["--preset", "no.such.preset"]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("unknown_preset")), "{json}");
    let (exit, json) = base(&["--preset", PRESET, "--in", "abc", "--out", "5"]);
    assert_eq!((exit, json["error"].as_str()), (2, Some("usage")), "{json}");
    assert!(json["message"].as_str().unwrap().contains("--in"), "{json}");
    let (exit, json) = base(&["--preset", PRESET, "--in", "0"]);
    assert_eq!((exit, json["error"].as_str()), (2, Some("usage")), "{json}");
    let (in_t, out_t) = ((2 * F).to_string(), F.to_string());
    let (exit, json) = base(&["--preset", PRESET, "--in", &in_t, "--out", &out_t]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("export_range")), "{json}");
    let (exit, json) = cli(&["export-sequence", pkg.to_str().unwrap(), "--preset", PRESET, "--output", &out, "--media", &media]);
    assert_eq!((exit, json["error"].as_str()), (2, Some("usage")), "{json}");
    assert!(json["message"].as_str().unwrap().contains("--sequence"), "{json}");
    let (exit, json) = cli(&["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--output", &out]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("frame_decoder_missing")), "{json}");
    assert!(!Path::new(&out).exists(), "no file");
}
