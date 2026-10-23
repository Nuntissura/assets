//! VC-3 (DNxHD / DNxHR) encoder, lane SMB-REEL-ENCODE, written from SMPTE ST 2019-1:2016 (clause 7 coding
//! unit, 7.1 equation (7.1) RI frame size, 7.2 header, 7.3 payload and padding, 7.4 EOF signature, 8.2
//! block coding, Annex C/D/E tables). It is the inverse of this crate's decoder (`super::decode_frame`):
//! the same code tables (`super::tables`, generated from the standard), the same zig-zag (`ZIGZAG`), the
//! same inverse quantization (`dequantize`, equation 8.1) and the transpose of the decoder's IDCT basis.
//!
//! Per frame: 8x8 blocks in the macroblock layout of Table 5 (4:2:2) / Table 4 (4:2:0), augmentation
//! samples and lines (6.3) filled by replicating the last real sample / line (the decoder discards
//! them); level shift by `2^(b-1)`; forward DCT `X(u,v) = sum_i sum_j B[u][i] B[v][j] x(i,j)` with the
//! decoder's orthonormal basis `B`; DC unquantized and DPCM-coded per component with predictors restarting
//! at each macroblock scan line (8.2.4); AC magnitudes quantized to the level whose equation-8.1
//! reconstruction is nearest, coded with sign, index (`P`) and run codewords (8.2.5); EOB per block; 12-bit
//! macroblock header `qsf << 1` (Figure 30); scan lines padded to 32 bits (7.3.1.3) and indexed (7.2.11).
//!
//! Rate control (CBR, the only mode HD rasters allow, 7.1): one quantization scale factor per frame, the
//! smallest `qsf` in 1..=2047 whose compressed data fits the Compressed Payload Size, found by bisection;
//! the rest of the payload is zero padding (7.3.2). Frame sizes: Table C.1 for HD Compression IDs,
//! equation (7.1) (alpha factor 0, rounding to the nearest 4096, minimum 8192) for RI.
//!
//! Header bytes that ST 2019-1's figures print as constant bits (0x006 = 0x80, 0x007 = 0xA0, the two set
//! bits of 0x021 and 0x022, 0x05F = 0x01, 0x167 = 0x02, 0x16F = 0x10) and the MSIPS value `4 * NS + 4`
//! were read from the figures and checked against the bytes of ffmpeg-written DNxHR frames
//! (tests/fixtures/dnx); the figure text extracted from the PDF is ambiguous for them, so they are
//! UNVERIFIED against the standard's printed bit layout and proven only by independent decoders reading the
//! output (tests/vc3_encode.rs).
#![deny(clippy::arithmetic_side_effects)]

use super::entropy::{AcCode, Vlc, ZIGZAG, dequantize};
use super::header::{BASE_HEADER_SIZE, CIDS, CidInfo, EOF_SIGNATURE, MAX_SIDE, ri_header_size};
use crate::error::{ReelError, check_cancel};
use crate::idct8::BASIS;
use crate::model::{ChromaFormat, ColorInfo, VideoFormat};
use hsk_studio_accord::CancellationToken;

/// Encoder identity copied into export receipts.
pub const VC3_ENCODER_ID: &str = "studio-vc3-enc";
/// Bumped whenever an output bit can change.
pub const VC3_ENCODER_VERSION: &str = "1";

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

/// The VC-3 Compression IDs this encoder writes (progressive, frame encoded, no alpha, YCbCr).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Vc3Profile {
    /// CID 1274 (RI, 8-bit 4:2:2 or 4:2:0, any raster): DNxHR LB.
    DnxhrLb,
    /// CID 1273 (RI, 8-bit): DNxHR SQ.
    DnxhrSq,
    /// CID 1272 (RI, 8-bit): DNxHR HQ.
    DnxhrHq,
    /// CID 1271 (RI, 10-bit): DNxHR HQX.
    DnxhrHqx,
    /// CID 1235 (HD, 1920x1080, 10-bit): DNxHD 220x class.
    Dnxhd1080p10,
    /// CID 1238 (HD, 1920x1080, 8-bit, 917504 bytes): DNxHD 220 class.
    Dnxhd1080pHigh,
    /// CID 1237 (HD, 1920x1080, 8-bit, 606208 bytes): DNxHD 145 class.
    Dnxhd1080pMid,
    /// CID 1253 (HD, 1920x1080, 8-bit, 188416 bytes): DNxHD 36 class.
    Dnxhd1080pLow,
    /// CID 1250 (HD, 1280x720, 10-bit).
    Dnxhd720p10,
    /// CID 1251 (HD, 1280x720, 8-bit, 458752 bytes).
    Dnxhd720pHigh,
    /// CID 1252 (HD, 1280x720, 8-bit, 303104 bytes).
    Dnxhd720pMid,
}

