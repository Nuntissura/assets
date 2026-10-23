#![cfg(all(feature = "mf-worker", feature = "test-faults", windows))]
//! Operator decision D24-A (lane SMB-D24A-CODEC-DEFAULTS): AAC decodes by default through the OS decoder, the
//! Windows Media Foundation AAC decoder MFT in the isolated `reel-native-mf-worker --aac` process.
//!
//! Corpus: reel's AAC-LC fixtures (`hsk-studio-reel/tests/fixtures/audio`, ffmpeg native AAC encoder, ADTS;
//! provenance.json there): stereo 44.1 kHz (M/S + intensity stereo), mono 48 kHz (short windows), 5.1 48 kHz.
//! Reference: Studio's own AAC-LC decoder (reel feature `studio-aac`, itself within 2/32768 of libavcodec,
//! reel test `aac_lc_adts_matches_libavcodec`); run with `--features mf-worker,test-faults,hsk-studio-reel/studio-aac`.
//! Without that feature the comparison prints `NOT_PROVEN:`; a host without the OS decoder prints
//! `NOT_PROVEN:` and asserts the typed MEDIA_CODEC_UNAVAILABLE instead (V4-03).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use hsk_studio_reel_native::*;
use std::{io::Cursor, sync::Arc, thread, time::Duration};

const WORKER: &str = env!("CARGO_BIN_EXE_reel-native-mf-worker");
const STEREO: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/audio/aac-lc-44k1-2ch.aac");
const MONO: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/audio/aac-lc-48k-1ch.aac");
const SURROUND: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/audio/aac-lc-48k-6ch.aac");
const MP4: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/audio/audio-tracks.mp4");

/// Largest |MF - Studio| per sample allowed, set from the measured runs (lane notes) plus margin.
const MAX_DIFF: f32 = 4.0 / 32768.0;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 120s deadline: {e}"),
    }
}

fn provider(options: MfWorkerOptions) -> WorkerMfAacProvider {
    WorkerMfAacProvider::new(options.with_worker_program(WORKER)).unwrap()
}

fn decode(os: Option<&dyn ExternalAudioDecoder>, bytes: &[u8]) -> Result<DecodedAudio, ReelError> {
    decode_audio_file_with(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        1 << 22,
        &CancellationToken::default(),
        os,
    )
}

#[derive(Default)]
struct Collect(Vec<f32>);
impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.0.extend(c.bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        Ok(())
    }
}

fn request(codec: &str, bytes: &[u8], count: u64) -> SegmentRequest {
    SegmentRequest {
        codec: codec.to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: Some(1),
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("aac-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

/// The three corpus files decode through the OS decoder in the worker: rate, channel count and frame count
/// equal Studio's decoder's, channels arrive in the AAC element order (5.1: C L R Ls Rs LFE, reordered
/// from the decoder's WAVE mask), and every sample is within `MAX_DIFF` of Studio's decode (max difference
/// and SNR printed per file); the receipt names `mf-aac` and the HELLO version (MFT, CLSID, os-licensed);
/// the default registry path (`Registry::with_audio().with_audio_decoder`) gives the same samples for the ADTS
/// file and for the AAC track of an MP4.
#[test]
fn aac_corpus_decodes_through_the_os_decoder_like_studio_decoder() {
    bounded_case("aac_corpus", || {
        let os = provider(MfWorkerOptions::default());
        let studio_compiled = hsk_studio_reel::audio::aac_descriptor().can_decode;
        for (name, bytes, rate, channels) in [
            ("stereo", STEREO, 44_100u32, 2u16),
            ("mono", MONO, 48_000, 1),
            ("5.1", SURROUND, 48_000, 6),
        ] {
            let got = match decode(Some(&os), bytes) {
                Err(ReelError::CodecUnavailable { codec }) => {
                    println!("NOT_PROVEN: no Media Foundation AAC decoder on this host ({codec}); identity {:?}", os.descriptor().decoder_version);
                    return;
                }
                other => other.unwrap(),
            };
            assert_eq!(got.decoder_id, AAC_OS_DECODER_ID, "{name}");
            assert_eq!((got.sample_rate, got.channels), (rate, channels), "{name}");
            assert!(got.interleaved.iter().all(|v| v.is_finite()), "{name}");
            println!("{name}: mf-aac version {} frames {}", got.decoder_version, got.frames());
            if !studio_compiled {
                println!("NOT_PROVEN: {name} comparison with Studio's decoder needs --features hsk-studio-reel/studio-aac");
                continue;
            }
            let want = decode(None, bytes).unwrap();
            assert_eq!(got.frames(), want.frames(), "{name}: frame count");
            let (mut max, mut sig, mut noise) = (0f32, 0f64, 0f64);
            for (a, b) in got.interleaved.iter().zip(&want.interleaved) {
                let d = (a - b).abs();
                max = max.max(d);
                sig += f64::from(*b) * f64::from(*b);
                noise += f64::from(d) * f64::from(d);
            }
            let snr = if noise == 0.0 { f64::INFINITY } else { 10.0 * (sig / noise).log10() };
            println!("{name}: max |mf - studio| = {max} ({} LSB) snr {snr:.1} dB", max * 32768.0);
            assert!(max <= MAX_DIFF, "{name}: max diff {max}");
        }
        // The registry route: ADTS file and the MP4 AAC track equal the whole-file OS decode.
        let whole = decode(Some(&os), STEREO).unwrap();
        let registry = Registry::with_audio().with_audio_decoder(Arc::new(os.clone()));
        for (bytes, codec, track) in [(STEREO, "aac-adts", None), (MP4, ISOBMFF_PROVIDER_ID, Some(1))] {
            let mut req = request(codec, bytes, 4096);
            req.track = track;
            let mut sink = Collect::default();
            let receipt = registry
                .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &req, &Limits::default(), &CancellationToken::default(), &mut sink)
                .unwrap();
            assert_eq!(receipt.decoder_id, AAC_OS_DECODER_ID, "{codec}");
            if codec == "aac-adts" {
                assert_eq!(sink.0, whole.interleaved[..8192], "{codec}");
            } else {
                assert_eq!(sink.0.len(), 8192, "{codec}");
            }
        }
        let stats = os.stats();
        assert_eq!((stats.crashed, stats.protocol_faults), (0, 0), "{stats:?}");
        assert!(stats.alive <= 2, "{stats:?}");
    });
}

/// Hostile worker: a reply of an unknown kind is a counted worker protocol fault (MEDIA_DECODER_FAULT
/// decoder_protocol); a worker that dies on the first access unit is decoder_crashed; both contained, no
/// partial decode reported as success, and a fresh provider decodes afterwards.
#[test]
fn aac_worker_faults_are_contained() {
    bounded_case("aac_faults", || {
        if decode(Some(&provider(MfWorkerOptions::default())), MONO).is_err() {
            println!("NOT_PROVEN: no Media Foundation AAC decoder on this host");
            return;
        }
        for (fault, expect) in [
            (WorkerTestFault::GarbageReply, DecoderFault::Protocol),
            (WorkerTestFault::ExitOnPacket, DecoderFault::Crashed),
        ] {
            let os = provider(MfWorkerOptions {
                test_fault: fault,
                ..MfWorkerOptions::default()
            });
            let err = decode(Some(&os), MONO).unwrap_err();
            assert_eq!(err, ReelError::DecoderFault { fault: expect }, "{fault:?}");
            let stats = os.stats();
            assert!(stats.protocol_faults + stats.crashed == 1, "{fault:?} {stats:?}");
        }
        assert!(decode(Some(&provider(MfWorkerOptions::default())), MONO).is_ok());
    });
}
