//! Lane SMB-AUDIO-IMPORT-WIRING (feature `media`): opening an MP3, FLAC or Opus file lands in a
//! one-clip sequence whose score mixdown (pulse `TimelineMixStream`) is the decoded file; the mixdown
//! exports to FLAC and AIFF and decodes back; hostile files fail typed. With feature `audio-device`
//! the imported file also plays through `audio_out` on score-device's software device.
//! Fixtures are reel's generated audio fixtures (provenance in
//! `crates/hsk-studio-reel/tests/fixtures/audio/provenance.json`).
#![cfg(feature = "media")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::audio_import::{
    AudioImport, ImportLimits, MixFormat, export_mix, import_audio, open_audio_file,
};
use hsk_studio_reel as reel;
use std::io::Cursor;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hsk-studio-reel/tests/fixtures/audio")
        .join(name)
}

/// Independent reference: reel's decode of the same file, planar.
fn reference(name: &str) -> (u32, Vec<Vec<f32>>) {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let d = reel::decode_audio_file(&mut Cursor::new(bytes), &reel::Limits::default(), 1 << 24, &CancellationToken::default())
        .unwrap();
    (d.sample_rate, d.planar())
}

/// The whole mix, rendered in 1000-frame blocks (not a divisor of the lengths).
fn render_all(import: AudioImport) -> (Vec<f32>, Vec<f32>, usize) {
    let cancel = CancellationToken::default();
    let mut mix = import.mix_stream(1000, &cancel).unwrap();
    let n = mix.length_samples() as usize;
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    for start in (0..n).step_by(1000) {
        let end = (start + 1000).min(n);
        mix.render(&mut l[start..end], &mut r[start..end], &cancel).unwrap();
    }
    assert!(mix.stats().sessions_built >= 1, "the mix runs through a score session");
    (l, r, mix.stats().sessions_built as usize)
}

const FILES: [(&str, u32, usize); 3] = [
    ("mp3-44k1-2ch-cbr.mp3", 44_100, 13_230),
    ("flac-s16-44k1-2ch.flac", 44_100, 11_025),
    ("opus-48k-2ch.opus", 48_000, 12_000),
];

#[test]
fn opening_mp3_flac_and_opus_lands_in_a_score_mix_of_the_decoded_file() {
    let cancel = CancellationToken::default();
    // Registry::with_audio is standard() plus the audio file providers; standard() is unchanged.
    let ids = |r: reel::Registry| r.descriptors().into_iter().map(|d| d.id).collect::<Vec<_>>();
    let standard = ids(reel::Registry::standard());
    let with_audio = ids(reel::Registry::with_audio());
    assert_eq!(with_audio[..standard.len()], standard[..]);
    for id in ["flac", "ogg-audio", "mpeg-audio", "aac-adts", "aiff-pcm"] {
        assert!(with_audio.contains(&id) && !standard.contains(&id), "{id}");
    }
    for (name, rate, frames) in FILES {
        let import = open_audio_file(&fixture(name), &ImportLimits::default(), &cancel).unwrap();
        assert_eq!((import.audio.sample_rate, import.audio.frames), (rate, frames as u64), "{name}");
        let seq = &import.sequence;
        assert_eq!(seq.tracks.len(), 1);
        assert_eq!(seq.tracks[0].clips.len(), 1);
        assert_eq!(seq.tracks[0].clips[0].source_ref, import.audio.source);
        assert!(seq.duration >= seq.tracks[0].clips[0].duration);
        assert_eq!(seq.duration.value() % seq.frame_rate.ticks_per_frame, 0, "whole frames");
        let (want_rate, want) = reference(name);
        assert_eq!(want_rate, rate);
        let (l, r, _) = render_all(import);
        assert_eq!(l.len(), frames, "{name}: the mix is exactly the file length");
        // Stereo source, centre pan, unity fader: the mix is the decode (stated bound 1e-6).
        let worst = l
            .iter()
            .zip(&want[0])
            .chain(r.iter().zip(&want[1]))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst <= 1e-6, "{name}: mix differs from the decode by {worst}");
        assert!(want[0].iter().any(|v| *v != 0.0), "{name}: fixture is not silence");
    }
}

