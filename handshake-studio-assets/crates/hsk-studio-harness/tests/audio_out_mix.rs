//! Lane SMB-AUDIO-OUT-MIX-1 (features `audio-batch`, `audio-device`): the program audio output has
//! its caller in the viewer (device selection, transport on the audio clock, dropouts, meters), the
//! routed audio-only mixdown writes WAV / FLAC / AIFF, stems print per bus with a sum check against
//! the print, and repaired audio (the waveform edit list of a clip's source) reaches the mix and
//! survives save and reopen. Devices: score-device's software device only (a clocked thread, no
//! operating-system audio: nothing reaches the speakers and the Operator's device is never opened);
//! real device output stays NOT_PROVEN here. Oracles: the bus stream's own render (the mixer's
//! stream) and hand-made sources.
#![cfg(all(feature = "audio-batch", feature = "audio-device"))]

mod support_audio;

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::audio_import::MixFormat;
use hsk_studio_harness::view::audio_session::playback::{AudioTransport, MAX_RUN_MS};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::waveform::WaveformEdit;
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_reel as reel;
use hsk_studio_score::{ALL_CHANNELS, WaveSelection};
use hsk_studio_score_device::{DeviceGrant, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions, SoftwareProvider};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;
const CLIP1: &str = "SCLP-019abcde-0000-7000-8000-000000000001";

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
/// Track 1 (dc 0.25 stereo, fader 0.5) and track 2 (an impulse, mono) over the submix track 3 (DX).
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
fn dc() -> Vec<Vec<f32>> {
    vec![vec![0.25; 12 * SPF], vec![0.25; 12 * SPF]]
}
fn sources_with(dc: Vec<Vec<f32>>) -> Sources {
    let mut imp = vec![0.0; 12 * SPF];
    imp[1000] = 0.5;
    Sources(Arc::new(vec![("dc".into(), dc), ("imp".into(), vec![imp])]))
}
fn sources() -> Sources {
    sources_with(dc())
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

/// Track 1 to DX, track 2 straight to the master; with `send`, track 2 also feeds DX pre-fader.
fn routing(send: bool) -> StudioAudioRoutingSet {
    let mut r = StudioAudioRouting::new("seq-main");
    let sends = if send { vec![StudioAudioSend { send_id: "s1".into(), target: id(3), level: 0.5, pre_fader: true, pan: 0.0 }] } else { vec![] };
    r.routes = vec![StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] }, StudioTrackRoute { track_id: id(2), output: None, sends }];
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

/// A viewer on a fresh session with `routing` stored; with `port`, the sound port bound.
fn session(label: &str, routing: &StudioAudioRoutingSet, port: bool) -> (PathBuf, Viewer) {
    let dir = support_audio::scratch(&format!("out-mix-{label}"));
    let path = package(&dir);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.set_audio_routing(routing).unwrap();
    if port {
        v.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    }
    (dir, v)
}

fn grant(directions: &[Direction]) -> DeviceGrant {
    DeviceGrant::new(
        ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap(),
        DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap(),
        directions,
        None,
    )
}

