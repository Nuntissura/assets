//! AIFF and AIFF-C (Apple Audio Interchange File Format 1.3 / AIFF-C draft 1991): reader and writer,
//! Studio-owned. Reader: FORM/AIFF and FORM/AIFC, COMM (channels, frames, sample size, 80-bit IEEE
//! extended rate, AIFC compression type), SSND (offset, block size, data); compression types NONE /
//! twos (big-endian signed), sowt (little-endian), in24, in32, raw (unsigned 8-bit), fl32 / FL32,
//! fl64 / FL64, ulaw / alaw (G.711, decoded by formula). Writer: AIFF for integer PCM (8/16/24/32 bit)
//! and AIFF-C `fl32` / `fl64` for float. Every chunk size, count and the rate are checked before use.
#![deny(clippy::arithmetic_side_effects)]
use super::MAX_SAMPLE_RATE;
use super::engine::{FrameSource, StreamShape};
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::CancellationToken;
use std::io::{SeekFrom, Write};

/// Most channels an AIFF stream may have here (the WAV encoder's bound).
pub const AIFF_MAX_CHANNELS: u16 = 64;
const MAX_CHUNKS: u32 = 256;
const BLOCK_FRAMES: usize = 4096;

/// Sample layout of an AIFF / AIFF-C stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiffEncoding {
    /// Signed integer, big-endian (`NONE`, `twos`, `in24`, `in32`), bits 1..=32.
    IntBe(u16),
    /// Signed 16-bit little-endian (`sowt`).
    Int16Le,
    /// Unsigned 8-bit (`raw `).
    U8,
    FloatBe32,
    FloatBe64,
    ULaw,
    ALaw,
}

impl AiffEncoding {
    fn bytes(self) -> usize {
        match self {
            Self::IntBe(bits) => usize::from(bits.div_ceil(8)),
            Self::Int16Le => 2,
            Self::U8 | Self::ULaw | Self::ALaw => 1,
            Self::FloatBe32 => 4,
            Self::FloatBe64 => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiffInfo {
    pub channels: u16,
    pub frames: u64,
    pub sample_rate: u32,
    pub encoding: AiffEncoding,
    pub data_offset: u64,
    /// Bytes of sample data present after `data_offset` (may be fewer than `frames` need).
    pub data_available: u64,
}

const CORRUPT: ReelError = ReelError::Corrupt { what: "aiff-comm" };

/// 80-bit IEEE 754 extended (big-endian) to an integral sample rate.
fn extended_rate(b: &[u8]) -> Result<u32, ReelError> {
    let e = b.get(..10).ok_or(CORRUPT)?;
    let exponent = (u16::from(e[0] & 0x7F) << 8) | u16::from(e[1]);
    if e[0] & 0x80 != 0 {
        return Err(CORRUPT);
    }
    let mut mantissa = 0u64;
    for byte in &e[2..10] {
        mantissa = (mantissa << 8) | u64::from(*byte);
    }
    if mantissa == 0 {
        return Err(ReelError::Corrupt {
            what: "aiff-sample-rate",
        });
    }
    // value = mantissa * 2^(exponent - 16383 - 63)
    let shift = i32::from(exponent).wrapping_sub(16383).wrapping_sub(63);
    let value = if shift >= 0 {
        if shift > 0 || mantissa > u64::from(MAX_SAMPLE_RATE) {
            return Err(ReelError::LimitExceeded {
                what: "aiff-sample-rate",
            });
        }
        mantissa
    } else {
        let s = shift.unsigned_abs();
        if s >= 64 {
            0
        } else {
            let whole = mantissa >> s;
            if whole << s != mantissa {
                return Err(ReelError::Unsupported {
                    what: "aiff-fractional-sample-rate",
                });
            }
            whole
        }
    };
    if value == 0 {
        return Err(ReelError::Corrupt {
            what: "aiff-sample-rate",
        });
    }
    u32::try_from(value)
        .ok()
        .filter(|r| *r <= MAX_SAMPLE_RATE)
        .ok_or(ReelError::LimitExceeded {
            what: "aiff-sample-rate",
        })
}

/// The 10-byte extended encoding of an integral rate.
fn rate_extended(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let lz = rate.leading_zeros();
    let exponent = 16383u32.wrapping_add(31u32.wrapping_sub(lz));
    let mantissa = u64::from(rate) << (32u32.wrapping_add(lz));
    out[0] = (exponent >> 8) as u8;
    out[1] = exponent as u8;
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

fn be32(b: &[u8], at: usize) -> Result<u32, ReelError> {
    b.get(at..at.saturating_add(4))
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_be_bytes)
        .ok_or(CORRUPT)
}

fn be16(b: &[u8], at: usize) -> Result<u16, ReelError> {
    b.get(at..at.saturating_add(2))
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_be_bytes)
        .ok_or(CORRUPT)
}

fn read_exact(src: &mut dyn ReadSeek, buf: &mut [u8]) -> Result<(), ReelError> {
    src.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ReelError::Corrupt {
                what: "aiff-truncated-header",
            }
        } else {
            ReelError::Io(e.kind())
        }
    })
}

