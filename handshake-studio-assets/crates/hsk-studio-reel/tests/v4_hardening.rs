//! V4-01/V4-02 hardening proofs for the byte parsers that carried unchecked arithmetic: Y4M, WAV and the
//! sampled source identity. Each decoder has a `dims_u32_max`-style case (every number a file header or a
//! caller can set pushed to its 32/64-bit limit) and an `aggregate_over_budget`-style case (many small
//! items, or a running total, past a declared budget). Every refusal is a typed `ReelError`, never a
//! panic, a wrap or a silent clamp. The ISOBMFF (MP4/MOV and fragmented MP4) cases live further down.
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
mod common;
use common::*;
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

fn probe(
    provider: &dyn MediaProvider,
    bytes: &[u8],
    limits: &Limits,
) -> Result<StreamLayout, ReelError> {
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
            (
                "W4294967295 H4294967295 F25:1 C444",
                corrupt("y4m-dimensions"),
            ),
            ("W4294967295 H1 F25:1", corrupt("y4m-dimensions")),
            ("W65537 H1 F25:1", corrupt("y4m-dimensions")),
            ("W99999999999999999999 H1 F25:1", corrupt("y4m-width")),
            // At MAX_DIMENSION the frame is 12.9 GB: over max_chunk_bytes.
            ("W65536 H65536 F25:1 C444", limit("frame-bytes")),
            ("W65536 H65536 F25:1 C420", limit("frame-bytes")),
        ] {
            let bytes = y4m_header(text);
            let (result, sink) =
                decode(&Y4mProvider, &bytes, &request("y4m", 0, 1, &bytes), &limits);
            assert_eq!(result.unwrap_err(), expected, "{text}");
            assert_eq!(sink.chunks, 0);
            assert_eq!(
                probe(&Y4mProvider, &bytes, &limits).unwrap_err(),
                expected,
                "{text}"
            );
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
        assert_eq!(
            out.len(),
            header + 2 * (6 + 64),
            "exactly the two declared frames were written"
        );
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
        let (result, sink) = decode(
            &WavProvider,
            &bytes,
            &request("wav-pcm", 0, 3, &bytes),
            &limits,
        );
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 2 });
        assert_eq!((sink.chunks, sink.bytes), (1, 8));
        // data size u32::MAX is the streaming marker: the declared length is the real length.
        let bytes = wav_with_data_size(u32::MAX, 2);
        assert_eq!(probe(&WavProvider, &bytes, &limits).unwrap().units, 2);
        let (result, _) = decode(
            &WavProvider,
            &bytes,
            &request("wav-pcm", 0, 3, &bytes),
            &limits,
        );
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
        assert_eq!(
            encode(48000, u64::MAX).0.unwrap_err(),
            limit("wav-data-size")
        );
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
        assert_eq!(
            probe(&WavProvider, &bytes, &tight).unwrap_err(),
            limit("probe-bytes")
        );
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
        let (result, sink) = decode(
            &WavProvider,
            &ok,
            &request("wav-pcm", 0, 2, &ok),
            &small_chunks,
        );
        assert_eq!(result.unwrap_err(), limit("chunk-smaller-than-sample"));
        assert_eq!(sink.chunks, 0);
    });
}

// ----- sampled source identity ----------------------------------------------------------------------

/// A source of `len` zero bytes that is never materialised: reads hand out zeros, seeks move a cursor,
/// and `reads` counts the bytes handed out.
struct Sparse {
    len: u64,
    pos: u64,
    reads: u64,
}

