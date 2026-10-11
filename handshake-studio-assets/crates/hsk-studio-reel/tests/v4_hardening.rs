//! V4-01/V4-02 hardening proofs for the byte parsers that carried unchecked arithmetic: Y4M, WAV and the
//! sampled source identity. Each decoder has a `dims_u32_max`-style case (every number a file header or a
//! caller can set pushed to its 32/64-bit limit) and an `aggregate_over_budget`-style case (many small
//! items, or a running total, past a declared budget). Every refusal is a typed `ReelError`, never a
//! panic, a wrap or a silent clamp. The ISOBMFF (MP4/MOV and fragmented MP4) cases live further down.
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
use std::io::{Cursor, Read, Seek, SeekFrom};

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

// ----- shared helpers ------------------------------------------------------------------------------

#[derive(Default)]
struct Count {
    chunks: u64,
    bytes: u64,
}

impl ChunkSink for Count {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks += 1;
        self.bytes += chunk.bytes.len() as u64;
        Ok(())
    }
}

/// Encode source that hands out the prepared (bytes, claimed units) chunks one by one.
struct Scripted(Vec<(Vec<u8>, u64)>);

impl ChunkSource for Scripted {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let (bytes, units) = self.0.remove(0);
        buf.extend_from_slice(&bytes);
        Ok(Some(units))
    }
}

fn grant_for(bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse("v4-hardening").unwrap(),
        revision: 1,
        expected_len: bytes.len() as u64,
    }
}

fn request(codec: &str, start_pts: u64, count: u64, bytes: &[u8]) -> SegmentRequest {
    SegmentRequest {
        codec: codec.into(),
        start_pts: Ticks::new(start_pts),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: grant_for(bytes),
        expected_revision: 1,
    }
}

fn decode(
    provider: &dyn MediaProvider,
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
) -> (Result<SegmentReceipt, ReelError>, Count) {
    let mut sink = Count::default();
    let result = provider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        req,
        limits,
        &CancellationToken::default(),
        &mut sink,
    );
    (result, sink)
}

fn probe(provider: &dyn MediaProvider, bytes: &[u8], limits: &Limits) -> Result<StreamLayout, ReelError> {
    provider.probe(
        &mut Cursor::new(bytes.to_vec()),
        limits,
        &CancellationToken::default(),
    )
}

fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

// ----- Y4M -----------------------------------------------------------------------------------------

const TPF_25: u64 = TICKS_PER_SECOND / 25;

fn y4m_header(text: &str) -> Vec<u8> {
    let mut out = format!("YUV4MPEG2 {text}\n").into_bytes();
    out.extend_from_slice(b"FRAME\n");
    out
}

