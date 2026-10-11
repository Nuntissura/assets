//! Matroska `A_PCM/*` decode through the shared PCM path, and `ContentEncodings` (header stripping, zlib,
//! stacked encodings, `CodecPrivate` scope) undone per frame; encryption and everything else refused with
//! a typed error. Fixtures and their oracles: `tests/fixtures/README.md` (ffmpeg/ffprobe/Python zlib ran
//! once, offline; nothing here runs them). Expected values below are the oracles' output, not the
//! implementation's.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use sha2::{Digest, Sha256};
use std::io::Cursor;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 60s deadline: {e}"),
    }
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
struct Collect {
    /// (kind, pts, index, preroll, bytes)
    chunks: Vec<(ChunkKind, i64, u64, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks
            .push((c.kind, c.pts, c.index, c.preroll, c.bytes.to_vec()));
        Ok(())
    }
}

struct NeverReached;
impl ChunkSink for NeverReached {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        panic!("the sink must not be reached");
    }
}

fn request(bytes: &[u8], start: u64, count: u64, output: OutputMode) -> SegmentRequest {
    SegmentRequest {
        codec: MATROSKA_PROVIDER_ID.into(),
        start_pts: Ticks::new(start),
        count,
        track: Some(1),
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("mkv-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn run(
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    MatroskaProvider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        req,
        limits,
        &CancellationToken::default(),
        sink,
    )
}

fn index_of(bytes: &[u8]) -> Result<MkvIndex, ReelError> {
    build_matroska_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

/// `bytes` with the single occurrence of `pattern` replaced.
fn patched(bytes: &[u8], pattern: &[u8], replacement: &[u8]) -> Vec<u8> {
    assert_eq!(pattern.len(), replacement.len());
    let hits: Vec<usize> = bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| *w == pattern)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits.len(), 1, "pattern {pattern:02x?} must occur once");
    let mut out = bytes.to_vec();
    out[hits[0]..hits[0] + pattern.len()].copy_from_slice(replacement);
    out
}

const MS: u64 = 254_016_000; // Pulse ticks per millisecond

struct PcmFixture {
    name: &'static str,
    bytes: &'static [u8],
    codec_id: &'static str,
    rate: u32,
    channels: u16,
    bits: u16,
    output: &'static str,
    /// SHA-256 and size of the raw little-endian PCM libavcodec decodes from the file.
    raw_sha: &'static str,
    raw_bytes: usize,
    /// (pts ms, payload bytes, block position) as libavformat demuxes the blocks: `pos` is where the
    /// Block data starts, i.e. the track-number vint, after the SimpleBlock id and size.
    blocks: [(i64, u32, u64); 5],
}

/// Block header in these files: track-number vint (1) + relative timestamp (2) + flags (1) = 4 bytes
/// between libavformat's `pos` and the first PCM byte (RFC 9559 section 10.1, no lacing).
const SIMPLE_BLOCK_HEADER: u64 = 4;

