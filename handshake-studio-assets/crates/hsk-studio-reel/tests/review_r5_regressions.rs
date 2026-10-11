//! One regression per confirmed STUDIO-R5 reel finding (kernel validation/R5-review-b.json, reviewed
//! commit 1d404f8): typed errors, never an arithmetic-overflow panic, and limits that follow the stream.
//! The AV1 1080p limit regression needs the decoder and lives in `av1_decode.rs` (feature `av1`).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
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
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}

const MP4: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mp4");

/// An external decoder that lies about the pictures it returns: the declared format cannot be matched
/// by any real plane, and the arithmetic that checks it must not overflow.
struct Lying {
    width: u32,
    height: u32,
    bit_depth: u8,
}

struct LyingSession {
    format: VideoFormat,
}

impl ExternalVideoDecoder for Lying {
    fn identity(&self) -> DecoderIdentity {
        DecoderIdentity {
            id: "lying-av1",
            version: "0",
        }
    }
    fn supports(&self, codec: VideoCodec) -> bool {
        codec == VideoCodec::Av1
    }
    fn open(
        &self,
        _spec: &VideoStreamSpec<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError> {
        Ok(Box::new(LyingSession {
            format: VideoFormat {
                width: self.width,
                height: self.height,
                bit_depth: self.bit_depth,
                chroma: ChromaFormat::Yuv420,
                color: ColorInfo {
                    primaries: 1,
                    transfer: 1,
                    matrix: 1,
                    full_range: false,
                    origin: ColorOrigin::SequenceHeader,
                },
            },
        }))
    }
}

impl VideoDecodeSession for LyingSession {
    fn send(
        &mut self,
        _packet: &[u8],
        tag: u64,
        _cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        out.accept(DecodedPicture {
            tag,
            format: self.format,
            film_grain_applied: false,
            planes: vec![vec![0; 4]; 3],
        })?;
        Ok(())
    }
    fn finish(
        &mut self,
        _cancel: &CancellationToken,
        _out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        Ok(())
    }
}

#[derive(Default)]
struct Collect(Vec<u64>);

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.0.push(c.index);
        Ok(())
    }
}

/// R5 SMB-REEL-AV1-ROUTE (video.rs:269): `planes_match_format` multiplied the picture's declared
/// width x height x bytes-per-sample unchecked. A decoder that reports u32::MAX x u32::MAX at 10 bit
/// overflowed u64 and panicked the host that the isolation exists to protect. It is a protocol fault.
#[test]
fn r5_route_lying_decoder_dimensions_cannot_overflow_the_plane_check() {
    bounded_case("r5_route_lying_decoder_dimensions", || {
        for (width, height, bit_depth) in [
            (u32::MAX, u32::MAX, 10),
            (u32::MAX, u32::MAX, 8),
            (u32::MAX, 3, 12),
            (65_536, 65_536, 12),
        ] {
            let registry = Registry::standard().with_video_decoder(std::sync::Arc::new(Lying {
                width,
                height,
                bit_depth,
            }));
            let req = SegmentRequest {
                codec: ISOBMFF_PROVIDER_ID.to_owned(),
                start_pts: Ticks::new(0),
                count: 1,
                track: None,
                output: OutputMode::Decoded,
                prefer: SourcePreference::Original,
                purpose: Purpose::Preview,
                grant: Grant {
                    content_id: ContentId::parse("av1-fixture").unwrap(),
                    revision: 1,
                    expected_len: MP4.len() as u64,
                },
                expected_revision: 1,
            };
            let mut sink = Collect::default();
            let result = registry.decode_segment(
                &mut Cursor::new(MP4.to_vec()),
                None,
                &req,
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            );
            assert_eq!(
                result.unwrap_err(),
                ReelError::DecoderFault {
                    fault: DecoderFault::Protocol
                },
                "{width}x{height}@{bit_depth}"
            );
            assert!(sink.0.is_empty(), "nothing reaches the sink");
        }
    });
}

fn block(track: u8, rel_ts: i16, key: bool, frames: &[u32]) -> MkvBlock {
    MkvBlock {
        track,
        rel_ts,
        key,
        frames: frames.to_vec(),
        lacing: Lacing::None,
        group: None,
        raw_lace_header: None,
    }
}