impl Sparse {
    fn new(len: u64) -> Self {
        Self {
            len,
            pos: 0,
            reads: 0,
        }
    }
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        let n = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        buf[..n].fill(0);
        self.pos += n as u64;
        self.reads += n as u64;
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
fn identity_aggregate_over_budget() {
    bounded_case("identity_aggregate_over_budget", || {
        let cancel = CancellationToken::default();
        let params = IdentityParams {
            window: 4096,
            windows: 8,
        };
        // A 1 TiB source costs exactly windows x window bytes of reads, however long it is.
        let mut huge = Sparse::new(1 << 40);
        let identity = sampled_identity(&mut huge, params, &cancel).unwrap();
        assert!(!identity.exhaustive);
        assert_eq!(huge.reads, 8 * 4096);
        // A source that fits the covered bytes is read once, in full, and says so.
        let mut small = Sparse::new(8 * 4096);
        let identity = sampled_identity(&mut small, params, &cancel).unwrap();
        assert!(identity.exhaustive);
        assert_eq!(small.reads, 8 * 4096);
        // One byte more is sampled, not exhaustive.
        let mut over = Sparse::new(8 * 4096 + 1);
        assert!(
            !sampled_identity(&mut over, params, &cancel)
                .unwrap()
                .exhaustive
        );
        assert_eq!(over.reads, 8 * 4096);
    });
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
            let mut src = Sparse::new(1 << 20);
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
        let first = sampled_identity(&mut Sparse::new(u64::MAX), params, &cancel).unwrap();
        let again = sampled_identity(&mut Sparse::new(u64::MAX), params, &cancel).unwrap();
        assert_eq!((first.len, first.exhaustive), (u64::MAX, false));
        assert_eq!(first, again);
        // One byte less is a different identity (the length is framed into the digest).
        let shorter = sampled_identity(&mut Sparse::new(u64::MAX - 1), params, &cancel).unwrap();
        assert_ne!(first.digest, shorter.digest);
        // The full digest chunk size is bounded in both directions.
        for chunk_bytes in [0, usize::MAX, (8 << 20) + 1] {
            assert_eq!(
                full_digest(&mut Sparse::new(16), chunk_bytes, &cancel).unwrap_err(),
                limit("digest-chunk-bytes")
            );
        }
    });
}

// ----- ISOBMFF: MP4 / MOV ---------------------------------------------------------------------------

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A box whose 32-bit size field is 1 and whose 64-bit largesize is `size`, followed by `tail`.
fn big_box(kind: &[u8; 4], size: u64, tail: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(tail);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
    body.extend_from_slice(payload);
    bx(kind, &body)
}

/// Offset of the first `tag` fourcc in `bytes` (the tag, not the box start).
fn at(bytes: &[u8], tag: &[u8; 4]) -> usize {
    bytes
        .windows(4)
        .position(|w| w == tag)
        .unwrap_or_else(|| panic!("no {tag:?} box in the fixture"))
}

fn get32(bytes: &[u8], tag: &[u8; 4], rel: usize) -> u32 {
    let p = at(bytes, tag) + rel;
    u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap())
}

fn put32(bytes: &mut [u8], tag: &[u8; 4], rel: usize, value: u32) {
    let p = at(bytes, tag) + rel;
    bytes[p..p + 4].copy_from_slice(&value.to_be_bytes());
}

fn put64(bytes: &mut [u8], tag: &[u8; 4], rel: usize, value: u64) {
    let p = at(bytes, tag) + rel;
    bytes[p..p + 8].copy_from_slice(&value.to_be_bytes());
}

fn index_of(bytes: &[u8], limits: &Limits) -> Result<MediaIndex, ReelError> {
    build_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        limits,
        &CancellationToken::default(),
    )
}

fn sample(duration: u32, cts_offset: i32, size: u32, sync: bool) -> Mp4Sample {
    Mp4Sample {
        duration,
        cts_offset,
        size,
        sync,
    }
}

fn four_samples() -> Vec<Mp4Sample> {
    (0..4).map(|k| sample(20, 0, 16, k == 0)).collect()
}

fn mp4_spec(samples: Vec<Mp4Sample>) -> Mp4Spec {
    Mp4Spec {
        movie_timescale: 600,
        media_timescale: 600,
        codec: *b"avc1",
        width: 64,
        height: 36,
        samples,
        edits: None,
        samples_per_chunk: 2,
        co64: false,
        ctts_version: 0,
        moov_first: true,
        colr_nclx: None,
        pasp: None,
    }
}