#[test]
fn mixdown_exports_to_flac_and_aiff_and_decodes_back() {
    let cancel = CancellationToken::default();
    let (name, _, frames) = FILES[0];
    let import = || open_audio_file(&fixture(name), &ImportLimits::default(), &cancel).unwrap();
    let (l, r, _) = render_all(import());
    let interleaved: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
    for format in [
        MixFormat::Flac { bits: 16 },
        MixFormat::Flac { bits: 24 },
        MixFormat::Aiff { bits: 16 },
        MixFormat::Aiff { bits: 24 },
        MixFormat::AiffFloat,
    ] {
        let mut out = Vec::new();
        let mix = import().mix_stream(1000, &cancel).unwrap();
        let receipt = export_mix(mix, format, &mut out, &cancel).unwrap();
        assert_eq!((receipt.frames, receipt.channels, receipt.sample_rate), (frames as u64, 2, 44_100));
        assert_eq!(receipt.bytes_written, out.len() as u64);
        let back = reel::decode_audio_file(&mut Cursor::new(&out), &reel::Limits::default(), 1 << 24, &cancel).unwrap();
        assert_eq!((back.sample_rate, back.channels, back.frames()), (44_100, 2, frames), "{format:?}");
        let (step, clipped) = match format {
            MixFormat::Flac { bits } | MixFormat::Aiff { bits } => {
                let scale = f64::from(1u32 << (bits - 1));
                let clipped = interleaved.iter().filter(|v| (f64::from(**v) * scale).round() > scale - 1.0 || (f64::from(**v) * scale).round() < -scale).count();
                (1.0 / scale, clipped as u64)
            }
            MixFormat::AiffFloat => (0.0, 0),
        };
        assert_eq!(receipt.clipped_samples, clipped, "{format:?}: clip count");
        for (i, (got, want)) in back.interleaved.iter().zip(&interleaved).enumerate() {
            let bound = if want.abs() >= 1.0 { 1.0 } else { step / 2.0 + 1e-9 };
            assert!(f64::from((got - want).abs()) <= bound, "{format:?} sample {i}: {got} vs {want}");
        }
        if format == MixFormat::AiffFloat {
            assert_eq!(back.interleaved, interleaved, "float AIFF is the exact mix");
        }
    }
    let mix = import().mix_stream(1000, &cancel).unwrap();
    let refused = export_mix(mix, MixFormat::Flac { bits: 20 }, &mut Vec::new(), &cancel).err().unwrap();
    assert_eq!(refused.code, "export_format");
}

#[test]
fn hostile_or_unsupported_audio_files_fail_typed() {
    let cancel = CancellationToken::default();
    let limits = ImportLimits::default();
    let code = |bytes: Vec<u8>, limits: &ImportLimits| import_audio(&mut Cursor::new(bytes), limits, &cancel).err().unwrap();
    // Not audio, and a truncated MP3 header region.
    assert_eq!(code(b"not an audio file at all".to_vec(), &limits).code, "audio_decode");
    assert_eq!(code(Vec::new(), &limits).code, "audio_decode");
    let flac = std::fs::read(fixture("flac-s16-44k1-2ch.flac")).unwrap();
    assert_eq!(code(flac[..40].to_vec(), &limits).code, "audio_decode");
    // Over the frame budget: refused by reel before the PCM buffer exists.
    let tight = ImportLimits { max_frames: 1000, ..limits };
    let over = code(flac.clone(), &tight);
    assert_eq!(over.code, "audio_decode");
    assert!(over.detail.contains("MEDIA_LIMIT_EXCEEDED"), "{}", over.detail);
    // 5.1: the timeline mix takes mono or stereo sources, no silent downmix.
    let six = std::fs::read(fixture("opus-48k-6ch.opus")).unwrap();
    assert_eq!(code(six, &limits).code, "audio_channels");
    // File size bound checked from the metadata before reading.
    let small = ImportLimits { max_file_bytes: 100, ..limits };
    assert_eq!(open_audio_file(&fixture("flac-s16-44k1-2ch.flac"), &small, &cancel).err().unwrap().code, "audio_read");
    // A byte-flip sweep over the MP3 never panics: each result imports or fails typed.
    let mp3 = std::fs::read(fixture("mp3-44k1-2ch-cbr.mp3")).unwrap();
    for at in (0..mp3.len()).step_by(97) {
        let mut b = mp3.clone();
        b[at] ^= 0xA5;
        if let Err(e) = import_audio(&mut Cursor::new(b), &limits, &cancel) {
            assert!(["audio_decode", "audio_empty", "audio_channels", "audio_rate"].contains(&e.code), "{at}: {}", e.code);
        }
    }
}