/// Parse the chunk structure (COMM and the start of SSND); reads only chunk headers and COMM.
pub(crate) fn parse(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
) -> Result<AiffInfo, ReelError> {
    src.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; 12];
    read_exact(src, &mut head)?;
    if &head[0..4] != b"FORM" {
        return Err(ReelError::ImportUnsupported {
            detected: "not-aiff".into(),
        });
    }
    let aifc = match &head[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => {
            return Err(ReelError::ImportUnsupported {
                detected: "iff-form-other".into(),
            });
        }
    };
    let mut at = 12u64;
    let mut comm: Option<(u16, u64, u16, u32, AiffEncoding)> = None;
    for _ in 0..MAX_CHUNKS {
        check_bounds(at, limits)?;
        if at.saturating_add(8) > file_len {
            break;
        }
        src.seek(SeekFrom::Start(at))?;
        let mut ch = [0u8; 8];
        read_exact(src, &mut ch)?;
        let size = u64::from(u32::from_be_bytes([ch[4], ch[5], ch[6], ch[7]]));
        let body = at.checked_add(8).ok_or(CORRUPT)?;
        match &ch[0..4] {
            b"COMM" => {
                if !(18..=256).contains(&size) {
                    return Err(CORRUPT);
                }
                let mut c = vec![0u8; size as usize];
                read_exact(src, &mut c)?;
                let channels = be16(&c, 0)?;
                let frames = u64::from(be32(&c, 2)?);
                let bits = be16(&c, 6)?;
                let rate = extended_rate(c.get(8..18).ok_or(CORRUPT)?)?;
                let encoding = if aifc {
                    let kind = c.get(18..22).ok_or(CORRUPT)?;
                    match kind {
                        b"NONE" | b"twos" => int_encoding(bits)?,
                        b"in24" => AiffEncoding::IntBe(24),
                        b"in32" => AiffEncoding::IntBe(32),
                        b"sowt" if bits == 16 => AiffEncoding::Int16Le,
                        b"raw " if bits == 8 => AiffEncoding::U8,
                        b"fl32" | b"FL32" => AiffEncoding::FloatBe32,
                        b"fl64" | b"FL64" => AiffEncoding::FloatBe64,
                        b"ulaw" | b"ULAW" => AiffEncoding::ULaw,
                        b"alaw" | b"ALAW" => AiffEncoding::ALaw,
                        other => {
                            return Err(ReelError::CodecUnavailable {
                                codec: format!("aifc-{}", String::from_utf8_lossy(other)),
                            });
                        }
                    }
                } else {
                    int_encoding(bits)?
                };
                if channels == 0 {
                    return Err(CORRUPT);
                }
                if channels > AIFF_MAX_CHANNELS {
                    return Err(ReelError::LimitExceeded {
                        what: "aiff-channels",
                    });
                }
                comm = Some((channels, frames, bits, rate, encoding));
            }
            b"SSND" => {
                let (channels, frames, _, rate, encoding) = comm.ok_or(ReelError::Corrupt {
                    what: "aiff-ssnd-before-comm",
                })?;
                let mut s = [0u8; 8];
                read_exact(src, &mut s)?;
                let offset = u64::from(u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
                let data_offset = body
                    .checked_add(8)
                    .and_then(|d| d.checked_add(offset))
                    .ok_or(CORRUPT)?;
                let declared = size.saturating_sub(8).saturating_sub(offset);
                let data_available = declared.min(file_len.saturating_sub(data_offset));
                return Ok(AiffInfo {
                    channels,
                    frames,
                    sample_rate: rate,
                    encoding,
                    data_offset,
                    data_available,
                });
            }
            _ => {}
        }
        at = body
            .checked_add(size)
            .and_then(|e| e.checked_add(size & 1))
            .ok_or(ReelError::LimitExceeded { what: "aiff-chunk" })?;
    }
    Err(ReelError::Corrupt {
        what: "aiff-no-ssnd",
    })
}