fn packet_request(start_pts: u64, count: u64, bytes: &[u8]) -> SegmentRequest {
    SegmentRequest {
        output: OutputMode::Packets,
        ..request("isobmff", start_pts, count, bytes)
    }
}

fn decode_packets(
    bytes: &[u8],
    index: &MediaIndex,
    req: &SegmentRequest,
    limits: &Limits,
) -> Result<SegmentReceipt, ReelError> {
    IsobmffProvider.decode_packets(
        &mut Cursor::new(bytes.to_vec()),
        index,
        req,
        limits,
        &CancellationToken::default(),
        &mut Count::default(),
    )
}

#[test]
fn isobmff_dims_u32_max() {
    bounded_case("isobmff_dims_u32_max", || {
        let limits = Limits::default();
        // Hostile top-level and nested box sizes, including 64-bit largesize boxes.
        let ftyp = bx(b"ftyp", b"isom\0\0\0\0isom");
        let file = |tail: Vec<u8>| [ftyp.clone(), tail].concat();
        let mut over_32 = u32::MAX.to_be_bytes().to_vec();
        over_32.extend_from_slice(b"moov");
        over_32.extend_from_slice(&[0; 16]);
        // 20 bytes is smaller than the 8 + 16 byte header of a uuid box.
        let mut uuid = 20u32.to_be_bytes().to_vec();
        uuid.extend_from_slice(b"uuid");
        uuid.extend_from_slice(&[0; 12]);
        for (bytes, expected) in [
            (
                file(big_box(b"free", u64::MAX, &[])),
                corrupt("moov-missing"),
            ),
            (
                file(big_box(b"moov", u64::MAX, &[])),
                corrupt("moov-exceeds-file"),
            ),
            (file(over_32), corrupt("moov-exceeds-file")),
            (file(big_box(b"free", 15, &[])), corrupt("box-size")),
            (
                file(bx(b"moov", &big_box(b"trak", u64::MAX, &[]))),
                corrupt("box-size"),
            ),
            (file(bx(b"moov", &uuid)), corrupt("box-size")),
        ] {
            assert_eq!(index_of(&bytes, &limits).unwrap_err(), expected);
        }

        // Table counts at u32::MAX against a valid file (the count fields sit at fixed offsets).
        let valid = mp4_bytes(&mp4_spec(four_samples()));
        assert_eq!(get32(&valid, b"stts", 8), 1, "one run-length entry");
        assert_eq!(get32(&valid, b"stsz", 12), 4, "four samples");
        let patched = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = valid.clone();
            edit(&mut bytes);
            index_of(&bytes, &limits).unwrap_err()
        };
        assert_eq!(
            patched(&|b| put32(b, b"stts", 8, u32::MAX)),
            corrupt("table-exceeds-box")
        );
        assert_eq!(
            patched(&|b| {
                put32(b, b"stts", 12, u32::MAX);
                put32(b, b"stsz", 12, u32::MAX);
            }),
            limit("sample-count")
        );
        assert_eq!(
            patched(&|b| put32(b, b"stsz", 12, u32::MAX)),
            corrupt("stts-count-mismatch")
        );
        // A chunk offset of u32::MAX is representable; reading it is Truncated, not a wrapped seek.
        let mut far = valid.clone();
        put32(&mut far, b"stco", 12, u32::MAX);
        let index = index_of(&far, &limits).unwrap();
        assert_eq!(
            decode_packets(&far, &index, &packet_request(0, 1, &far), &limits).unwrap_err(),
            ReelError::Truncated { delivered: 0 }
        );
        // A 64-bit chunk offset of u64::MAX cannot take a sample after it.
        let mut wide_spec = mp4_spec(four_samples());
        wide_spec.co64 = true;
        let mut wide = mp4_bytes(&wide_spec);
        put64(&mut wide, b"co64", 12, u64::MAX);
        assert_eq!(
            index_of(&wide, &limits).unwrap_err(),
            corrupt("chunk-offset-overflow")
        );
        // A sample size of u32::MAX is refused against the chunk limit, never read.
        let mut huge = valid.clone();
        put32(&mut huge, b"stsz", 16, u32::MAX);
        let index = index_of(&huge, &limits).unwrap();
        assert_eq!(index.default_track().unwrap().samples()[0].size, u32::MAX);
        assert_eq!(
            decode_packets(&huge, &index, &packet_request(0, 1, &huge), &limits).unwrap_err(),
            limit("sample-bytes")
        );

        // Edit lists at their extremes.
        let with_edits = |edits: Vec<(u64, i64)>| {
            let mut spec = mp4_spec(four_samples());
            spec.edits = Some(edits);
            mp4_bytes(&spec)
        };
        for (edits, expected) in [
            (vec![(u64::MAX, 0)], limit("tick-range")),
            (vec![(i64::MAX as u64, 0)], limit("tick-range")),
            (
                vec![(20_000_000_000, 0), (20_000_000_000, 0)],
                limit("tick-range"),
            ),
            (vec![(10, -2)], corrupt("elst-media-time")),
            (
                vec![(u64::from(u32::MAX) + 1, i64::MIN)],
                corrupt("elst-media-time"),
            ),
        ] {
            assert_eq!(
                index_of(&with_edits(edits.clone()), &limits).unwrap_err(),
                expected,
                "{edits:?}"
            );
        }
        // media_time = i64::MAX starts past the last sample: a valid, empty presentation.
        let index = index_of(&with_edits(vec![(10_000_000_000, i64::MAX)]), &limits).unwrap();
        let track = index.default_track().unwrap();
        assert!(track.presented().is_empty());
        assert!(matches!(
            track.seek(Ticks::new(0)).unwrap_err(),
            ReelError::OutOfRange { .. }
        ));

        // ctts and durations at their extremes: nothing wraps, composition may precede decode time.
        let spec = Mp4Spec {
            ctts_version: 1,
            ..mp4_spec(vec![
                sample(u32::MAX, i32::MIN, 16, true),
                sample(u32::MAX, i32::MAX, 16, false),
                sample(u32::MAX, 0, 16, false),
            ])
        };
        let index = index_of(&mp4_bytes(&spec), &limits).unwrap();
        let s = index.default_track().unwrap().samples();
        assert_eq!(s[0].pts, i64::from(i32::MIN));
        assert_eq!(s[1].dts, i64::from(u32::MAX));
        assert_eq!(s[1].pts, i64::from(u32::MAX) + i64::from(i32::MAX));
        assert_eq!(s[2].dts, 2 * i64::from(u32::MAX));
        // Timescale u32::MAX on both headers (tiny durations so the fixture itself cannot overflow).
        let tiny = Mp4Spec {
            movie_timescale: u32::MAX,
            media_timescale: u32::MAX,
            ..mp4_spec(four_samples())
        };
        assert_eq!(
            index_of(&mp4_bytes(&tiny), &limits).unwrap().tracks.len(),
            1
        );
        // One u32::MAX-unit sample at timescale 1 is 1.1e21 ticks: past i64, a typed limit.
        let slow = Mp4Spec {
            movie_timescale: 1,
            media_timescale: 1,
            ..mp4_spec(vec![sample(u32::MAX, 0, 16, true)])
        };
        assert_eq!(
            index_of(&mp4_bytes(&slow), &limits).unwrap_err(),
            limit("tick-range")
        );

        // Requests at u64::MAX against a valid index: out of range, never a wrapped index.
        let index = index_of(&valid, &limits).unwrap();
        for (start, count) in [(u64::MAX, 1), (0, u64::MAX), (u64::MAX, u64::MAX)] {
            assert!(
                matches!(
                    decode_packets(
                        &valid,
                        &index,
                        &packet_request(start, count, &valid),
                        &limits
                    )
                    .unwrap_err(),
                    ReelError::OutOfRange { .. }
                ),
                "start {start} count {count}"
            );
        }
    });
}