impl Vc3Profile {
    pub const ALL: [Self; 11] = [
        Self::DnxhrLb,
        Self::DnxhrSq,
        Self::DnxhrHq,
        Self::DnxhrHqx,
        Self::Dnxhd1080p10,
        Self::Dnxhd1080pHigh,
        Self::Dnxhd1080pMid,
        Self::Dnxhd1080pLow,
        Self::Dnxhd720p10,
        Self::Dnxhd720pHigh,
        Self::Dnxhd720pMid,
    ];

    pub const fn cid(self) -> u32 {
        match self {
            Self::DnxhrLb => 1274,
            Self::DnxhrSq => 1273,
            Self::DnxhrHq => 1272,
            Self::DnxhrHqx => 1271,
            Self::Dnxhd1080p10 => 1235,
            Self::Dnxhd1080pHigh => 1238,
            Self::Dnxhd1080pMid => 1237,
            Self::Dnxhd1080pLow => 1253,
            Self::Dnxhd720p10 => 1250,
            Self::Dnxhd720pHigh => 1251,
            Self::Dnxhd720pMid => 1252,
        }
    }

    /// Stable token (recipes, receipts).
    pub const fn token(self) -> &'static str {
        match self {
            Self::DnxhrLb => "dnxhr_lb",
            Self::DnxhrSq => "dnxhr_sq",
            Self::DnxhrHq => "dnxhr_hq",
            Self::DnxhrHqx => "dnxhr_hqx",
            Self::Dnxhd1080p10 => "dnxhd_1080p_10bit_cid1235",
            Self::Dnxhd1080pHigh => "dnxhd_1080p_cid1238",
            Self::Dnxhd1080pMid => "dnxhd_1080p_cid1237",
            Self::Dnxhd1080pLow => "dnxhd_1080p_cid1253",
            Self::Dnxhd720p10 => "dnxhd_720p_10bit_cid1250",
            Self::Dnxhd720pHigh => "dnxhd_720p_cid1251",
            Self::Dnxhd720pMid => "dnxhd_720p_cid1252",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.token() == token)
    }

    /// RI (DNxHR, header version 3) vs HD (DNxHD) profile.
    pub const fn is_dnxhr(self) -> bool {
        matches!(
            self,
            Self::DnxhrLb | Self::DnxhrSq | Self::DnxhrHq | Self::DnxhrHqx
        )
    }

    pub const fn bit_depth(self) -> u8 {
        match self {
            Self::DnxhrHqx | Self::Dnxhd1080p10 | Self::Dnxhd720p10 => 10,
            _ => 8,
        }
    }

    /// HD raster and Compressed Frame Size (Table C.1); `None` for RI.
    const fn hd_raster(self) -> Option<(u32, u32, u32)> {
        match self {
            Self::Dnxhd1080p10 | Self::Dnxhd1080pHigh => Some((1920, 1080, 917_504)),
            Self::Dnxhd1080pMid => Some((1920, 1080, 606_208)),
            Self::Dnxhd1080pLow => Some((1920, 1080, 188_416)),
            Self::Dnxhd720p10 | Self::Dnxhd720pHigh => Some((1280, 720, 458_752)),
            Self::Dnxhd720pMid => Some((1280, 720, 303_104)),
            _ => None,
        }
    }

    /// RI reference compressed frame size C0 (Table C.2, macroblock count MT = 8160).
    const fn ri_reference_size(self) -> u64 {
        match self {
            Self::DnxhrHqx | Self::DnxhrHq => 917_504,
            Self::DnxhrSq => 606_208,
            _ => 188_416,
        }
    }
}