#[test]
fn y4m_dims_u32_max() {
    bounded_case("y4m_dims_u32_max", || {
        let limits = Limits::default();
        // Header numbers at u32/u64 max: refused from the header text, before any frame buffer.
        for (text, expected) in [
            ("W4294967295 H4294967295 F25:1 C444", corrupt("y4m-dimensions")),
            ("W4294967295 H1 F25:1", corrupt("y4m-dimensions")),
            ("W65537 H1 F25:1", corrupt("y4m-dimensions")),
            ("W99999999999999999999 H1 F25:1", corrupt("y4m-width")),
            // At MAX_DIMENSION the frame is 12.9 GB: over max_chunk_bytes.
            ("W65536 H65536 F25:1 C444", limit("frame-bytes")),
            ("W65536 H65536 F25:1 C420", limit("frame-bytes")),
        ] {
            let bytes = y4m_header(text);
            let (result, sink) = decode(&Y4mProvider, &bytes, &request("y4m", 0, 1, &bytes), &limits);
            assert_eq!(result.unwrap_err(), expected, "{text}");
            assert_eq!(sink.chunks, 0);
            assert_eq!(probe(&Y4mProvider, &bytes, &limits).unwrap_err(), expected, "{text}");
        }
        // Frame rates at the 64-bit limit are an unsupported (non-broadcast) rate, never a wrap.
        for text in [
            "W64 H64 F18446744073709551615:1",
            "W64 H64 F1:18446744073709551615",
            "W64 H64 F18446744073709551615:18446744073709551615",
        ] {
            let bytes = y4m_header(text);
            assert_eq!(
                probe(&Y4mProvider, &bytes, &limits).unwrap_err(),
                ReelError::Unsupported {
                    what: "frame-rate-not-broadcast"
                },
                "{text}"
            );
        }
        // A frame count/offset at u64 max: byte offset = frame * record overflows -> typed limit. The
        // caller-chosen limits let the 12.9 GB header through; nothing is allocated for it.
        let wide = Limits {
            max_chunk_bytes: usize::MAX,
            ..Limits::default()
        };
        let bytes = y4m_header("W65536 H65536 F25:1 C444");
        let (result, sink) = decode(
            &Y4mProvider,
            &bytes,
            &request("y4m", u64::MAX - u64::MAX % TPF_25, 1, &bytes),
            &wide,
        );
        assert_eq!(result.unwrap_err(), limit("byte-offset"));
        assert_eq!(sink.chunks, 0);
        // start frame 1 + count u64::MAX: the end frame does not exist as a u64.
        let small = y4m_bytes(8, 8, (25, 1), "mono", 2);
        let (result, sink) = decode(
            &Y4mProvider,
            &small,
            &request("y4m", TPF_25, u64::MAX, &small),
            &limits,
        );
        assert_eq!(result.unwrap_err(), limit("frame-range"));
        assert_eq!(sink.chunks, 0);
        // count u64::MAX from frame 0 streams what exists, then reports Truncated with the exact count:
        // never Ok with fewer frames, never a wrapped receipt.
        let (result, sink) = decode(
            &Y4mProvider,
            &small,
            &request("y4m", 0, u64::MAX, &small),
            &limits,
        );
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 2 });
        assert_eq!(sink.chunks, 2);
        // Encode: a lying source claims 2^52 + 1 frames with the bytes of one 64x64 mono frame
        // (4096 * (2^52 + 1) wraps to 4096 modulo 2^64). It must be refused, not reported as written.
        let lie = (1u64 << 52) + 1;
        let spec = EncodeSpec {
            codec: "y4m".into(),
            params: EncodeParams::Video {
                width: 64,
                height: 64,
                ticks_per_frame: TPF_25,
                chroma: "mono",
            },
            total_units: lie,
        };
        let mut out = Vec::new();
        let result = Y4mProvider.encode_segment(
            &spec,
            &mut Scripted(vec![(vec![7u8; 4096], lie)]),
            &mut out,
            &limits,
            &CancellationToken::default(),
        );
        assert_eq!(result.unwrap_err(), corrupt("chunk-size-mismatch"));
    });
}

#[test]
fn y4m_aggregate_over_budget() {
    bounded_case("y4m_aggregate_over_budget", || {
        let limits = Limits::default();
        // Header text past the 4096-byte structure budget, no newline inside it.
        let mut bytes = b"YUV4MPEG2 W8 H8 F25:1".to_vec();
        bytes.extend(std::iter::repeat_n(b'X', 5000));
        let (result, _) = decode(&Y4mProvider, &bytes, &request("y4m", 0, 1, &bytes), &limits);
        assert_eq!(result.unwrap_err(), limit("y4m-header-bytes"));
        // The same header with a newline but a probe budget smaller than the header: cut, not read on.
        let tight = Limits {
            max_probe_bytes: 16,
            ..Limits::default()
        };
        let small = y4m_bytes(8, 8, (25, 1), "mono", 2);
        assert_eq!(
            probe(&Y4mProvider, &small, &tight).unwrap_err(),
            corrupt("y4m-header-unterminated")
        );
        // Encode: the running total of frames from the source may not pass the declared total; the
        // third frame is refused after two were written.
        let spec = EncodeSpec {
            codec: "y4m".into(),
            params: EncodeParams::Video {
                width: 8,
                height: 8,
                ticks_per_frame: TPF_25,
                chroma: "mono",
            },
            total_units: 2,
        };
        let frames = (0..3).map(|n| (y4m_frame(n, 8, 8, "mono"), 1)).collect();
        let mut out = Vec::new();
        let result = Y4mProvider.encode_segment(
            &spec,
            &mut Scripted(frames),
            &mut out,
            &limits,
            &CancellationToken::default(),
        );
        assert_eq!(result.unwrap_err(), limit("source-exceeds-declared"));
        let header = b"YUV4MPEG2 W8 H8 F25:1 Ip A1:1 Cmono\n".len();
        assert_eq!(out.len(), header + 2 * (6 + 64), "exactly the two declared frames were written");
        // Decode: asking for more frames than the file holds is Truncated with the delivered count.
        let (result, sink) = decode(&Y4mProvider, &small, &request("y4m", 0, 3, &small), &limits);
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 2 });
        assert_eq!(sink.chunks, 2);
    });
}