fn check_bounds(at: u64, limits: &Limits) -> Result<(), ReelError> {
    if at > limits.max_probe_bytes {
        return Err(ReelError::LimitExceeded {
            what: "probe-bytes",
        });
    }
    Ok(())
}

fn int_encoding(bits: u16) -> Result<AiffEncoding, ReelError> {
    if (1..=32).contains(&bits) {
        Ok(AiffEncoding::IntBe(bits))
    } else {
        Err(ReelError::Unsupported {
            what: "aiff-sample-size",
        })
    }
}

pub(crate) fn shape(info: &AiffInfo) -> StreamShape {
    StreamShape {
        rate: info.sample_rate,
        channels: info.channels,
        total: Some(info.frames),
        skip: 0,
    }
}

/// G.711 mu-law to linear (ITU-T G.711), scaled to [-1, 1).
fn ulaw(v: u8) -> f32 {
    let u = !v;
    let sign = u & 0x80;
    let exponent = (u >> 4) & 7;
    let mantissa = i32::from(u & 0x0F);
    let magnitude = ((mantissa << 3) | 0x84) << exponent;
    let sample = magnitude.wrapping_sub(0x84);
    let s = if sign != 0 {
        sample.wrapping_neg()
    } else {
        sample
    };
    s as f32 / 32768.0
}

/// G.711 A-law to linear, scaled to [-1, 1).
fn alaw(v: u8) -> f32 {
    let a = v ^ 0x55;
    let sign = a & 0x80;
    let exponent = (a >> 4) & 7;
    let mantissa = i32::from(a & 0x0F);
    let magnitude = if exponent == 0 {
        (mantissa << 4) | 8
    } else {
        ((mantissa << 4) | 0x108) << exponent.wrapping_sub(1)
    };
    let s = if sign != 0 {
        magnitude
    } else {
        magnitude.wrapping_neg()
    };
    s as f32 / 32768.0
}

pub(crate) struct AiffReader<'a> {
    src: &'a mut dyn ReadSeek,
    info: AiffInfo,
    next_frame: u64,
    buf: Vec<u8>,
    bytes: u64,
}

impl<'a> AiffReader<'a> {
    pub(crate) fn open(src: &'a mut dyn ReadSeek, info: AiffInfo) -> Result<Self, ReelError> {
        src.seek(SeekFrom::Start(info.data_offset))?;
        Ok(Self {
            src,
            info,
            next_frame: 0,
            buf: Vec::new(),
            bytes: 0,
        })
    }
}

