//! Length-prefixed, versioned binary protocol between `WorkerAv1Provider` (host) and the
//! `reel-native-av1-worker` process (stdin/stdout, little-endian, no third-party serializer).
//!
//! Frame: `u32 length` + payload, length bounded by a caller-supplied cap BEFORE any allocation.
//! Payload: `u8 kind, u16 protocol version, u64 request id` + body. The worker speaks first with HELLO
//! (protocol version + decoder identity). One request is answered by zero or more FRAME replies and
//! exactly one terminal reply (OPENED, BATCH_END or ERROR), every reply echoing the request id, so a
//! stale or mismatched frame is a protocol fault. All decoding is bounds-checked and all size arithmetic
//! is checked: a malformed frame (including one declaring u32::MAX dimensions, test
//! `frame_dims_u32_max_is_malformed`) is `Malformed`, and nothing is allocated beyond the bytes present.
//! Worker-side encoders are compiled only for the worker (`av1-worker`) and the unit tests.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_reel::{ChromaFormat, ColorInfo, ColorOrigin, ColorTag, ReelError, VideoFormat};
use std::io::{self, Read, Write};

pub(crate) const PROTOCOL_VERSION: u16 = 1;
/// Largest request frame (one packet plus header) either side accepts.
pub(crate) const MAX_REQUEST_FRAME: usize = 32 << 20;
pub(crate) const HELLO_CAP: usize = 1024;
/// Bytes a reply may carry beyond its pixel planes (header, format, plane lengths, error text).
pub(crate) const REPLY_SLACK: usize = 512;
/// Most FRAME replies one request may produce before it is a protocol fault.
pub(crate) const MAX_FRAMES_PER_BATCH: usize = 64;

const KIND_HELLO: u8 = 1;
const KIND_OPEN: u8 = 2;
const KIND_PACKET: u8 = 3;
const KIND_FINISH: u8 = 4;
const KIND_OPENED: u8 = 5;
const KIND_FRAME: u8 = 6;
const KIND_BATCH_END: u8 = 7;
const KIND_ERROR: u8 = 8;

/// Fixed table of the `what` strings a decoder error can carry: errors cross the wire as an index, so a
/// peer can never make the host invent or leak a new static string. Index 0 is the catch-all.
const WHATS: [&str; 20] = [
    "av1-worker-error",
    "av1-frame-resources",
    "av1-unsupported-stream-feature",
    "av1-bitstream",
    "av1-packet-buffer",
    "av1-frame-bytes",
    "av1-picture-size",
    "av1-bit-depth",
    "av1-pixel-layout",
    "av1-missing-sequence-header",
    "av1-missing-plane",
    "av1-plane-stride",
    "av1c-truncated",
    "av1c-marker",
    "av1c-version",
    "av1-profile",
    "av1c-bit-depth",
    "av1-decoder-stalled",
    "av1-format-change-mid-segment",
    "av1-worker-no-open",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    /// End of stream or I/O failure: the peer is gone.
    Closed,
    /// A declared frame length above the cap (nothing was allocated or read).
    TooLarge,
    /// Wrong kind, version, id, truncated or inconsistent content.
    Malformed,
}

type Res<T> = Result<T, WireError>;

pub(crate) fn read_frame(reader: &mut impl Read, cap: usize) -> Res<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|_| WireError::Closed)?;
    let length = usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| WireError::TooLarge)?;
    if length > cap {
        return Err(WireError::TooLarge);
    }
    let mut payload = Vec::new();
    let wanted = u64::try_from(length).map_err(|_| WireError::TooLarge)?;
    let got = reader
        .take(wanted)
        .read_to_end(&mut payload)
        .map_err(|_| WireError::Closed)?;
    if got != length {
        return Err(WireError::Closed);
    }
    Ok(payload)
}

pub(crate) fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(payload.len().saturating_add(4));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()
}

/// True for the reply kinds that end a request (the host's I/O thread stops reading after one).
pub(crate) fn is_terminal(frame: &[u8]) -> bool {
    matches!(
        frame.first(),
        Some(&KIND_OPENED | &KIND_BATCH_END | &KIND_ERROR)
    )
}

struct Out(Vec<u8>);

impl Out {
    fn header(kind: u8, id: u64) -> Self {
        let mut out = Self(Vec::with_capacity(64));
        out.u8(kind);
        out.0.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        out.u64(id);
        out
    }
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(value);
    }
    #[cfg(any(test, feature = "av1-worker"))]
    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
}

