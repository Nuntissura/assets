#![cfg(feature = "av1")]
//! AV1 worker-process isolation contract (run with `--features av1`; cargo builds the
//! `reel-native-av1-worker` binary for this test).
//!
//! Fixture: reel's `av1-16x16-3f.{mp4,mkv}` (16x16 4:2:0 8-bit, 3 frames). The oracle is libdav1d's
//! per-frame SHA-256 recorded in reel's `provenance.json` (the same strings reel's own AV1 test uses), plus
//! equality with reel's in-process decode of the same segment. The hostile mutations are single-byte
//! replacements of the mp4 `mdat` payload (file offsets 817..1073) that reel's lane measured as process
//! ABORTS of the in-process decoder (rav1d 1.1.0 `decode.rs:4997`, raw rows in
//! `Handshake_Artifacts/WP-KERNEL-STUDIO/reel-av1/mut-results.txt`); here they must end as typed errors
//! with the test process alive.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use hsk_studio_reel_native::*;
use std::{
    io::Cursor,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_reel-native-av1-worker");
const MP4: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/av1/av1-16x16-3f.mp4");
const MKV: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/av1/av1-16x16-3f.mkv");
/// Raw yuv420p frame SHA-256 from `ffmpeg -c:v libdav1d ... -f framehash -hash sha256`.
const ORACLE: [&str; 3] = [
    "124247cf2dfe43d502cbcd58d1247c61b495507271ccdc9442b39b5dd8f82423",
    "c7d6df9a67f2b718788ff80c36a4692f2916a0ccff393bf5bef02a92d872b099",
    "8059c70336722e95b8b1514f79fa12772669a2c932f80b16afea4900d79ecf32",
];
const MS: u64 = 254_016_000; // Pulse ticks per millisecond
const PTS_MP4: [u64; 3] = [0, 10_584_000_000, 21_168_000_000];
const PTS_MKV: [u64; 3] = [0, 42 * MS, 83 * MS];

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

#[derive(Debug, PartialEq, Eq)]
struct Owned {
    kind: ChunkKind,
    pts: i64,
    duration: Ticks,
    index: u64,
    units: u64,
    sync: bool,
    preroll: bool,
    trim_start: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Collect {
    chunks: Vec<Owned>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push(Owned {
            kind: c.kind,
            pts: c.pts,
            duration: c.duration,
            index: c.index,
            units: c.units,
            sync: c.sync,
            preroll: c.preroll,
            trim_start: c.trim_start,
            bytes: c.bytes.to_vec(),
        });
        Ok(())
    }
}

fn request(codec: &str, bytes: &[u8], start: u64, count: u64) -> SegmentRequest {
    SegmentRequest {
        codec: codec.to_owned(),
        start_pts: Ticks::new(start),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("av1-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = full_digest(&mut Cursor::new(bytes), 4096, &CancellationToken::default()).unwrap();
    hex(&digest)
}

fn provider(options: Av1WorkerOptions) -> WorkerAv1Provider {
    WorkerAv1Provider::new(options.with_worker_program(WORKER)).unwrap()
}

/// reel's in-process decode (container provider id, `OutputMode::Decoded`) for a TRUSTED stream only: the
/// route runs rav1d in this process only when the caller says so (these are reel's own fixtures).
fn in_process(
    registry: &Registry,
    bytes: &[u8],
    start: u64,
    count: u64,
) -> (SegmentReceipt, Collect) {
    let mut src = Cursor::new(bytes.to_vec());
    let id = registry.detect(&mut src).unwrap().descriptor().id;
    let mut sink = Collect::default();
    let receipt = registry
        .decode_segment_routed(
            &mut src,
            None,
            &request(id, bytes, start, count),
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
            SourceTrust::Trusted,
        )
        .unwrap();
    (receipt, sink)
}

fn via_worker(
    provider: &WorkerAv1Provider,
    bytes: &[u8],
    start: u64,
    count: u64,
    limits: &Limits,
) -> Result<(SegmentReceipt, Collect), Av1Error> {
    let mut sink = Collect::default();
    let receipt = provider.decode_av1(
        &mut Cursor::new(bytes.to_vec()),
        &request(AV1_PROVIDER_ID, bytes, start, count),
        limits,
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

fn digests(sink: &Collect) -> Vec<String> {
    sink.chunks.iter().map(|c| sha256_hex(&c.bytes)).collect()
}

#[test]
fn worker_decode_is_bit_exact_with_in_process_and_the_dav1d_oracle() {
    bounded_case("worker_decode_is_bit_exact", || {
        let worker = provider(Av1WorkerOptions::default());
        let mut registry = Registry::standard();
        registry.register(Box::new(worker.clone())).unwrap();
        let listed = registry.descriptors();
        assert!(
            listed
                .iter()
                .any(|d| d.id == "av1" && d.can_decode && d.bit_depths == [8, 10, 12])
        );
        let limits = Limits::default();

        for (name, bytes, pts) in [("mp4", MP4, PTS_MP4), ("mkv", MKV, PTS_MKV)] {
            // Whole clip, random access with pre-roll (last frame only), and a two-frame middle span:
            // chunks (pts, index, flags, bytes) and the whole receipt equal reel's in-process decode.
            for (start, count) in [(0, 3), (pts[2], 1), (pts[1], 2)] {
                let (expected_receipt, expected) = in_process(&registry, bytes, start, count);
                let mut sink = Collect::default();
                let mut src = Cursor::new(bytes.to_vec());
                let receipt = registry
                    .decode_segment(
                        &mut src,
                        None,
                        &request(AV1_PROVIDER_ID, bytes, start, count),
                        &limits,
                        &CancellationToken::default(),
                        &mut sink,
                    )
                    .unwrap();
                assert_eq!(receipt, expected_receipt, "{name} {start}+{count} receipt");
                assert_eq!(sink, expected, "{name} {start}+{count} chunks");
                assert_eq!(receipt.decoder_id, "rav1d");
                assert!(receipt.complete && receipt.video.is_some());
            }
            // Hash test: every frame equals libdav1d's output bit for bit; a repeat is identical.
            let (_, all) = via_worker(&worker, bytes, 0, 3, &limits).unwrap();
            assert_eq!(digests(&all), ORACLE, "{name}");
            let (_, again) = via_worker(&worker, bytes, 0, 3, &limits).unwrap();
            assert_eq!(all, again, "{name} second decode");
            let (receipt, last) = via_worker(&worker, bytes, pts[2], 1, &limits).unwrap();
            assert_eq!(digests(&last), [ORACLE[2]], "{name} random access");
            assert_eq!(receipt.actual.start_pts, Ticks::new(pts[2]));
        }

        // One worker served every segment above (no respawn, nothing crashed, nothing left running).
        let stats = worker.stats();
        assert_eq!(
            (stats.spawned, stats.crashed, stats.alive),
            (1, 0, 1),
            "{stats:?}"
        );

        // Typed refusals match the in-process ones and never cost a process: a reserved profile in the
        // av1C record is refused before any spawn; frames above the chunk limit are refused as a limit.
        let mut patched = MP4.to_vec();
        let at = patched.windows(4).position(|w| w == b"av1C").unwrap() + 4;
        patched[at + 1] = (patched[at + 1] & 0x1F) | (3 << 5);
        let before = worker.stats().spawned;
        let error = via_worker(&worker, &patched, 0, 1, &limits).unwrap_err();
        assert_eq!(
            error,
            Av1Error::Reel(ReelError::Unsupported {
                what: "av1-profile"
            })
        );
        assert_eq!(worker.stats().spawned, before);
        let tiny = Limits {
            max_chunk_bytes: 256,
            ..Limits::default()
        };
        assert_eq!(
            via_worker(&worker, MP4, 0, 1, &tiny).unwrap_err().code(),
            "MEDIA_LIMIT_EXCEEDED"
        );
        // A request for another id, or a packets request, is not this provider's job.
        let mut wrong = request("isobmff", MP4, 0, 1);
        let mut sink = Collect::default();
        let refused = worker.decode_av1(
            &mut Cursor::new(MP4.to_vec()),
            &wrong,
            &limits,
            &CancellationToken::default(),
            &mut sink,
        );
        assert_eq!(refused.unwrap_err().code(), "MEDIA_CODEC_UNAVAILABLE");
        wrong.codec = AV1_PROVIDER_ID.to_owned();
        wrong.output = OutputMode::Packets;
        let refused = worker.decode_av1(
            &mut Cursor::new(MP4.to_vec()),
            &wrong,
            &limits,
            &CancellationToken::default(),
            &mut sink,
        );
        assert_eq!(refused.unwrap_err().code(), "MEDIA_UNSUPPORTED");
        assert!(sink.chunks.is_empty());
    });
}

#[test]
fn hostile_aborting_mutations_end_as_typed_errors_and_the_host_survives() {
    bounded_case("hostile_aborting_mutations", || {
        // The bound is exercised in the supervision case; here every crash may respawn.
        let worker = provider(Av1WorkerOptions {
            max_spawns: 64,
            spawn_window: Duration::from_secs(600),
            ..Av1WorkerOptions::default()
        });
        let limits = Limits::default();
        let mutate = |offset: usize, value: u8| {
            let mut bytes = MP4.to_vec();
            assert!(bytes[offset] != value, "mutation must change the byte");
            bytes[offset] = value;
            bytes
        };

        // Clean decode first: the worker is warm and correct.
        let (_, clean) = via_worker(&worker, MP4, 0, 3, &limits).unwrap();
        assert_eq!(digests(&clean), ORACLE);

        // Recorded as MEDIA_CORRUPT in the in-process probe: a typed decoder error that does NOT cost the
        // worker (the same process answers the next request).
        let corrupt = via_worker(&worker, &mutate(817, 0), 0, 3, &limits).unwrap_err();
        assert_eq!(corrupt.code(), "MEDIA_CORRUPT", "{corrupt:?}");
        assert!(matches!(corrupt, Av1Error::Reel(_)));
        // Recorded as decoding fine with changed content: same worker, complete, three frames.
        let (receipt, changed) = via_worker(&worker, &mutate(824, 0), 0, 3, &limits).unwrap();
        assert!(receipt.complete && changed.chunks.len() == 3);
        assert_ne!(
            digests(&changed),
            ORACLE,
            "the mutation must change the picture"
        );
        assert_eq!(worker.stats().spawned, 1, "typed errors must not respawn");

        // Recorded as PROCESS ABORTS in-process (rav1d decode.rs:4997 unwrap on None, inside an
        // extern "C" fn). Each one kills a worker; the call returns the contained fault.
        let aborting: [(usize, u8); 6] = [
            (826, 0),
            (949, 128),
            (966, 255),
            (994, 0),
            (1004, 128),
            (1009, 255),
        ];
        let started = Instant::now();
        for (offset, value) in aborting {
            let error = via_worker(&worker, &mutate(offset, value), 0, 3, &limits).unwrap_err();
            assert!(
                matches!(
                    error,
                    Av1Error::Worker(Av1WorkerFault::DecoderCrashed { .. })
                ),
                "mutation {offset}={value}: {error:?}"
            );
            assert_eq!(error.code(), "decoder_crashed");
        }
        // Through the reel port the same fault is reel's distinct ReelError::DecoderFault (not Corrupt).
        let mut sink = Collect::default();
        let ported = worker
            .decode_segment(
                &mut Cursor::new(mutate(826, 0)),
                &request(AV1_PROVIDER_ID, MP4, 0, 3),
                &limits,
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap_err();
        assert_eq!(
            ported,
            ReelError::DecoderFault {
                fault: DecoderFault::Crashed
            }
        );
        assert_eq!(ported.code(), "MEDIA_DECODER_FAULT");
        let crashes = aborting.len() as u64 + 1;
        let elapsed = started.elapsed();

        // The provider is still usable: a fresh worker decodes the clean clip bit-exactly again, and
        // every crashed process was reaped (only the one healthy idle worker is alive).
        let (_, after) = via_worker(&worker, MP4, 0, 3, &limits).unwrap();
        assert_eq!(digests(&after), ORACLE);
        let stats = worker.stats();
        assert_eq!(stats.crashed, crashes, "{stats:?}");
        assert_eq!(stats.spawned, 1 + crashes, "{stats:?}");
        assert_eq!(stats.alive, 1, "{stats:?}");
        assert!(stats.last_crash_exit_code.is_some());
        println!(
            "hostile: {crashes} crashes contained in {elapsed:?}; exit code {:?}",
            stats.last_crash_exit_code
        );
    });
}

#[test]
fn supervision_kills_on_deadline_cancel_oversize_garbage_and_bounds_respawn() {
    bounded_case("supervision", || {
        let limits = Limits::default();
        let faulty = |fault: WorkerTestFault, tweak: &dyn Fn(&mut Av1WorkerOptions)| {
            let mut options = Av1WorkerOptions {
                test_fault: fault,
                ..Av1WorkerOptions::default()
            };
            tweak(&mut options);
            provider(options)
        };

        // Wall-clock deadline: a worker that never answers is killed at the batch deadline.
        let hung = faulty(WorkerTestFault::HangOnPacket, &|o| {
            o.batch_deadline = Duration::from_millis(400);
        });
        let started = Instant::now();
        let error = via_worker(&hung, MP4, 0, 3, &limits).unwrap_err();
        assert_eq!(error, Av1Error::Worker(Av1WorkerFault::DecoderDeadline));
        assert!(started.elapsed() < Duration::from_secs(20));
        let stats = hung.stats();
        assert_eq!((stats.killed_deadline, stats.alive), (1, 0), "{stats:?}");

        // Output cap: a reply far above the declared 16x16 frame kills the worker.
        let big = faulty(WorkerTestFault::OversizeFrame, &|_| {});
        let error = via_worker(&big, MP4, 0, 3, &limits).unwrap_err();
        assert_eq!(error, Av1Error::Worker(Av1WorkerFault::OutputTooLarge));
        let stats = big.stats();
        assert_eq!((stats.killed_oversize, stats.alive), (1, 0), "{stats:?}");

        // Protocol violation: a frame of unknown kind kills the worker.
        let junk = faulty(WorkerTestFault::GarbageReply, &|_| {});
        let error = via_worker(&junk, MP4, 0, 3, &limits).unwrap_err();
        assert_eq!(error, Av1Error::Worker(Av1WorkerFault::Protocol));
        let stats = junk.stats();
        assert_eq!((stats.protocol_faults, stats.alive), (1, 0), "{stats:?}");

        // Bounded respawn: a worker that dies on every request (exit code 86) is replaced at most twice
        // per window, then the provider refuses to spawn instead of looping.
        let dying = faulty(WorkerTestFault::ExitOnPacket, &|o| {
            o.max_spawns = 2;
            o.spawn_window = Duration::from_secs(600);
        });
        for _ in 0..2 {
            let error = via_worker(&dying, MP4, 0, 3, &limits).unwrap_err();
            assert_eq!(
                error,
                Av1Error::Worker(Av1WorkerFault::DecoderCrashed {
                    exit_code: Some(86)
                })
            );
        }
        let error = via_worker(&dying, MP4, 0, 3, &limits).unwrap_err();
        assert_eq!(error, Av1Error::Worker(Av1WorkerFault::RespawnLimited));
        let stats = dying.stats();
        assert_eq!(
            (
                stats.spawned,
                stats.crashed,
                stats.spawn_denied,
                stats.alive
            ),
            (2, 2, 1, 0),
            "{stats:?}"
        );

        // Cancel and pool bound: with one worker allowed, a second call while the first is stuck is
        // refused at once (pool busy); cancelling the first kills its worker and returns Canceled.
        let stuck = faulty(WorkerTestFault::HangOnPacket, &|o| {
            o.max_workers = 1;
        });
        let cancel = CancellationToken::default();
        let first = {
            let (stuck, cancel) = (stuck.clone(), cancel.clone());
            thread::spawn(move || {
                let mut sink = Collect::default();
                stuck.decode_av1(
                    &mut Cursor::new(MP4.to_vec()),
                    &request(AV1_PROVIDER_ID, MP4, 0, 3),
                    &Limits::default(),
                    &cancel,
                    &mut sink,
                )
            })
        };
        // Wait until OPEN and the first PACKET are on the wire: the worker is hung inside a request.
        let waiting = Instant::now();
        while stuck.stats().requests < 2 {
            assert!(
                waiting.elapsed() < Duration::from_secs(20),
                "request never sent"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let error = via_worker(&stuck, MP4, 0, 3, &limits).unwrap_err();
        assert_eq!(error, Av1Error::Worker(Av1WorkerFault::PoolBusy));
        cancel.cancel();
        let outcome = first.join().unwrap();
        assert_eq!(outcome.unwrap_err(), Av1Error::Reel(ReelError::Canceled));
        let stats = stuck.stats();
        assert_eq!((stats.killed_canceled, stats.alive), (1, 0), "{stats:?}");
        // A token cancelled before the call never reaches the pool.
        let done = CancellationToken::default();
        done.cancel();
        let mut sink = Collect::default();
        let error = stuck
            .decode_av1(
                &mut Cursor::new(MP4.to_vec()),
                &request(AV1_PROVIDER_ID, MP4, 0, 3),
                &limits,
                &done,
                &mut sink,
            )
            .unwrap_err();
        assert_eq!(error, Av1Error::Reel(ReelError::Canceled));
        assert_eq!(stuck.stats().spawned, 1);
    });
}

#[test]
fn untrusted_decode_through_a_registry_is_routed_to_the_worker_and_survives_aborting_input() {
    bounded_case("untrusted_registry_routing", || {
        let worker = provider(Av1WorkerOptions {
            max_spawns: 64,
            spawn_window: Duration::from_secs(600),
            ..Av1WorkerOptions::default()
        });
        // The worker is registered as the registry's VIDEO DECODER (not as a provider): ordinary
        // isobmff/matroska calls, untrusted by default, reach it.
        let registry = Registry::standard().with_video_decoder(Arc::new(worker.clone()));
        assert_eq!(
            registry
                .video_decoder_identity()
                .map(|identity| identity.id),
            Some("rav1d")
        );
        let limits = Limits::default();
        let decode = |registry: &Registry, bytes: &[u8], container: &str| {
            let mut sink = Collect::default();
            let result = registry.decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                None,
                &request(container, bytes, 0, 3),
                &limits,
                &CancellationToken::default(),
                &mut sink,
            );
            (result, sink)
        };

        for (bytes, container) in [(MP4, ISOBMFF_PROVIDER_ID), (MKV, MATROSKA_PROVIDER_ID)] {
            let before = worker.stats().requests;
            let (receipt, sink) = decode(&registry, bytes, container);
            let receipt = receipt.unwrap();
            // OPEN + three PACKETs + FINISH went to the worker process, and the output is bit-exact
            // with the dav1d oracle and equal (chunks and whole receipt) to the trusted in-process decode.
            assert!(worker.stats().requests >= before + 5, "{container}");
            assert_eq!(digests(&sink), ORACLE, "{container}");
            let (expected_receipt, expected) = in_process(&registry, bytes, 0, 3);
            assert_eq!(receipt, expected_receipt, "{container}");
            assert_eq!(sink, expected, "{container}");
            assert_eq!(receipt.decoder_id, "rav1d");
        }
        assert_eq!(
            worker.stats().spawned,
            1,
            "one worker served both containers"
        );

        // A byte replacement that ABORTS the in-process decoder, through the same plain registry call:
        // this test process is still alive to assert, and the answer is reel's distinct decoder fault.
        let mut hostile = MP4.to_vec();
        hostile[826] = 0;
        let (result, _) = decode(&registry, &hostile, ISOBMFF_PROVIDER_ID);
        let error = result.unwrap_err();
        assert_eq!(
            error,
            ReelError::DecoderFault {
                fault: DecoderFault::Crashed
            }
        );
        assert_eq!(error.code(), "MEDIA_DECODER_FAULT");
        assert_eq!(worker.stats().crashed, 1);
        // The pool recovered: the next untrusted call starts a fresh worker and decodes correctly.
        let (receipt, sink) = decode(&registry, MP4, ISOBMFF_PROVIDER_ID);
        assert!(receipt.unwrap().complete);
        assert_eq!(digests(&sink), ORACLE);
        assert_eq!(worker.stats().spawned, 2);

        // Without a registered worker the same untrusted call is CodecUnavailable: never in-process.
        let (result, sink) = decode(&Registry::standard(), MP4, ISOBMFF_PROVIDER_ID);
        assert_eq!(
            result.unwrap_err(),
            ReelError::CodecUnavailable {
                codec: "av01".to_owned()
            }
        );
        assert!(sink.chunks.is_empty());
    });
}