impl FrameSource for AiffReader<'_> {
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        check_cancel(cancel)?;
        let left = self.info.frames.saturating_sub(self.next_frame);
        if left == 0 {
            return Ok(None);
        }
        let n = usize::try_from(left)
            .unwrap_or(BLOCK_FRAMES)
            .min(BLOCK_FRAMES);
        let width = self.info.encoding.bytes();
        let frame_bytes = width.saturating_mul(usize::from(self.info.channels));
        let bytes = n
            .checked_mul(frame_bytes)
            .ok_or(ReelError::LimitExceeded { what: "aiff-block" })?;
        self.buf.resize(bytes, 0);
        self.src.read_exact(&mut self.buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                ReelError::Truncated { delivered: 0 }
            } else {
                ReelError::Io(e.kind())
            }
        })?;
        self.bytes = self.bytes.saturating_add(bytes as u64);
        out.try_reserve(n.saturating_mul(usize::from(self.info.channels)))
            .map_err(|_| ReelError::LimitExceeded {
                what: "audio-alloc",
            })?;
        for s in self.buf.chunks_exact(width) {
            out.push(match (self.info.encoding, s) {
                (AiffEncoding::IntBe(bits), _) => {
                    let mut v: i64 = 0;
                    for b in s {
                        v = (v << 8) | i64::from(*b);
                    }
                    let total_bits = (s.len() as u32).saturating_mul(8);
                    // Sign-extend the container word, then scale by the declared sample size.
                    let shift = 64u32.saturating_sub(total_bits);
                    let v = (v << shift) >> shift;
                    let scale =
                        2f64.powi(i32::try_from(total_bits).unwrap_or(32).saturating_sub(1));
                    let _ = bits;
                    (v as f64 / scale) as f32
                }
                (AiffEncoding::Int16Le, [a, b]) => {
                    f32::from(i16::from_le_bytes([*a, *b])) / 32768.0
                }
                (AiffEncoding::U8, [a]) => (f32::from(*a) - 128.0) / 128.0,
                (AiffEncoding::FloatBe32, [a, b, c, d]) => f32::from_be_bytes([*a, *b, *c, *d]),
                (AiffEncoding::FloatBe64, s) => {
                    let mut x = [0u8; 8];
                    x.copy_from_slice(s);
                    f64::from_be_bytes(x) as f32
                }
                (AiffEncoding::ULaw, [a]) => ulaw(*a),
                (AiffEncoding::ALaw, [a]) => alaw(*a),
                _ => 0.0,
            });
        }
        self.next_frame = self.next_frame.saturating_add(n as u64);
        Ok(Some(n))
    }

    fn bytes_read(&self) -> u64 {
        self.bytes
    }
}

