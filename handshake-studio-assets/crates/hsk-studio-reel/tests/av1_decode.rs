//! AV1 decode (feature `av1`): the in-process rav1d decoder behind the ISOBMFF and Matroska providers,
//! reached through the explicit trusted route (`VideoRoute::trusted()`).
//!
//! Fixture: `tests/fixtures/av1/av1-16x16-3f.{mp4,mkv}` (16x16 4:2:0 8-bit, 3 frames @24 fps, one GOP),
//! authored once offline by `generate.sh` (ffmpeg is a generator only). The expected per-frame SHA-256 are
//! the oracle: libdav1d's raw planar output recorded in `provenance.json`; the test cannot edit them.
#![cfg(feature = "av1")]
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::av1::{decoder_version, parse_av1c};
use hsk_studio_reel::*;
use sha2::{Digest, Sha256};
use std::io::Cursor;

const MP4: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mp4");
const MKV: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mkv");
/// Raw yuv420p frame SHA-256 from `ffmpeg -c:v libdav1d ... -f framehash -hash sha256`.
const ORACLE: [&str; 3] = [
    "124247cf2dfe43d502cbcd58d1247c61b495507271ccdc9442b39b5dd8f82423",
    "c7d6df9a67f2b718788ff80c36a4692f2916a0ccff393bf5bef02a92d872b099",
    "8059c70336722e95b8b1514f79fa12772669a2c932f80b16afea4900d79ecf32",
];
const MS: u64 = 254_016_000; // Pulse ticks per millisecond
/// Presentation times in ticks. MP4 keeps the exact 1/24 s grid; ffmpeg's Matroska muxer rounds frame
/// times to whole milliseconds (0, 42, 83 ms), which the Matroska index reports exactly as stored.
const PTS_MP4: [u64; 3] = [0, 10_584_000_000, 21_168_000_000];
const PTS_MKV: [u64; 3] = [0, 42 * MS, 83 * MS];