/// The mixer's stream of the viewer's document over `sources`, rendered from the start.
fn render(v: &Viewer, sources: Sources, frames: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let mut stream = v.bus_stream("seq-main", RATE, 480, sources, &Ext).unwrap();
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    for at in (0..frames).step_by(480) {
        let n = 480.min(frames - at);
        stream.render(&mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
    }
    [l, r]
}

fn decode(path: &Path) -> Vec<Vec<f32>> {
    let bytes = std::fs::read(path).unwrap();
    reel::decode_audio_file(&mut Cursor::new(bytes), &reel::Limits::default(), 1 << 24, &CancellationToken::default()).unwrap().planar()
}

fn assert_close(got: &[Vec<f32>], want: &[Vec<f32>; 2], tolerance: f32, what: &str) {
    assert_eq!(got.len(), 2, "{what}: channels");
    for c in 0..2 {
        assert_eq!(got[c].len(), want[c].len(), "{what}: frames");
        for (i, (g, w)) in got[c].iter().zip(&want[c]).enumerate() {
            assert!((g - w).abs() <= tolerance, "{what}: ch {c} sample {i}: {g} vs {w}");
        }
    }
}

#[test]
fn audio_out_plays_the_program_sequence_on_the_audio_clock_with_dropouts_and_meters() {
    let (_dir, mut v) = session("play", &routing(false), true);
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let devices = v.audio_out_devices(&provider, &grant(&[Direction::Output])).unwrap();
    assert!(devices.as_array().unwrap().iter().any(|d| d["id"] == SOFTWARE_OUTPUT_DEVICE), "{devices}");
    // Typed refusals before anything opens.
    assert_eq!(v.audio_out_open(&provider, &grant(&[Direction::Output]), "no-such-device", 480, None).unwrap_err().code, "unknown_device");
    assert_eq!(v.audio_out_open(&provider, &grant(&[Direction::Input]), SOFTWARE_OUTPUT_DEVICE, 480, None).unwrap_err().code, "unauthorized");
    assert_eq!(v.audio_out_open(&provider, &grant(&[Direction::Output]), SOFTWARE_OUTPUT_DEVICE, 0, None).unwrap_err().code, "audio_config");
    assert_eq!(v.audio_out_open(&provider, &grant(&[Direction::Output]), SOFTWARE_OUTPUT_DEVICE, 480, Some("nope")).unwrap_err().code, "mixdown");
    assert_eq!(v.audio_out_transport(AudioTransport::Play).unwrap_err().code, "audio_out_closed");
    assert!(v.audio_out_close().is_none());

    v.audio_out_open(&provider, &grant(&[Direction::Output]), SOFTWARE_OUTPUT_DEVICE, 480, Some("seq-main")).unwrap();
    assert_eq!(v.monitor().unwrap().sequence().sequence_id, "seq-main", "the program monitor opened on the sequence");
    v.audio_out_transport(AudioTransport::Play).unwrap();
    let state = v.audio_out_run(300).unwrap();
    assert!(state["consumed_frames"].as_u64().unwrap() > 0, "{state}");
    assert!(state["underruns"].is_u64() && state["underrun_frames"].is_u64(), "dropouts are counted: {state}");
    assert!(state["meters"]["master"]["peak"][0].as_f64().unwrap() > 0.1, "meters follow what was rendered for the device: {state}");
    let dx = &state["meters"]["buses"][0];
    assert_eq!(dx["track_id"], id(3));
    assert!(dx["level"]["peak"][0].as_f64().unwrap() > 0.1, "{state}");
    assert_eq!(v.state_json()["audio_out"]["device"], SOFTWARE_OUTPUT_DEVICE);
    assert_eq!(v.audio_out_run(MAX_RUN_MS + 1).unwrap_err().code, "run_too_long");
    v.audio_out_transport(AudioTransport::Pause).unwrap();
    let report = v.audio_out_close().unwrap();
    assert!(report.reconciles() && !report.device_lost, "{report:?}");
    assert!(v.state_json()["audio_out"].is_null());

    // The device received the mixer's stream, sample for sample.
    let frames = 10 * SPF;
    let [l, _] = render(&v, sources(), frames);
    let played: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[0]).collect();
    let first = played.iter().position(|s| *s != 0.0).expect("audio reached the device");
    let n = played[first..].iter().take_while(|s| **s != 0.0).count().min(frames);
    assert!(n >= 480 * 10, "only {n} samples played");
    for i in 0..n {
        assert_eq!(played[first + i].to_bits(), l[i].to_bits(), "device sample {i}");
    }
    println!("NOT_PROVEN: real device output (score-device cpal-backend); this test opens only the software device, so nothing reaches the speakers");
}

#[test]
fn routed_mixdown_writes_wav_flac_and_aiff_of_what_the_mixer_plays() {
    let (dir, mut v) = session("mix-noport", &routing(false), false);
    assert_eq!(v.export_sequence_mix("seq-main", MixFormat::Wav { bits: 24 }, &dir.join("a.wav"), false).unwrap_err().code, "audio_sources_missing");
    let (dir, mut v) = session("mix", &routing(true), true);
    let frames = 10 * SPF;
    let want = render(&v, sources(), frames);
    let float = dir.join("ep12.wav");
    let r = v.export_sequence_mix("seq-main", MixFormat::WavFloat, &float, false).unwrap();
    assert_eq!((r.frames, r.channels, r.sample_rate), (frames as u64, 2, RATE));
    assert_close(&decode(&float), &want, 0.0, "wav float");
    for (format, name, tolerance) in [(MixFormat::Wav { bits: 24 }, "ep12-24.wav", 2.0 / 8_388_608.0), (MixFormat::Flac { bits: 24 }, "ep12.flac", 2.0 / 8_388_608.0), (MixFormat::Aiff { bits: 16 }, "ep12.aiff", 2.0 / 32_768.0)] {
        let out = dir.join(name);
        v.export_sequence_mix("seq-main", format, &out, false).unwrap();
        assert_close(&decode(&out), &want, tolerance, name);
    }
    assert_eq!(v.export_sequence_mix("seq-main", MixFormat::WavFloat, &float, false).unwrap_err().code, "output_exists");
    assert_eq!(v.export_sequence_mix("nope", MixFormat::WavFloat, &dir.join("b.wav"), false).unwrap_err().code, "unknown_sequence");
    assert_eq!(v.export_sequence_mix("seq-main", MixFormat::Wav { bits: 20 }, &dir.join("c.wav"), false).unwrap_err().code, "export_format");
    assert_eq!(v.export_sequence_mix("seq-main", MixFormat::WavFloat, &dir.join("session.handshake"), true).unwrap_err().code, "export_over_source");
    assert_eq!(MixFormat::parse("wav24"), Some(MixFormat::Wav { bits: 24 }));
    assert_eq!(MixFormat::parse("mp3"), None);
}