/// Write an AIFF (integer PCM) or AIFF-C `fl32` / `fl64` (float) stream. Source chunks carry interleaved
/// samples in WAV byte layout (8-bit unsigned, 16/24/32-bit signed little-endian, 32/64-bit
/// little-endian float); they are written big-endian. The header declares `spec.total_units` frames.
pub(crate) fn encode(
    spec: &EncodeSpec,
    source: &mut dyn ChunkSource,
    out: &mut dyn Write,
    limits: &Limits,
    cancel: &CancellationToken,
    codec: &str,
) -> Result<EncodeReceipt, ReelError> {
    let EncodeParams::Audio {
        rate,
        channels,
        bits,
        float,
    } = spec.params
    else {
        return Err(ReelError::Unsupported {
            what: "aiff-video-params",
        });
    };
    let ok = if float {
        matches!(bits, 32 | 64)
    } else {
        matches!(bits, 8 | 16 | 24 | 32)
    };
    if !ok || channels == 0 || channels > AIFF_MAX_CHANNELS || rate == 0 || rate > MAX_SAMPLE_RATE {
        return Err(ReelError::Unsupported {
            what: "aiff-encode-format",
        });
    }
    let width = usize::from(bits / 8);
    let frame = usize::from(channels).saturating_mul(width);
    let frames = u32::try_from(spec.total_units).map_err(|_| ReelError::LimitExceeded {
        what: "aiff-frames",
    })?;
    let data = u64::from(frames).saturating_mul(frame as u64);
    let comm_len: u32 = if float { 18 + 4 + 2 } else { 18 };
    let ssnd_len = data
        .checked_add(8)
        .filter(|v| *v <= u64::from(u32::MAX - 64))
        .ok_or(ReelError::LimitExceeded {
            what: "aiff-data-size",
        })?;
    let pad = ssnd_len & 1;
    let form_len = 4u64
        .saturating_add(8)
        .saturating_add(u64::from(comm_len))
        .saturating_add(if float { 12 } else { 0 })
        .saturating_add(8)
        .saturating_add(ssnd_len)
        .saturating_add(pad);
    let mut h = Vec::with_capacity(80);
    h.extend_from_slice(b"FORM");
    h.extend_from_slice(&(form_len as u32).to_be_bytes());
    h.extend_from_slice(if float { b"AIFC" } else { b"AIFF" });
    if float {
        // FVER: AIFF-C version 1 timestamp.
        h.extend_from_slice(b"FVER");
        h.extend_from_slice(&4u32.to_be_bytes());
        h.extend_from_slice(&0xA280_5140u32.to_be_bytes());
    }
    h.extend_from_slice(b"COMM");
    h.extend_from_slice(&comm_len.to_be_bytes());
    h.extend_from_slice(&channels.to_be_bytes());
    h.extend_from_slice(&frames.to_be_bytes());
    h.extend_from_slice(&bits.to_be_bytes());
    h.extend_from_slice(&rate_extended(rate));
    if float {
        h.extend_from_slice(if bits == 32 { b"fl32" } else { b"fl64" });
        h.extend_from_slice(&[0, 0]); // empty pstring + pad
    }
    h.extend_from_slice(b"SSND");
    h.extend_from_slice(&(ssnd_len as u32).to_be_bytes());
    h.extend_from_slice(&[0; 8]); // offset, block size
    out.write_all(&h)?;
    let mut bytes_written = h.len() as u64;
    let (mut buf, mut be) = (Vec::new(), Vec::new());
    let (mut written, mut canceled) = (0u64, false);
    loop {
        if check_cancel(cancel).is_err() {
            canceled = true;
            break;
        }
        buf.clear();
        let Some(units) = source.next_chunk(&mut buf)? else {
            break;
        };
        if usize::try_from(units)
            .ok()
            .and_then(|u| u.checked_mul(frame))
            != Some(buf.len())
        {
            return Err(ReelError::Corrupt {
                what: "chunk-size-mismatch",
            });
        }
        if buf.len() > limits.max_chunk_bytes {
            return Err(ReelError::LimitExceeded {
                what: "chunk-bytes",
            });
        }
        written = written
            .checked_add(units)
            .filter(|t| *t <= spec.total_units)
            .ok_or(ReelError::LimitExceeded {
                what: "source-exceeds-declared",
            })?;
        be.clear();
        for s in buf.chunks_exact(width) {
            if width == 1 {
                // WAV 8-bit unsigned -> AIFF 8-bit signed.
                be.push(s[0] ^ 0x80);
            } else {
                be.extend(s.iter().rev());
            }
        }
        out.write_all(&be)?;
        bytes_written = bytes_written.saturating_add(be.len() as u64);
    }
    let complete = !canceled && written == spec.total_units;
    if complete && pad == 1 {
        out.write_all(&[0])?;
        bytes_written = bytes_written.saturating_add(1);
    }
    Ok(EncodeReceipt {
        codec: codec.into(),
        complete,
        units_written: written,
        units_declared: spec.total_units,
        bytes_written,
        canceled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_rates_round_trip() {
        for rate in [8_000u32, 11_025, 44_100, 48_000, 96_000, 192_000, 768_000] {
            assert_eq!(extended_rate(&rate_extended(rate)).unwrap(), rate);
        }
        // 44100 as written by Apple tools.
        assert_eq!(
            extended_rate(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]).unwrap(),
            44_100
        );
        // 1e9 Hz and a fractional rate are refused.
        assert!(extended_rate(&rate_extended(1_000_000_000)).is_err());
        assert!(extended_rate(&[0x40, 0x0E, 0xAC, 0x44, 0x80, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn g711_endpoints() {
        assert_eq!(ulaw(0xFF), 0.0);
        assert_eq!(alaw(0xD5), 8.0 / 32768.0);
        assert!((ulaw(0x00) + 32124.0 / 32768.0).abs() < 1e-6);
    }
}