#[test]
fn isobmff_aggregate_over_budget() {
    bounded_case("isobmff_aggregate_over_budget", || {
        let limits = Limits::default();
        let valid = mp4_bytes(&mp4_spec(four_samples()));
        // The top-level scan has a box budget: the moov must be found within 65,536 boxes.
        let moov_at = at(&valid, b"moov") - 4;
        let (head, rest) = valid.split_at(moov_at);
        let padded = |free_boxes: usize| {
            let mut bytes = head.to_vec();
            bytes.extend_from_slice(&bx(b"free", &[]).repeat(free_boxes));
            bytes.extend_from_slice(rest);
            bytes
        };
        assert!(
            index_of(&padded(65_534), &limits).is_ok(),
            "ftyp + 65,534 free + moov = 65,536 boxes"
        );
        assert_eq!(
            index_of(&padded(65_535), &limits).unwrap_err(),
            corrupt("moov-missing")
        );
        // A probe budget smaller than the movie box.
        let tight = Limits {
            max_probe_bytes: 64,
            ..Limits::default()
        };
        assert_eq!(index_of(&valid, &tight).unwrap_err(), limit("moov-bytes"));
        // The sample budget: exactly enough passes, one short fails.
        for (max_samples, ok) in [(4, true), (3, false)] {
            let tight = Limits {
                max_samples,
                ..Limits::default()
            };
            let result = index_of(&valid, &tight);
            assert_eq!(result.is_ok(), ok, "max_samples {max_samples}");
            if !ok {
                assert_eq!(result.unwrap_err(), limit("sample-count"));
            }
        }
        // One long audio track plus a video track share ONE sample budget: audio alone fits 5, video +
        // audio does not.
        let audio = PcmMovSpec {
            fourcc: *b"twos",
            kind: PcmKind::S16,
            big_endian: true,
            lpcm_v2: false,
            non_interleaved: false,
            channels: 2,
            rate: 48000,
            frames: 300,
            frames_per_chunk: 128,
            movie_timescale: 48000,
            edits: None,
            moov_first: true,
            sound_version: None,
        };
        let five = Limits {
            max_samples: 5,
            ..Limits::default()
        };
        assert!(index_of(&mov_pcm_bytes(&audio), &five).is_ok());
        assert!(matches!(
            index_of(&mov_av_bytes(4, &audio), &five).unwrap_err(),
            ReelError::LimitExceeded { .. }
        ));
        assert!(index_of(&mov_av_bytes(4, &audio), &limits).is_ok());
        // Edit-list budgets: the caller's, and the hard 4096 bound.
        let many = |n: usize| {
            let mut spec = mp4_spec(four_samples());
            spec.edits = Some(vec![(20, 0); n]);
            mp4_bytes(&spec)
        };
        assert!(index_of(&many(2), &limits).is_ok());
        let two = Limits {
            max_edit_entries: 2,
            ..Limits::default()
        };
        assert_eq!(index_of(&many(3), &two).unwrap_err(), limit("edit-count"));
        assert_eq!(
            index_of(&many(4097), &limits).unwrap_err(),
            limit("edit-count")
        );
        // A zero-length empty edit is no segment, but it is still an entry the file declares: the hard
        // bound counts entries, so skipping them is not a way around it (SMB-REEL-ENG-1).
        let zero_length = |n: usize| {
            let mut spec = mp4_spec(four_samples());
            spec.edits = Some(vec![(0, -1); n]);
            mp4_bytes(&spec)
        };
        assert!(index_of(&zero_length(4096), &limits).is_ok());
        assert_eq!(
            index_of(&zero_length(4097), &limits).unwrap_err(),
            limit("edit-count")
        );
        // Track budget.
        let none = Limits {
            max_tracks: 0,
            ..Limits::default()
        };
        assert_eq!(index_of(&valid, &none).unwrap_err(), limit("track-count"));
    });
}

