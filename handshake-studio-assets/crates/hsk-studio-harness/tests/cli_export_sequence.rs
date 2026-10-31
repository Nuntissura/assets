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
    package_with(dir, None)
}

/// [`package`], plus (R44 F-R44-2) an A1 track whose one clip plays `audio` at [0, 3) when given.
fn package_with(dir: &Path, audio: Option<ClipSource>) -> PathBuf {
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
            clips: vec![clip.clone()],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let mut sequence = sequence;
    if let Some(source) = audio {
        let mut a1 = sequence.tracks[0].clone();
        a1.track_id = "STRK-019abcde-0000-7000-8000-000000000002".into();
        a1.kind = TrackKind::Audio;
        a1.index = 0;
        a1.name = "A1".into();
        a1.clips = vec![StudioClip { clip_id: "SCLP-019abcde-0000-7000-8000-000000000002".into(), source_ref: source, ..clip }];
        sequence.tracks.push(a1);
    }
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

// ---- R44 findings F-R44-2 and F-R44-15 (SMB-R44-FOLLOWUPS-1) ------------------------------------

/// 16-bit PCM stereo WAV at `rate` with `frames` frames: square waves, left and right different (so a
/// swapped or dropped channel would show).
fn wav(rate: u32, frames: u32) -> Vec<u8> {
    let data_len = frames * 4;
    let mut v = b"RIFF".to_vec();
    v.extend((36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * 4).to_le_bytes());
    v.extend(4u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend(data_len.to_le_bytes());
    for i in 0..frames {
        let l: i16 = if (i / 64) % 2 == 0 { 8192 } else { -8192 };
        let r: i16 = if (i / 32) % 2 == 0 { -4096 } else { 4096 };
        v.extend(l.to_le_bytes());
        v.extend(r.to_le_bytes());
    }
    v
}

fn source_of(bytes: &[u8]) -> ClipSource {
    let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(bytes), 1 << 20, &CancellationToken::default()).unwrap();
    let hex = hsk_studio_reel::hex(&digest);
    ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None }
}

/// The test's own sound port: reel's decode of the file, keyed like the CLI keys it.
struct Port(ClipSource, Vec<Vec<f32>>, Ticks);
impl hsk_studio_pulse::AudioSources for Port {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        (*source == self.0).then_some(self.1.as_slice())
    }
}
impl hsk_studio_pulse::SourceExtents for Port {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        (*source == self.0).then_some(self.2)
    }
}
struct Shared(std::sync::Arc<Port>);
impl hsk_studio_pulse::AudioSources for Shared {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        self.0.audio(source)
    }
}

