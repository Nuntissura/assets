//! FLAC decoder and encoder written from RFC 9639 (Free Lossless Audio Codec, December 2024), Studio-owned.
//!
//! Decoder: STREAMINFO (section 8.2), frame header with CRC-8 (9.1), CONSTANT / VERBATIM / FIXED / LPC
//! subframes with wasted bits (9.2), partitioned Rice residual coding with escapes (9.2.7), inter-channel
//! decorrelation (4.2), frame footer CRC-16 (9.3) and the MD5 of the unencoded audio (8.2). Every size a
//! header declares is checked against `FlacLimits` before anything is allocated for it.
//!
//! Encoder: fixed block size, per channel the cheapest of CONSTANT, VERBATIM and FIXED orders 0..=4 with
//! a partitioned Rice search; stereo picks the cheapest of independent, left/side, side/right and
//! mid/side. Output is a valid stream that any RFC 9639 decoder reproduces bit-exactly.
#![deny(clippy::arithmetic_side_effects)]
use super::bits::{BitWriter, Bits};
use super::md5::Md5;
use crate::error::ReelError;

/// Hard bounds a FLAC header must respect before the decoder allocates for it (V4-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlacLimits {
    pub max_channels: u8,
    pub max_sample_rate: u32,
    pub max_block_size: u32,
}

impl Default for FlacLimits {
    fn default() -> Self {
        Self {
            max_channels: 8,
            max_sample_rate: super::MAX_SAMPLE_RATE,
            max_block_size: 65_535,
        }
    }
}

/// STREAMINFO metadata block (RFC 9639 section 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    /// 0 = unknown.
    pub total_samples: u64,
    /// All zero = unknown.
    pub md5: [u8; 16],
}

const STREAMINFO_LEN: usize = 34;
const CORRUPT_STREAMINFO: ReelError = ReelError::Corrupt {
    what: "flac-streaminfo",
};

impl StreamInfo {
    /// Parse the 34-byte STREAMINFO body and check it against `limits`.
    pub fn parse(body: &[u8], limits: &FlacLimits) -> Result<Self, ReelError> {
        if body.len() < STREAMINFO_LEN {
            return Err(CORRUPT_STREAMINFO);
        }
        let mut b = Bits::new(body);
        let mut field = |n: u32| b.read(n).ok_or(CORRUPT_STREAMINFO);
        let min_block_size = field(16)? as u16;
        let max_block_size = field(16)? as u16;
        let min_frame_size = field(24)?;
        let max_frame_size = field(24)?;
        let sample_rate = field(20)?;
        let channels = (field(3)? as u8).saturating_add(1);
        let bits_per_sample = (field(5)? as u8).saturating_add(1);
        let total_hi = u64::from(field(4)?);
        let total_lo = u64::from(field(32)?);
        let mut md5 = [0u8; 16];
        for byte in &mut md5 {
            *byte = field(8)? as u8;
        }
        let info = Self {
            min_block_size,
            max_block_size,
            min_frame_size,
            max_frame_size,
            sample_rate,
            channels,
            bits_per_sample,
            total_samples: (total_hi << 32) | total_lo,
            md5,
        };
        info.check(limits)?;
        Ok(info)
    }

    fn check(&self, limits: &FlacLimits) -> Result<(), ReelError> {
        if self.max_block_size < 16 || self.min_block_size > self.max_block_size {
            return Err(ReelError::Corrupt {
                what: "flac-block-size",
            });
        }
        if u32::from(self.max_block_size) > limits.max_block_size {
            return Err(ReelError::LimitExceeded {
                what: "flac-block-size",
            });
        }
        if self.sample_rate == 0 {
            return Err(ReelError::Corrupt {
                what: "flac-sample-rate",
            });
        }
        if self.sample_rate > limits.max_sample_rate {
            return Err(ReelError::LimitExceeded {
                what: "flac-sample-rate",
            });
        }
        if self.channels > limits.max_channels {
            return Err(ReelError::LimitExceeded {
                what: "flac-channels",
            });
        }
        if self.bits_per_sample < 4 {
            return Err(ReelError::Corrupt {
                what: "flac-bits-per-sample",
            });
        }
        Ok(())
    }

    /// The 34-byte body.
    pub fn to_bytes(&self) -> [u8; STREAMINFO_LEN] {
        let mut w = BitWriter::with_capacity(STREAMINFO_LEN);
        w.write(16, u32::from(self.min_block_size));
        w.write(16, u32::from(self.max_block_size));
        w.write(24, self.min_frame_size);
        w.write(24, self.max_frame_size);
        w.write(20, self.sample_rate);
        w.write(3, u32::from(self.channels.saturating_sub(1)));
        w.write(5, u32::from(self.bits_per_sample.saturating_sub(1)));
        w.write(4, (self.total_samples >> 32) as u32);
        w.write(32, self.total_samples as u32);
        for byte in self.md5 {
            w.write(8, u32::from(byte));
        }
        let mut out = [0u8; STREAMINFO_LEN];
        let bytes = w.into_bytes();
        let n = bytes.len().min(STREAMINFO_LEN);
        if let (Some(dst), Some(src)) = (out.get_mut(..n), bytes.get(..n)) {
            dst.copy_from_slice(src);
        }
        out
    }