/// Compressed frame size of an RI raster, equation (7.1) with no alpha (fA = 0): `C0 * NT / MT` rounded to
/// the nearest multiple of 4096 (a remainder of 2048 rounds up), at least 8192.
pub fn ri_frame_size(profile: Vc3Profile, width: u32, height: u32) -> Result<u64, ReelError> {
    let nt = u64::from(width.div_ceil(16))
        .checked_mul(u64::from(height.div_ceil(16)))
        .ok_or(limit("vc3-dimensions"))?;
    let size = profile
        .ri_reference_size()
        .checked_mul(nt)
        .and_then(|s| s.checked_div(8160))
        .ok_or(limit("vc3-dimensions"))?;
    let rem = size.checked_rem(4096).unwrap_or(0);
    let rounded = if rem >= 2048 {
        4096u64.checked_sub(rem).and_then(|up| size.checked_add(up))
    } else {
        size.checked_sub(rem)
    }
    .ok_or(limit("vc3-dimensions"))?;
    Ok(rounded.max(8192))
}

/// MSB-first bit writer (Figure 28 mapping).
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
            acc: 0,
            bits: 0,
        }
    }

    /// Append the low `len` (<= 32) bits of `value`.
    fn put(&mut self, value: u32, len: u32) {
        if len == 0 {
            return;
        }
        let mask = (1u64 << len).wrapping_sub(1);
        self.acc = (self.acc << len) | (u64::from(value) & mask);
        self.bits = self.bits.wrapping_add(len);
        while self.bits >= 8 {
            self.bits = self.bits.wrapping_sub(8);
            self.bytes.push((self.acc >> self.bits) as u8);
        }
        self.acc &= (1u64 << self.bits).wrapping_sub(1);
    }

    /// Zero-pad to the next multiple of 32 bits (7.3.1.3).
    fn align32(&mut self) {
        if self.bits > 0 {
            self.put(0, 8u32.wrapping_sub(self.bits));
        }
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }
}

#[derive(Clone, Copy, Default)]
struct Code {
    bits: u32,
    len: u32,
}

fn code_of(start: u16, len: u8) -> Code {
    let len = u32::from(len);
    Code {
        bits: u32::from(start).checked_shr(16u32.saturating_sub(len)).unwrap_or(0),
        len,
    }
}

/// Reverse lookup of one Annex E table set.
struct Codebook {
    /// `[run][index][amp]` for amp 1..=64 (slot 0 unused).
    ac: [[[Code; 65]; 2]; 2],
    eob: Code,
    run: [Code; 63],
    dc: [Code; 14],
    dc_max: u32,
}

impl Codebook {
    fn new(ac: &[AcCode], run: &[Vlc], dc: &[Vlc]) -> Result<Self, ReelError> {
        let mut book = Self {
            ac: [[[Code::default(); 65]; 2]; 2],
            eob: Code::default(),
            run: [Code::default(); 63],
            dc: [Code::default(); 14],
            dc_max: 0,
        };
        let bad = || ReelError::Corrupt {
            what: "vc3-encoder-tables",
        };
        for c in ac {
            if c.amp == 0 {
                book.eob = code_of(c.start, c.len);
                continue;
            }
            let slot = book
                .ac
                .get_mut(usize::from(c.run))
                .and_then(|r| r.get_mut(usize::from(c.index)))
                .and_then(|i| i.get_mut(usize::from(c.amp)))
                .ok_or_else(bad)?;
            *slot = code_of(c.start, c.len);
        }
        for v in run {
            *book.run.get_mut(usize::from(v.value)).ok_or_else(bad)? = code_of(v.start, v.len);
        }
        for v in dc {
            *book.dc.get_mut(usize::from(v.value)).ok_or_else(bad)? = code_of(v.start, v.len);
            book.dc_max = book.dc_max.max(u32::from(v.value));
        }
        Ok(book)
    }
}

