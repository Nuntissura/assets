//! Lanes SMB-AUDIO-IMPORT-WIRING and SMB-AUDIO-IMPORT-2 (feature `media`): opening an MP3, FLAC, Opus
//! or WAV file lands in a one-clip sequence whose score mixdown (pulse `TimelineMixStream`) is the
//! decoded file; files with more than 2 channels fold to stereo by the BS.775 3/2 -> 2/0 equations (or
//! a caller channel map); the mixdown exports to FLAC (STREAMINFO MD5 set) and AIFF and decodes back;
//! the desktop viewer opens audio files; hostile files fail typed. With feature `audio-device` the
//! imported file also plays through `audio_out` on score-device's software device.
//! Fixtures are reel's generated audio fixtures (provenance in
//! `crates/hsk-studio-reel/tests/fixtures/audio/provenance.json`) and WAV files written by the test
//! from known samples (`support_audio::wav`).
#![cfg(feature = "media")]

mod support_audio;

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::audio_import::{
    AudioImport, BS775_K, ImportLimits, ImportOptions, MixFormat, Speaker, export_mix, import_audio,
    import_audio_with, open_audio_file,
};
use hsk_studio_reel as reel;
use std::io::Cursor;
use support_audio::{WavFormat, fixture, signal, wav};

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
        let mut cursor = Cursor::new(Vec::new());
        let mix = import().mix_stream(1000, &cancel).unwrap();
        let receipt = export_mix(mix, format, &mut cursor, &cancel).unwrap();
        let out = cursor.into_inner();
        assert_eq!((receipt.frames, receipt.channels, receipt.sample_rate), (frames as u64, 2, 44_100));
        assert_eq!(receipt.bytes_written, out.len() as u64);
        let back = reel::decode_audio_file(&mut Cursor::new(&out), &reel::Limits::default(), 1 << 24, &cancel).unwrap();
        assert_eq!((back.sample_rate, back.channels, back.frames()), (44_100, 2, frames), "{format:?}");
        if let MixFormat::Flac { .. } = format {
            // The STREAMINFO MD5 written after streaming is the one reel's decoder verifies.
            assert_eq!(back.checksum_verified, Some(true), "{format:?}");
            let md5 = receipt.flac_md5.as_deref().unwrap();
            assert_eq!(reel::hex(&out[26..42]), md5);
            assert_ne!(md5, "00000000000000000000000000000000");
        } else {
            assert_eq!(receipt.flac_md5, None);
        }
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
    let refused = export_mix(mix, MixFormat::Flac { bits: 20 }, &mut Cursor::new(Vec::new()), &cancel).err().unwrap();
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
    // 6 channels without a channel order (plain WAV, no mask): a channel map is required.
    let six = wav(6, 48_000, WavFormat::Int16, None, &signal(100, 6));
    let refused = code(six.clone(), &limits);
    assert_eq!(refused.code, "audio_channel_map_required", "{}", refused.detail);
    // A map of the wrong length.
    let wrong = ImportOptions { channel_map: Some(vec![Speaker::FrontLeft; 5]), ..ImportOptions::default() };
    assert_eq!(import_audio_with(&mut Cursor::new(six), &limits, &wrong, &cancel).err().unwrap().code, "audio_channel_map");
    // Hostile WAV headers: RF64, a data chunk before fmt, a block align that disagrees, 9 channels.
    let good = wav(2, 48_000, WavFormat::Int16, None, &signal(10, 2));
    let mut rf64 = good.clone();
    rf64[..4].copy_from_slice(b"RF64");
    assert!(code(rf64, &limits).detail.contains("rf64"));
    let mut align = good.clone();
    let fmt_at = good.windows(4).position(|w| w == b"fmt ").unwrap();
    align[fmt_at + 8 + 12] = 3;
    assert!(code(align, &limits).detail.contains("wav-block-align"));
    let mut nine = good.clone();
    nine[fmt_at + 8 + 2] = 9;
    assert!(code(nine, &limits).detail.contains("wav-channels"));
    for cut in [12, 20, fmt_at + 12] {
        let e = code(good[..cut].to_vec(), &limits);
        assert_eq!(e.code, "audio_decode", "cut {cut}: {}", e.detail);
    }
    // A data chunk shorter than declared imports the whole frames present.
    let short = import_audio(&mut Cursor::new(good[..good.len() - 6].to_vec()), &limits, &cancel).unwrap();
    assert_eq!(short.frames, 8);
    // File size bound checked from the metadata before reading.
    let small = ImportLimits { max_file_bytes: 100, ..limits };
    assert_eq!(open_audio_file(&fixture("flac-s16-44k1-2ch.flac"), &small, &cancel).err().unwrap().code, "audio_read");
    // A byte-flip sweep over the MP3 never panics: each result imports or fails typed.
    let mp3 = std::fs::read(fixture("mp3-44k1-2ch-cbr.mp3")).unwrap();
    for at in (0..mp3.len()).step_by(97) {
        let mut b = mp3.clone();
        b[at] ^= 0xA5;
        if let Err(e) = import_audio(&mut Cursor::new(b), &limits, &cancel) {
            assert!(["audio_decode", "audio_empty", "audio_channel_map_required", "audio_rate"].contains(&e.code), "{at}: {}", e.code);
        }
    }
}