// ----- WAV -----------------------------------------------------------------------------------------

fn chunk(id: &[u8; 4], declared: u32, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn fmt_body(channels: u16, rate: u32, block_align: u16, bits: u16) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&rate.to_le_bytes());
    body.extend_from_slice(&rate.wrapping_mul(u32::from(block_align)).to_le_bytes());
    body.extend_from_slice(&block_align.to_le_bytes());
    body.extend_from_slice(&bits.to_le_bytes());
    body
}

fn riff(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn wav_with_data_size(declared: u32, real_frames: usize) -> Vec<u8> {
    riff(&[
        chunk(b"fmt ", 16, &fmt_body(2, 48000, 4, 16)),
        chunk(b"data", declared, &vec![1u8; real_frames * 4]),
    ])
}

const TPS_48K: u64 = TICKS_PER_SECOND / 48000;

#[test]
fn wav_dims_u32_max() {
    bounded_case("wav_dims_u32_max", || {
        let limits = Limits::default();
        // data size u32::MAX - 1 over a 2-frame file: the declared count is huge but nothing is
        // allocated or read for it; a request past the real bytes is Truncated with the delivered count.
        let bytes = wav_with_data_size(u32::MAX - 1, 2);
        let layout = probe(&WavProvider, &bytes, &limits).unwrap();
        assert_eq!(layout.units, u64::from(u32::MAX - 1) / 4);
        let (result, sink) = decode(&WavProvider, &bytes, &request("wav-pcm", 0, 3, &bytes), &limits);
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 2 });
        assert_eq!((sink.chunks, sink.bytes), (1, 8));
        // data size u32::MAX is the streaming marker: the declared length is the real length.
        let bytes = wav_with_data_size(u32::MAX, 2);
        assert_eq!(probe(&WavProvider, &bytes, &limits).unwrap().units, 2);
        let (result, _) = decode(&WavProvider, &bytes, &request("wav-pcm", 0, 3, &bytes), &limits);
        assert!(matches!(result.unwrap_err(), ReelError::OutOfRange { .. }));
        // start at u64::MAX ticks: the sample index is far past the end: typed OutOfRange.
        let (result, _) = decode(
            &WavProvider,
            &bytes,
            &request("wav-pcm", u64::MAX, 1, &bytes),
            &limits,
        );
        assert!(matches!(result.unwrap_err(), ReelError::OutOfRange { .. }));
        // start sample 1 + count u64::MAX: the end sample does not exist as a u64.
        let (result, sink) = decode(
            &WavProvider,
            &bytes,
            &request("wav-pcm", TPS_48K, u64::MAX, &bytes),
            &limits,
        );
        assert_eq!(result.unwrap_err(), limit("sample-range"));
        assert_eq!(sink.chunks, 0);
        // Chunk sizes at u32::MAX before the data chunk.
        let junk = riff(&[
            chunk(b"fmt ", 16, &fmt_body(2, 48000, 4, 16)),
            chunk(b"junk", u32::MAX, &[0; 4]),
        ]);
        assert_eq!(
            probe(&WavProvider, &junk, &limits).unwrap_err(),
            corrupt("wav-chunk-exceeds-file")
        );
        let big_fmt = riff(&[chunk(b"fmt ", u32::MAX, &fmt_body(2, 48000, 4, 16))]);
        assert_eq!(
            probe(&WavProvider, &big_fmt, &limits).unwrap_err(),
            corrupt("wav-fmt-size")
        );
        // Format fields at their maxima: channels 65535, block align 65535, rate u32::MAX.
        for (channels, rate, block_align, expected) in [
            (u16::MAX, 48000, 4, corrupt("wav-channels-or-rate")),
            (2, 48000, u16::MAX, corrupt("wav-block-align")),
            (
                2,
                u32::MAX,
                4,
                ReelError::Unsupported {
                    what: "sample-rate-not-tick-exact",
                },
            ),
        ] {
            let bytes = riff(&[
                chunk(b"fmt ", 16, &fmt_body(channels, rate, block_align, 16)),
                chunk(b"data", 8, &[0; 8]),
            ]);
            assert_eq!(probe(&WavProvider, &bytes, &limits).unwrap_err(), expected);
        }
        // Encode: a declared total at u64::MAX and a rate whose byte rate does not fit u32 are refused;
        // the header never carries a clamped (wrong) byte rate.
        let encode = |rate: u32, total_units: u64| {
            let spec = EncodeSpec {
                codec: "wav-pcm".into(),
                params: EncodeParams::Audio {
                    rate,
                    channels: 2,
                    bits: 16,
                    float: false,
                },
                total_units,
            };
            let mut out = Vec::new();
            let result = WavProvider.encode_segment(
                &spec,
                &mut Scripted(Vec::new()),
                &mut out,
                &Limits::default(),
                &CancellationToken::default(),
            );
            (result, out)
        };
        assert_eq!(encode(48000, u64::MAX).0.unwrap_err(), limit("wav-data-size"));
        assert_eq!(
            encode(48000, u64::from(u32::MAX) / 4 + 1).0.unwrap_err(),
            limit("wav-data-size")
        );
        let (result, out) = encode(u32::MAX, 0);
        assert_eq!(result.unwrap_err(), limit("wav-byte-rate"));
        assert!(out.is_empty(), "nothing is written for a refused header");
        // A lying source: 2^62 + 1 stereo s16 frames claimed with the bytes of one (4 * (2^62 + 1)
        // wraps to 4) must be a size mismatch, not a counted write.
        let spec = EncodeSpec {
            codec: "wav-pcm".into(),
            params: EncodeParams::Audio {
                rate: 48000,
                channels: 2,
                bits: 16,
                float: false,
            },
            total_units: 4,
        };
        let mut out = Vec::new();
        let result = WavProvider.encode_segment(
            &spec,
            &mut Scripted(vec![(vec![0u8; 4], (1u64 << 62) + 1)]),
            &mut out,
            &limits,
            &CancellationToken::default(),
        );
        assert_eq!(result.unwrap_err(), corrupt("chunk-size-mismatch"));
    });
}