/// Block layout of a macroblock (component, x, y) as in the decoder (Table 5 / Table 4).
const LAYOUT_422: [(usize, u32, u32); 8] = [
    (0, 0, 0),
    (0, 8, 0),
    (1, 0, 0),
    (2, 0, 0),
    (0, 0, 8),
    (0, 8, 8),
    (1, 0, 8),
    (2, 0, 8),
];
const LAYOUT_420: [(usize, u32, u32); 6] = [
    (0, 0, 0),
    (0, 8, 0),
    (1, 0, 0),
    (2, 0, 0),
    (0, 0, 8),
    (0, 8, 8),
];

/// Largest quantization scale factor (11 bits).
const MAX_QSF: u32 = 2047;

/// Stateful VC-3 encoder for one raster (one stream).
pub struct Vc3Encoder {
    profile: Vc3Profile,
    info: CidInfo,
    format: VideoFormat,
    book: Codebook,
    header: Vec<u8>,
    header_size: u32,
    frame_bytes: usize,
    mb_cols: u32,
    mb_rows: u32,
    last_qsf: u32,
}

/// Colour volume code CLV (7.2.5) of a frame's H.273 matrix.
fn clv_of(color: &ColorInfo, dnxhr: bool) -> u8 {
    match color.matrix {
        1 => 0,
        9 if dnxhr => 1,
        10 if dnxhr => 2,
        _ => 3,
    }
}

