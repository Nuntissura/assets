//! Shared TIFF/EP CFA raw IFD reader for proprietary TIFF-based containers (NEF, ...): selects the
//! full-resolution CFA IFD (NewSubfileType 0, PhotometricInterpretation 32803), reads the TIFF/EP
//! CFARepeatPatternDim / CFAPattern tags (33421 / 33422) and uncompressed strips. Written from the
//! public TIFF 6.0 specification (strips, BitsPerSample, byte order; samples of a non-byte width
//! are packed most-significant bit first and every row starts on a byte boundary) and the TIFF/EP
//! CFA tag definitions (as also restated in the public Adobe DNG specification).
//!
//! Packed widths other than 8/16 are read only in big-endian (`MM`) files, where the bit order is
//! unambiguous; the little-endian packed case is refused as an unverified variant.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::MAX_DNG_PIXELS;
use crate::pipeline::{Bayer, CfaPattern, XTRANS};
pub use hsk_studio_interop_tiff::{Ifd, IfdEntry as Entry};
use hsk_studio_interop_tiff::{ByteOrder, Container, Limits, TiffError};

/// Develop's bounded budgets for the TIFF container walk (the previous private reader's limits).
const MAX_ENTRIES_PER_IFD: u32 = 1024;
const MAX_IFDS: u32 = 64;

fn map_err(error: TiffError) -> DevelopError {
    match error {
        TiffError::Corrupt("non_finite") => DevelopError::NonFinite,
        _ => DevelopError::InvalidContainer,
    }
}

/// Develop's view of a TIFF container: the shared interop-tiff container facade with develop's error
/// vocabulary and u32 narrowing (classic TIFF offsets and counts).
pub struct Tiff<'a> {
    container: Container<'a>,
}

impl<'a> Tiff<'a> {
    /// Validate the header; returns the reader and the first IFD offset. BigTIFF is refused.
    pub fn parse(data: &'a [u8]) -> Result<(Self, u32), DevelopError> {
        let limits = Limits { max_ifd_entries: MAX_ENTRIES_PER_IFD, max_pages: MAX_IFDS, ..Limits::default() };
        let container = Container::parse(data, &limits).map_err(map_err)?;
        if container.is_big_tiff() {
            return Err(DevelopError::UnsupportedContainer);
        }
        let first = u32::try_from(container.first_ifd_offset()).map_err(|_| DevelopError::InvalidContainer)?;
        Ok((Self { container }, first))
    }

    pub fn is_big_endian(&self) -> bool {
        self.container.byte_order() == ByteOrder::Big
    }

    pub fn u32(&self, offset: usize) -> Result<u32, DevelopError> {
        let raw: [u8; 4] = self
            .region(u64::try_from(offset).map_err(|_| DevelopError::InvalidContainer)?, 4)?
            .try_into()
            .map_err(|_| DevelopError::InvalidContainer)?;
        Ok(if self.is_big_endian() { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) })
    }

    pub fn ifd(&self, offset: u32) -> Result<Ifd, DevelopError> {
        self.container.ifd(u64::from(offset)).map_err(map_err)
    }

    /// All IFDs reachable from the first one through next-pointers and SubIFDs (tag 330), without
    /// revisiting an offset.
    pub fn walk(&self, _first: u32) -> Result<Vec<Ifd>, DevelopError> {
        self.container.walk().map_err(map_err)
    }

    pub fn bytes(&self, e: &Entry) -> Result<&'a [u8], DevelopError> {
        self.container.bytes(e).map_err(map_err)
    }

    pub fn ascii(&self, e: &Entry) -> Result<String, DevelopError> {
        self.container.ascii(e).map_err(map_err)
    }

    /// Unsigned integer values (BYTE, SHORT, LONG, IFD).
    pub fn u32s(&self, e: &Entry) -> Result<Vec<u32>, DevelopError> {
        if matches!(e.field_type, 16 | 18) {
            return Err(DevelopError::InvalidContainer);
        }
        let values = self.container.uints(e).map_err(map_err)?;
        values.into_iter().map(|v| u32::try_from(v).map_err(|_| DevelopError::InvalidContainer)).collect()
    }

    /// Any numeric type as binary64 (RATIONAL/SRATIONAL divide; zero denominator refuses).
    pub fn f64s(&self, e: &Entry) -> Result<Vec<f64>, DevelopError> {
        self.container.f64s(e).map_err(map_err)
    }

    /// Raw bytes of a region of the file (strip/tile payloads).
    pub fn region(&self, offset: u64, len: u64) -> Result<&'a [u8], DevelopError> {
        self.container.region(offset, len).map_err(map_err)
    }
}