#[test]
fn wav_aggregate_over_budget() {
    bounded_case("wav_aggregate_over_budget", || {
        let limits = Limits::default();
        // 300 empty chunks before `data`: past the chunk-count budget.
        let mut parts = vec![chunk(b"fmt ", 16, &fmt_body(2, 48000, 4, 16))];
        parts.extend((0..300).map(|_| chunk(b"junk", 0, &[])));
        parts.push(chunk(b"data", 4, &[0; 4]));
        let bytes = riff(&parts);
        assert_eq!(
            probe(&WavProvider, &bytes, &limits).unwrap_err(),
            limit("wav-chunks-before-data")
        );
        // One skipped chunk is within the chunk budget but its offset runs past the probe-byte budget.
        let bytes = riff(&[
            chunk(b"fmt ", 16, &fmt_body(2, 48000, 4, 16)),
            chunk(b"junk", 1000, &[0; 1000]),
            chunk(b"data", 4, &[0; 4]),
        ]);
        let tight = Limits {
            max_probe_bytes: 64,
            ..Limits::default()
        };
        assert_eq!(probe(&WavProvider, &bytes, &tight).unwrap_err(), limit("probe-bytes"));
        assert_eq!(probe(&WavProvider, &bytes, &limits).unwrap().units, 1);
        // Encode: the running total of samples may not pass the declared total (2 + 2 > 3).
        let spec = EncodeSpec {
            codec: "wav-pcm".into(),
            params: EncodeParams::Audio {
                rate: 48000,
                channels: 2,
                bits: 16,
                float: false,
            },
            total_units: 3,
        };
        let mut out = Vec::new();
        let result = WavProvider.encode_segment(
            &spec,
            &mut Scripted(vec![(vec![0u8; 8], 2), (vec![0u8; 8], 2)]),
            &mut out,
            &limits,
            &CancellationToken::default(),
        );
        assert_eq!(result.unwrap_err(), limit("source-exceeds-declared"));
        // A chunk smaller than one sample frame is refused before any read.
        let small_chunks = Limits {
            max_chunk_bytes: 3,
            ..Limits::default()
        };
        let ok = wav_pcm16(48000, 2, 4);
        let (result, sink) = decode(&WavProvider, &ok, &request("wav-pcm", 0, 2, &ok), &small_chunks);
        assert_eq!(result.unwrap_err(), limit("chunk-smaller-than-sample"));
        assert_eq!(sink.chunks, 0);
    });
}

