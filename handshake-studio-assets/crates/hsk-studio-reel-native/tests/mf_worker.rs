#![cfg(all(feature = "mf-worker", feature = "test-faults", windows))]
//! H.264 through the OS-provided Windows Media Foundation decoder in the isolated `reel-native-mf-worker`
//! (run with `--features mf-worker,test-faults` on Windows; cargo builds the worker binary for this test).
//!
//! Fixtures (`tests/fixtures/h264/provenance.json`): ffmpeg 8.1.1 / libx264 encodes of `testsrc2` 128x72,
//! oracle = ffmpeg's per-frame SHA-256 of the decoded yuv420p frame (`*.framehash`). H.264 decoding is
//! normative, so the comparison is exact (no tolerance). A host without Media Foundation or without a
//! synchronous software H.264 decoder MFT cannot prove the decode: those tests print `NOT_PROVEN:` and
//! assert the typed `MEDIA_CODEC_UNAVAILABLE` instead (V4-03).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use hsk_studio_reel_native::*;
use std::{io::Cursor, sync::Arc, thread, time::Duration};

const WORKER: &str = env!("CARGO_BIN_EXE_reel-native-mf-worker");
const BASELINE: &[u8] = include_bytes!("fixtures/h264/h264-baseline-128x72-24f.mp4");
const BASELINE_HASH: &str = include_str!("fixtures/h264/h264-baseline-128x72-24f.framehash");
const HIGH_BF2: &[u8] = include_bytes!("fixtures/h264/h264-high-bf2-128x72-24f.mp4");
const HIGH_BF2_HASH: &str = include_str!("fixtures/h264/h264-high-bf2-128x72-24f.framehash");
const HIGH422: &[u8] = include_bytes!("fixtures/h264/h264-high422-10bit-128x72-12f.mp4");
/// One frame at 30000/1001 fps in Pulse ticks (254,016,000 per millisecond).
const FRAME_TICKS: u64 = 8_475_667_200;
/// 128 x 72 4:2:0 8-bit, tight planes.
const FRAME_BYTES: usize = 128 * 72 + 2 * 64 * 36;

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