// ----- ISOBMFF: fragmented MP4 -----------------------------------------------------------------------

fn frag_spec(fragments: usize, per: usize) -> FragSpec {
    // Uniform samples, sync first: valid for both fixture styles (full trun fields and tfhd defaults).
    let uniform: Vec<Mp4Sample> = (0..per).map(|k| sample(20, 0, 8, k == 0)).collect();
    FragSpec {
        movie_timescale: 600,
        media_timescale: 600,
        codec: *b"avc1",
        width: 64,
        height: 36,
        fragments: vec![uniform; fragments],
        tfdt: vec![true; fragments],
        default_base_is_moof: true,
        with_sidx: false,
        edits: None,
    }
}

/// `ftyp` + `moov` (with `mvex`) of a fragmented file with no fragments.
fn frag_head() -> Vec<u8> {
    fmp4_bytes(&frag_spec(0, 1)).bytes
}

struct Run {
    flags: u32,
    count: u32,
    data_offset: i32,
    per_sample: Vec<u8>,
}

fn run(flags: u32, count: u32, data_offset: i32, per_sample: Vec<u8>) -> Run {
    Run {
        flags,
        count,
        data_offset,
        per_sample,
    }
}

fn raw_moof(tfhd_flags: u32, base: u64, tfdt: Option<u64>, run: &Run) -> Vec<u8> {
    let mut tfhd = 1u32.to_be_bytes().to_vec();
    if tfhd_flags & 1 != 0 {
        tfhd.extend_from_slice(&base.to_be_bytes());
    }
    let mut traf = full(b"tfhd", 0, tfhd_flags, &tfhd);
    if let Some(time) = tfdt {
        traf.extend_from_slice(&full(b"tfdt", 1, 0, &time.to_be_bytes()));
    }
    let mut trun = run.count.to_be_bytes().to_vec();
    if run.flags & 1 != 0 {
        trun.extend_from_slice(&run.data_offset.to_be_bytes());
    }
    trun.extend_from_slice(&run.per_sample);
    traf.extend_from_slice(&full(b"trun", 0, run.flags, &trun));
    let mut moof = full(b"mfhd", 0, 0, &1u32.to_be_bytes());
    moof.extend_from_slice(&bx(b"traf", &traf));
    bx(b"moof", &moof)
}