    /// Worst-case byte size of one frame of this stream (verbatim subframes plus headers): the bound the
    /// native reader's look-ahead window needs.
    pub fn worst_frame_bytes(&self) -> Result<usize, ReelError> {
        let bits = u64::from(self.max_block_size)
            .checked_mul(u64::from(self.channels))
            .and_then(|s| s.checked_mul(u64::from(self.bits_per_sample).checked_add(1)?))
            .and_then(|b| b.checked_add(8 * 64))
            .ok_or(ReelError::LimitExceeded {
                what: "flac-frame-size",
            })?;
        usize::try_from(bits.div_ceil(8)).map_err(|_| ReelError::LimitExceeded {
            what: "flac-frame-size",
        })
    }
}

// ----- CRCs (RFC 9639 sections 9.1.8 and 9.3) -------------------------------------------------------

/// CRC-8, polynomial x^8 + x^2 + x + 1, initial 0.
pub(crate) fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16, polynomial x^16 + x^15 + x^2 + 1, initial 0.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ----- decoder ----------------------------------------------------------------------------------------

/// Outcome of trying to decode one frame from the front of a byte window.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FrameOutcome {
    /// A frame of `frames` samples per channel was decoded from the first `consumed` bytes.
    Frame { consumed: usize, frames: usize },
    /// The window ends inside the frame: more bytes are needed (or the stream is truncated).
    NeedMore,
}

const CORRUPT_FRAME: ReelError = ReelError::Corrupt { what: "flac-frame" };

/// Channel layout of a frame (RFC 9639 section 9.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assignment {
    Independent(u8),
    LeftSide,
    SideRight,
    MidSide,
}

struct FrameHeader {
    block_size: u32,
    sample_rate: u32,
    assignment: Assignment,
    bits_per_sample: u32,
    header_bytes: usize,
}

pub struct FlacDecoder {
    info: StreamInfo,
    limits: FlacLimits,
    /// Per-channel decoded samples of the last frame.
    channels: Vec<Vec<i64>>,
    residual_scratch: Vec<i64>,
    md5: Md5,
    md5_bytes: Vec<u8>,
}

/// Distinguishes "the window ended" from a real decoding error inside the bit reader.
enum Short {
    End,
    Bad(ReelError),
}

impl From<ReelError> for Short {
    fn from(e: ReelError) -> Self {
        Self::Bad(e)
    }
}

type R<T> = Result<T, Short>;

fn need<T>(v: Option<T>) -> R<T> {
    v.ok_or(Short::End)
}