struct In<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> In<'a> {
    /// Validates kind and version, returns the request id and a reader over the body.
    fn open(frame: &'a [u8], kind: u8) -> Res<(u64, Self)> {
        let mut reader = Self { buf: frame, pos: 0 };
        if reader.u8()? != kind || reader.u16()? != PROTOCOL_VERSION {
            return Err(WireError::Malformed);
        }
        let id = reader.u64()?;
        Ok((id, reader))
    }
    fn take(&mut self, count: usize) -> Res<&'a [u8]> {
        let end = self.pos.checked_add(count).ok_or(WireError::Malformed)?;
        let slice = self.buf.get(self.pos..end).ok_or(WireError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }
    fn array<const N: usize>(&mut self) -> Res<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| WireError::Malformed)
    }
    fn u8(&mut self) -> Res<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Res<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Res<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Res<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    /// A length-prefixed byte string that must fit in the bytes still present.
    fn bytes(&mut self) -> Res<&'a [u8]> {
        let length = usize::try_from(self.u32()?).map_err(|_| WireError::Malformed)?;
        self.take(length)
    }
    fn text(&mut self) -> Res<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| WireError::Malformed)
    }
    #[cfg(any(test, feature = "av1-worker"))]
    fn rest(&mut self) -> &'a [u8] {
        let rest = &self.buf[self.pos..];
        self.pos = self.buf.len();
        rest
    }
    fn done(&self) -> Res<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(WireError::Malformed)
        }
    }
}

// ---- hello --------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hello {
    pub protocol: u16,
    pub decoder_id: String,
    pub decoder_version: String,
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn encode_hello(decoder_id: &str, decoder_version: &str) -> Vec<u8> {
    let mut out = Out::header(KIND_HELLO, 0);
    out.text(decoder_id);
    out.text(decoder_version);
    out.0
}

/// Parses HELLO without judging the version: the host compares it and reports a mismatch as such.
pub(crate) fn decode_hello(frame: &[u8]) -> Res<Hello> {
    let mut reader = In { buf: frame, pos: 0 };
    if reader.u8()? != KIND_HELLO {
        return Err(WireError::Malformed);
    }
    let protocol = reader.u16()?;
    let _id = reader.u64()?;
    let hello = Hello {
        protocol,
        decoder_id: reader.text()?,
        decoder_version: reader.text()?,
    };
    reader.done()?;
    Ok(hello)
}

// ---- requests -----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenRequest {
    /// Decoder-side bound for one packed frame (the worker derives rav1d's pixel limit from it).
    pub max_chunk_bytes: u64,
    pub color: ColorTag,
    /// `av1C` record (or Matroska CodecPrivate); empty when the container has none.
    pub config: Vec<u8>,
}

#[cfg(any(test, feature = "av1-worker"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Open(u64, OpenRequest),
    Packet(u64, u64, Vec<u8>),
    Finish(u64),
}

pub(crate) fn encode_open(id: u64, open: &OpenRequest) -> Vec<u8> {
    let mut out = Out::header(KIND_OPEN, id);
    out.u64(open.max_chunk_bytes);
    match open.color {
        ColorTag::Untagged => out.u8(0),
        ColorTag::Nclx {
            primaries,
            transfer,
            matrix,
            full_range,
        } => {
            out.u8(1);
            out.u16(primaries);
            out.u16(transfer);
            out.u16(matrix);
            out.u8(match full_range {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            });
        }
        ColorTag::IccProfile { bytes } => {
            out.u8(2);
            out.u32(bytes);
        }
    }
    out.bytes(&open.config);
    out.0
}

pub(crate) fn encode_packet(id: u64, tag: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Out::header(KIND_PACKET, id);
    out.u64(tag);
    out.0.extend_from_slice(bytes);
    out.0
}