pub const NEW_SUBFILE_TYPE: u16 = 254;
pub const WIDTH: u16 = 256;
pub const HEIGHT: u16 = 257;
pub const BITS_PER_SAMPLE: u16 = 258;
pub const COMPRESSION: u16 = 259;
pub const PHOTOMETRIC: u16 = 262;
pub const MAKE: u16 = 271;
pub const MODEL: u16 = 272;
pub const STRIP_OFFSETS: u16 = 273;
pub const SAMPLES_PER_PIXEL: u16 = 277;
pub const ROWS_PER_STRIP: u16 = 278;
pub const STRIP_BYTE_COUNTS: u16 = 279;
pub const CFA_REPEAT_PATTERN_DIM: u16 = 33421;
pub const CFA_PATTERN: u16 = 33422;
pub const DNG_VERSION: u16 = 50706;
const PHOTOMETRIC_CFA: u32 = 32803;

pub fn first_u32(tiff: &Tiff<'_>, e: &Entry) -> Result<u32, DevelopError> {
    tiff.u32s(e)?.first().copied().ok_or(DevelopError::InvalidContainer)
}

/// The full-resolution CFA IFD among `ifds` (first match in walk order).
pub fn cfa_ifd<'a>(tiff: &Tiff<'_>, ifds: &'a [Ifd]) -> Result<Option<&'a Ifd>, DevelopError> {
    for ifd in ifds {
        let photometric = match ifd.find(PHOTOMETRIC) {
            Some(e) => first_u32(tiff, e)?,
            None => continue,
        };
        let subfile = match ifd.find(NEW_SUBFILE_TYPE) {
            Some(e) => first_u32(tiff, e)?,
            None => 0,
        };
        if photometric == PHOTOMETRIC_CFA && subfile == 0 {
            return Ok(Some(ifd));
        }
    }
    Ok(None)
}