impl FlacDecoder {
    pub fn new(info: StreamInfo, limits: FlacLimits) -> Result<Self, ReelError> {
        info.check(&limits)?;
        let block = usize::from(info.max_block_size);
        let mut channels = Vec::new();
        channels
            .try_reserve_exact(usize::from(info.channels))
            .map_err(|_| ReelError::LimitExceeded { what: "flac-alloc" })?;
        for _ in 0..info.channels {
            channels.push(vec![0i64; block]);
        }
        Ok(Self {
            info,
            limits,
            channels,
            residual_scratch: Vec::new(),
            md5: Md5::default(),
            md5_bytes: Vec::new(),
        })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Decode the frame at the start of `window`, appending `frames * channels` interleaved samples
    /// scaled to [-1, 1) to `out`. A frame that does not fit the window is `NeedMore`, not an error,
    /// unless `at_eof` (then it is `Truncated`).
    pub(crate) fn decode_frame(
        &mut self,
        window: &[u8],
        at_eof: bool,
        out: &mut Vec<f32>,
        ints: Option<&mut Vec<i32>>,
    ) -> Result<FrameOutcome, ReelError> {
        match self.decode_frame_inner(window, out, ints) {
            Ok(outcome) => Ok(outcome),
            Err(Short::End) if at_eof => Err(ReelError::Truncated { delivered: 0 }),
            Err(Short::End) => Ok(FrameOutcome::NeedMore),
            Err(Short::Bad(e)) => Err(e),
        }
    }

    fn parse_header(&self, window: &[u8]) -> R<FrameHeader> {
        let mut b = Bits::new(window);
        let sync = need(b.read(15))?;
        if sync != 0x7FFC {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-frame-sync",
            }));
        }
        let _variable = need(b.bit())?;
        let bs_code = need(b.read(4))?;
        let sr_code = need(b.read(4))?;
        let ch_code = need(b.read(4))?;
        let ss_code = need(b.read(3))?;
        if need(b.bit())? {
            return Err(Short::Bad(CORRUPT_FRAME));
        }
        // Coded frame/sample number: UTF-8-like, up to 7 bytes (RFC 9639 section 9.1.5).
        let first = need(b.read(8))?;
        let extra = match first {
            0x00..=0x7F => 0,
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            0xF8..=0xFB => 4,
            0xFC..=0xFD => 5,
            0xFE => 6,
            _ => return Err(Short::Bad(CORRUPT_FRAME)),
        };
        for _ in 0..extra {
            if need(b.read(8))? & 0xC0 != 0x80 {
                return Err(Short::Bad(CORRUPT_FRAME));
            }
        }
        let block_size = match bs_code {
            0 => return Err(Short::Bad(CORRUPT_FRAME)),
            1 => 192,
            2..=5 => 576u32 << (bs_code.saturating_sub(2)),
            6 => need(b.read(8))?.saturating_add(1),
            7 => need(b.read(16))?.saturating_add(1),
            _ => 256u32 << (bs_code.saturating_sub(8)),
        };
        let sample_rate = match sr_code {
            0 => self.info.sample_rate,
            1 => 88_200,
            2 => 176_400,
            3 => 192_000,
            4 => 8_000,
            5 => 16_000,
            6 => 22_050,
            7 => 24_000,
            8 => 32_000,
            9 => 44_100,
            10 => 48_000,
            11 => 96_000,
            12 => need(b.read(8))?.saturating_mul(1000),
            13 => need(b.read(16))?,
            14 => need(b.read(16))?.saturating_mul(10),
            _ => return Err(Short::Bad(CORRUPT_FRAME)),
        };
        let assignment = match ch_code {
            0..=7 => Assignment::Independent((ch_code as u8).saturating_add(1)),
            8 => Assignment::LeftSide,
            9 => Assignment::SideRight,
            10 => Assignment::MidSide,
            _ => return Err(Short::Bad(CORRUPT_FRAME)),
        };
        let bits_per_sample = match ss_code {
            0 => u32::from(self.info.bits_per_sample),
            1 => 8,
            2 => 12,
            4 => 16,
            5 => 20,
            6 => 24,
            7 => 32,
            _ => return Err(Short::Bad(CORRUPT_FRAME)),
        };
        let header_len = b.bytes_consumed();
        let crc = need(b.read(8))? as u8;
        let header = need(window.get(..header_len))?;
        if crc8(header) != crc {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-header-crc",
            }));
        }
        // The frame must agree with the stream: a header may not grow the allocation STREAMINFO bounded.
        if block_size > u32::from(self.info.max_block_size)
            || block_size > self.limits.max_block_size
        {
            return Err(Short::Bad(ReelError::LimitExceeded {
                what: "flac-block-size",
            }));
        }
        if sample_rate != self.info.sample_rate {
            return Err(Short::Bad(ReelError::Unsupported {
                what: "flac-sample-rate-change",
            }));
        }
        let channels = match assignment {
            Assignment::Independent(n) => n,
            _ => 2,
        };
        if channels != self.info.channels {
            return Err(Short::Bad(ReelError::Unsupported {
                what: "flac-channel-change",
            }));
        }
        if bits_per_sample != u32::from(self.info.bits_per_sample) {
            return Err(Short::Bad(ReelError::Unsupported {
                what: "flac-bit-depth-change",
            }));
        }
        Ok(FrameHeader {
            block_size,
            sample_rate,
            assignment,
            bits_per_sample,
            header_bytes: header_len.saturating_add(1),
        })
    }

    fn decode_frame_inner(
        &mut self,
        window: &[u8],
        out: &mut Vec<f32>,
        ints: Option<&mut Vec<i32>>,
    ) -> R<FrameOutcome> {
        let header = self.parse_header(window)?;
        let _ = header.sample_rate;
        let block = header.block_size as usize;
        let body = need(window.get(header.header_bytes..))?;
        let mut b = Bits::new(body);
        let nch = usize::from(self.info.channels);
        for ch in 0..nch {
            let side = matches!(
                (header.assignment, ch),
                (Assignment::LeftSide, 1) | (Assignment::SideRight, 0) | (Assignment::MidSide, 1)
            );
            let bps = header.bits_per_sample.saturating_add(u32::from(side));
            let mut samples = std::mem::take(&mut self.channels[ch]);
            let result = self.decode_subframe(&mut b, block, bps, &mut samples);
            self.channels[ch] = samples;
            result?;
        }
        b.align();
        let crc_at = header.header_bytes.saturating_add(b.bytes_consumed());
        let mut tail = Bits::new(need(window.get(crc_at..))?);
        let crc = need(tail.read(16))? as u16;
        let frame = need(window.get(..crc_at))?;
        if crc16(frame) != crc {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-frame-crc",
            }));
        }
        self.decorrelate(header.assignment, block)?;
        self.emit(block, header.bits_per_sample, out, ints)?;
        Ok(FrameOutcome::Frame {
            consumed: crc_at.saturating_add(2),
            frames: block,
        })
    }

    fn decode_subframe(
        &mut self,
        b: &mut Bits<'_>,
        block: usize,
        bps: u32,
        samples: &mut [i64],
    ) -> R<()> {
        let samples = need(samples.get_mut(..block))?;
        if need(b.bit())? {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-subframe-padding",
            }));
        }
        let kind = need(b.read(6))?;
        let wasted = if need(b.bit())? {
            need(b.unary(bps))?.saturating_add(1)
        } else {
            0
        };
        if wasted >= bps {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-wasted-bits",
            }));
        }
        let bps = bps.saturating_sub(wasted);
        match kind {
            0 => {
                let v = read_signed_wide(b, bps)?;
                samples.fill(v);
            }
            1 => {
                for s in samples.iter_mut() {
                    *s = read_signed_wide(b, bps)?;
                }
            }
            8..=12 => {
                let order = (kind.saturating_sub(8)) as usize;
                if order > block {
                    return Err(Short::Bad(CORRUPT_FRAME));
                }
                for s in need(samples.get_mut(..order))?.iter_mut() {
                    *s = read_signed_wide(b, bps)?;
                }
                self.read_residual(b, block, order, samples)?;
                restore_fixed(samples, order)?;
            }
            32..=63 => {
                let order = (kind.saturating_sub(31)) as usize;
                if order > block {
                    return Err(Short::Bad(CORRUPT_FRAME));
                }
                for s in need(samples.get_mut(..order))?.iter_mut() {
                    *s = read_signed_wide(b, bps)?;
                }
                let precision = need(b.read(4))?;
                if precision == 15 {
                    return Err(Short::Bad(ReelError::Corrupt {
                        what: "flac-lpc-precision",
                    }));
                }
                let precision = precision.saturating_add(1);
                let shift = need(b.read_signed(5))?;
                if shift < 0 {
                    return Err(Short::Bad(ReelError::Corrupt {
                        what: "flac-lpc-shift",
                    }));
                }
                let mut coefs = [0i64; 32];
                for c in need(coefs.get_mut(..order))?.iter_mut() {
                    *c = i64::from(need(b.read_signed(precision))?);
                }
                self.read_residual(b, block, order, samples)?;
                restore_lpc(samples, need(coefs.get(..order))?, shift as u32)?;
            }
            _ => {
                return Err(Short::Bad(ReelError::Corrupt {
                    what: "flac-subframe-type",
                }));
            }
        }
        if wasted > 0 {
            for s in samples.iter_mut() {
                *s = s.checked_shl(wasted).ok_or(Short::Bad(CORRUPT_FRAME))?;
            }
        }
        Ok(())
    }

    /// Partitioned Rice residual (RFC 9639 section 9.2.7) into `samples[order..block]`.
    fn read_residual(
        &mut self,
        b: &mut Bits<'_>,
        block: usize,
        order: usize,
        samples: &mut [i64],
    ) -> R<()> {
        let method = need(b.read(2))?;
        let (param_bits, escape) = match method {
            0 => (4, 15),
            1 => (5, 31),
            _ => {
                return Err(Short::Bad(ReelError::Corrupt {
                    what: "flac-residual-method",
                }));
            }
        };
        let partition_order = need(b.read(4))?;
        let partitions = 1usize << partition_order;
        if !block.is_multiple_of(partitions) {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-partition-order",
            }));
        }
        let per = block.checked_div(partitions).unwrap_or(0);
        if per < order {
            return Err(Short::Bad(ReelError::Corrupt {
                what: "flac-partition-order",
            }));
        }
        let mut at = order;
        for p in 0..partitions {
            let count = if p == 0 {
                per.saturating_sub(order)
            } else {
                per
            };
            let param = need(b.read(param_bits))?;
            let dst = need(samples.get_mut(at..at.saturating_add(count)))?;
            if param == escape {
                let raw_bits = need(b.read(5))?;
                for s in dst.iter_mut() {
                    *s = i64::from(need(b.read_signed(raw_bits))?);
                }
            } else {
                for s in dst.iter_mut() {
                    // A quotient this long cannot belong to a residual of at most 32 bits.
                    let q = u64::from(need(b.unary(1 << 26))?);
                    let r = u64::from(need(b.read(param))?);
                    let u = q
                        .checked_shl(param)
                        .filter(|v| v >> param == q)
                        .map(|v| v | r)
                        .ok_or(Short::Bad(CORRUPT_FRAME))?;
                    let magnitude = i64::try_from(u >> 1).map_err(|_| Short::Bad(CORRUPT_FRAME))?;
                    *s = if u & 1 == 1 { !magnitude } else { magnitude };
                }
            }
            at = at.saturating_add(count);
        }
        let _ = &self.residual_scratch;
        Ok(())
    }

    fn decorrelate(&mut self, assignment: Assignment, block: usize) -> R<()> {
        if matches!(assignment, Assignment::Independent(_)) {
            return Ok(());
        }
        let (first, rest) = self.channels.split_at_mut(1);
        let a = need(first.first_mut().and_then(|c| c.get_mut(..block)))?;
        let s = need(rest.first_mut().and_then(|c| c.get_mut(..block)))?;
        let bad = || Short::Bad(CORRUPT_FRAME);
        for (x, y) in a.iter_mut().zip(s.iter_mut()) {
            match assignment {
                // a = left, y = side: right = left - side.
                Assignment::LeftSide => *y = x.checked_sub(*y).ok_or_else(bad)?,
                // x = side, y = right: left = side + right.
                Assignment::SideRight => *x = x.checked_add(*y).ok_or_else(bad)?,
                Assignment::MidSide => {
                    let mid = x.checked_shl(1).map(|m| m | (*y & 1)).ok_or_else(bad)?;
                    let left = mid.checked_add(*y).ok_or_else(bad)? >> 1;
                    let right = mid.checked_sub(*y).ok_or_else(bad)? >> 1;
                    *x = left;
                    *y = right;
                }
                Assignment::Independent(_) => {}
            }
        }
        Ok(())
    }

    fn emit(
        &mut self,
        block: usize,
        bps: u32,
        out: &mut Vec<f32>,
        mut ints: Option<&mut Vec<i32>>,
    ) -> R<()> {
        let nch = self.channels.len();
        let total = block
            .checked_mul(nch)
            .ok_or(Short::Bad(ReelError::LimitExceeded {
                what: "flac-frame-size",
            }))?;
        out.try_reserve(total)
            .map_err(|_| Short::Bad(ReelError::LimitExceeded { what: "flac-alloc" }))?;
        let scale = 1.0f64 / (1u64 << bps.saturating_sub(1).min(62)) as f64;
        let bytes_per_sample = bps.div_ceil(8) as usize;
        self.md5_bytes.clear();
        let hi = (1i64 << bps.saturating_sub(1).min(62)).saturating_sub(1);
        let lo = (!hi).min(0);
        for i in 0..block {
            for ch in 0..nch {
                let v = need(self.channels.get(ch).and_then(|c| c.get(i)).copied())?;
                if v < lo || v > hi {
                    return Err(Short::Bad(ReelError::Corrupt {
                        what: "flac-sample-range",
                    }));
                }
                out.push((v as f64 * scale) as f32);
                if let Some(ints) = ints.as_deref_mut() {
                    ints.push(v as i32);
                }
                let le = v.to_le_bytes();
                self.md5_bytes
                    .extend_from_slice(need(le.get(..bytes_per_sample))?);
            }
        }
        self.md5.update(&self.md5_bytes);
        Ok(())
    }

    /// MD5 of every sample decoded so far (meaningful after a whole-stream decode from the first frame).
    pub(crate) fn md5_matches(self) -> Option<bool> {
        let declared = self.info.md5;
        if declared == [0u8; 16] {
            return None;
        }
        Some(self.md5.finish() == declared)
    }
}