/// WAV goes through the same import (16/24-bit integer and float, plain and extensible); a 5.1 file
/// folds to stereo by BS.775-4 Annex 4 Table 2 (`L' = L + 0.7071 C + 0.7071 LS`, LFE omitted) whatever
/// the codec's channel order (WAV mask, Opus / Vorbis order, AAC element order); a channel map
/// overrides the decoder's layout; normalisation divides by the largest coefficient sum.
#[test]
fn wav_and_surround_files_import_with_the_bs775_downmix_and_channel_map() {
    let cancel = CancellationToken::default();
    let limits = ImportLimits::default();
    let k = BS775_K;
    let stereo = signal(1000, 2);
    for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
        for mask in [None, Some(0x3)] {
            let a = import_audio(&mut Cursor::new(wav(2, 44_100, format, mask, &stereo)), &limits, &cancel).unwrap();
            assert_eq!((a.sample_rate, a.frames, a.channels_in_file, a.decoder_id), (44_100, 1000, 2, reel::audio::WAV_FILE_DECODER_ID));
            assert!(a.downmix.is_none());
            let got: Vec<f32> = a.planar[0].iter().zip(&a.planar[1]).flat_map(|(l, r)| [*l, *r]).collect();
            assert_eq!(got, stereo, "the written samples come back exactly");
        }
    }
    // 5.1 extensible WAV (mask 0x3F: FL FR FC LFE BL BR) against the hand-written equations.
    let six = signal(800, 6);
    let a = import_audio(&mut Cursor::new(wav(6, 48_000, WavFormat::Float32, Some(0x3F), &six)), &limits, &cancel).unwrap();
    let d = a.downmix.as_ref().unwrap();
    assert_eq!((d.layout_source, a.channels_in_file, a.planar.len()), ("decoder", 6, 2));
    for (i, f) in six.chunks_exact(6).enumerate() {
        let (fl, fr, c, bl, br) = (f[0], f[1], f[2], f[4], f[5]);
        assert!((a.planar[0][i] - (fl + k * c + k * bl)).abs() <= 1e-6, "L {i}");
        assert!((a.planar[1][i] - (fr + k * c + k * br)).abs() <= 1e-6, "R {i}");
    }
    // Normalised: divided by 1 + 2 x 0.7071.
    let norm = ImportOptions { normalize_downmix: true, ..ImportOptions::default() };
    let n = import_audio_with(&mut Cursor::new(wav(6, 48_000, WavFormat::Float32, Some(0x3F), &six)), &limits, &norm, &cancel).unwrap();
    assert!(n.downmix.as_ref().unwrap().normalized);
    for i in 0..800 {
        assert!((n.planar[0][i] - a.planar[0][i] / (1.0 + 2.0 * k)).abs() <= 1e-6);
    }
    // The codecs' own orders: Opus 5.1 (Vorbis order FL C FR RL RR LFE) and AAC 5.1 (element order
    // C L R Ls Rs LFE); each downmix equals the equations on reel's own decode.
    // order[j] = index in the file of FL, FR, C, LFE, BL, BR.
    for (name, order) in [("opus-48k-6ch.opus", [0usize, 2, 1, 5, 3, 4]), ("aac-lc-48k-6ch.aac", [1, 2, 0, 5, 3, 4])] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let decoded = reel::decode_audio_file(&mut Cursor::new(&bytes), &reel::Limits::default(), 1 << 24, &cancel).unwrap();
        let a = import_audio(&mut Cursor::new(bytes), &limits, &cancel).unwrap();
        assert_eq!(a.downmix.as_ref().unwrap().layout_source, "decoder", "{name}");
        for (i, f) in decoded.interleaved.chunks_exact(6).enumerate() {
            let g = |j: usize| f[order[j]];
            assert!((a.planar[0][i] - (g(0) + k * g(2) + k * g(4))).abs() <= 1e-6, "{name} L {i}");
            assert!((a.planar[1][i] - (g(1) + k * g(2) + k * g(5))).abs() <= 1e-6, "{name} R {i}");
        }
    }
    // A channel map: a plain 6-channel WAV (no order of its own) mapped as 5.1 with side surrounds,
    // and a swapped stereo file.
    use Speaker::*;
    let map = ImportOptions {
        channel_map: Some(vec![FrontLeft, FrontRight, FrontCenter, LowFrequency, SideLeft, SideRight]),
        ..ImportOptions::default()
    };
    let m = import_audio_with(&mut Cursor::new(wav(6, 48_000, WavFormat::Float32, None, &six)), &limits, &map, &cancel).unwrap();
    assert_eq!(m.downmix.as_ref().unwrap().layout_source, "channel_map");
    for (i, f) in six.chunks_exact(6).enumerate() {
        assert!((m.planar[0][i] - (f[0] + k * f[2] + k * f[4])).abs() <= 1e-6);
        assert!((m.planar[1][i] - (f[1] + k * f[2] + k * f[5])).abs() <= 1e-6);
    }
    let swap = ImportOptions { channel_map: Some(vec![FrontRight, FrontLeft]), ..ImportOptions::default() };
    let s = import_audio_with(&mut Cursor::new(wav(2, 44_100, WavFormat::Int16, None, &stereo)), &limits, &swap, &cancel).unwrap();
    assert_eq!(s.planar[0], stereo.chunks_exact(2).map(|f| f[1]).collect::<Vec<_>>());
    assert_eq!(s.planar[1], stereo.chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>());
}