#[test]
fn stems_print_per_bus_and_sum_to_the_print_and_a_cross_send_shows_in_the_residual() {
    let (dir, mut v) = session("stems", &routing(false), true);
    let stems_dir = dir.join("stems");
    std::fs::create_dir_all(&stems_dir).unwrap();
    let report = v.export_sequence_stems("seq-main", &[id(3)], true, MixFormat::WavFloat, &stems_dir, false).unwrap();
    assert!(report.sums_to_print(), "{}", report.to_json());
    assert!(report.print_peak > 0.1);
    let names: Vec<_> = report.stems.iter().map(|s| s.output.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["T3.wav", "rest.wav"]);
    let dx = decode(&stems_dir.join("T3.wav"));
    let rest = decode(&stems_dir.join("rest.wav"));
    assert_eq!((dx[0].len() as u64, rest[0].len() as u64), (report.frames, report.frames), "equal length and start");
    // DX carries track 1 only (0.25 x fader 0.5), the rest track 2 only (the impulse).
    assert!((dx[0][4800] - 0.125).abs() < 1e-6 && (dx[0][1000] - 0.125).abs() < 1e-6, "{} {}", dx[0][4800], dx[0][1000]);
    assert!(rest[0][4800].abs() < 1e-9 && rest[0][1000] > 0.3);
    // Typed refusals before anything is written.
    assert_eq!(v.export_sequence_stems("seq-main", &[id(3)], true, MixFormat::WavFloat, &stems_dir, false).unwrap_err().code, "output_exists");
    assert_eq!(v.export_sequence_stems("seq-main", &[id(1)], false, MixFormat::WavFloat, &stems_dir, true).unwrap_err().code, "unknown_bus");
    assert_eq!(v.export_sequence_stems("seq-main", &[], false, MixFormat::WavFloat, &stems_dir, true).unwrap_err().code, "stem_none");
    assert_eq!(v.export_sequence_stems("seq-main", &[id(3), id(3)], false, MixFormat::WavFloat, &stems_dir, true).unwrap_err().code, "stem_duplicate");

    // A pre-fader send from a direct track into DX sits in no stem: the sum check says so.
    let (dir, mut w) = session("stems-send", &routing(true), true);
    let report = w.export_sequence_stems("seq-main", &[id(3)], true, MixFormat::WavFloat, &dir, false).unwrap();
    assert!(!report.sums_to_print(), "{}", report.to_json());
    assert!(report.residual_dbfs().unwrap() > -20.0, "{}", report.to_json());
}

#[test]
fn repaired_audio_reaches_the_mix_and_survives_save_and_reopen() {
    let (dir, mut v) = session("repair", &routing(false), true);
    let frames = 10 * SPF;
    let original = render(&v, sources(), frames);
    assert_eq!(v.open_waveform_from_clip("SCLP-019abcde-0000-7000-8000-00000000ffff").unwrap_err().code, "unknown_clip");
    let opened = v.open_waveform_from_clip(CLIP1).unwrap();
    assert_eq!(opened["selection"], serde_json::json!([0, frames]), "the clip's window is selected");
    let half = (5 * SPF) as u64;
    v.waveform_edit(WaveformEdit::Invert(WaveSelection { start: 0, end: half, channels: ALL_CHANNELS })).unwrap();
    assert_eq!(v.sequence_sound().unwrap().repaired_sources(), vec![&source("dc")]);
    let mut repaired_dc = dc();
    for c in &mut repaired_dc {
        for s in &mut c[..5 * SPF] {
            *s = -*s;
        }
    }
    let want = render(&v, sources_with(repaired_dc), frames);
    let out = dir.join("repaired.wav");
    v.export_sequence_mix("seq-main", MixFormat::WavFloat, &out, false).unwrap();
    assert_close(&decode(&out), &want, 0.0, "repaired mix");
    assert_ne!(want[0][100], original[0][100]);

    // Saved and reopened, the edit list resolves the same audio into the mix.
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    let again = dir.join("reopened.wav");
    w.export_sequence_mix("seq-main", MixFormat::WavFloat, &again, false).unwrap();
    assert_close(&decode(&again), &want, 0.0, "reopened mix");

    // The edit is one undo step: undone, the mix is the original again.
    v.undo().unwrap();
    assert!(v.sequence_sound().unwrap().repaired_sources().is_empty());
    let undone = dir.join("undone.wav");
    v.export_sequence_mix("seq-main", MixFormat::WavFloat, &undone, false).unwrap();
    assert_close(&decode(&undone), &original, 0.0, "undone mix");
}