fn read_signed_wide(b: &mut Bits<'_>, bits: u32) -> R<i64> {
    if bits == 0 {
        return Ok(0);
    }
    if bits <= 32 {
        return Ok(i64::from(need(b.read_signed(bits))?));
    }
    // 33-bit side channel of a 32-bit stream.
    let hi = i64::from(need(b.read_signed(bits.saturating_sub(32)))?);
    let lo = i64::from(need(b.read(32))?);
    Ok((hi << 32) | lo)
}

fn restore_fixed(s: &mut [i64], order: usize) -> R<()> {
    let bad = || Short::Bad(CORRUPT_FRAME);
    for i in order..s.len() {
        let p = |k: usize| -> R<i64> { need(i.checked_sub(k).and_then(|j| s.get(j)).copied()) };
        let prediction: i64 = match order {
            0 => 0,
            1 => p(1)?,
            2 => p(1)?
                .checked_mul(2)
                .and_then(|v| v.checked_sub(p(2).ok()?))
                .ok_or_else(bad)?,
            3 => p(1)?
                .checked_sub(p(2)?)
                .and_then(|d| d.checked_mul(3))
                .and_then(|v| v.checked_add(p(3).ok()?))
                .ok_or_else(bad)?,
            _ => p(1)?
                .checked_mul(4)
                .and_then(|v| v.checked_sub(p(2).ok()?.checked_mul(6)?))
                .and_then(|v| v.checked_add(p(3).ok()?.checked_mul(4)?))
                .and_then(|v| v.checked_sub(p(4).ok()?))
                .ok_or_else(bad)?,
        };
        let cell = need(s.get_mut(i))?;
        *cell = cell.checked_add(prediction).ok_or_else(bad)?;
    }
    Ok(())
}