impl Vc3Encoder {
    /// Validates the raster against the profile (HD rasters are fixed by Table C.1; RI accepts 1..=16384
    /// per side, even widths, and 4:2:0 only with even heights) before anything is allocated.
    pub fn new(profile: Vc3Profile, format: VideoFormat) -> Result<Self, ReelError> {
        let VideoFormat {
            width,
            height,
            bit_depth,
            chroma,
            ..
        } = format;
        if width == 0 || height == 0 {
            return Err(unsupported("vc3-encode-zero-dimension"));
        }
        if width > MAX_SIDE || height > MAX_SIDE {
            return Err(limit("vc3-encode-dimensions"));
        }
        if bit_depth != profile.bit_depth() {
            return Err(unsupported("vc3-encode-bit-depth"));
        }
        match chroma {
            ChromaFormat::Yuv422 => {}
            ChromaFormat::Yuv420 if profile.is_dnxhr() => {}
            _ => return Err(unsupported("vc3-encode-chroma")),
        }
        if !width.is_multiple_of(2) || (chroma == ChromaFormat::Yuv420 && !height.is_multiple_of(2))
        {
            return Err(unsupported("vc3-encode-odd-dimension"));
        }
        let info = CIDS
            .iter()
            .find(|c| c.cid == profile.cid())
            .copied()
            .ok_or(unsupported("vc3-encode-profile"))?;
        let (header_size, frame_bytes) = match profile.hd_raster() {
            Some((w, h, bytes)) => {
                if (w, h) != (width, height) {
                    return Err(unsupported("vc3-encode-profile-raster"));
                }
                (BASE_HEADER_SIZE, u64::from(bytes))
            }
            None => (ri_header_size(height), ri_frame_size(profile, width, height)?),
        };
        let frame_bytes = usize::try_from(frame_bytes).map_err(|_| limit("vc3-encode-dimensions"))?;
        let (ac, run, dc) = info.set.tables();
        let book = Codebook::new(ac, run, dc)?;
        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);
        let header = header_template(&info, &format, header_size, mb_rows, profile.is_dnxhr())?;
        Ok(Self {
            profile,
            info,
            format,
            book,
            header,
            header_size,
            frame_bytes,
            mb_cols,
            mb_rows,
            last_qsf: 0,
        })
    }

    pub fn profile(&self) -> Vc3Profile {
        self.profile
    }

    /// Bytes of every coding unit this encoder writes (CBR).
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// Quantization scale factor of the last encoded frame.
    pub fn last_qsf(&self) -> u32 {
        self.last_qsf
    }

    fn payload_capacity(&self) -> Result<usize, ReelError> {
        usize::try_from(self.header_size)
            .ok()
            .and_then(|h| self.frame_bytes.checked_sub(h))
            .and_then(|p| p.checked_sub(4))
            .ok_or(limit("vc3-encode-frame-size"))
    }

    /// Encode one frame given as the planes Y, Cb, Cr in `VideoFormat` delivery layout (tight rows; one
    /// byte per sample at 8 bits, two little-endian bytes at 10 bits).
    pub fn encode(&mut self, planes: &[&[u8]], cancel: &CancellationToken) -> Result<Vec<u8>, ReelError> {
        let source = Source::new(&self.format, planes)?;
        let capacity = self.payload_capacity()?;
        // Bisection over qsf for the smallest scale whose data fits: `lo` never fits, `hi` fits.
        let (mut lo, mut hi) = (0u32, MAX_QSF);
        let mut best = match self.encode_payload(&source, MAX_QSF, capacity, cancel)? {
            Some(p) => p,
            None => return Err(limit("vc3-encode-rate-not-met")),
        };
        if self.last_qsf > 0 && self.last_qsf < MAX_QSF {
            // Start from the previous frame's scale (neighbouring frames compress alike).
            match self.encode_payload(&source, self.last_qsf, capacity, cancel)? {
                Some(p) => {
                    hi = self.last_qsf;
                    best = p;
                }
                None => lo = self.last_qsf,
            }
        }
        while hi.saturating_sub(lo) > 1 {
            let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
            match self.encode_payload(&source, mid, capacity, cancel)? {
                Some(p) => {
                    hi = mid;
                    best = p;
                }
                None => lo = mid,
            }
        }
        self.last_qsf = hi;
        let (data, indices) = best;
        let mut frame = self.header.clone();
        for (row, index) in indices.iter().enumerate() {
            let at = row
                .checked_mul(4)
                .and_then(|o| o.checked_add(0x170))
                .ok_or(limit("vc3-encode-frame-size"))?;
            frame
                .get_mut(at..at.saturating_add(4))
                .ok_or(limit("vc3-encode-frame-size"))?
                .copy_from_slice(&index.to_be_bytes());
        }
        frame.extend_from_slice(&data);
        frame.resize(self.frame_bytes.saturating_sub(4), 0);
        frame.extend_from_slice(&EOF_SIGNATURE);
        Ok(frame)
    }

    /// Compressed data and scan indices at `qsf`, or `None` when it exceeds `capacity`.
    #[allow(clippy::type_complexity)]
    fn encode_payload(
        &self,
        source: &Source<'_>,
        qsf: u32,
        capacity: usize,
        cancel: &CancellationToken,
    ) -> Result<Option<(Vec<u8>, Vec<u32>)>, ReelError> {
        let layout: &[(usize, u32, u32)] = match self.format.chroma {
            ChromaFormat::Yuv420 => &LAYOUT_420,
            _ => &LAYOUT_422,
        };
        let chroma_mb_h: u32 = if self.format.chroma == ChromaFormat::Yuv420 {
            8
        } else {
            16
        };
        let level = 1i32 << (u32::from(self.format.bit_depth).saturating_sub(1));
        let index_bits = if self.format.bit_depth == 8 { 4u32 } else { 6u32 };
        let max_q = 64i64 << index_bits;
        let p = self.info.p;
        let mut w = BitWriter::new(capacity.min(1 << 24));
        let mut indices = Vec::with_capacity(usize::try_from(self.mb_rows).unwrap_or(0));
        let mut block = [[0f64; 8]; 8];
        for row in 0..self.mb_rows {
            check_cancel(cancel)?;
            indices.push(u32::try_from(w.len()).map_err(|_| limit("vc3-encode-frame-size"))?);
            let mut pred = [0i32; 3];
            for col in 0..self.mb_cols {
                w.put(qsf << 1, 12);
                for &(component, dx, dy) in layout {
                    let (mb_w, mb_h) = if component == 0 {
                        (16u32, 16u32)
                    } else {
                        (8u32, chroma_mb_h)
                    };
                    let x0 = col.saturating_mul(mb_w).saturating_add(dx);
                    let y0 = row.saturating_mul(mb_h).saturating_add(dy);
                    source.block(component, x0, y0, level, &mut block);
                    let coeffs = forward_dct(&block);
                    let weights = if component == 0 {
                        self.info.luma
                    } else {
                        self.info.chroma
                    };
                    let predictor = pred.get_mut(component).ok_or(limit("vc3-encode"))?;
                    self.encode_block(&mut w, &coeffs, weights, qsf, p, max_q, predictor)?;
                }
                if w.len() > capacity {
                    return Ok(None);
                }
            }
            w.align32();
            if w.len() > capacity {
                return Ok(None);
            }
        }
        Ok(Some((w.bytes, indices)))
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_block(
        &self,
        w: &mut BitWriter,
        coeffs: &[[f64; 8]; 8],
        weights: &[[u8; 8]; 8],
        qsf: u32,
        p: i64,
        max_q: i64,
        pred: &mut i32,
    ) -> Result<(), ReelError> {
        // DC (8.2.4): unquantized, differential per component.
        let dc = round_to_i32(coeffs[0][0]);
        let delta = dc.saturating_sub(*pred);
        *pred = dc;
        let magnitude = delta.unsigned_abs();
        let eps = u32::BITS.saturating_sub(magnitude.leading_zeros());
        if eps > self.book.dc_max {
            return Err(limit("vc3-encode-dc-range"));
        }
        let size = *self
            .book
            .dc
            .get(usize::try_from(eps).unwrap_or(usize::MAX))
            .ok_or(limit("vc3-encode-dc-range"))?;
        w.put(size.bits, size.len);
        if eps > 0 {
            // Figure 46 inverse: kappa = delta (positive) or delta + 2^eps - 1 (negative).
            let kappa = if delta > 0 {
                delta.unsigned_abs()
            } else {
                // delta < 0 and |delta| < 2^eps: the result lies in 0..2^(eps-1).
                (1i64 << eps)
                    .checked_sub(1)
                    .and_then(|m| m.checked_add(i64::from(delta)))
                    .and_then(|k| u32::try_from(k).ok())
                    .ok_or(limit("vc3-encode-dc-range"))?
            };
            w.put(kappa, eps);
        }
        // AC (8.2.5).
        let mut zeros = 0u32;
        for &(u, v) in ZIGZAG.iter().skip(1) {
            let (u, v) = (usize::from(u), usize::from(v));
            let c = coeffs[v][u];
            let weight = i64::from(weights[v][u]);
            let q = quantize(c.abs(), weight, i64::from(qsf), p, max_q)?;
            if q == 0 {
                zeros = zeros.saturating_add(1);
                continue;
            }
            // amplitude = 64 * P + amp with amp in 1..=64 (P coded only when the index flag is set).
            let below = q.saturating_sub(1);
            let (pidx, amp) = (below / 64, (below % 64).saturating_add(1));
            let index = usize::from(pidx > 0);
            let has_run = usize::from(zeros > 0);
            let code = self.book.ac[has_run][index][usize::try_from(amp).unwrap_or(0)];
            w.put(code.bits, code.len);
            w.put(u32::from(c < 0.0), 1);
            if index == 1 {
                let bits = if self.format.bit_depth == 8 { 4 } else { 6 };
                w.put(u32::try_from(pidx).map_err(|_| limit("vc3-encode-ac-range"))?, bits);
            }
            if zeros > 0 {
                let run = self.book.run[zeros as usize];
                w.put(run.bits, run.len);
            }
            zeros = 0;
        }
        w.put(self.book.eob.bits, self.book.eob.len);
        Ok(())
    }
}