pub(crate) fn encode_finish(id: u64) -> Vec<u8> {
    Out::header(KIND_FINISH, id).0
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn decode_request(frame: &[u8]) -> Res<Request> {
    let kind = *frame.first().ok_or(WireError::Malformed)?;
    match kind {
        KIND_OPEN => {
            let (id, mut reader) = In::open(frame, KIND_OPEN)?;
            let max_chunk_bytes = reader.u64()?;
            let color = match reader.u8()? {
                0 => ColorTag::Untagged,
                1 => ColorTag::Nclx {
                    primaries: reader.u16()?,
                    transfer: reader.u16()?,
                    matrix: reader.u16()?,
                    full_range: match reader.u8()? {
                        0 => None,
                        1 => Some(false),
                        2 => Some(true),
                        _ => return Err(WireError::Malformed),
                    },
                },
                2 => ColorTag::IccProfile {
                    bytes: reader.u32()?,
                },
                _ => return Err(WireError::Malformed),
            };
            let config = reader.bytes()?.to_vec();
            reader.done()?;
            Ok(Request::Open(
                id,
                OpenRequest {
                    max_chunk_bytes,
                    color,
                    config,
                },
            ))
        }
        KIND_PACKET => {
            let (id, mut reader) = In::open(frame, KIND_PACKET)?;
            let tag = reader.u64()?;
            Ok(Request::Packet(id, tag, reader.rest().to_vec()))
        }
        KIND_FINISH => {
            let (id, reader) = In::open(frame, KIND_FINISH)?;
            reader.done()?;
            Ok(Request::Finish(id))
        }
        _ => Err(WireError::Malformed),
    }
}

// ---- replies ------------------------------------------------------------------------------------

/// One decoded picture as it crosses the wire (planes tight, as `VideoFormat` documents).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WireFrame {
    pub tag: u64,
    pub format: VideoFormat,
    pub film_grain_applied: bool,
    pub planes: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Opened,
    Frame(WireFrame),
    BatchEnd(u32),
    Error(ReelError),
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn encode_opened(id: u64) -> Vec<u8> {
    Out::header(KIND_OPENED, id).0
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn encode_batch_end(id: u64, frames: u32) -> Vec<u8> {
    let mut out = Out::header(KIND_BATCH_END, id);
    out.u32(frames);
    out.0
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn encode_frame(id: u64, frame: &WireFrame) -> Vec<u8> {
    let planes_len = frame
        .planes
        .iter()
        .map(Vec::len)
        .fold(0_usize, usize::saturating_add);
    let mut out = Out(Vec::with_capacity(planes_len.saturating_add(96)));
    out.0.extend_from_slice(&Out::header(KIND_FRAME, id).0);
    out.u64(frame.tag);
    let format = &frame.format;
    out.u32(format.width);
    out.u32(format.height);
    out.u8(format.bit_depth);
    out.u8(match format.chroma {
        ChromaFormat::Mono => 0,
        ChromaFormat::Yuv420 => 1,
        ChromaFormat::Yuv422 => 2,
        ChromaFormat::Yuv444 => 3,
    });
    out.u16(format.color.primaries);
    out.u16(format.color.transfer);
    out.u16(format.color.matrix);
    out.u8(u8::from(format.color.full_range));
    out.u8(match format.color.origin {
        ColorOrigin::SequenceHeader => 0,
        ColorOrigin::Container => 1,
        ColorOrigin::Untagged => 2,
    });
    out.u8(u8::from(frame.film_grain_applied));
    out.u8(u8::try_from(frame.planes.len()).unwrap_or(u8::MAX));
    for plane in &frame.planes {
        out.bytes(plane);
    }
    out.0
}

#[cfg(any(test, feature = "av1-worker"))]
pub(crate) fn encode_error(id: u64, error: &ReelError) -> Vec<u8> {
    let (class, what) = match error {
        ReelError::Corrupt { what } => (1_u8, *what),
        ReelError::LimitExceeded { what } => (2, *what),
        ReelError::Unsupported { what } => (3, *what),
        _ => (3, WHATS[0]),
    };
    let index = WHATS.iter().position(|known| *known == what).unwrap_or(0);
    let mut out = Out::header(KIND_ERROR, id);
    out.u8(class);
    out.u8(u8::try_from(index).unwrap_or(0));
    out.0
}

/// A frame of `kind` FRAME that claims `filler` bytes of body: the worker's oversize test fault.
#[cfg(all(feature = "av1-worker", feature = "test-faults"))]
pub(crate) fn encode_oversize_frame(id: u64, filler: usize) -> Vec<u8> {
    let mut out = Out::header(KIND_FRAME, id);
    out.0.resize(out.0.len().saturating_add(filler), 0);
    out.0
}

/// A frame of an unknown kind: the worker's protocol-garbage test fault.
#[cfg(all(feature = "av1-worker", feature = "test-faults"))]
pub(crate) fn encode_garbage() -> Vec<u8> {
    vec![0xEE, 1, 2, 3]
}

pub(crate) fn decode_reply(frame: &[u8], id: u64) -> Res<Reply> {
    let kind = *frame.first().ok_or(WireError::Malformed)?;
    let (got, mut reader) = In::open(frame, kind)?;
    if got != id {
        return Err(WireError::Malformed);
    }
    match kind {
        KIND_OPENED => {
            reader.done()?;
            Ok(Reply::Opened)
        }
        KIND_BATCH_END => {
            let frames = reader.u32()?;
            reader.done()?;
            Ok(Reply::BatchEnd(frames))
        }
        KIND_ERROR => {
            let class = reader.u8()?;
            let what = WHATS
                .get(usize::from(reader.u8()?))
                .copied()
                .ok_or(WireError::Malformed)?;
            reader.done()?;
            Ok(Reply::Error(match class {
                1 => ReelError::Corrupt { what },
                2 => ReelError::LimitExceeded { what },
                3 => ReelError::Unsupported { what },
                _ => return Err(WireError::Malformed),
            }))
        }
        KIND_FRAME => decode_frame(&mut reader).map(Reply::Frame),
        _ => Err(WireError::Malformed),
    }
}

fn decode_frame(reader: &mut In<'_>) -> Res<WireFrame> {
    let tag = reader.u64()?;
    let width = reader.u32()?;
    let height = reader.u32()?;
    let bit_depth = reader.u8()?;
    let chroma = match reader.u8()? {
        0 => ChromaFormat::Mono,
        1 => ChromaFormat::Yuv420,
        2 => ChromaFormat::Yuv422,
        3 => ChromaFormat::Yuv444,
        _ => return Err(WireError::Malformed),
    };
    let primaries = reader.u16()?;
    let transfer = reader.u16()?;
    let matrix = reader.u16()?;
    let full_range = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err(WireError::Malformed),
    };
    let origin = match reader.u8()? {
        0 => ColorOrigin::SequenceHeader,
        1 => ColorOrigin::Container,
        2 => ColorOrigin::Untagged,
        _ => return Err(WireError::Malformed),
    };
    let film_grain_applied = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err(WireError::Malformed),
    };
    if width == 0 || height == 0 || !matches!(bit_depth, 8 | 10 | 12) {
        return Err(WireError::Malformed);
    }
    let format = VideoFormat {
        width,
        height,
        bit_depth,
        chroma,
        color: ColorInfo {
            primaries,
            transfer,
            matrix,
            full_range,
            origin,
        },
    };
    let count = usize::from(reader.u8()?);
    let dims = format.plane_dims();
    if count != dims.len() {
        return Err(WireError::Malformed);
    }
    let bytes_per_sample = format.bytes_per_sample() as u64;
    let mut planes = Vec::with_capacity(count);
    for (w, h) in dims {
        // Each plane is exactly width * height samples: any other length is a lying worker. Dimensions
        // whose byte size does not fit u64 are a lying worker too (never an overflow).
        let expected = u64::from(w)
            .checked_mul(u64::from(h))
            .and_then(|samples| samples.checked_mul(bytes_per_sample))
            .ok_or(WireError::Malformed)?;
        let plane = reader.bytes()?;
        if plane.len() as u64 != expected {
            return Err(WireError::Malformed);
        }
        planes.push(plane.to_vec());
    }
    reader.done()?;
    Ok(WireFrame {
        tag,
        format,
        film_grain_applied,
        planes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> WireFrame {
        let format = VideoFormat {
            width: 4,
            height: 2,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv420,
            color: ColorInfo {
                primaries: 1,
                transfer: 1,
                matrix: 1,
                full_range: false,
                origin: ColorOrigin::SequenceHeader,
            },
        };
        WireFrame {
            tag: 7,
            format,
            film_grain_applied: false,
            planes: vec![vec![1; 8], vec![2; 2], vec![3; 2]],
        }
    }

    #[test]
    fn wire_roundtrip_and_bounds() {
        // Requests round trip, including every colour tag shape and an empty configuration.
        for color in [
            ColorTag::Untagged,
            ColorTag::Nclx {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: Some(true),
            },
            ColorTag::Nclx {
                primaries: 2,
                transfer: 2,
                matrix: 2,
                full_range: None,
            },
            ColorTag::IccProfile { bytes: 99 },
        ] {
            let open = OpenRequest {
                max_chunk_bytes: 1 << 20,
                color,
                config: vec![0x81, 0, 0, 0, 10, 11],
            };
            assert_eq!(
                decode_request(&encode_open(5, &open)),
                Ok(Request::Open(5, open))
            );
        }
        assert_eq!(
            decode_request(&encode_packet(6, 9, &[1, 2, 3])),
            Ok(Request::Packet(6, 9, vec![1, 2, 3]))
        );
        assert_eq!(decode_request(&encode_finish(7)), Ok(Request::Finish(7)));
        // Replies round trip and every error class keeps its static `what`.
        let frame = sample_frame();
        let encoded = encode_frame(3, &frame);
        assert_eq!(decode_reply(&encoded, 3), Ok(Reply::Frame(frame)));
        assert_eq!(decode_reply(&encode_opened(3), 3), Ok(Reply::Opened));
        assert_eq!(
            decode_reply(&encode_batch_end(3, 2), 3),
            Ok(Reply::BatchEnd(2))
        );
        for error in [
            ReelError::Corrupt {
                what: "av1-bitstream",
            },
            ReelError::LimitExceeded {
                what: "av1-frame-bytes",
            },
            ReelError::Unsupported {
                what: "av1-profile",
            },
        ] {
            assert_eq!(
                decode_reply(&encode_error(3, &error), 3),
                Ok(Reply::Error(error))
            );
        }
        // A string outside the table degrades to the catch-all, never to a new static string.
        assert_eq!(
            decode_reply(
                &encode_error(3, &ReelError::Corrupt { what: "invented" }),
                3
            ),
            Ok(Reply::Error(ReelError::Corrupt {
                what: "av1-worker-error"
            }))
        );
        // A stale id, a wrong version and every truncation of a frame are malformed, never a panic.
        assert_eq!(decode_reply(&encoded, 4), Err(WireError::Malformed));
        let mut wrong_version = encoded.clone();
        wrong_version[1] ^= 0xFF;
        assert_eq!(decode_reply(&wrong_version, 3), Err(WireError::Malformed));
        for cut in 0..encoded.len() {
            assert!(decode_reply(&encoded[..cut], 3).is_err(), "cut {cut}");
        }
        // A plane whose length disagrees with the declared dimensions is a lying peer.
        let mut lying = sample_frame();
        lying.planes[0].pop();
        assert_eq!(
            decode_reply(&encode_frame(3, &lying), 3),
            Err(WireError::Malformed)
        );
        let mut missing_plane = sample_frame();
        missing_plane.planes.pop();
        assert_eq!(
            decode_reply(&encode_frame(3, &missing_plane), 3),
            Err(WireError::Malformed)
        );
        // Framing: a declared length above the cap is refused before anything is read or allocated.
        let mut huge = Vec::new();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            read_frame(&mut huge.as_slice(), 1 << 20),
            Err(WireError::TooLarge)
        );
        let mut short = Vec::new();
        short.extend_from_slice(&100_u32.to_le_bytes());
        short.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            read_frame(&mut short.as_slice(), 1 << 20),
            Err(WireError::Closed)
        );
        let mut piped = Vec::new();
        write_frame(&mut piped, &encoded).unwrap();
        assert_eq!(
            read_frame(&mut piped.as_slice(), 1 << 20),
            Ok(encoded.clone())
        );
        // Hello: version and identity travel, a different version still parses so it can be reported.
        let hello = encode_hello("rav1d", "1.1.0");
        assert_eq!(
            decode_hello(&hello),
            Ok(Hello {
                protocol: PROTOCOL_VERSION,
                decoder_id: "rav1d".to_owned(),
                decoder_version: "1.1.0".to_owned(),
            })
        );
        assert!(is_terminal(&encode_batch_end(1, 0)) && !is_terminal(&encoded));
    }

    /// R5: a lying worker FRAME declaring u32::MAX x u32::MAX at 10 or 12 bit is a few dozen bytes, so it
    /// passes the reply cap; its declared plane size must be Malformed, never an arithmetic overflow.
    #[test]
    fn frame_dims_u32_max_is_malformed() {
        for (bit_depth, chroma) in [
            (10, ChromaFormat::Yuv420),
            (12, ChromaFormat::Yuv444),
            (10, ChromaFormat::Mono),
            (8, ChromaFormat::Yuv444),
        ] {
            let mut lying = sample_frame();
            lying.format.width = u32::MAX;
            lying.format.height = u32::MAX;
            lying.format.bit_depth = bit_depth;
            lying.format.chroma = chroma;
            lying.planes = vec![Vec::new(); lying.format.plane_dims().len()];
            let encoded = encode_frame(3, &lying);
            assert!(encoded.len() < 128, "{}", encoded.len());
            assert_eq!(
                decode_reply(&encoded, 3),
                Err(WireError::Malformed),
                "{bit_depth} {chroma:?}"
            );
        }
        // Aggregate over budget: a frame whose declared length exceeds the reply cap is refused by the
        // framing before any byte of it is read or allocated.
        let big = encode_frame(3, &sample_frame());
        let mut piped = Vec::new();
        write_frame(&mut piped, &big).unwrap();
        assert_eq!(
            read_frame(&mut piped.as_slice(), big.len().saturating_sub(1)),
            Err(WireError::TooLarge)
        );
    }
}