fn restore_lpc(s: &mut [i64], coefs: &[i64], shift: u32) -> R<()> {
    let bad = || Short::Bad(CORRUPT_FRAME);
    let order = coefs.len();
    for i in order..s.len() {
        let mut sum: i64 = 0;
        for (j, c) in coefs.iter().enumerate() {
            let prev = need(
                i.checked_sub(j.saturating_add(1))
                    .and_then(|k| s.get(k))
                    .copied(),
            )?;
            sum = c
                .checked_mul(prev)
                .and_then(|v| sum.checked_add(v))
                .ok_or_else(bad)?;
        }
        let cell = need(s.get_mut(i))?;
        *cell = cell.checked_add(sum >> shift).ok_or_else(bad)?;
    }
    Ok(())
}

// ----- native stream reading --------------------------------------------------------------------------

/// Metadata of a native FLAC stream ("fLaC" + metadata blocks): STREAMINFO and where the frames start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHeader {
    pub info: StreamInfo,
    /// Byte offset of the first frame.
    pub frames_offset: u64,
}

/// Parse the metadata blocks of a native FLAC stream from `head` (which must hold all of them). Blocks
/// other than STREAMINFO are skipped by their declared length; the total is bounded by `max_bytes`.
type ReadAt<'a> = dyn FnMut(u64, &mut [u8]) -> Result<(), ReelError> + 'a;

pub(crate) fn parse_native_header(
    read_at: &mut ReadAt<'_>,
    max_bytes: u64,
    limits: &FlacLimits,
) -> Result<NativeHeader, ReelError> {
    let mut magic = [0u8; 4];
    read_at(0, &mut magic)?;
    if &magic != b"fLaC" {
        return Err(ReelError::ImportUnsupported {
            detected: "not-flac".into(),
        });
    }
    let mut at: u64 = 4;
    let mut info = None;
    for index in 0..1024u32 {
        let mut hdr = [0u8; 4];
        read_at(at, &mut hdr)?;
        let last = hdr[0] & 0x80 != 0;
        let kind = hdr[0] & 0x7F;
        let len = u32::from_be_bytes([0, hdr[1], hdr[2], hdr[3]]);
        let body_at = at.checked_add(4).ok_or(ReelError::LimitExceeded {
            what: "flac-metadata",
        })?;
        if index == 0 && kind != 0 {
            return Err(ReelError::Corrupt {
                what: "flac-first-block-not-streaminfo",
            });
        }
        if kind == 0 {
            if len as usize != STREAMINFO_LEN || info.is_some() {
                return Err(CORRUPT_STREAMINFO);
            }
            let mut body = [0u8; STREAMINFO_LEN];
            read_at(body_at, &mut body)?;
            info = Some(StreamInfo::parse(&body, limits)?);
        } else if kind == 127 {
            return Err(ReelError::Corrupt {
                what: "flac-metadata-type",
            });
        }
        at = body_at
            .checked_add(u64::from(len))
            .filter(|end| *end <= max_bytes)
            .ok_or(ReelError::LimitExceeded {
                what: "flac-metadata",
            })?;
        if last {
            let info = info.ok_or(CORRUPT_STREAMINFO)?;
            return Ok(NativeHeader {
                info,
                frames_offset: at,
            });
        }
    }
    Err(ReelError::LimitExceeded {
        what: "flac-metadata-blocks",
    })
}

