//! Framed, versioned, little-endian protocol between the host (`IsolatedHost`) and the worker process
//! over the child's stdin/stdout. No third-party serializer.
//!
//! Frame = header (`u32 magic, u16 version, u8 kind, u32 payload length`, 11 bytes) + payload. The length
//! is checked against the caller's cap BEFORE any payload allocation; the payload is read incrementally,
//! so a peer that lies about the length cannot make the reader allocate more than it actually sent.
//! The worker speaks first (HELLO); every request carries a `u64` id that its single reply echoes, so a
//! stale or mismatched reply is a protocol fault. Decoding is bounds-checked: malformed input never
//! panics. Blocks are planar `f32` (channel after channel) with explicit frame and channel counts.
use crate::error::{PluginError, ProtocolFault};
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS};
use std::io::{self, Read, Write};

pub(crate) const MAGIC: u32 = 0x4B50_5348; // bytes "HSPK"
pub(crate) const PROTOCOL_VERSION: u16 = 1;
pub(crate) const HEADER_BYTES: usize = 11;
pub(crate) const HELLO_CAP: u32 = 1024;
/// Bytes a reply may carry beyond its samples or state blob (ids, counts, text).
pub(crate) const REPLY_SLACK: u32 = 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PARAMS: usize = 256;

pub(crate) const K_PROBE: u8 = 1;
pub(crate) const K_CONFIGURE: u8 = 2;
pub(crate) const K_PROCESS: u8 = 3;
pub(crate) const K_SET_PARAM: u8 = 4;
pub(crate) const K_CAPTURE: u8 = 5;
pub(crate) const K_SHUTDOWN: u8 = 6;
pub(crate) const K_HELLO: u8 = 0x80;
pub(crate) const K_PROBED: u8 = 0x82;
pub(crate) const K_CONFIGURED: u8 = 0x83;
pub(crate) const K_PROCESSED: u8 = 0x84;
pub(crate) const K_PARAM_SET: u8 = 0x85;
pub(crate) const K_STATE: u8 = 0x86;
pub(crate) const K_BYE: u8 = 0x87;
pub(crate) const K_ERROR: u8 = 0xFF;

/// Worker error codes (reply `Error`), mapped to [`PluginError`] by the host.
pub(crate) const E_HASH: u8 = 1;
pub(crate) const E_ABI: u8 = 2;
pub(crate) const E_STALE: u8 = 3;
pub(crate) const E_BUDGET: u8 = 4;
pub(crate) const E_CONFIG: u8 = 5;
pub(crate) const E_NOT_CONFIGURED: u8 = 6;
pub(crate) const E_UNAVAILABLE: u8 = 7;