/// `--audio FILE` exports the sound of a sequence that holds an audio clip: the CLI's receipt and file equal
/// the headless `Viewer::export_sequence` with the same decoded file bound by `Viewer::set_export_audio`;
/// without `--audio` the same command is refused `audio_sources_missing` and the hint names `--audio`.
#[test]
fn export_sequence_cli_exports_the_sound_of_audio_files() {
    let s = Scratch::new("audio");
    let sound = wav(48_000, 3 * 1920);
    let pkg = package_with(&s.0, Some(source_of(&sound)));
    let media = s.path("dnxhr-lb.mov");
    std::fs::write(&media, DNX).unwrap();
    let audio = s.path("tone.wav");
    std::fs::write(&audio, &sound).unwrap();
    // Without --audio: typed refusal naming the flag, no file.
    let out0 = s.path("silent.mov");
    let (exit, json) = cli(&["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--output", &out0, "--media", &media, "--width", "64", "--height", "32"]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("audio_sources_missing")), "{json}");
    assert!(json["detail"]["hint"].as_str().unwrap_or("").contains("--audio"), "{json}");
    assert!(!Path::new(&out0).exists());
    // Headless with the same decoded sound.
    let decoded = hsk_studio_reel::decode_audio_file(&mut std::io::Cursor::new(&sound[..]), &hsk_studio_reel::Limits::default(), 1 << 20, &CancellationToken::default()).unwrap();
    assert_eq!((decoded.sample_rate, decoded.channels, decoded.frames()), (48_000, 2, 3 * 1920));
    let extent = hsk_studio_pulse::ticks_from_samples(3 * 1920, 48_000).unwrap();
    let port = std::sync::Arc::new(Port(source_of(&sound), decoded.planar(), extent));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&pkg).unwrap();
    v.bind_program_media("seq-main", vec![DNX.to_vec()], 64, 32).unwrap();
    v.set_export_audio(Shared(port.clone()), port, 48_000).unwrap();
    let mut request = ExportSequenceRequest::new(PRESET, s.0.join("headless.mov"));
    request.width = 64;
    request.height = 32;
    let headless = v.export_sequence("seq-main", request).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    assert_eq!(headless["receipt"]["outcome"], "complete", "{headless}");
    assert_eq!(headless["receipt"]["audio_frames"].as_u64(), Some(3 * 1920), "{headless}");
    // CLI with --audio.
    let out = s.path("cli.mov");
    let (exit, json) = cli(&["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--output", &out, "--media", &media, "--audio", &audio, "--width", "64", "--height", "32", "--json"]);
    assert_eq!(exit, 0, "{json}");
    assert_eq!(json["export"]["receipt"], headless["receipt"], "same receipt JSON");
    assert_eq!(json["export"]["audio_rate"].as_u64(), Some(48_000), "{json}");
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(s.0.join("headless.mov")).unwrap(), "byte-identical files");
    // The sound is in the file: the video-only package (silence) exports a different file.
    let q = Scratch::new("audio-quiet");
    let quiet = package(&q.0);
    let out_q = s.path("quiet.mov");
    let (exit, json) = cli(&["export-sequence", quiet.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--output", &out_q, "--media", &media, "--width", "64", "--height", "32"]);
    assert_eq!(exit, 0, "{json}");
    assert_ne!(std::fs::read(&out_q).unwrap(), std::fs::read(&out).unwrap(), "the mixed sound changes the file");
}

/// F-R44-2 and F-R44-15 refusals, all typed: two `--audio` files at different rates (`audio_rate_mismatch`);
/// an `--out` past the sequence end with a `--media` path that does not exist (`export_range`, so nothing was
/// read; the end itself is in range); an OUT `std::path::absolute` refuses (the empty path:
/// `output_unresolved`, stage `output`); `--media` plus `--audio` sizes over `MAX_EXPORT_INPUT_BYTES` while each
/// file is under the per-item limit (`media_too_large` with `total_limit`, from the sizes only).
#[test]
fn export_sequence_cli_refuses_audio_range_output_and_total_size_typed() {
    let s = Scratch::new("r44-refusals");
    let sound = wav(48_000, 3 * 1920);
    let pkg = package_with(&s.0, Some(source_of(&sound)));
    let media = s.path("dnxhr-lb.mov");
    std::fs::write(&media, DNX).unwrap();
    let (a48, a44) = (s.path("a48.wav"), s.path("a44.wav"));
    std::fs::write(&a48, &sound).unwrap();
    std::fs::write(&a44, wav(44_100, 4410)).unwrap();
    let out = s.path("never.mov");
    let args = |extra: &[&str]| -> (i32, Value) {
        let mut a = vec!["export-sequence", pkg.to_str().unwrap(), "--sequence", "seq-main", "--preset", PRESET, "--width", "64", "--height", "32"];
        a.extend_from_slice(extra);
        cli(&a)
    };
    let (exit, json) = args(&["--output", &out, "--media", &media, "--audio", &a48, "--audio", &a44]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("audio_rate_mismatch")), "{json}");
    assert_eq!(json["detail"]["stage"], "audio", "{json}");
    let missing = s.path("not-there.mov");
    let (in_t, past) = ("0".to_string(), (4 * F).to_string());
    let (exit, json) = args(&["--output", &out, "--media", &missing, "--in", &in_t, "--out", &past]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("export_range")), "{json}");
    assert_eq!(json["detail"]["sequence_end"].as_u64(), Some(3 * F), "{json}");
    let end = (3 * F).to_string();
    let (exit, json) = args(&["--output", &out, "--media", &missing, "--in", &in_t, "--out", &end]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("media_unreadable")), "the end itself is in range: {json}");
    let (exit, json) = args(&["--output", "", "--media", &media]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("output_unresolved")), "{json}");
    assert_eq!(json["detail"]["stage"], "output", "{json}");
    // Two files of 1 GiB + 1 byte each (each under the 2 GiB item limit, together over the total): set_len only,
    // nothing is written or read.
    let (big1, big2) = (s.0.join("big1.wav"), s.0.join("big2.wav"));
    for p in [&big1, &big2] {
        std::fs::File::create(p).unwrap().set_len((1 << 30) + 1).unwrap();
    }
    let (exit, json) = args(&["--output", &out, "--media", big1.to_str().unwrap(), "--audio", big2.to_str().unwrap()]);
    assert_eq!((exit, json["error"].as_str()), (1, Some("media_too_large")), "{json}");
    assert_eq!(json["detail"]["total_limit"].as_u64(), Some(hsk_studio_harness::cli::MAX_EXPORT_INPUT_BYTES), "{json}");
    assert_eq!(json["detail"]["stage"], "audio", "{json}");
    assert!(!Path::new(&out).exists(), "no file");
}