/// The desktop viewer opens audio files (headless egui context, render-cpu): the import lands in the
/// audio panel and the program monitor opens on its sequence; the panel's export writes a FLAC that
/// decodes back with its MD5 verified; a WAV opens the same way.
#[test]
fn desktop_viewer_opens_audio_files_headlessly() {
    use hsk_studio_harness::view::audio_import::widget;
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    let cancel = CancellationToken::default();
    let dir = support_audio::scratch("viewer");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&fixture("mp3-44k1-2ch-cbr.mp3")).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    let state = v.state_json();
    assert_eq!(state["audio"]["import"]["sample_rate"], 44_100, "{state}");
    assert_eq!(state["audio"]["import"]["frames"], 13_230);
    assert_eq!(state["audio"]["import"]["decoder"]["id"], reel::audio::MP3_DECODER_ID);
    assert_eq!(state["audio"]["sequence_id"], "seq-audio-import");
    assert!(v.monitor().is_some(), "the program monitor opens on the import sequence");
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for id in [widget::PANEL, widget::EXPORT_FLAC, widget::EXPORT_AIFF] {
        assert!(v.drawn().iter().any(|d| d == id), "{id} not drawn: {:?}", v.drawn());
    }
    // The mixdown a host plays (audio_out) is the whole file.
    let mix = v.audio_mix_stream(1000).unwrap().unwrap();
    assert_eq!(mix.length_samples(), 13_230);
    // Export through the viewer: FLAC 24-bit, MD5 verified on decode.
    let out = dir.join("viewer-mix.flac");
    let receipt = v.export_audio(&out, MixFormat::Flac { bits: 24 }, false).unwrap();
    let back = reel::decode_audio_file(&mut Cursor::new(std::fs::read(&out).unwrap()), &reel::Limits::default(), 1 << 24, &cancel).unwrap();
    assert_eq!((back.frames(), back.checksum_verified), (13_230, Some(true)));
    assert_eq!(receipt.flac_md5.as_deref().map(str::len), Some(32));
    assert_eq!(v.state_json()["audio"]["last_export"]["frames"], 13_230);
    // No overwrite without the flag; never over the opened file.
    assert!(v.export_audio(&out, MixFormat::Aiff { bits: 16 }, false).unwrap_err().contains("output_exists"));
    assert!(v.export_audio(&fixture("mp3-44k1-2ch-cbr.mp3"), MixFormat::Aiff { bits: 16 }, true).unwrap_err().contains("opened file"));
    // A WAV opens through the same path.
    let wav_path = dir.join("tone.wav");
    std::fs::write(&wav_path, wav(2, 48_000, WavFormat::Int24, None, &signal(4800, 2))).unwrap();
    v.open(&wav_path).unwrap();
    assert_eq!(v.state_json()["audio"]["import"]["decoder"]["id"], reel::audio::WAV_FILE_DECODER_ID);
    assert_eq!(v.audio().unwrap().import.audio.frames, 4800);
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
        let _ = m.refresh(&out, &NoFrames, out.mix().timeline().unwrap().sources(), &cancel).unwrap();
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