fn spec() -> MkvSpec {
    MkvSpec {
        doc_type: "matroska",
        timecode_scale: 1_000_000,
        unknown_sizes: false,
        with_cues: false,
        video_default_duration_ns: Some(40_000_000),
        content_encoding_on_video: false,
        clusters: vec![MkvCluster {
            timecode: 0,
            blocks: vec![block(1, 0, true, &[8]), block(2, 0, true, &[8])],
        }],
    }
}

fn index_with_dims(width: u64, height: u64) -> Result<MkvIndex, ReelError> {
    let bytes = mkv_bytes_with_video_dims(&spec(), width, height).bytes;
    build_matroska_index(
        &mut Cursor::new(bytes.clone()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

/// R5 SMB-REEL-NATIVE-AV1-WORKER cross-lane finding (matroska.rs:391-392): PixelWidth / PixelHeight were
/// accepted up to u32::MAX (and a value above u32::MAX silently became 0 = "undeclared"). Consumers do
/// width x height x planes x bytes arithmetic on those numbers; a hostile track must be refused at the
/// source with a typed limit instead.
#[test]
fn r5_matroska_declared_video_dimensions_are_bounded_at_the_source() {
    bounded_case("r5_matroska_declared_video_dimensions", || {
        let refused = ReelError::LimitExceeded {
            what: "video-dimensions",
        };
        for (w, h) in [
            (0xFFFF_FFFF_u64, 0xFFFF_FFFF_u64),
            (1 << 32, 16),
            (16, 1 << 32),
            (u64::MAX, 1),
            ((1 << 20) + 1, 16),
            (16, (1 << 20) + 1),
        ] {
            assert_eq!(index_with_dims(w, h).unwrap_err(), refused, "{w}x{h}");
        }
        // Real sizes are untouched, up to and including the bound itself (16K video, 1 << 20 per side).
        for (w, h) in [(64, 36), (8192, 4320), (16_384, 8_640), (1 << 20, 1 << 20)] {
            let index = index_with_dims(w, h).unwrap();
            let info = &index.track(1).unwrap().info;
            assert_eq!((u64::from(info.width), u64::from(info.height)), (w, h));
        }
        // A track that declares nothing (0 x 0) stays "undeclared".
        let index = index_with_dims(0, 0).unwrap();
        assert_eq!(index.track(1).unwrap().info.width, 0);
    });
}

/// R5 SMB-REEL-ENG-3: the decoder's pixel limit follows the `av1C` layout and bit depth instead of the
/// 4:4:4 16-bit worst case (`max_chunk_bytes / 6`, 1,398,101 px by default). The decode proof with the
/// real decoder is `av1_decode.rs` (feature `av1`); this pins the numbers in every build.
#[test]
fn r5_av1_pixel_limit_follows_the_record_layout_and_bit_depth() {
    // av1C: marker+version, profile/level, flags byte (high_bitdepth 0x40, twelve 0x20, mono 0x10,
    // subsampling_x 0x08, subsampling_y 0x04), presentation byte.
    let record = |profile: u8, flags: u8| parse_av1c(&[0x81, profile << 5, flags, 0]).unwrap();
    let chunk = Limits::default().max_chunk_bytes;
    let full_hd = 1920 * 1080;
    let worst_case = u32::try_from(chunk / 6).unwrap();
    assert_eq!(worst_case, 1_398_101);
    // (record, expected pixels): 4:2:0 8-bit, 4:2:0 10-bit, 4:2:2 8-bit, 4:4:4 8-bit, 4:4:4 10-bit,
    // monochrome 8-bit, 4:4:4 12-bit (worst case).
    for (config, expected) in [
        (record(0, 0x0C), 5_592_405u32),
        (record(0, 0x4C), 2_796_202),
        (record(2, 0x08), 4_194_304),
        (record(1, 0x00), 2_796_202),
        (record(1, 0x40), 1_398_101),
        (record(0, 0x1C), 8_388_608),
        (record(2, 0x60), 1_398_101),
    ] {
        assert_eq!(config.pixel_limit(chunk), expected, "{config:?}");
    }
    // The ordinary cases a studio meets hold 1920x1080; the worst case alone did not.
    assert!(record(0, 0x0C).pixel_limit(chunk) >= full_hd);
    assert!(record(0, 0x4C).pixel_limit(chunk) >= full_hd);
    assert!(worst_case < full_hd);
    // A smaller chunk limit shrinks it proportionally and a huge one saturates instead of wrapping.
    assert_eq!(record(0, 0x0C).pixel_limit(1_048_576), 699_050);
    assert_eq!(record(0, 0x0C).pixel_limit(usize::MAX), u32::MAX);
}