/// IFD0 ASCII tag (e.g. Make, Model), `None` when absent.
pub fn ascii(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<String>, DevelopError> {
    match ifd.find(tag) {
        Some(e) => Ok(Some(tiff.ascii(e)?)),
        None => Ok(None),
    }
}

/// TIFF/EP CFA pattern of the IFD (2x2 Bayer or the 6x6 X-Trans grid).
pub fn cfa_pattern(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<CfaPattern, DevelopError> {
    let dim = tiff.u32s(ifd.find(CFA_REPEAT_PATTERN_DIM).ok_or(DevelopError::InvalidContainer)?)?;
    let pattern = tiff.bytes(ifd.find(CFA_PATTERN).ok_or(DevelopError::InvalidContainer)?)?;
    match (dim.as_slice(), pattern) {
        ([2, 2], [0, 1, 1, 2]) => Ok(CfaPattern::Bayer(Bayer::Rggb)),
        ([2, 2], [2, 1, 1, 0]) => Ok(CfaPattern::Bayer(Bayer::Bggr)),
        ([2, 2], [1, 0, 2, 1]) => Ok(CfaPattern::Bayer(Bayer::Grbg)),
        ([2, 2], [1, 2, 0, 1]) => Ok(CfaPattern::Bayer(Bayer::Gbrg)),
        ([6, 6], p) if p.len() == 36 && (0..36).all(|i| p.get(i) == XTRANS.get(i / 6).and_then(|r| r.get(i % 6))) => {
            Ok(CfaPattern::XTrans)
        }
        ([_, _], _) => Err(DevelopError::UnsupportedPattern),
        _ => Err(DevelopError::InvalidContainer),
    }
}

/// Frame geometry of a CFA IFD: (width, height, bits, compression).
pub fn geometry(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<(u32, u32, u16, u32), DevelopError> {
    let req = |t: u16| ifd.find(t).ok_or(DevelopError::InvalidContainer);
    let width = first_u32(tiff, req(WIDTH)?)?;
    let height = first_u32(tiff, req(HEIGHT)?)?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    if let Some(e) = ifd.find(SAMPLES_PER_PIXEL)
        && first_u32(tiff, e)? != 1
    {
        return Err(DevelopError::UnsupportedContainer);
    }
    let bits = u16::try_from(first_u32(tiff, req(BITS_PER_SAMPLE)?)?).map_err(|_| DevelopError::InvalidContainer)?;
    if !(8..=16).contains(&bits) {
        return Err(DevelopError::UnsupportedContainer);
    }
    let compression = match ifd.find(COMPRESSION) {
        Some(e) => first_u32(tiff, e)?,
        None => 1,
    };
    Ok((width, height, bits, compression))
}

/// Uncompressed strips: byte-aligned 8/16-bit samples in file byte order, or (big-endian files
/// only) samples packed MSB first with every row starting on a byte boundary. Every strip must lie
/// inside the file and hold its rows before the frame is allocated (V4-02).
pub fn uncompressed_strips(
    tiff: &Tiff<'_>,
    ifd: &Ifd,
    width: u32,
    height: u32,
    bits: u16,
    file_len: usize,
) -> Result<Vec<u16>, DevelopError> {
    if !matches!(bits, 8 | 16) && !tiff.is_big_endian() {
        return Err(DevelopError::UnsupportedContainer);
    }
    let rows_per_strip = match ifd.find(ROWS_PER_STRIP) {
        Some(e) => first_u32(tiff, e)?.min(height),
        None => height,
    };
    if rows_per_strip == 0 {
        return Err(DevelopError::InvalidContainer);
    }
    let offsets = tiff.u32s(ifd.find(STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?;
    let counts = tiff.u32s(ifd.find(STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?;
    let strips = height.div_ceil(rows_per_strip) as usize;
    if offsets.len() != strips || counts.len() != strips {
        return Err(DevelopError::InvalidContainer);
    }
    // Bytes per row: ceil(width * bits / 8) (u32 * 16 fits u64).
    let row_bytes = u64::from(width)
        .checked_mul(u64::from(bits))
        .map(|b| b.div_ceil(8))
        .ok_or(DevelopError::InvalidContainer)?;
    let mut regions = Vec::with_capacity(strips);
    let mut payload = 0u64;
    for (index, (offset, count)) in offsets.iter().zip(&counts).enumerate() {
        let first_row = u32::try_from(index)
            .ok()
            .and_then(|i| i.checked_mul(rows_per_strip))
            .ok_or(DevelopError::InvalidContainer)?;
        let rows = rows_per_strip.min(height.saturating_sub(first_row));
        let needed = row_bytes.checked_mul(u64::from(rows)).ok_or(DevelopError::InvalidContainer)?;
        if u64::from(*count) < needed {
            return Err(DevelopError::InvalidContainer);
        }
        regions.push(tiff.region(u64::from(*offset), needed)?);
        payload = payload.saturating_add(needed);
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    // Aggregate: the strips together must carry every row (they may not all alias one region of a
    // tiny file: the sum is capped by the file length).
    if payload.min(file_len as u64) < row_bytes.saturating_mul(u64::from(height)) {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut samples = vec![0u16; usize::try_from(pixels).map_err(|_| DevelopError::InvalidDimensions)?];
    let w = width as usize;
    let rb = usize::try_from(row_bytes).map_err(|_| DevelopError::InvalidContainer)?;
    let mut y = 0usize;
    for region in regions {
        for row in region.chunks_exact(rb) {
            let start = y.checked_mul(w).ok_or(DevelopError::Overflow)?;
            let end = start.checked_add(w).ok_or(DevelopError::Overflow)?;
            let out = samples.get_mut(start..end).ok_or(DevelopError::InvalidContainer)?;
            unpack_row(row, out, bits, tiff.is_big_endian())?;
            y = y.checked_add(1).ok_or(DevelopError::Overflow)?;
        }
    }
    if y != height as usize {
        return Err(DevelopError::InvalidContainer);
    }
    Ok(samples)
}

fn unpack_row(row: &[u8], out: &mut [u16], bits: u16, big_endian: bool) -> Result<(), DevelopError> {
    match bits {
        8 => out.iter_mut().zip(row).for_each(|(o, b)| *o = u16::from(*b)),
        16 => out.iter_mut().zip(row.chunks_exact(2)).for_each(|(o, b)| {
            *o = if big_endian {
                u16::from_be_bytes([b[0], b[1]])
            } else {
                u16::from_le_bytes([b[0], b[1]])
            }
        }),
        _ => {
            // MSB-first bit stream (big-endian files only, checked by the caller).
            let bits = u32::from(bits);
            let mask = 1u32.checked_shl(bits).and_then(|v| v.checked_sub(1)).ok_or(DevelopError::Overflow)?;
            let mut acc = 0u32;
            let mut have = 0u32;
            let mut bytes = row.iter();
            for o in out.iter_mut() {
                while have < bits {
                    let byte = bytes.next().ok_or(DevelopError::InvalidContainer)?;
                    acc = (acc << 8) | u32::from(*byte);
                    have = have.checked_add(8).ok_or(DevelopError::Overflow)?;
                }
                have = have.checked_sub(bits).ok_or(DevelopError::Overflow)?;
                *o = u16::try_from((acc >> have) & mask).map_err(|_| DevelopError::Overflow)?;
                acc &= 1u32.checked_shl(have).and_then(|v| v.checked_sub(1)).unwrap_or(0);
            }
        }
    }
    Ok(())
}