/// Playback: the imported MP3 plays through `audio_out` on score-device's software device (no OS
/// audio) and the device receives the mixdown sample for sample.
#[cfg(feature = "audio-device")]
#[test]
fn imported_file_plays_through_the_device() {
    use hsk_studio_accord::{ActorContext, DomainId};
    use hsk_studio_folio::ClipSource;
    use hsk_studio_harness::view::audio_out::AudioOut;
    use hsk_studio_harness::view::monitor::{DropPolicy, FrameSource, ProgramMonitor, SourceFrame};
    use hsk_studio_score_device::{
        DeviceGrant, DeviceId, DeviceProvider, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions,
        SoftwareProvider, StreamConfig,
    };
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    struct NoFrames;
    impl FrameSource for NoFrames {
        fn frame(&self, _: &ClipSource, _: hsk_studio_accord::Ticks) -> Option<Arc<SourceFrame>> {
            None
        }
    }
    let cancel = CancellationToken::default();
    let (name, rate, _) = FILES[0];
    let import = open_audio_file(&fixture(name), &ImportLimits::default(), &cancel).unwrap();
    let sequence = import.sequence.clone();
    let mix = import.mix_stream(441, &cancel).unwrap();
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let grant = DeviceGrant::new(
        ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap(),
        DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap(),
        &[Direction::Output],
        None,
    );
    let config = StreamConfig { sample_rate_hz: rate, layout: mix.layout(), block_frames: 441, ring_blocks: 4 };
    let stream = provider
        .open_output(&DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap(), &config, Some(&grant), &cancel)
        .unwrap();
    let mut out = AudioOut::new(mix, stream).unwrap();
    let mut m = ProgramMonitor::new(sequence, 2, 2, DropPolicy::DropLate).unwrap();
    out.command(&mut m, |m, c| m.play(c)).unwrap().unwrap();
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        out.pump(&cancel).unwrap();
        let _ = m.refresh(&out, &NoFrames, out.mix().sources(), &cancel).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    out.command(&mut m, |m, c| m.pause(c)).unwrap().unwrap();
    let report = out.close();
    assert!(report.reconciles() && !report.device_lost, "{report:?}");
    let (_, want) = reference(name);
    let left: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[0]).collect();
    let right: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[1]).collect();
    let first = left.iter().position(|s| *s != 0.0).expect("audio reached the device");
    let skip = want[0].iter().position(|s| *s != 0.0).unwrap();
    let played = left[first..].len().min(want[0].len() - skip);
    assert!(played >= 441 * 20, "only {played} samples played");
    for i in 0..played.min(want[0].len() - skip) {
        let (wl, wr) = (want[0][skip + i], want[1][skip + i]);
        if left[first + i] == 0.0 && right[first + i] == 0.0 && (wl != 0.0 || wr != 0.0) {
            // Paused: the rest is silence.
            assert!(i >= 441 * 20, "silence at {i}");
            break;
        }
        assert!((left[first + i] - wl).abs() <= 1e-6 && (right[first + i] - wr).abs() <= 1e-6, "sample {i}");
    }
}