fn round_to_i32(x: f64) -> i32 {
    let r = x.round();
    if r >= f64::from(i32::MAX) {
        i32::MAX
    } else if r <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        r as i32
    }
}

/// The quantized magnitude whose equation-8.1 reconstruction is nearest `magnitude` (0 when zero is
/// nearest), at most `max_q`.
fn quantize(magnitude: f64, weight: i64, qsf: i64, p: i64, max_q: i64) -> Result<i64, ReelError> {
    let step = (weight.saturating_mul(qsf)) as f64 / p as f64;
    if step <= 0.0 {
        return Ok(0);
    }
    let guess = (magnitude / step).floor() as i64;
    let mut best = (0i64, magnitude);
    for q in [guess.saturating_sub(1), guess, guess.saturating_add(1)] {
        if q < 1 || q > max_q {
            continue;
        }
        let rec = dequantize(q, weight, qsf, p)? as f64;
        let err = (rec - magnitude).abs();
        if err < best.1 {
            best = (q, err);
        }
    }
    if guess > max_q {
        // Beyond the codable range: the largest level.
        let rec = dequantize(max_q, weight, qsf, p)? as f64;
        if (rec - magnitude).abs() < best.1 {
            best = (max_q, 0.0);
        }
    }
    Ok(best.0)
}