/// Configuration carried by a container for FLAC-in-ISOBMFF (`dfLa`, RFC 9639 Annex / "Encapsulation of
/// FLAC in ISO-BMFF" v0: version/flags then metadata blocks) and FLAC-in-Matroska / Ogg (CodecPrivate
/// = "fLaC" + metadata blocks). Returns STREAMINFO.
#[allow(dead_code)] // container hooks (FLAC in MP4 / Matroska / Ogg) use it from the next slice
pub(crate) fn streaminfo_from_config(
    config: &[u8],
    limits: &FlacLimits,
) -> Result<StreamInfo, ReelError> {
    let blocks = if config.starts_with(b"fLaC") {
        config.get(4..)
    } else if config.len() >= 4 && config.get(..4) == Some(&[0, 0, 0, 0]) {
        // dfLa FullBox version 0, flags 0.
        config.get(4..)
    } else {
        Some(config)
    }
    .ok_or(CORRUPT_STREAMINFO)?;
    let hdr = blocks.get(..4).ok_or(CORRUPT_STREAMINFO)?;
    if hdr.first().is_some_and(|b| b & 0x7F != 0) {
        return Err(ReelError::Corrupt {
            what: "flac-first-block-not-streaminfo",
        });
    }
    let body = blocks
        .get(4..4 + STREAMINFO_LEN)
        .ok_or(CORRUPT_STREAMINFO)?;
    StreamInfo::parse(body, limits)
}

// ----- encoder ----------------------------------------------------------------------------------------

/// Block size the encoder writes (RFC 9639 block size code 12 = 4096).
pub const ENCODER_BLOCK_SIZE: usize = 4096;

/// FLAC encoder over interleaved integer samples.
pub struct FlacEncoder {
    info: StreamInfo,
    pending: Vec<i32>,
    frame_number: u64,
    md5: Md5,
    min_frame: u32,
    max_frame: u32,
    total: u64,
}

#[allow(clippy::arithmetic_side_effects)]
impl FlacEncoder {
    /// `bits_per_sample` 4..=24 (the range every deployed decoder supports), 1..=8 channels.
    pub fn new(sample_rate: u32, channels: u8, bits_per_sample: u8) -> Result<Self, ReelError> {
        if !(1..=8).contains(&channels)
            || !(4..=24).contains(&bits_per_sample)
            || sample_rate == 0
            || sample_rate > super::MAX_SAMPLE_RATE
            || sample_rate >= 1 << 20
        {
            return Err(ReelError::Unsupported {
                what: "flac-encode-format",
            });
        }
        Ok(Self {
            info: StreamInfo {
                min_block_size: ENCODER_BLOCK_SIZE as u16,
                max_block_size: ENCODER_BLOCK_SIZE as u16,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate,
                channels,
                bits_per_sample,
                total_samples: 0,
                md5: [0; 16],
            },
            pending: Vec::new(),
            frame_number: 0,
            md5: Md5::default(),
            min_frame: u32::MAX,
            max_frame: 0,
            total: 0,
        })
    }

    /// The stream header ("fLaC" + last STREAMINFO block) with `declared_total` samples, no MD5 and
    /// unknown frame sizes: the form a forward-only writer can emit before the audio.
    pub fn stream_header(&self, declared_total: u64) -> Vec<u8> {
        let mut info = self.info;
        info.total_samples = declared_total;
        header_bytes(&info)
    }

    /// Feed interleaved samples; whole blocks are encoded and appended to `out`.
    pub fn push(&mut self, interleaved: &[i32], out: &mut Vec<u8>) -> Result<(), ReelError> {
        let nch = usize::from(self.info.channels);
        if !interleaved.len().is_multiple_of(nch) {
            return Err(ReelError::Corrupt {
                what: "flac-encode-partial-frame",
            });
        }
        let lo = -(1i32 << (self.info.bits_per_sample - 1));
        let hi = (1i32 << (self.info.bits_per_sample - 1)) - 1;
        if interleaved.iter().any(|v| *v < lo || *v > hi) {
            return Err(ReelError::Corrupt {
                what: "flac-encode-sample-range",
            });
        }
        self.pending.extend_from_slice(interleaved);
        let block_values = ENCODER_BLOCK_SIZE * nch;
        let mut start = 0;
        while self.pending.len() - start >= block_values {
            let block = self.pending[start..start + block_values].to_vec();
            self.encode_block(&block, out)?;
            start += block_values;
        }
        self.pending.drain(..start);
        Ok(())
    }

    /// Encode the final short block; returns the final STREAMINFO (with MD5 and frame sizes).
    pub fn finish(mut self, out: &mut Vec<u8>) -> Result<StreamInfo, ReelError> {
        if !self.pending.is_empty() {
            let block = std::mem::take(&mut self.pending);
            self.encode_block(&block, out)?;
        }
        let mut info = self.info;
        info.total_samples = self.total;
        info.md5 = self.md5.finish();
        if self.max_frame > 0 {
            info.min_frame_size = self.min_frame;
            info.max_frame_size = self.max_frame;
        }
        Ok(info)
    }