// ----- sampled source identity ----------------------------------------------------------------------

/// A source of `len` zero bytes that is never materialised: reads hand out zeros, seeks move a cursor.
struct Sparse {
    len: u64,
    pos: u64,
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        let n = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        buf[..n].fill(0);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = target.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(self.pos)
    }
}

#[test]
fn identity_dims_u32_max() {
    bounded_case("identity_dims_u32_max", || {
        let cancel = CancellationToken::default();
        // Parameters at u32::MAX are refused before anything is allocated.
        for params in [
            IdentityParams {
                window: u32::MAX,
                windows: 8,
            },
            IdentityParams {
                window: 4096,
                windows: u32::MAX,
            },
            IdentityParams {
                window: u32::MAX,
                windows: u32::MAX,
            },
        ] {
            let mut src = Sparse { len: 1 << 20, pos: 0 };
            assert_eq!(
                sampled_identity(&mut src, params, &cancel).unwrap_err(),
                limit("identity-params")
            );
        }
        // A source of u64::MAX bytes: the evenly spaced offsets are computed in 128 bits and the last
        // window ends exactly at the end of the source. Deterministic across runs.
        let params = IdentityParams {
            window: 4096,
            windows: 64,
        };
        let first = sampled_identity(&mut Sparse { len: u64::MAX, pos: 0 }, params, &cancel).unwrap();
        let again = sampled_identity(&mut Sparse { len: u64::MAX, pos: 0 }, params, &cancel).unwrap();
        assert_eq!((first.len, first.exhaustive), (u64::MAX, false));
        assert_eq!(first, again);
        // One byte less is a different identity (the length is framed into the digest).
        let shorter =
            sampled_identity(&mut Sparse { len: u64::MAX - 1, pos: 0 }, params, &cancel).unwrap();
        assert_ne!(first.digest, shorter.digest);
        // The full digest chunk size is bounded in both directions.
        for chunk_bytes in [0, usize::MAX, (8 << 20) + 1] {
            assert_eq!(
                full_digest(&mut Sparse { len: 16, pos: 0 }, chunk_bytes, &cancel).unwrap_err(),
                limit("digest-chunk-bytes")
            );
        }
    });
}