/// Forward 8x8 DCT, the transpose of `idct8x8`: `X(u,v) = sum_j B[v][j] sum_i B[u][i] x(i,j)`.
#[allow(clippy::needless_range_loop)]
fn forward_dct(block: &[[f64; 8]; 8]) -> [[f64; 8]; 8] {
    // Horizontal: tmp[j][u] = sum_i B[u][i] x[j][i].
    let mut tmp = [[0f64; 8]; 8];
    for j in 0..8 {
        for u in 0..8 {
            let mut acc = 0f64;
            for i in 0..8 {
                acc += BASIS[u][i] * block[j][i];
            }
            tmp[j][u] = acc;
        }
    }
    // Vertical: out[v][u] = sum_j B[v][j] tmp[j][u].
    let mut out = [[0f64; 8]; 8];
    for v in 0..8 {
        for u in 0..8 {
            let mut acc = 0f64;
            for j in 0..8 {
                acc += BASIS[v][j] * tmp[j][u];
            }
            out[v][u] = acc;
        }
    }
    out
}

/// Validated input planes.
struct Source<'a> {
    planes: [&'a [u8]; 3],
    dims: [(u32, u32); 3],
    wide: bool,
}

impl<'a> Source<'a> {
    fn new(format: &VideoFormat, planes: &[&'a [u8]]) -> Result<Self, ReelError> {
        let dims = format.plane_dims();
        if planes.len() != 3 || dims.len() != 3 {
            return Err(unsupported("vc3-encode-planes"));
        }
        let wide = format.bit_depth > 8;
        let bps: u64 = if wide { 2 } else { 1 };
        let max = (1u32 << format.bit_depth).saturating_sub(1);
        for (plane, &(w, h)) in planes.iter().zip(&dims) {
            let expected = u64::from(w)
                .checked_mul(u64::from(h))
                .and_then(|n| n.checked_mul(bps))
                .ok_or(limit("vc3-encode-dimensions"))?;
            if plane.len() as u64 != expected {
                return Err(unsupported("vc3-encode-plane-size"));
            }
            if wide
                && plane
                    .chunks_exact(2)
                    .any(|s| u32::from(u16::from_le_bytes([s[0], s[1]])) > max)
            {
                return Err(unsupported("vc3-encode-sample-range"));
            }
        }
        Ok(Self {
            planes: [planes[0], planes[1], planes[2]],
            dims: [dims[0], dims[1], dims[2]],
            wide,
        })
    }

    /// Level-shifted 8x8 block at `(x0, y0)` of `component`; samples beyond the plane replicate the last
    /// column / row (augmentation, 6.3).
    fn block(&self, component: usize, x0: u32, y0: u32, level: i32, out: &mut [[f64; 8]; 8]) {
        let (w, h) = self.dims[component];
        let data = self.planes[component];
        for (dy, row) in (0u32..).zip(out.iter_mut()) {
            let y = y0.saturating_add(dy).min(h.saturating_sub(1));
            for (dx, cell) in (0u32..).zip(row.iter_mut()) {
                let x = x0.saturating_add(dx).min(w.saturating_sub(1));
                // In range: x < w, y < h and the plane length was checked against w * h.
                let index = u64::from(y)
                    .saturating_mul(u64::from(w))
                    .saturating_add(u64::from(x)) as usize;
                let sample = if self.wide {
                    let at = index.saturating_mul(2);
                    i32::from(u16::from_le_bytes([data[at], data[at.saturating_add(1)]]))
                } else {
                    i32::from(data[index])
                };
                *cell = f64::from(sample.saturating_sub(level));
            }
        }
    }
}

/// Header with every constant field; scan indices are filled per frame.
fn header_template(
    info: &CidInfo,
    format: &VideoFormat,
    header_size: u32,
    mb_rows: u32,
    dnxhr: bool,
) -> Result<Vec<u8>, ReelError> {
    let mut h = vec![0u8; usize::try_from(header_size).map_err(|_| limit("vc3-encode-header"))?];
    let be16 = |h: &mut [u8], at: usize, v: u32| {
        h[at] = (v >> 8) as u8;
        h[at.saturating_add(1)] = v as u8;
    };
    h[0..4].copy_from_slice(&header_size.to_be_bytes());
    h[4] = if dnxhr { 3 } else { 1 };
    h[5] = 0x01; // CBR, FFC = 01 (progressive frame)
    h[6] = 0x80; // MACF = CRCF = 0
    h[7] = 0xA0; // no alpha
    be16(&mut h, 0x18, format.height); // ALPF
    be16(&mut h, 0x1A, format.width); // SPL
    be16(&mut h, 0x1D, format.height); // NAL
    let sbd: u8 = match format.bit_depth {
        8 => 1,
        10 => 2,
        _ => return Err(unsupported("vc3-encode-bit-depth")),
    };
    h[0x21] = (sbd << 5) | 0x18;
    h[0x22] = 0x88; // progressive (SST = 0)
    h[0x28..0x2C].copy_from_slice(&info.cid.to_be_bytes());
    let ssc: u8 = if format.chroma == ChromaFormat::Yuv420 { 1 } else { 0 };
    h[0x2C] = 0x80 | (ssc << 5) | (clv_of(&format.color, dnxhr) << 1); // FFE = 1, YCbCr
    h[0x5F] = 0x01; // UDL = 0
    h[0x167] = 0x02;
    let ns = mb_rows;
    be16(&mut h, 0x16A, ns.checked_mul(4).and_then(|v| v.checked_add(4)).ok_or(limit("vc3-encode-header"))?);
    be16(&mut h, 0x16C, ns);
    h[0x16F] = 0x10;
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ri_frame_size_follows_equation_7_1() {
        // 1920x1080: NT = 120 * 68 = 8160 = MT -> C0, already a multiple of 4096.
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrHq, 1920, 1080).unwrap(), 917_504);
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrSq, 1920, 1080).unwrap(), 606_208);
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrLb, 1920, 1080).unwrap(), 188_416);
        // 3840x2160: NT = 240 * 135 = 32400: 917504 * 32400 / 8160 = 3643030 -> rem 1686 -> 3641344.
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrHqx, 3840, 2160).unwrap(), 3_641_344);
        // Small rasters hit the 8192 floor.
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrLb, 264, 120).unwrap(), 8192);
        // 264x120 HQ: NT = 136: 917504 * 136 / 8160 = 15291 -> rem 3003 >= 2048 -> 16384 (ffmpeg's
        // fixture frames have the same size).
        assert_eq!(ri_frame_size(Vc3Profile::DnxhrHq, 264, 120).unwrap(), 16384);
    }

    #[test]
    fn quantizer_picks_the_nearest_reconstruction() {
        // W = 32, qsf = 1, p = 32: reconstructions 1 -> 1, 2 -> 2, ... (equation 8.1).
        assert_eq!(quantize(0.4, 32, 1, 32, 1024).unwrap(), 0);
        assert_eq!(quantize(1.0, 32, 1, 32, 1024).unwrap(), 1);
        assert_eq!(quantize(5000.0, 32, 1, 32, 1024).unwrap(), 1024);
        for q in 1..200 {
            let rec = dequantize(q, 37, 3, 32).unwrap() as f64;
            assert_eq!(quantize(rec, 37, 3, 32, 1024).unwrap(), q, "level {q}");
        }
    }

    #[test]
    fn forward_dct_inverts_the_decoder_idct() {
        let mut block = [[0f64; 8]; 8];
        for (j, row) in block.iter_mut().enumerate() {
            for (i, cell) in row.iter_mut().enumerate() {
                *cell = ((i * 13 + j * 7) % 50) as f64 - 25.0;
            }
        }
        let coeffs = forward_dct(&block);
        let mut back = [[0f64; 8]; 8];
        crate::idct8::idct8x8_f64(&coeffs, &mut back);
        for j in 0..8 {
            for i in 0..8 {
                assert!((back[j][i] - block[j][i]).abs() < 1e-9);
            }
        }
    }
}