    fn encode_block(&mut self, interleaved: &[i32], out: &mut Vec<u8>) -> Result<(), ReelError> {
        let nch = usize::from(self.info.channels);
        let n = interleaved.len() / nch;
        let bps = u32::from(self.info.bits_per_sample);
        let bytes = bps.div_ceil(8) as usize;
        for v in interleaved {
            let le = v.to_le_bytes();
            self.md5.update(&le[..bytes]);
        }
        let mut chans: Vec<Vec<i64>> = (0..nch)
            .map(|c| {
                (0..n)
                    .map(|i| i64::from(interleaved[i * nch + c]))
                    .collect()
            })
            .collect();
        let mut w = BitWriter::with_capacity(n * nch * bytes + 64);
        // Frame header (RFC 9639 section 9.1), fixed block size strategy.
        w.write(15, 0x7FFC);
        w.write(1, 0);
        let (bs_code, bs_extra) = if n == ENCODER_BLOCK_SIZE {
            (12u32, None)
        } else if n <= 256 {
            (6, Some((8u32, n as u32 - 1)))
        } else {
            (7, Some((16, n as u32 - 1)))
        };
        w.write(4, bs_code);
        w.write(4, 0); // sample rate from STREAMINFO
        // Channel assignment: try the stereo decorrelations.
        let (assignment, subframes) = if nch == 2 {
            let left = &chans[0];
            let right = &chans[1];
            let side: Vec<i64> = left.iter().zip(right).map(|(l, r)| l - r).collect();
            let mid: Vec<i64> = left.iter().zip(right).map(|(l, r)| (l + r) >> 1).collect();
            let l = encode_subframe(left, bps);
            let r = encode_subframe(right, bps);
            let s = encode_subframe(&side, bps + 1);
            let m = encode_subframe(&mid, bps);
            let options = [
                (0u32, l.bit_len() + r.bit_len()),
                (8, l.bit_len() + s.bit_len()),
                (9, s.bit_len() + r.bit_len()),
                (10, m.bit_len() + s.bit_len()),
            ];
            let best = options.iter().min_by_key(|o| o.1).map_or(0, |o| o.0);
            let pick = match best {
                8 => vec![l, s],
                9 => vec![s, r],
                10 => vec![m, s],
                _ => vec![l, r],
            };
            (if best == 0 { 1 } else { best }, pick)
        } else {
            let subs = chans.iter_mut().map(|c| encode_subframe(c, bps)).collect();
            ((nch - 1) as u32, subs)
        };
        w.write(4, assignment);
        let ss_code = match bps {
            8 => 1,
            12 => 2,
            16 => 4,
            20 => 5,
            24 => 6,
            _ => 0,
        };
        w.write(3, ss_code);
        w.write(1, 0);
        write_coded_number(&mut w, self.frame_number);
        if let Some((bits, value)) = bs_extra {
            w.write(bits, value);
        }
        let header = w.bytes().to_vec();
        w.write(8, u32::from(crc8(&header)));
        for sub in &subframes {
            append_bits(&mut w, sub);
        }
        w.align();
        let crc = crc16(w.bytes());
        w.write(16, u32::from(crc));
        let frame = w.into_bytes();
        let size = u32::try_from(frame.len()).unwrap_or(u32::MAX);
        self.min_frame = self.min_frame.min(size);
        self.max_frame = self.max_frame.max(size);
        out.extend_from_slice(&frame);
        self.frame_number += 1;
        self.total += n as u64;
        Ok(())
    }
}

/// "fLaC" + STREAMINFO as the last metadata block.
pub fn header_bytes(info: &StreamInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(42);
    out.extend_from_slice(b"fLaC");
    out.extend_from_slice(&[0x80, 0, 0, STREAMINFO_LEN as u8]);
    out.extend_from_slice(&info.to_bytes());
    out
}

/// Byte offset of the STREAMINFO body in a stream written by `header_bytes`.
pub const STREAMINFO_OFFSET: u64 = 8;

/// UTF-8-like coded frame number (RFC 9639 section 9.1.5): 1 byte below 2^7, else a lead byte with
/// `extra` leading ones and `extra` continuation bytes of 6 bits each (up to 36 bits).
#[allow(clippy::arithmetic_side_effects)]
fn write_coded_number(w: &mut BitWriter, n: u64) {
    if n < 0x80 {
        w.write(8, n as u32);
        return;
    }
    // extra continuation bytes: capacity 5 * extra + 6 bits for extra 1..=5, 36 bits for 6.
    let mut extra = 1u32;
    while extra < 6 && n >= 1u64 << (5 * extra + 6) {
        extra += 1;
    }
    let marker = (0xFF00u32 >> (extra + 1)) & 0xFF;
    let lead = (n >> (6 * extra)) as u32;
    w.write(8, marker | lead);
    for i in (0..extra).rev() {
        w.write(8, 0x80 | ((n >> (6 * i)) as u32 & 0x3F));
    }
}

#[allow(clippy::arithmetic_side_effects)]
fn append_bits(w: &mut BitWriter, sub: &BitWriter) {
    let full = sub.bytes();
    for &byte in full {
        w.write(8, u32::from(byte));
    }
    let extra = (sub.bit_len() - full.len() as u64 * 8) as u32;
    if extra > 0 {
        w.write(extra, sub.pending_bits());
    }
}