const PCM: [PcmFixture; 5] = [
    PcmFixture {
        name: "s16le 48k stereo",
        bytes: include_bytes!("fixtures/mkv/pcm-s16le-48k-2ch.mkv"),
        codec_id: "A_PCM/INT/LIT",
        rate: 48000,
        channels: 2,
        bits: 16,
        output: "pcm_s16le",
        raw_sha: "31d4adb2621cec7cf1c53d79f5db56eede744f5f126ac3cef6a5828a9882e387",
        raw_bytes: 19200,
        blocks: [
            (0, 4096, 457),
            (21, 4096, 4560),
            (43, 4096, 8663),
            (64, 4096, 12766),
            (85, 2816, 16869),
        ],
    },
    PcmFixture {
        name: "s16be 48k stereo (swapped to little endian)",
        bytes: include_bytes!("fixtures/mkv/pcm-s16be-48k-2ch.mkv"),
        codec_id: "A_PCM/INT/BIG",
        rate: 48000,
        channels: 2,
        bits: 16,
        output: "pcm_s16le",
        raw_sha: "31d4adb2621cec7cf1c53d79f5db56eede744f5f126ac3cef6a5828a9882e387",
        raw_bytes: 19200,
        blocks: [
            (0, 4096, 457),
            (21, 4096, 4560),
            (43, 4096, 8663),
            (64, 4096, 12766),
            (85, 2816, 16869),
        ],
    },
    PcmFixture {
        name: "s24le 48k stereo",
        bytes: include_bytes!("fixtures/mkv/pcm-s24le-48k-2ch.mkv"),
        codec_id: "A_PCM/INT/LIT",
        rate: 48000,
        channels: 2,
        bits: 24,
        output: "pcm_s24le",
        raw_sha: "d1f9db3a0ca0895b10acfa948673b01872e25ad021a298b82ce7eb602f6b8dcc",
        raw_bytes: 28800,
        blocks: [
            (0, 6144, 457),
            (21, 6144, 6608),
            (43, 6144, 12759),
            (64, 6144, 18910),
            (85, 4224, 25061),
        ],
    },
    PcmFixture {
        name: "f32le 44.1k mono",
        bytes: include_bytes!("fixtures/mkv/pcm-f32le-44k1-1ch.mkv"),
        codec_id: "A_PCM/FLOAT/IEEE",
        rate: 44100,
        channels: 1,
        bits: 32,
        output: "pcm_f32le",
        raw_sha: "ad5965850f27653c1b75dadd2a78fc75fbc18bc52bb1941cbf289319a9f47e48",
        raw_bytes: 17640,
        blocks: [
            (0, 4096, 460),
            (23, 4096, 4563),
            (46, 4096, 8666),
            (70, 4096, 12769),
            (93, 1256, 16872),
        ],
    },
    PcmFixture {
        name: "u8 48k mono (unsigned)",
        bytes: include_bytes!("fixtures/mkv/pcm-u8-48k-1ch.mkv"),
        codec_id: "A_PCM/INT/LIT",
        rate: 48000,
        channels: 1,
        bits: 8,
        output: "pcm_u8",
        raw_sha: "c96f0c517503a8a41189b8cab2366bfe973334843ea064ebbe99cf0fb3fa52ac",
        raw_bytes: 4800,
        blocks: [
            (0, 1024, 453),
            (21, 1024, 1484),
            (43, 1024, 2515),
            (64, 1024, 3546),
            (85, 704, 4577),
        ],
    },
];

fn pcm_decode(f: &PcmFixture, start: u64, count: u64) -> (SegmentReceipt, Collect) {
    let mut sink = Collect::default();
    let receipt = run(
        f.bytes,
        &request(f.bytes, start, count, OutputMode::Decoded),
        &Limits::default(),
        &mut sink,
    )
    .unwrap_or_else(|e| panic!("{}: {e}", f.name));
    (receipt, sink)
}

