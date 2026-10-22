//! Lane SMB-PR-SOURCE-MONITOR (lead addition; Operator decision D24-A, lane SMB-D24A-CODEC-DEFAULTS):
//! under feature `media-native` the desktop viewer decodes AAC through the OS decoder, reel-native's
//! isolated Media Foundation AAC worker (`WorkerMfAacProvider`), registered by `Viewer::new` and by
//! `Viewer::register_os_audio_decoder`. An AAC-LC track of the reel corpus opens, lands in the score
//! mixdown the program monitor plays, and its receipt names the OS decoder. The worker executable must
//! have been built next to the test target (`cargo build -p hsk-studio-reel-native --features mf-worker
//! --bin reel-native-mf-worker`); otherwise the test prints NOT_PROVEN and proves nothing.
#![cfg(feature = "media-native")]

use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_reel as reel;
use hsk_studio_reel_native::MfWorkerOptions;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-reel/tests/fixtures/audio").join(name)
}

#[test]
fn viewer_plays_an_aac_track_through_the_os_decoder() {
    // Without a worker the viewer reports it and AAC stays a typed refusal (no silent decoder).
    let mut none = Viewer::new(Prefer::Cpu);
    let missing = MfWorkerOptions::default().with_worker_program(fixture("no-such-worker.exe"));
    assert!(!none.register_os_audio_decoder(missing));
    assert!(none.audio_decoder().is_none());
    if !cfg!(feature = "studio-aac") {
        let e = none.open(&fixture("aac-lc-44k1-2ch.aac")).unwrap_err();
        assert!(e.to_ascii_lowercase().contains("codec_unavailable"), "{e}");
    }

    let exe = std::env::current_exe().unwrap();
    let worker = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|dir| dir.join(format!("reel-native-mf-worker{}", std::env::consts::EXE_SUFFIX)));
    let Some(worker) = worker.filter(|w| w.is_file()) else {
        println!("NOT_PROVEN: reel-native-mf-worker not built beside the test target; viewer AAC decode unproven");
        return;
    };
    let mut v = Viewer::new(Prefer::Cpu);
    assert!(v.register_os_audio_decoder(MfWorkerOptions::default().with_worker_program(worker)));
    let id = v.audio_decoder().expect("registered");
    assert_eq!(id.id, reel::AAC_OS_DECODER_ID);
    if let Err(e) = v.open(&fixture("aac-lc-44k1-2ch.aac")) {
        if e.to_ascii_lowercase().contains("codec_unavailable") {
            println!("NOT_PROVEN: Media Foundation AAC decoder absent on this host: {e}");
            return;
        }
        panic!("{e}: {}", v.state_json());
    }
    let state = v.state_json();
    assert_eq!(state["audio"]["import"]["decoder"]["id"], reel::AAC_OS_DECODER_ID, "{state}");
    assert_eq!(state["audio"]["import"]["sample_rate"], 44_100, "{state}");
    let frames = v.audio().unwrap().import.audio.frames;
    assert!(frames > 0);
    assert!(v.monitor().is_some(), "the program monitor opens on the AAC import sequence");
    // The mixdown the monitor plays (audio_out) is the whole decoded track, finite and not silent.
    let mut mix = v.audio_mix_stream(1000).unwrap().unwrap();
    assert_eq!(mix.length_samples(), frames);
    let cancel = hsk_studio_accord::CancellationToken::default();
    let n = usize::try_from(frames).unwrap();
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut start = 0;
    while start < n {
        let end = (start + 1000).min(n);
        mix.render(&mut l[start..end], &mut r[start..end], &cancel).unwrap();
        start = end;
    }
    assert!(l.iter().chain(&r).all(|s| s.is_finite() && s.abs() <= 8.0));
    let energy: f64 = l.iter().chain(&r).map(|s| f64::from(*s) * f64::from(*s)).sum();
    assert!(energy > 1.0, "the decoded track is not silent ({energy})");
}