/// Version 1 `sidx` with `declared` references of which `entries` (word, duration, sap) are present.
fn raw_sidx(first_offset: u64, declared: u16, entries: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut p = 1u32.to_be_bytes().to_vec();
    p.extend_from_slice(&600u32.to_be_bytes());
    p.extend_from_slice(&u64::MAX.to_be_bytes()); // earliest presentation time
    p.extend_from_slice(&first_offset.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&declared.to_be_bytes());
    for (word, duration, sap) in entries {
        p.extend_from_slice(&word.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
        p.extend_from_slice(&sap.to_be_bytes());
    }
    full(b"sidx", 1, 0, &p)
}

#[test]
fn fragments_dims_u32_max() {
    bounded_case("fragments_dims_u32_max", || {
        let limits = Limits::default();
        let head = frag_head();
        let with = |tail: Vec<u8>| [head.clone(), tail].concat();
        let max_i64 = i64::MAX as u64;
        let u32_max = u32::MAX.to_be_bytes().to_vec();
        for (name, tail, expected) in [
            (
                "tfdt u64::MAX",
                raw_moof(0, 0, Some(u64::MAX), &run(0x1, 1, 0, Vec::new())),
                limit("time-range"),
            ),
            (
                "dts i64::MAX + duration u32::MAX",
                raw_moof(0, 0, Some(max_i64), &run(0x101, 1, 0, u32_max.clone())),
                limit("time-range"),
            ),
            (
                "trun count u32::MAX with per-sample fields",
                raw_moof(0, 0, None, &run(0x101, u32::MAX, 0, u32_max.clone())),
                corrupt("trun-exceeds-box"),
            ),
            (
                "trun count u32::MAX over the sample budget",
                raw_moof(0, 0, None, &run(0x1, u32::MAX, 0, Vec::new())),
                limit("sample-count"),
            ),
            (
                "explicit base u64::MAX - 1 + data offset i32::MAX",
                raw_moof(0x1, u64::MAX - 1, None, &run(0x1, 1, i32::MAX, Vec::new())),
                corrupt("trun-data-offset"),
            ),
            (
                "explicit base u64::MAX + sample size u32::MAX",
                raw_moof(0x1, u64::MAX, None, &run(0x201, 1, 0, u32_max.clone())),
                corrupt("chunk-offset-overflow"),
            ),
            (
                "data offset i32::MIN below the moof",
                raw_moof(0, 0, None, &run(0x1, 1, i32::MIN, Vec::new())),
                corrupt("trun-data-offset"),
            ),
            (
                "moof size 4 (below its own header)",
                [&4u32.to_be_bytes()[..], &b"moof"[..]].concat(),
                corrupt("box-size"),
            ),
            (
                "second moov",
                head[at(&head, b"moov") - 4..].to_vec(),
                corrupt("second-moov"),
            ),
        ] {
            assert_eq!(
                index_of(&with(tail), &limits).unwrap_err(),
                expected,
                "{name}"
            );
        }
        // A moof whose size runs past the end is a cut recording, flagged and not an error, whether
        // the size is 32 or 64 bits.
        for tail in [
            [&u32::MAX.to_be_bytes()[..], &b"moof"[..], &[0u8; 8][..]].concat(),
            big_box(b"moof", u64::MAX, &[0; 8]),
        ] {
            let index = index_of(&with(tail), &limits).unwrap();
            assert!(index.truncated_tail && index.fragment_offsets.is_empty());
        }
        // sidx: reference offsets and counts at their limits.
        let two = [(0x7FFF_FFFF, 1, 0), (0x7FFF_FFFF, 1, 0)];
        let anchor = (head.len() + raw_sidx(0, 2, &two).len()) as u64;
        for (name, sidx, expected) in [
            (
                "first_offset u64::MAX",
                raw_sidx(u64::MAX, 1, &[(100, 1, 0)]),
                corrupt("sidx-offset"),
            ),
            (
                "65535 references, none present",
                raw_sidx(0, u16::MAX, &[]),
                corrupt("sidx-exceeds-box"),
            ),
            (
                "two 2 GiB references past u64::MAX",
                raw_sidx(u64::MAX - anchor - 0x7FFF_FFFF - 5, 2, &two),
                corrupt("sidx-offset"),
            ),
        ] {
            assert_eq!(
                index_of(&with(sidx), &limits).unwrap_err(),
                expected,
                "{name}"
            );
        }
    });
}

#[test]
fn fragments_aggregate_over_budget() {
    bounded_case("fragments_aggregate_over_budget", || {
        let limits = Limits::default();
        // The sample budget is shared by every fragment: 40 + 40 fits 100, the third fragment does not.
        let samples = Limits {
            max_samples: 100,
            ..Limits::default()
        };
        assert!(index_of(&fmp4_bytes(&frag_spec(2, 40)).bytes, &samples).is_ok());
        assert_eq!(
            index_of(&fmp4_bytes(&frag_spec(12, 40)).bytes, &samples).unwrap_err(),
            limit("sample-count")
        );
        // The probe budget is shared too: one fragment's index bytes fit, six together do not.
        let one = index_of(&fmp4_bytes(&frag_spec(1, 40)).bytes, &limits).unwrap();
        let probe = Limits {
            max_probe_bytes: one.bytes_read + 100,
            ..Limits::default()
        };
        assert!(index_of(&fmp4_bytes(&frag_spec(1, 40)).bytes, &probe).is_ok());
        assert_eq!(
            index_of(&fmp4_bytes(&frag_spec(6, 40)).bytes, &probe).unwrap_err(),
            limit("fragment-index-bytes")
        );
        // The top-level box budget (2^20): a file that ends exactly on it is complete; one more box is
        // a typed limit, never a silently partial index.
        let head = frag_head();
        let free = bx(b"free", &[]);
        let at_budget = [head.clone(), free.repeat(1 << 20)].concat();
        assert!(index_of(&at_budget, &limits).is_ok());
        let over_budget = [head, free.repeat((1 << 20) + 1)].concat();
        assert_eq!(
            index_of(&over_budget, &limits).unwrap_err(),
            limit("fragment-box-count")
        );
    });
}