#[derive(Debug, Default)]
struct Collect {
    chunks: Vec<(i64, u64, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push((c.pts, c.index, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(bytes: &[u8], start: u64, count: u64) -> SegmentRequest {
    SegmentRequest {
        codec: ISOBMFF_PROVIDER_ID.to_owned(),
        start_pts: Ticks::new(start),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("h264-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn provider(options: MfWorkerOptions) -> WorkerMfProvider {
    WorkerMfProvider::new(options.with_worker_program(WORKER)).unwrap()
}

fn decode(
    worker: &WorkerMfProvider,
    bytes: &[u8],
    start: u64,
    count: u64,
    limits: &Limits,
) -> Result<(SegmentReceipt, Collect), ReelError> {
    let registry = Registry::standard().with_video_decoder(Arc::new(worker.clone()));
    let mut sink = Collect::default();
    let receipt = registry.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        None,
        &request(bytes, start, count),
        limits,
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&full_digest(&mut Cursor::new(bytes), 4096, &CancellationToken::default()).unwrap())
}

/// The SHA-256 column of an ffmpeg `framehash` file, in presentation order.
fn oracle(framehash: &str) -> Vec<String> {
    framehash
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.rsplit(',').next().unwrap().trim().to_owned())
        .collect()
}

fn unavailable(error: &ReelError) -> bool {
    matches!(error, ReelError::CodecUnavailable { .. })
}

/// The HELLO identity names the MFT, its CLSID, `dxva=0` and `os-licensed` (printed so the lead can record
/// the host's real decoder); every frame equals ffmpeg's decode byte for byte; PTS, receipt and decoder id
/// are exact; random access with pre-roll from the previous sync sample gives the same frames.
#[test]
fn h264_baseline_software_decode_exact_vs_oracle() {
    bounded_case("h264_baseline", || {
        let worker = provider(MfWorkerOptions::default());
        let limits = Limits::default();
        let expected = oracle(BASELINE_HASH);
        assert_eq!(expected.len(), 24);
        assert!(expected[0].starts_with("38510c300e4699f7"));
        let (receipt, sink) = match decode(&worker, BASELINE, 0, 24, &limits) {
            Err(error) if unavailable(&error) => {
                println!(
                    "NOT_PROVEN: no Media Foundation H.264 software decoder on this host ({error:?}); identity {:?}",
                    worker.descriptor().decoder_version
                );
                return;
            }
            other => other.unwrap(),
        };
        let identity = worker.descriptor();
        println!(
            "mf-h264 identity: id {} version {} licence {}",
            identity.decoder_id, identity.decoder_version, identity.licence
        );
        assert_eq!(receipt.decoder_id, "mf-h264");
        assert!(receipt.decoder_version.ends_with("|dxva=0|os-licensed"));
        assert_eq!(identity.licence, H264_LICENCE);
        assert!(receipt.complete, "{receipt:?}");
        let video = receipt.video.unwrap();
        assert_eq!(
            (video.width, video.height, video.bit_depth, video.chroma),
            (128, 72, 8, ChromaFormat::Yuv420)
        );
        assert_eq!(sink.chunks.len(), 24);
        let digests: Vec<String> = sink.chunks.iter().map(|c| sha256_hex(&c.2)).collect();
        for (n, ((pts, _, bytes), digest)) in sink.chunks.iter().zip(&digests).enumerate() {
            assert_eq!(bytes.len(), FRAME_BYTES, "frame {n}");
            assert_eq!(*pts, (n as u64 * FRAME_TICKS) as i64, "frame {n} pts");
            assert_eq!(digest, &expected[n], "frame {n} differs from the ffmpeg oracle");
        }
        // Random access: frame 5 (pre-roll from the sync sample at 0) and frames 12..15 (sync at 12).
        for (start, count) in [(5, 1), (12, 4), (20, 4)] {
            let (receipt, sink) = decode(&worker, BASELINE, start * FRAME_TICKS, count, &limits).unwrap();
            assert!(receipt.complete);
            let got: Vec<String> = sink.chunks.iter().map(|c| sha256_hex(&c.2)).collect();
            let want = &expected[start as usize..(start + count) as usize];
            assert_eq!(got, want, "span {start}+{count}");
        }
        let stats = worker.stats();
        assert_eq!((stats.crashed, stats.protocol_faults), (0, 0), "{stats:?}");
        assert!(stats.spawned >= 1 && stats.alive <= 2, "{stats:?}");
    });
}

/// Spike S1 and the reorder policy: High profile with two B-frames (`max_num_reorder_frames = 2`). The
/// result is either the exact oracle in presentation order or the typed refusal
/// `Unsupported mf-reorder-needs-packet-timing` with a `NOT_PROVEN:` line (never a silent partial decode).
#[test]
fn h264_high_bf2_reorder_spike_and_policy() {
    bounded_case("h264_high_bf2", || {
        let worker = provider(MfWorkerOptions::default());
        let limits = Limits::default();
        let expected = oracle(HIGH_BF2_HASH);
        assert!(expected[0].starts_with("79c95135ab274e5c"));
        match decode(&worker, HIGH_BF2, 0, 24, &limits) {
            Ok((receipt, sink)) => {
                println!("SPIKE S1: decoder kept input times and released in packet order");
                assert!(receipt.complete);
                let got: Vec<String> = sink.chunks.iter().map(|c| sha256_hex(&c.2)).collect();
                assert_eq!(got, expected);
            }
            Err(error) if unavailable(&error) => {
                println!("NOT_PROVEN: no Media Foundation H.264 software decoder on this host ({error:?})");
            }
            Err(error) => {
                println!(
                    "NOT_PROVEN: B-frame stream refused ({error:?}); presentation-order delivery needs reel P2 packet timing"
                );
                assert_eq!(
                    error,
                    ReelError::Unsupported {
                        what: MF_REORDER_REFUSAL
                    }
                );
            }
        }
        let stats = worker.stats();
        assert_eq!((stats.crashed, stats.protocol_faults), (0, 0), "{stats:?}");
    });
}

fn baseline_config() -> Vec<u8> {
    let index = build_index(
        &mut Cursor::new(BASELINE.to_vec()),
        BASELINE.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    let config = index.tracks[0].codec_config.clone();
    assert_eq!(config.first(), Some(&1), "avcC version");
    config
}

/// Pre-spawn refusals (hostile declared sizes, decoder limits, unsupported profile/chroma) and the
/// aggregate frame budget: typed, and no worker process is ever started.
#[test]
fn mf_declared_limits_and_unsupported_streams() {
    bounded_case("mf_declared_limits", || {
        let worker = provider(MfWorkerOptions::default());
        let config = baseline_config();
        let cancel = CancellationToken::default();
        let spec = |width: u32, height: u32, limits: Limits| VideoStreamSpec {
            codec: VideoCodec::H264,
            container: "isobmff",
            config: &config,
            color: ColorTag::Untagged,
            width,
            height,
            limits,
        };
        let open = |s: VideoStreamSpec<'_>| match worker.open(&s, &cancel) {
            Ok(_) => panic!("{}x{} must be refused before a worker starts", s.width, s.height),
            Err(error) => error.into_reel(),
        };
        let limits = Limits::default();
        // dims_u32_max: the 4:2:0 byte size overflows.
        assert_eq!(
            open(spec(u32::MAX, u32::MAX, limits)),
            ReelError::LimitExceeded {
                what: "h264-declared-dimensions"
            }
        );
        for (width, height) in [(47, 47), (47, 72), (128, 47), (4097, 72), (128, 2305)] {
            assert_eq!(
                open(spec(width, height, limits)),
                ReelError::Unsupported {
                    what: "mf-picture-size"
                },
                "{width}x{height}"
            );
        }
        // aggregate_over_budget: the declared picture above the caller's chunk budget.
        let tight = Limits {
            max_chunk_bytes: FRAME_BYTES - 1,
            ..Limits::default()
        };
        assert_eq!(
            open(spec(128, 72, tight)),
            ReelError::LimitExceeded {
                what: "h264-frame-bytes"
            }
        );
        // Wrong codec on an H.264 provider.
        let mut other = spec(128, 72, limits);
        other.codec = VideoCodec::Av1;
        assert!(matches!(open(other), ReelError::CodecUnavailable { .. }));
        // High 4:2:2 10-bit (profile_idc 122) through the full route: typed before any spawn.
        let error = decode(&worker, HIGH422, 0, 12, &limits).unwrap_err();
        assert_eq!(
            error,
            ReelError::Unsupported {
                what: "h264-profile"
            }
        );
        assert_eq!(worker.stats().spawned, 0, "{:?}", worker.stats());
    });
}

/// Supervision through the shared core: crash on OPEN and on a packet, a hung decode, a garbage reply
/// end as typed decoder faults with the worker reaped; the provider stays usable afterwards; and a mutation
/// sweep over the fixture's sample bytes ends as `Ok` or a typed error within the deadline, host alive.
#[test]
fn mf_worker_fault_matrix() {
    bounded_case("mf_worker_fault_matrix", || {
        let limits = Limits::default();
        let clean = provider(MfWorkerOptions::default());
        if let Err(error) = decode(&clean, BASELINE, 0, 2, &limits)
            && unavailable(&error)
        {
            println!("NOT_PROVEN: no Media Foundation H.264 software decoder on this host ({error:?})");
            return;
        }
        for (fault, expected) in [
            (WorkerTestFault::ExitOnOpen, DecoderFault::Crashed),
            (WorkerTestFault::ExitOnPacket, DecoderFault::Crashed),
            (WorkerTestFault::HangOnPacket, DecoderFault::Deadline),
            (WorkerTestFault::GarbageReply, DecoderFault::Protocol),
        ] {
            let faulty = provider(MfWorkerOptions {
                test_fault: fault,
                batch_deadline: Duration::from_secs(2),
                ..MfWorkerOptions::default()
            });
            let error = decode(&faulty, BASELINE, 0, 2, &limits).unwrap_err();
            assert_eq!(error, ReelError::DecoderFault { fault: expected }, "{fault:?}");
            let stats = faulty.stats();
            assert_eq!(stats.alive, 0, "{fault:?}: worker reaped {stats:?}");
        }
        // Mutation sweep: single-byte replacements across the mdat payload.
        let mdat = BASELINE
            .windows(4)
            .position(|w| w == b"mdat")
            .expect("mdat box")
            + 4;
        let mut outcomes = (0_u32, 0_u32);
        for (step, offset) in (mdat..BASELINE.len()).step_by(331).enumerate() {
            let mut mutated = BASELINE.to_vec();
            mutated[offset] ^= 0x5A_u8.rotate_left(step as u32 % 8);
            match decode(&clean, &mutated, 0, 24, &limits) {
                Ok(_) => outcomes.0 += 1,
                Err(error) => {
                    assert!(
                        matches!(
                            error,
                            ReelError::Corrupt { .. }
                                | ReelError::Unsupported { .. }
                                | ReelError::DecoderFault { .. }
                                | ReelError::LimitExceeded { .. }
                        ),
                        "offset {offset}: {error:?}"
                    );
                    outcomes.1 += 1;
                }
            }
        }
        println!("mutation sweep: {} ok, {} typed errors", outcomes.0, outcomes.1);
        assert!(outcomes.0 + outcomes.1 >= 40);
        // Still usable after the sweep, and exact.
        let (receipt, sink) = decode(&clean, BASELINE, 0, 24, &limits).unwrap();
        assert!(receipt.complete);
        assert_eq!(sha256_hex(&sink.chunks[23].2), oracle(BASELINE_HASH)[23]);
    });
}