/// Cheapest subframe for one channel's samples at `bps` bits.
#[allow(clippy::arithmetic_side_effects)]
fn encode_subframe(samples: &[i64], bps: u32) -> BitWriter {
    let n = samples.len();
    if samples.iter().all(|s| *s == samples[0]) {
        let mut w = BitWriter::default();
        w.write(1, 0);
        w.write(6, 0);
        w.write(1, 0);
        write_wide(&mut w, bps, samples[0]);
        return w;
    }
    let mut best: Option<BitWriter> = None;
    let mut residual = vec![0i64; n];
    for order in 0..=4usize.min(n) {
        for i in order..n {
            let s = |k: usize| samples[i - k];
            residual[i] = samples[i]
                - match order {
                    0 => 0,
                    1 => s(1),
                    2 => 2 * s(1) - s(2),
                    3 => 3 * s(1) - 3 * s(2) + s(3),
                    _ => 4 * s(1) - 6 * s(2) + 4 * s(3) - s(4),
                };
        }
        // Residuals must fit 32 bits signed for the Rice coder (RFC 9639 section 9.2.7.2).
        if residual[order..]
            .iter()
            .any(|r| *r > i64::from(i32::MAX) || *r < i64::from(i32::MIN) + 1)
        {
            continue;
        }
        let mut w = BitWriter::default();
        w.write(1, 0);
        w.write(6, 8 + order as u32);
        w.write(1, 0);
        for &s in &samples[..order] {
            write_wide(&mut w, bps, s);
        }
        write_residual(&mut w, &residual[order..], n, order);
        if best.as_ref().is_none_or(|b| w.bit_len() < b.bit_len()) {
            best = Some(w);
        }
    }
    let mut verbatim = BitWriter::default();
    verbatim.write(1, 0);
    verbatim.write(6, 1);
    verbatim.write(1, 0);
    for &s in samples {
        write_wide(&mut verbatim, bps, s);
    }
    match best {
        Some(b) if b.bit_len() < verbatim.bit_len() => b,
        _ => verbatim,
    }
}

#[allow(clippy::arithmetic_side_effects)]
fn write_wide(w: &mut BitWriter, bps: u32, v: i64) {
    if bps <= 32 {
        w.write_signed(bps, v as i32);
    } else {
        w.write(bps - 32, (v >> 32) as u32);
        w.write(32, v as u32);
    }
}

/// Partitioned Rice coding with the best partition order (0..=8) and per-partition parameter.
#[allow(clippy::arithmetic_side_effects)]
fn write_residual(w: &mut BitWriter, residual: &[i64], block: usize, order: usize) {
    let zig = |r: i64| -> u64 { ((r << 1) ^ (r >> 63)) as u64 };
    let mut best: Option<(u64, u32, Vec<u32>)> = None;
    for porder in 0..=8u32 {
        let parts = 1usize << porder;
        if !block.is_multiple_of(parts) || block / parts < order {
            break;
        }
        let per = block / parts;
        let mut total = 0u64;
        let mut params = Vec::with_capacity(parts);
        let mut at = 0usize;
        for p in 0..parts {
            let count = if p == 0 { per - order } else { per };
            let slice = &residual[at..at + count];
            at += count;
            let sum: u64 = slice.iter().map(|r| zig(*r)).sum();
            let mean = if count == 0 { 0 } else { sum / count as u64 };
            let mut k = 0u32;
            while k < 30 && (1u64 << (k + 1)) <= mean.max(1) {
                k += 1;
            }
            let mut best_k = (u64::MAX, 0u32);
            for cand in k.saturating_sub(1)..=(k + 1).min(30) {
                let bits: u64 = slice
                    .iter()
                    .map(|r| (zig(*r) >> cand) + 1 + u64::from(cand))
                    .sum();
                if bits < best_k.0 {
                    best_k = (bits, cand);
                }
            }
            total += best_k.0 + 5;
            params.push(best_k.1);
        }
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((total, porder, params));
        }
    }
    let (_, porder, params) = best.unwrap_or((0, 0, vec![0]));
    w.write(2, 1); // 5-bit Rice parameters
    w.write(4, porder);
    let parts = 1usize << porder;
    let per = block / parts;
    let mut at = 0usize;
    for (p, k) in params.iter().enumerate() {
        let count = if p == 0 { per - order } else { per };
        w.write(5, *k);
        for &r in &residual[at..at + count] {
            let u = zig(r);
            let q = u >> k;
            w.write_unary(q as u32);
            w.write(*k, (u & ((1u64 << k) - 1)) as u32);
        }
        at += count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_values() {
        // CRC-8 (poly 0x07) and CRC-16 (poly 0x8005, non-reflected, init 0) of "123456789".
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn coded_numbers_round_trip() {
        for n in [
            0u64, 0x7F, 0x80, 0x7FF, 0x800, 0xFFFF, 0x1_0000, 0x1F_FFFF, 0x20_0000, 0x3FF_FFFF,
        ] {
            let mut w = BitWriter::default();
            write_coded_number(&mut w, n);
            let bytes = w.into_bytes();
            let lead = u32::from(bytes[0]);
            let ones = bytes[0].leading_ones();
            let extra = ones.saturating_sub(1) as usize;
            assert_eq!(bytes.len(), extra + 1, "n={n:#x}");
            let mut v = u64::from(lead & (0xFFu32 >> (ones + 1)));
            for b in &bytes[1..] {
                assert_eq!(b & 0xC0, 0x80);
                v = (v << 6) | u64::from(b & 0x3F);
            }
            assert_eq!(v, n);
        }
    }
}