#[test]
fn matroska_pcm_decodes_bit_exact_against_the_ffmpeg_oracle() {
    bounded_case("matroska_pcm_decodes_bit_exact", || {
        for f in &PCM {
            let block = usize::from(f.channels) * usize::from(f.bits / 8);
            let frames = (f.raw_bytes / block) as u64;
            let tps = 254_016_000_000 / u64::from(f.rate);

            // Index: layout facts, the block positions libavformat reports, and the compact PCM stream.
            let index = index_of(f.bytes).unwrap();
            let track = index.track(1).unwrap();
            assert_eq!(track.info.codec_id, f.codec_id, "{}", f.name);
            assert_eq!(
                (track.info.channels, track.info.bit_depth),
                (f.channels, f.bits),
                "{}",
                f.name
            );
            assert_eq!(track.info.sampling_frequency, f64::from(f.rate), "{}", f.name);
            let samples = track.index.samples();
            assert_eq!(samples.len(), f.blocks.len(), "{}", f.name);
            for (k, (pts, size, pos)) in f.blocks.iter().enumerate() {
                assert_eq!(samples[k].offset, pos + SIMPLE_BLOCK_HEADER, "{} {k}", f.name);
                assert_eq!(
                    (samples[k].pts, samples[k].size),
                    (*pts, *size),
                    "{} {k}",
                    f.name
                );
            }
            let pcm = track.pcm.as_ref().expect("valid PCM track has a stream");
            let window = pcm.window.clone().unwrap();
            assert_eq!(
                (window.presentation_start, window.first_frame, window.frames),
                (0, 0, frames),
                "{}",
                f.name
            );
            assert_eq!(window.ticks_per_frame, tps, "{}", f.name);

            // Whole stream: bytes equal libavcodec's raw decode, accounting is exact.
            let (receipt, sink) = pcm_decode(f, 0, frames);
            let raw: Vec<u8> = sink.chunks.iter().flat_map(|c| c.4.clone()).collect();
            assert_eq!(raw.len(), f.raw_bytes, "{}", f.name);
            assert_eq!(sha(&raw), f.raw_sha, "{}", f.name);
            assert!(receipt.complete && receipt.ticks_exact, "{}", f.name);
            assert_eq!(receipt.disposition, Disposition::Exact, "{}", f.name);
            assert_eq!(receipt.codec, f.output, "{}", f.name);
            assert_eq!(receipt.decoder_id, "matroska-pcm");
            assert_eq!(receipt.actual.units, frames, "{}", f.name);
            assert_eq!(receipt.actual.end_pts, Ticks::new(frames * tps), "{}", f.name);
            // Payload bytes read plus the index that was built from the same source for this call.
            assert_eq!(
                receipt.bytes_read,
                f.raw_bytes as u64 + index.bytes_read,
                "{}",
                f.name
            );
            // One chunk per block, positioned by the running frame count (not the rounded block times).
            assert_eq!(sink.chunks.len(), f.blocks.len(), "{}", f.name);
            let mut cursor = 0u64;
            for (k, chunk) in sink.chunks.iter().enumerate() {
                assert_eq!(chunk.0, ChunkKind::AudioSamples);
                assert_eq!((chunk.1 as u64, chunk.2), (cursor * tps, cursor), "{} {k}", f.name);
                cursor += (chunk.4.len() / block) as u64;
            }

            // A range crossing the first block boundary (1024 frames), cut mid-block on both sides.
            let (receipt, sink) = pcm_decode(f, 1000 * tps, 100);
            let part: Vec<u8> = sink.chunks.iter().flat_map(|c| c.4.clone()).collect();
            assert_eq!(part, raw[1000 * block..1100 * block], "{}", f.name);
            assert_eq!(receipt.disposition, Disposition::Exact, "{}", f.name);
            assert_eq!(receipt.actual.start_pts, Ticks::new(1000 * tps), "{}", f.name);
            // A start inside a frame is reported as the containing frame, never silently moved.
            let (receipt, _) = pcm_decode(f, 1000 * tps + 1, 100);
            assert_eq!(receipt.disposition, Disposition::Approximate, "{}", f.name);
            assert_eq!(receipt.actual.start_pts, Ticks::new(1000 * tps), "{}", f.name);

            // Beyond the stream: typed, never clamped.
            for (start, count) in [(frames * tps, 1), (0, frames + 1), (u64::MAX / 2, 1)] {
                let err = run(
                    f.bytes,
                    &request(f.bytes, start, count, OutputMode::Decoded),
                    &Limits::default(),
                    &mut NeverReached,
                )
                .unwrap_err();
                assert_eq!(err.code(), "MEDIA_OUT_OF_RANGE", "{} {start} {count}", f.name);
            }
            // A chunk limit smaller than one frame is a limit, not a truncated frame.
            let tiny = Limits {
                max_chunk_bytes: block - 1,
                ..Limits::default()
            };
            let err = run(
                f.bytes,
                &request(f.bytes, 0, 1, OutputMode::Decoded),
                &tiny,
                &mut NeverReached,
            )
            .unwrap_err();
            assert_eq!(
                err,
                ReelError::LimitExceeded {
                    what: "chunk-smaller-than-sample"
                },
                "{}",
                f.name
            );
        }

        // Through the registry: magic-number detection, then the provider id.
        let f = &PCM[0];
        let registry = Registry::standard();
        let provider = registry.detect(&mut Cursor::new(f.bytes.to_vec())).unwrap();
        assert_eq!(provider.descriptor().id, MATROSKA_PROVIDER_ID);
        let mut sink = Collect::default();
        registry
            .decode_segment(
                &mut Cursor::new(f.bytes.to_vec()),
                None,
                &request(f.bytes, 0, 4800, OutputMode::Decoded),
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(sink.chunks.len(), 5);
        // probe reports the PCM window (frames and exact ticks per frame), not the block count.
        let layout = provider
            .probe(
                &mut Cursor::new(f.bytes.to_vec()),
                &Limits::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!((layout.units, layout.ticks_per_unit, layout.channels), (4800, 5_292_000, 2));
    });
}

#[test]
fn matroska_pcm_hostile_parameters_and_timestamps_refuse_cleanly() {
    bounded_case("matroska_pcm_hostile", || {
        let f = &PCM[0];
        let decode = |bytes: &[u8]| {
            run(
                bytes,
                &request(bytes, 0, 100, OutputMode::Decoded),
                &Limits::default(),
                &mut Collect::default(),
            )
        };
        // Block timestamps are only a consistency check: a 1-unit rounding difference is accepted ...
        let rounded = patched(f.bytes, &[0x81, 0x00, 0x15, 0x80], &[0x81, 0x00, 0x16, 0x80]);
        assert!(decode(&rounded).is_ok());
        // ... a block that starts far from where the running frame count puts it (dropped audio) is a
        // typed refusal for decoding, while the file still indexes.
        let gap = patched(f.bytes, &[0x81, 0x00, 0x15, 0x80], &[0x81, 0x00, 0x50, 0x80]);
        assert!(index_of(&gap).is_ok());
        assert_eq!(
            decode(&gap).unwrap_err(),
            ReelError::Unsupported {
                what: "pcm-timestamp-discontinuity"
            }
        );
        // Declared layout the PCM path cannot honour: bit depth 12, channel count 0, sampling rate 0.
        for (pattern, replacement, what) in [
            (&[0x62u8, 0x64, 0x81, 0x10][..], &[0x62u8, 0x64, 0x81, 0x0C][..], "pcm-bit-depth"),
            (&[0x9Fu8, 0x81, 0x02][..], &[0x9Fu8, 0x81, 0x00][..], "pcm-channel-count"),
        ] {
            let bytes = patched(f.bytes, pattern, replacement);
            assert!(index_of(&bytes).is_ok(), "{what}");
            assert_eq!(
                decode(&bytes).unwrap_err(),
                ReelError::Unsupported { what },
                "{what}"
            );
        }
        // A block that is not a whole number of frames (last block's size cut by one byte: the cluster
        // then has one trailing byte, which is not an element) is corruption of that track.
        let odd = patched(f.bytes, &[0xA3, 0x4B, 0x04], &[0xA3, 0x4B, 0x03]);
        assert!(index_of(&odd).is_ok());
        assert_eq!(
            decode(&odd).unwrap_err(),
            ReelError::Corrupt {
                what: "pcm-block-size"
            }
        );
        // Every single-byte corruption of the whole file: typed result or success, never a panic.
        for at in 0..f.bytes.len().min(2048) {
            let mut bytes = f.bytes.to_vec();
            bytes[at] ^= 0xFF;
            let _ = index_of(&bytes);
            let _ = decode(&bytes);
        }
        // The aggregate budget: more blocks than the sample budget allows is refused while indexing.
        let few = Limits {
            max_samples: 3,
            ..Limits::default()
        };
        let err = build_matroska_index(
            &mut Cursor::new(f.bytes.to_vec()),
            f.bytes.len() as u64,
            &few,
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(err, ReelError::LimitExceeded { what: "sample-count" });
    });
}

const ENC_ZLIB: &[u8] = include_bytes!("fixtures/mkv/enc-zlib.mkv");
const ENC_STRIP: &[u8] = include_bytes!("fixtures/mkv/enc-headerstrip.mkv");
const ENC_STACKED: &[u8] = include_bytes!("fixtures/mkv/enc-stacked-private.mkv");
const ENC_BOMB: &[u8] = include_bytes!("fixtures/mkv/enc-zlib-bomb.mkv");

/// Decoded frame sizes and SHA-256: ffprobe `-show_data_hash sha256` of the single-encoding files, and the
/// frames `make_encoded_mkv.py` generated (identical for all three files).
const FRAME_SIZES: [usize; 7] = [3000, 2500, 4096, 4, 700, 1200, 50];
const FRAME_SHA: [&str; 7] = [
    "c3dfa6c37cd827a270cf2a62b6bdc63fd6879ad8adef06154bba6cd6e148ce15",
    "9f56289419d8052b82174e9524c8e884e605711b49c64b1e61485bdc1034ee4e",
    "bbc781b5f4cc88281b5f8369a9de9d7663604337c84bee0f62542916c1fe487d",
    "4b6c2a58462fe8e03e0de52ca764e23845c78780c31d315b9317b70555094d34",
    "2fb716e871daf84b29dc5f7068e3d067bd1233023161cb423788591e30fac60a",
    "76f9dfa224463a85431f1ac19f79a591a7341b65fa9fb79b54bba136bea51af2",
    "ab620903370cb3bc4a6b0a9a92795054a2c2373c67942b8a267065fb0e2a6efd",
];
/// Presentation time (ms) of each frame: five blocks, the last one laced with three frames.
const FRAME_MS: [u64; 7] = [0, 40, 80, 120, 160, 200, 240];

fn packets(bytes: &[u8], start_ms: u64, count: u64) -> Result<(SegmentReceipt, Collect), ReelError> {
    let mut sink = Collect::default();
    let receipt = run(
        bytes,
        &request(bytes, start_ms * MS, count, OutputMode::Packets),
        &Limits::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

#[test]
fn matroska_content_encodings_are_undone_exactly_like_ffprobe_and_python_zlib() {
    bounded_case("matroska_content_encodings_undone", || {
        for (name, bytes) in [
            ("zlib", ENC_ZLIB),
            ("header stripping", ENC_STRIP),
            ("stacked zlib + header stripping, private scope", ENC_STACKED),
        ] {
            let index = index_of(bytes).unwrap();
            let track = index.track(1).unwrap();
            assert!(track.info.content_encoded && track.info.content_refusal.is_none(), "{name}");
            assert_eq!(track.index.samples().len(), 7, "{name}");

            // All seven frames (the last three share one Xiph-laced block; lacing data is not encoded).
            let (receipt, sink) = packets(bytes, 0, 7).unwrap();
            assert_eq!(sink.chunks.len(), 7, "{name}");
            for (k, chunk) in sink.chunks.iter().enumerate() {
                assert_eq!(chunk.4.len(), FRAME_SIZES[k], "{name} frame {k}");
                assert_eq!(sha(&chunk.4), FRAME_SHA[k], "{name} frame {k}");
                assert_eq!((chunk.1 as u64, chunk.2), (FRAME_MS[k] * MS, k as u64), "{name} {k}");
                assert!(!chunk.3, "{name} {k}: not pre-roll");
            }
            assert!(receipt.complete && receipt.disposition == Disposition::Exact, "{name}");
            // The receipt counts the STORED bytes it read (plus the index built for this call), not the
            // decoded size.
            let stored: u64 = track.index.samples().iter().map(|s| u64::from(s.size)).sum();
            assert_eq!(receipt.bytes_read, stored + index.bytes_read, "{name}");
            let decoded: usize = FRAME_SIZES.iter().sum();
            if name != "header stripping" {
                assert!(stored < decoded as u64, "{name}: zlib really compressed");
            }

            // Random access into the laced block: pre-roll frames are decoded the same way.
            let (_, sink) = packets(bytes, 200, 2).unwrap();
            assert_eq!(sink.chunks.len(), 7, "{name}: sync sample 0 .. frame 6");
            for (k, chunk) in sink.chunks.iter().enumerate() {
                assert_eq!(sha(&chunk.4), FRAME_SHA[k], "{name} random access frame {k}");
                assert_eq!(chunk.3, k < 5, "{name} frame {k} pre-roll flag");
            }
        }
        // The stored zero-length frame of the header-stripping file restores to the stripped header.
        let index = index_of(ENC_STRIP).unwrap();
        assert_eq!(index.track(1).unwrap().index.samples()[3].size, 0);
        let (_, sink) = packets(ENC_STRIP, 120, 1).unwrap();
        assert_eq!(sink.chunks.last().unwrap().4, [0x00, 0x00, 0x01, 0xB3]);

        // Structure as stored, and CodecPrivate decoded under the private scope.
        let index = index_of(ENC_STACKED).unwrap();
        let info = &index.track(1).unwrap().info;
        assert_eq!(
            info.content_encodings,
            [
                MkvContentEncoding {
                    order: 1,
                    scope: 3,
                    kind: MkvEncodingKind::Compression(MkvCompression::Zlib)
                },
                MkvContentEncoding {
                    order: 0,
                    scope: 1,
                    kind: MkvEncodingKind::Compression(MkvCompression::HeaderStripping(vec![
                        0, 0, 1, 0xB3
                    ]))
                },
            ]
        );
        assert_eq!(
            sha(&info.codec_private),
            "4e19ce0ace8389a5109c115da869892346465d77667371c1a707d6382fe0e61b"
        );
        // Decoded video routes pass the decoded config and decoded packets (nothing registered: typed).
        assert_eq!(
            run(
                ENC_ZLIB,
                &request(ENC_ZLIB, 0, 1, OutputMode::Decoded),
                &Limits::default(),
                &mut NeverReached
            )
            .unwrap_err(),
            ReelError::CodecUnavailable {
                codec: "V_VP8".into()
            }
        );
    });
}

#[test]
fn matroska_content_encodings_hostile_inputs_refuse_cleanly() {
    bounded_case("matroska_content_encodings_hostile", || {
        let packets_with = |bytes: &[u8], sink: &mut dyn ChunkSink| {
            run(
                bytes,
                &request(bytes, 0, 7, OutputMode::Packets),
                &Limits::default(),
                sink,
            )
        };
        let refused = |bytes: &[u8]| {
            // The file still indexes; delivery of the track is refused before anything is read.
            let index = index_of(bytes).unwrap();
            assert!(index.track(1).unwrap().info.content_refusal.is_some());
            packets_with(bytes, &mut NeverReached).unwrap_err()
        };
        // Encryption (ContentEncodingType 1) is refused with its own typed reason.
        assert_eq!(
            refused(&patched(ENC_ZLIB, &[0x50, 0x33, 0x81, 0x00], &[0x50, 0x33, 0x81, 0x01])),
            ReelError::Unsupported {
                what: "content-encryption"
            }
        );
        // Deprecated algorithms (bzlib 1, lzo1x 2), an unknown algorithm and bad scopes.
        for (algo, want) in [
            (0x01, ReelError::Unsupported { what: "content-compression-algo" }),
            (0x02, ReelError::Unsupported { what: "content-compression-algo" }),
            (0x09, ReelError::Unsupported { what: "content-encoding" }),
        ] {
            assert_eq!(
                refused(&patched(ENC_ZLIB, &[0x42, 0x54, 0x81, 0x00], &[0x42, 0x54, 0x81, algo])),
                want,
                "algo {algo}"
            );
        }
        for (scope, want) in [
            (0x04, ReelError::Unsupported { what: "content-encoding-scope-next" }),
            (0x00, ReelError::Corrupt { what: "content-encoding-scope" }),
            (0x08, ReelError::Corrupt { what: "content-encoding-scope" }),
        ] {
            assert_eq!(
                refused(&patched(ENC_ZLIB, &[0x50, 0x32, 0x81, 0x01], &[0x50, 0x32, 0x81, scope])),
                want,
                "scope {scope}"
            );
        }

        // Damaged zlib data and a damaged checksum are Corrupt for that frame.
        let index = index_of(ENC_ZLIB).unwrap();
        let first = index.track(1).unwrap().index.samples()[0];
        for at in [
            first.offset as usize + 5,
            first.offset as usize + first.size as usize - 1,
        ] {
            let mut bytes = ENC_ZLIB.to_vec();
            bytes[at] ^= 0xFF;
            assert_eq!(
                packets_with(&bytes, &mut Collect::default()).unwrap_err(),
                ReelError::Corrupt { what: "content-zlib" },
                "flip at {at}"
            );
        }

        // Decompression bomb: a 16 KB stored frame inflates to 16 MiB; the cap is the chunk limit.
        let bomb = |max_chunk_bytes: usize, sink: &mut dyn ChunkSink| {
            run(
                ENC_BOMB,
                &request(ENC_BOMB, 0, 1, OutputMode::Packets),
                &Limits {
                    max_chunk_bytes,
                    ..Limits::default()
                },
                sink,
            )
        };
        assert_eq!(
            bomb(8 * 1024 * 1024, &mut NeverReached).unwrap_err(),
            ReelError::LimitExceeded {
                what: "content-decoded-bytes"
            }
        );
        let mut sink = Collect::default();
        bomb(32 * 1024 * 1024, &mut sink).unwrap();
        let frame = &sink.chunks[0].4;
        assert_eq!(frame.len(), 16 * 1024 * 1024);
        assert!(frame[..4] == [0, 0, 1, 0xB3] && frame[4..].iter().all(|b| *b == 0));

        // The aggregate budget applies to encoded tracks too (7 frames, budget 3).
        let few = Limits {
            max_samples: 3,
            ..Limits::default()
        };
        assert_eq!(
            build_matroska_index(
                &mut Cursor::new(ENC_ZLIB.to_vec()),
                ENC_ZLIB.len() as u64,
                &few,
                &CancellationToken::default()
            )
            .unwrap_err(),
            ReelError::LimitExceeded { what: "sample-count" }
        );

        // Every single-byte corruption of both encoded files: a typed result or success, never a panic.
        for (bytes, step) in [(ENC_ZLIB, 1usize), (ENC_STRIP, 5)] {
            for at in (0..bytes.len()).step_by(step) {
                let mut hostile = bytes.to_vec();
                hostile[at] ^= 0xFF;
                let _ = index_of(&hostile);
                let _ = packets_with(&hostile, &mut Collect::default());
            }
        }
    });
}