#[derive(Debug, Default)]
struct Collect {
    frames: Vec<(i64, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        assert_eq!(c.kind, ChunkKind::VideoFrame);
        assert!(!c.preroll);
        self.frames.push((c.pts, c.sync, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(provider_id: &str, bytes: &[u8], start: u64, count: u64) -> SegmentRequest {
    SegmentRequest {
        codec: provider_id.to_owned(),
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

fn decode(
    bytes: &[u8],
    start: u64,
    count: u64,
    limits: &Limits,
) -> Result<(SegmentReceipt, Collect), ReelError> {
    let registry = Registry::standard();
    let mut src = Cursor::new(bytes.to_vec());
    let provider = registry.detect(&mut src)?;
    let id = provider.descriptor().id;
    let mut sink = Collect::default();
    // The in-process decoder runs only for sources the caller explicitly marks trusted (these are the
    // repository's own generated fixtures); the default route is covered by tests/video_route.rs.
    let receipt = provider.decode_segment_routed(
        &mut src,
        &request(id, bytes, start, count),
        limits,
        &CancellationToken::default(),
        &mut sink,
        &VideoRoute::trusted(),
    )?;
    Ok((receipt, sink))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn av1_decodes_frames_with_layout_color_and_decoder_receipt_from_both_containers() {
    for (name, bytes, pts) in [("mp4", MP4, PTS_MP4), ("mkv", MKV, PTS_MKV)] {
        let (receipt, sink) = decode(bytes, 0, 3, &Limits::default()).unwrap();
        assert_eq!(sink.frames.len(), 3, "{name}");
        assert_eq!(receipt.actual.units, 3, "{name}");
        assert!(receipt.complete, "{name}");
        assert_eq!(receipt.disposition, Disposition::Exact, "{name}");
        // Decoder id + version are on the receipt (STU deterministic-decode contract).
        assert_eq!(receipt.decoder_id, "rav1d");
        assert_eq!(receipt.decoder_version, decoder_version());
        assert!(!decoder_version().is_empty());
        let video = receipt.video.expect("video format on the receipt");
        assert_eq!((video.width, video.height, video.bit_depth), (16, 16, 8));
        assert_eq!(video.chroma, ChromaFormat::Yuv420);
        assert_eq!(video.frame_bytes(), Some(384));
        // Colour comes from the AV1 sequence header (BT.709 matrix, limited range), not guessed.
        assert_eq!(video.color.origin, ColorOrigin::SequenceHeader, "{name}");
        assert_eq!(video.color.matrix, 1, "{name}");
        assert!(!video.color.full_range, "{name}");
        for (k, (got, sync, planes)) in sink.frames.iter().enumerate() {
            assert_eq!(*got as u64, pts[k], "{name} frame {k}");
            assert_eq!(*sync, k == 0, "{name} frame {k}");
            assert_eq!(planes.len(), 384, "{name} frame {k}");
        }
    }
    // The capability is enumerable.
    let d = hsk_studio_reel::av1::descriptor();
    assert!(d.can_decode && d.bit_depths == [8, 10, 12]);
}

#[test]
fn av1_output_is_bit_exact_against_dav1d_oracle_deterministic_and_seekable() {
    let digests = |bytes: &[u8], start: u64, count: u64| -> Vec<String> {
        decode(bytes, start, count, &Limits::default())
            .unwrap()
            .1
            .frames
            .iter()
            .map(|(_, _, b)| hash(b))
            .collect()
    };
    for (name, bytes, pts) in [("mp4", MP4, PTS_MP4), ("mkv", MKV, PTS_MKV)] {
        // Every frame equals libdav1d's output, bit for bit, and a second decode is identical.
        let first = digests(bytes, 0, 3);
        assert_eq!(first, ORACLE, "{name}");
        assert_eq!(digests(bytes, 0, 3), first, "{name} second decode");
        // Random access: start on the last frame; the two earlier frames are decoded as pre-roll only.
        let (receipt, sink) = decode(bytes, pts[2], 1, &Limits::default()).unwrap();
        assert_eq!(sink.frames.len(), 1, "{name}");
        assert_eq!(hash(&sink.frames[0].2), ORACLE[2], "{name}");
        assert_eq!(receipt.actual.start_pts, Ticks::new(pts[2]), "{name}");
        assert_eq!(receipt.actual.units, 1);
    }
}

#[test]
fn av1_refuses_unsupported_or_hostile_streams_with_typed_errors() {
    // av1C record: unknown record version and a reserved profile are `Unsupported`, a missing marker
    // bit is `Corrupt`; the same patched record is refused through the provider before any decode.
    assert_eq!(
        parse_av1c(&[0x82, 0x00, 0x00, 0x00]).unwrap_err(),
        ReelError::Unsupported {
            what: "av1c-version"
        }
    );
    assert_eq!(
        parse_av1c(&[0x01, 0x00, 0x00, 0x00]).unwrap_err(),
        ReelError::Corrupt {
            what: "av1c-marker"
        }
    );
    let mut patched = MP4.to_vec();
    let at = patched
        .windows(4)
        .position(|w| w == b"av1C")
        .expect("fixture has an av1C box")
        + 4;
    assert_eq!(patched[at], 0x81, "marker + version 1");
    patched[at + 1] = (patched[at + 1] & 0x1F) | (3 << 5);
    let err = decode(&patched, 0, 1, &Limits::default()).unwrap_err();
    assert_eq!(
        err,
        ReelError::Unsupported {
            what: "av1-profile"
        }
    );
    assert_eq!(err.code(), "MEDIA_UNSUPPORTED");
    // A frame larger than the chunk limit is refused as a limit, never truncated.
    let tiny = Limits {
        max_chunk_bytes: 256,
        ..Limits::default()
    };
    assert_eq!(
        decode(MP4, 0, 1, &tiny).unwrap_err().code(),
        "MEDIA_LIMIT_EXCEEDED"
    );
}

const MP4_1080P: &[u8] = include_bytes!("fixtures/av1/av1-1920x1080-2f.mp4");
/// Raw yuv420p frame SHA-256 of `av1-1920x1080-2f.mp4` from libdav1d (see `provenance.json`).
const ORACLE_1080P: [&str; 2] = [
    "deb0b61137e5d33deeffe5eda5125bfb860c9d86a1c7523e1b21b991b91cbd80",
    "ff3ac0c64bc2772c846a2a7083b25fa57161a66cc25b4128ad2dc7c8459acf34",
];

/// R5 SMB-REEL-ENG-3 (av1/mod.rs:148-151): the pixel limit was `max_chunk_bytes / 6` (the 4:4:4 16-bit
/// worst case), 1,398,101 px at default limits, so an ordinary 1920x1080 8-bit 4:2:0 stream (3,110,400
/// bytes packed, well inside the 8 MiB chunk limit) was refused as `av1-frame-resources`. The limit now
/// follows the stream's own `av1C` layout and bit depth; a limit that really cannot hold the frame still
/// refuses it with the typed error.
#[test]
fn av1_1080p_decodes_at_default_limits_and_a_smaller_limit_still_refuses_it() {
    let (receipt, sink) = decode(MP4_1080P, 0, 2, &Limits::default()).unwrap();
    let video = receipt.video.expect("video format on the receipt");
    assert_eq!(
        (video.width, video.height, video.bit_depth),
        (1920, 1080, 8)
    );
    assert_eq!(video.chroma, ChromaFormat::Yuv420);
    assert_eq!(video.frame_bytes(), Some(3_110_400));
    assert!(receipt.complete && receipt.actual.units == 2);
    let digests: Vec<String> = sink.frames.iter().map(|(_, _, b)| hash(b)).collect();
    assert_eq!(digests, ORACLE_1080P);

    // 2 MiB holds 1,398,101 px of 4:2:0 8-bit: smaller than 1080p, so the decoder refuses it up front.
    let small = Limits {
        max_chunk_bytes: 2 * 1024 * 1024,
        ..Limits::default()
    };
    assert_eq!(
        decode(MP4_1080P, 0, 1, &small).unwrap_err(),
        ReelError::LimitExceeded {
            what: "av1-frame-resources"
        }
    );
}