const fn known_kind(kind: u8) -> bool {
    matches!(
        kind,
        K_PROBE
            | K_CONFIGURE
            | K_PROCESS
            | K_SET_PARAM
            | K_CAPTURE
            | K_SHUTDOWN
            | K_HELLO
            | K_PROBED
            | K_CONFIGURED
            | K_PROCESSED
            | K_PARAM_SET
            | K_STATE
            | K_BYE
            | K_ERROR
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    /// End of stream at a frame boundary, or an I/O failure: the peer is gone.
    Closed,
    /// The stream ended inside a frame.
    Truncated,
    /// A declared length above the cap (nothing was allocated or read).
    TooLarge,
    Garbage,
    VersionSkew,
    UnknownKind,
    /// Wrong content, length or count inside a well-framed payload.
    Malformed,
}

impl WireError {
    pub(crate) const fn fault(self) -> Option<ProtocolFault> {
        match self {
            Self::Closed => None,
            Self::Truncated => Some(ProtocolFault::Truncated),
            Self::TooLarge => Some(ProtocolFault::Oversize),
            Self::Garbage => Some(ProtocolFault::Garbage),
            Self::VersionSkew => Some(ProtocolFault::VersionSkew),
            Self::UnknownKind => Some(ProtocolFault::UnexpectedKind),
            Self::Malformed => Some(ProtocolFault::Inconsistent),
        }
    }
}

type Res<T> = Result<T, WireError>;

pub(crate) struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

pub(crate) fn read_frame(reader: &mut impl Read, cap: u32) -> Res<Frame> {
    let mut header = [0_u8; HEADER_BYTES];
    let mut got = 0;
    while got < HEADER_BYTES {
        match reader.read(&mut header[got..]) {
            Ok(0) => {
                return Err(if got == 0 {
                    WireError::Closed
                } else {
                    WireError::Truncated
                });
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(WireError::Closed),
        }
    }
    let field = |range: std::ops::Range<usize>| -> Res<&[u8]> {
        header.get(range).ok_or(WireError::Malformed)
    };
    let magic = u32::from_le_bytes(field(0..4)?.try_into().map_err(|_| WireError::Malformed)?);
    let version = u16::from_le_bytes(field(4..6)?.try_into().map_err(|_| WireError::Malformed)?);
    let kind = header[6];
    let length = u32::from_le_bytes(field(7..11)?.try_into().map_err(|_| WireError::Malformed)?);
    if magic != MAGIC {
        return Err(WireError::Garbage);
    }
    if version != PROTOCOL_VERSION {
        return Err(WireError::VersionSkew);
    }
    if !known_kind(kind) {
        return Err(WireError::UnknownKind);
    }
    if length > cap {
        return Err(WireError::TooLarge);
    }
    let wanted = u64::from(length);
    let mut payload = Vec::new();
    let read = reader
        .take(wanted)
        .read_to_end(&mut payload)
        .map_err(|_| WireError::Closed)?;
    if u64::try_from(read) != Ok(wanted) {
        return Err(WireError::Truncated);
    }
    Ok(Frame { kind, payload })
}

pub(crate) fn write_frame(writer: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    writer.write_all(frame)?;
    writer.flush()
}

/// Builds a complete frame (header + payload) in one buffer.
struct Out(Vec<u8>);

impl Out {
    fn begin(kind: u8, capacity: usize) -> Self {
        let mut out = Vec::with_capacity(HEADER_BYTES + capacity);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&[0; 4]);
        Self(out)
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn text(&mut self, v: &str) {
        self.u16(u16::try_from(v.len()).unwrap_or(u16::MAX));
        self.0
            .extend_from_slice(&v.as_bytes()[..v.len().min(usize::from(u16::MAX))]);
    }
    fn samples(&mut self, v: &[f32]) {
        for sample in v {
            self.0.extend_from_slice(&sample.to_le_bytes());
        }
    }
    fn finish(mut self) -> Vec<u8> {
        let length = u32::try_from(self.0.len() - HEADER_BYTES).unwrap_or(u32::MAX);
        self.0[7..HEADER_BYTES].copy_from_slice(&length.to_le_bytes());
        self.0
    }
}

struct In<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> In<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
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
    fn f64(&mut self) -> Res<f64> {
        Ok(f64::from_le_bytes(self.array()?))
    }
    fn text(&mut self, max: usize) -> Res<String> {
        let length = usize::from(self.u16()?);
        if length > max {
            return Err(WireError::Malformed);
        }
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| WireError::Malformed)
    }
    /// `frames * channels` samples that must be exactly the bytes still present.
    fn samples(&mut self, frames: u32, channels: u16) -> Res<Vec<f32>> {
        let frames = usize::try_from(frames).map_err(|_| WireError::Malformed)?;
        let channels = usize::from(channels);
        if frames == 0 || frames > MAX_BLOCK_FRAMES || channels == 0 || channels > MAX_CHANNELS {
            return Err(WireError::Malformed);
        }
        let count = frames * channels;
        let bytes = self.take(count * 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
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
    pub identity: String,
    /// CLAP ABI major version the worker backend supports; 0 = none.
    pub clap_abi: u32,
    pub arch: u8,
}

pub(crate) fn encode_hello(hello: &Hello) -> Vec<u8> {
    let mut out = Out::begin(K_HELLO, 64);
    out.text(&hello.identity);
    out.u32(hello.clap_abi);
    out.u8(hello.arch);
    out.finish()
}

pub(crate) fn decode_hello(frame: &Frame) -> Res<Hello> {
    if frame.kind != K_HELLO {
        return Err(WireError::UnknownKind);
    }
    let mut r = In::new(&frame.payload);
    let hello = Hello {
        identity: r.text(256)?,
        clap_abi: r.u32()?,
        arch: r.u8()?,
    };
    r.done()?;
    Ok(hello)
}

// ---- requests -----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginSpec {
    pub format: u8,
    pub arch: u8,
    pub abi: u32,
    pub sha: [u8; 32],
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfigureSpec {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub max_frames: u32,
    pub params: Vec<(u32, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    Probe {
        id: u64,
        spec: PluginSpec,
    },
    Configure {
        id: u64,
        spec: PluginSpec,
        cfg: ConfigureSpec,
    },
    Process {
        id: u64,
        expected_version: u64,
        start_sample: u64,
        frames: u32,
        channels: u16,
        samples: Vec<f32>,
    },
    SetParam {
        id: u64,
        param: u32,
        value: f64,
    },
    Capture {
        id: u64,
        max_bytes: u32,
    },
    Shutdown {
        id: u64,
    },
}

fn put_spec(out: &mut Out, spec: &PluginSpec) {
    out.u8(spec.format);
    out.u8(spec.arch);
    out.u32(spec.abi);
    out.0.extend_from_slice(&spec.sha);
    out.text(&spec.path);
}

fn get_spec(r: &mut In<'_>) -> Res<PluginSpec> {
    Ok(PluginSpec {
        format: r.u8()?,
        arch: r.u8()?,
        abi: r.u32()?,
        sha: r.array()?,
        path: r.text(MAX_PATH_BYTES)?,
    })
}

pub(crate) fn encode_probe(id: u64, spec: &PluginSpec) -> Vec<u8> {
    let mut out = Out::begin(K_PROBE, 64 + spec.path.len());
    out.u64(id);
    put_spec(&mut out, spec);
    out.finish()
}

pub(crate) fn encode_configure(id: u64, spec: &PluginSpec, cfg: &ConfigureSpec) -> Vec<u8> {
    let mut out = Out::begin(K_CONFIGURE, 96 + spec.path.len() + cfg.params.len() * 12);
    out.u64(id);
    put_spec(&mut out, spec);
    out.u32(cfg.sample_rate_hz);
    out.u16(cfg.channels);
    out.u32(cfg.max_frames);
    out.u16(u16::try_from(cfg.params.len()).unwrap_or(u16::MAX));
    for (param, value) in &cfg.params {
        out.u32(*param);
        out.f64(*value);
    }
    out.finish()
}

pub(crate) fn encode_process(
    id: u64,
    expected_version: u64,
    start_sample: u64,
    frames: u32,
    channels: u16,
    samples: &[f32],
) -> Vec<u8> {
    let mut out = Out::begin(K_PROCESS, 32 + samples.len() * 4);
    out.u64(id);
    out.u64(expected_version);
    out.u64(start_sample);
    out.u32(frames);
    out.u16(channels);
    out.samples(samples);
    out.finish()
}

pub(crate) fn encode_set_param(id: u64, param: u32, value: f64) -> Vec<u8> {
    let mut out = Out::begin(K_SET_PARAM, 20);
    out.u64(id);
    out.u32(param);
    out.f64(value);
    out.finish()
}

pub(crate) fn encode_capture(id: u64, max_bytes: u32) -> Vec<u8> {
    let mut out = Out::begin(K_CAPTURE, 12);
    out.u64(id);
    out.u32(max_bytes);
    out.finish()
}

pub(crate) fn encode_shutdown(id: u64) -> Vec<u8> {
    let mut out = Out::begin(K_SHUTDOWN, 8);
    out.u64(id);
    out.finish()
}

pub(crate) fn decode_request(frame: &Frame) -> Res<Request> {
    let mut r = In::new(&frame.payload);
    let id = r.u64()?;
    let request = match frame.kind {
        K_PROBE => Request::Probe {
            id,
            spec: get_spec(&mut r)?,
        },
        K_CONFIGURE => {
            let spec = get_spec(&mut r)?;
            let sample_rate_hz = r.u32()?;
            let channels = r.u16()?;
            let max_frames = r.u32()?;
            let count = usize::from(r.u16()?);
            if count > MAX_PARAMS {
                return Err(WireError::Malformed);
            }
            let mut params = Vec::with_capacity(count);
            for _ in 0..count {
                params.push((r.u32()?, r.f64()?));
            }
            Request::Configure {
                id,
                spec,
                cfg: ConfigureSpec {
                    sample_rate_hz,
                    channels,
                    max_frames,
                    params,
                },
            }
        }
        K_PROCESS => {
            let expected_version = r.u64()?;
            let start_sample = r.u64()?;
            let frames = r.u32()?;
            let channels = r.u16()?;
            Request::Process {
                id,
                expected_version,
                start_sample,
                frames,
                channels,
                samples: r.samples(frames, channels)?,
            }
        }
        K_SET_PARAM => Request::SetParam {
            id,
            param: r.u32()?,
            value: r.f64()?,
        },
        K_CAPTURE => Request::Capture {
            id,
            max_bytes: r.u32()?,
        },
        K_SHUTDOWN => Request::Shutdown { id },
        _ => return Err(WireError::UnknownKind),
    };
    r.done()?;
    Ok(request)
}

// ---- replies ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reply {
    Probed {
        id: u64,
        plugin_id: String,
        name: String,
        format: u8,
        abi: u32,
        inputs: u16,
        outputs: u16,
    },
    Configured {
        id: u64,
        identity: String,
        latency: u32,
        version: u64,
    },
    Processed {
        id: u64,
        version: u64,
        completeness: u8,
        latency: u32,
        frames: u32,
        channels: u16,
        samples: Vec<f32>,
    },
    ParamSet {
        id: u64,
        version: u64,
    },
    State {
        id: u64,
        version: u64,
        completeness: u8,
        bytes: Vec<u8>,
    },
    Bye {
        id: u64,
    },
    Error {
        id: u64,
        code: u8,
        detail: u64,
    },
}

impl Reply {
    pub(crate) const fn id(&self) -> u64 {
        match self {
            Self::Probed { id, .. }
            | Self::Configured { id, .. }
            | Self::Processed { id, .. }
            | Self::ParamSet { id, .. }
            | Self::State { id, .. }
            | Self::Bye { id }
            | Self::Error { id, .. } => *id,
        }
    }
}

pub(crate) fn encode_reply(reply: &Reply) -> Vec<u8> {
    match reply {
        Reply::Probed {
            id,
            plugin_id,
            name,
            format,
            abi,
            inputs,
            outputs,
        } => {
            let mut out = Out::begin(K_PROBED, 64 + plugin_id.len() + name.len());
            out.u64(*id);
            out.text(plugin_id);
            out.text(name);
            out.u8(*format);
            out.u32(*abi);
            out.u16(*inputs);
            out.u16(*outputs);
            out.finish()
        }
        Reply::Configured {
            id,
            identity,
            latency,
            version,
        } => {
            let mut out = Out::begin(K_CONFIGURED, 32 + identity.len());
            out.u64(*id);
            out.text(identity);
            out.u32(*latency);
            out.u64(*version);
            out.finish()
        }
        Reply::Processed {
            id,
            version,
            completeness,
            latency,
            frames,
            channels,
            samples,
        } => {
            let mut out = Out::begin(K_PROCESSED, 40 + samples.len() * 4);
            out.u64(*id);
            out.u64(*version);
            out.u8(*completeness);
            out.u32(*latency);
            out.u32(*frames);
            out.u16(*channels);
            out.samples(samples);
            out.finish()
        }
        Reply::ParamSet { id, version } => {
            let mut out = Out::begin(K_PARAM_SET, 16);
            out.u64(*id);
            out.u64(*version);
            out.finish()
        }
        Reply::State {
            id,
            version,
            completeness,
            bytes,
        } => {
            let mut out = Out::begin(K_STATE, 24 + bytes.len());
            out.u64(*id);
            out.u64(*version);
            out.u8(*completeness);
            out.u32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            out.0.extend_from_slice(bytes);
            out.finish()
        }
        Reply::Bye { id } => {
            let mut out = Out::begin(K_BYE, 8);
            out.u64(*id);
            out.finish()
        }
        Reply::Error { id, code, detail } => {
            let mut out = Out::begin(K_ERROR, 17);
            out.u64(*id);
            out.u8(*code);
            out.u64(*detail);
            out.finish()
        }
    }
}

pub(crate) fn decode_reply(frame: &Frame) -> Res<Reply> {
    let mut r = In::new(&frame.payload);
    let id = r.u64()?;
    let reply = match frame.kind {
        K_PROBED => Reply::Probed {
            id,
            plugin_id: r.text(256)?,
            name: r.text(256)?,
            format: r.u8()?,
            abi: r.u32()?,
            inputs: r.u16()?,
            outputs: r.u16()?,
        },
        K_CONFIGURED => Reply::Configured {
            id,
            identity: r.text(256)?,
            latency: r.u32()?,
            version: r.u64()?,
        },
        K_PROCESSED => {
            let version = r.u64()?;
            let completeness = r.u8()?;
            let latency = r.u32()?;
            let frames = r.u32()?;
            let channels = r.u16()?;
            Reply::Processed {
                id,
                version,
                completeness,
                latency,
                frames,
                channels,
                samples: r.samples(frames, channels)?,
            }
        }
        K_PARAM_SET => Reply::ParamSet {
            id,
            version: r.u64()?,
        },
        K_STATE => {
            let version = r.u64()?;
            let completeness = r.u8()?;
            let length = usize::try_from(r.u32()?).map_err(|_| WireError::Malformed)?;
            Reply::State {
                id,
                version,
                completeness,
                bytes: r.take(length)?.to_vec(),
            }
        }
        K_BYE => Reply::Bye { id },
        K_ERROR => Reply::Error {
            id,
            code: r.u8()?,
            detail: r.u64()?,
        },
        _ => return Err(WireError::UnknownKind),
    };
    r.done()?;
    Ok(reply)
}

/// Host-side meaning of a worker `Error` reply.
pub(crate) fn error_of(code: u8) -> PluginError {
    match code {
        E_HASH => PluginError::HashMismatch,
        E_ABI => PluginError::AbiMismatch,
        E_STALE => PluginError::StaleState,
        E_BUDGET => PluginError::BudgetExceeded,
        E_CONFIG => PluginError::InvalidConfig,
        E_UNAVAILABLE => PluginError::Unavailable,
        E_NOT_CONFIGURED => PluginError::Protocol(ProtocolFault::UnexpectedKind),
        _ => PluginError::Protocol(ProtocolFault::Inconsistent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(bytes: &[u8]) -> Frame {
        let mut cursor = io::Cursor::new(bytes.to_vec());
        read_frame(&mut cursor, 1 << 20).unwrap()
    }

    #[test]
    fn wire_roundtrip_and_hostile_input() {
        let spec = PluginSpec {
            format: 1,
            arch: 1,
            abi: 1,
            sha: [7; 32],
            path: "a/b.clap".to_owned(),
        };
        let cfg = ConfigureSpec {
            sample_rate_hz: 48_000,
            channels: 2,
            max_frames: 256,
            params: vec![(0, 0.5)],
        };
        let samples = [0.0_f32, 0.25, -0.25, f32::from_bits(0x3f00_0001), 1.0, -1.0];
        let requests = [
            encode_probe(5, &spec),
            encode_configure(6, &spec, &cfg),
            encode_process(7, 3, 99, 3, 2, &samples),
            encode_set_param(8, 0, 0.5),
            encode_capture(9, 64),
            encode_shutdown(10),
        ];
        for bytes in &requests {
            let request = decode_request(&frame_of(bytes)).unwrap();
            // Every strict prefix of the payload is rejected, never accepted or panicking.
            let frame = frame_of(bytes);
            for cut in 0..frame.payload.len() {
                let short = Frame {
                    kind: frame.kind,
                    payload: frame.payload[..cut].to_vec(),
                };
                assert_eq!(
                    decode_request(&short),
                    Err(WireError::Malformed),
                    "cut {cut} of {request:?}"
                );
            }
        }
        match decode_request(&frame_of(&requests[2])).unwrap() {
            Request::Process { samples: got, .. } => {
                let bits = |s: &[f32]| s.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&got), bits(&samples));
            }
            other => panic!("unexpected {other:?}"),
        }
        let reply = Reply::Processed {
            id: 7,
            version: 3,
            completeness: 1,
            latency: 4,
            frames: 3,
            channels: 2,
            samples: samples.to_vec(),
        };
        assert_eq!(
            decode_reply(&frame_of(&encode_reply(&reply))).unwrap(),
            reply
        );

        // Header hostility: oversize is refused before allocation, skew/garbage/unknown kind are typed.
        let ok = encode_shutdown(1);
        let mut cursor = io::Cursor::new(ok.clone());
        assert_eq!(read_frame(&mut cursor, 4).err(), Some(WireError::TooLarge));
        let mut skew = ok.clone();
        skew[4] = 9;
        assert_eq!(
            read_frame(&mut io::Cursor::new(skew), 64).err(),
            Some(WireError::VersionSkew)
        );
        let mut kind = ok.clone();
        kind[6] = 0x42;
        assert_eq!(
            read_frame(&mut io::Cursor::new(kind), 64).err(),
            Some(WireError::UnknownKind)
        );
        assert_eq!(
            read_frame(&mut io::Cursor::new(vec![0xAB; 16]), 64).err(),
            Some(WireError::Garbage)
        );
        assert_eq!(
            read_frame(&mut io::Cursor::new(Vec::new()), 64).err(),
            Some(WireError::Closed)
        );
        assert_eq!(
            read_frame(&mut io::Cursor::new(ok[..5].to_vec()), 64).err(),
            Some(WireError::Truncated)
        );
        assert_eq!(
            read_frame(&mut io::Cursor::new(ok[..ok.len() - 1].to_vec()), 64).err(),
            Some(WireError::Truncated)
        );
    }
}
