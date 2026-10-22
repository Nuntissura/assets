//! Canon CRX sensor sample decompression (the CR3 raw track codec).
//!
//! Adapted from LightCraft `crates/raw/src/vendor/crx.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's
//! `NOTICE`). LightCraft's stated clean-room sources: Laurent Clevy's public prose CR3 description
//! (CMP1 and marker layouts), Canon patent US20160323602A1 (MED prediction, signed mapping,
//! adaptive Rice coding), Canon patent JP2017192077A (zero-run flag, MELCODE of run length minus
//! one), ITU-T T.87 (MELCODE run index table); CRX-specific boundaries and the quantisation
//! sequence were established black-box against CC0 raw.pixls.us files. No copyleft decoder source
//! was read for this module.
//!
//! Supported (as LightCraft): version 0x100 lossless Bayer (0 wavelet levels) and horizontal-tile
//! C-RAW (3 levels, uniform quantisation), version 0x200 single-tile 14-bit C-RAW with an adaptive
//! QP map; marker families ff01/ff02/ff03 (0x100) and ff11/ff12/ff13 (0x200). Everything else is
//! a typed `unsupported_compression`. Changes: serial decode (no thread pool), checked arithmetic
//! with develop's typed errors (V4-01), every allocation bounded by the sample size first (V4-02).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::crx_wavelet::{self, Band as WaveletBand, MAX_SAMPLES, TileEdges, zeroed};
use std::ops::Range;

const MAX_TILES: usize = 1024;
const MAX_HEADER_BYTES: usize = 1 << 20;
const MAX_EDGE: u32 = 65_535;
/// Unary QP-map prefix length that announces an 8-bit escaped symbol.
const QP_ESCAPE_ZEROS: u32 = 23;
const RUN_EXPONENT: [u8; 32] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The CMP1 coding descriptor (payload after the box header), as read by `cr3`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Coding {
    pub version: u16,
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub bit_depth: u8,
    pub planes: u8,
    /// 0 = RGGB, 1 = GRBG, 2 = GBRG, 3 = BGGR.
    pub cfa_pattern: u8,
    pub encoding: u8,
    pub levels: u8,
    /// Bytes at the start of the sample before its entropy-coded data.
    pub header_size: u32,
    /// Extended-header median precision (burst encodings): refused.
    pub median_bit_depth: Option<u8>,
}

fn be16(src: &[u8], at: usize) -> Result<u16, DevelopError> {
    let b: [u8; 2] = src.get(at..add(at, 2)?).and_then(|s| s.try_into().ok()).ok_or_else(corrupt)?;
    Ok(u16::from_be_bytes(b))
}

fn be32(src: &[u8], at: usize) -> Result<u32, DevelopError> {
    let b: [u8; 4] = src.get(at..add(at, 4)?).and_then(|s| s.try_into().ok()).ok_or_else(corrupt)?;
    Ok(u32::from_be_bytes(b))
}

impl Coding {
    /// The public CMP1 field layout; a truncated or impossible descriptor is `invalid_container`.
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, DevelopError> {
        let header_len = add(usize::from(be16(payload, 2)?), 4)?;
        if header_len < 52 || header_len > payload.len() {
            return Err(corrupt());
        }
        let byte = |at: usize| payload.get(at).copied().ok_or_else(corrupt);
        let (packed, coding) = (byte(25)?, byte(26)?);
        let mut out = Self {
            version: be16(payload, 4)?,
            width: be32(payload, 8)?,
            height: be32(payload, 12)?,
            tile_width: be32(payload, 16)?,
            tile_height: be32(payload, 20)?,
            bit_depth: byte(24)?,
            planes: packed >> 4,
            cfa_pattern: packed & 15,
            encoding: coding >> 4,
            levels: coding & 15,
            header_size: be32(payload, 28)?,
            median_bit_depth: None,
        };
        if out.width == 0
            || out.height == 0
            || out.tile_width == 0
            || out.tile_height == 0
            || out.tile_width > out.width
            || out.tile_height > out.height
            || !(1..=16).contains(&out.bit_depth)
            || !(1..=4).contains(&out.planes)
            || out.header_size == 0
        {
            return Err(corrupt());
        }
        if payload.get(32).is_some_and(|flags| flags & 0x80 != 0) {
            let extension_len = usize::try_from(be32(payload, 52)?).map_err(|_| corrupt())?;
            let extension_end = add(52, extension_len)?;
            if extension_len < 32 || extension_end > payload.len() {
                return Err(corrupt());
            }
            if payload.get(56).is_some_and(|flags| flags & 0x40 != 0) {
                out.median_bit_depth = Some(byte(84)?);
            }
        }
        Ok(out)
    }
}

fn corrupt() -> DevelopError {
    DevelopError::InvalidContainer
}

fn unsupported() -> DevelopError {
    DevelopError::UnsupportedCompression
}

fn add(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_add(b).ok_or(DevelopError::Overflow)
}

fn sub(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_sub(b).ok_or_else(corrupt)
}

fn mul(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_mul(b).ok_or(DevelopError::Overflow)
}

#[derive(Debug)]
struct Band {
    data: Range<usize>,
    padding: usize,
    quant: Quantization,
    partial: bool,
}

#[derive(Debug)]
enum Quantization {
    Uniform(u8),
    Adaptive { base: u32, gain: u32 },
}

struct QpMap {
    width: usize,
    height: usize,
    data: Vec<i32>,
}

#[derive(Debug)]
struct Plane {
    partial: bool,
    rounded_bits: u8,
    bands: Vec<Band>,
}

#[derive(Debug)]
struct Tile {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    planes: Vec<Plane>,
    qp: Option<Range<usize>>,
}

/// Decode a CRX sample to full sensor-order CFA samples, including masked borders.
pub(crate) fn decode(sample: &[u8], config: &Coding) -> Result<Vec<u16>, DevelopError> {
    supports(config)?;
    let width = usize::try_from(config.width).map_err(|_| DevelopError::InvalidDimensions)?;
    let count = image_size(config)?;
    let tiles = layout(sample, config)?;
    validate_planes(&tiles, config)?;
    let bits = u32::from(config.bit_depth);
    let midpoint = 1i32 << bits.saturating_sub(1);
    let maximum = (1i32 << bits).saturating_sub(1);
    // Allocated once the first plane decoded: a tiny crafted file can claim a huge image (V4-02).
    let mut output: Vec<u16> = Vec::new();
    for tile in tiles {
        let qp = tile_qp(sample, &tile)?;
        let (pw, ph) = (tile.width / 2, tile.height / 2);
        for (p, plane) in tile.planes.iter().enumerate() {
            let decoded = if config.levels == 0 {
                lossless_plane(band_bytes(sample, plane.bands.first().ok_or_else(corrupt)?)?, pw, ph)?
            } else {
                let edges = TileEdges { left: tile.x > 0, right: add(tile.x, tile.width)? < width, top: false, bottom: false };
                wavelet_plane(sample, plane, pw, ph, edges, qp.as_ref())?
            };
            if output.is_empty() {
                output.try_reserve_exact(count).map_err(|_| DevelopError::InvalidDimensions)?;
                output.resize(count, 0);
            }
            for (i, &value) in decoded.iter().enumerate() {
                let v = sensor_sample(value, midpoint, maximum, config.levels != 0)?;
                let x = add(add(tile.x, mul(2, i.checked_rem(pw).ok_or_else(corrupt)?)?)?, p.checked_rem(2).ok_or_else(corrupt)?)?;
                let y = add(add(tile.y, mul(2, i.checked_div(pw).ok_or_else(corrupt)?)?)?, p / 2)?;
                *output.get_mut(add(mul(y, width)?, x)?).ok_or_else(corrupt)? = v;
            }
        }
    }
    if output.len() != count {
        return Err(corrupt());
    }
    Ok(output)
}

/// A decoded plane value as a sensor sample. Lossless planes reproduce the sensor exactly (out of
/// range = corrupt); quantised C-RAW planes may overshoot next to clipped highlights and are clamped
/// to the recordable range (LightCraft measured at most 36 codes above 16383 on 17 CC0 files).
fn sensor_sample(value: i32, midpoint: i32, maximum: i32, quantized: bool) -> Result<u16, DevelopError> {
    let s = value.checked_add(midpoint).ok_or_else(corrupt)?;
    let s = if quantized { s.clamp(0, maximum) } else { s };
    if s > maximum {
        return Err(corrupt());
    }
    u16::try_from(s).map_err(|_| corrupt())
}

fn image_size(config: &Coding) -> Result<usize, DevelopError> {
    if config.width > MAX_EDGE || config.height > MAX_EDGE {
        return Err(DevelopError::InvalidDimensions);
    }
    mul(config.width as usize, config.height as usize)?
        .checked_add(0)
        .filter(|&n| n > 0 && n <= MAX_SAMPLES)
        .ok_or(DevelopError::InvalidDimensions)
}

fn supports(config: &Coding) -> Result<(), DevelopError> {
    if ![0x100, 0x200].contains(&config.version)
        || config.encoding != 0
        || config.planes != 4
        || config.cfa_pattern > 3
        || ![0, 3].contains(&config.levels)
        || !(8..=16).contains(&config.bit_depth)
        || (config.levels == 3 && config.tile_height != config.height)
        || (config.version == 0x200 && (config.levels != 3 || config.bit_depth != 14 || config.tile_width != config.width))
        || config.median_bit_depth.is_some()
    {
        return Err(unsupported());
    }
    Ok(())
}

fn validate_planes(tiles: &[Tile], config: &Coding) -> Result<(), DevelopError> {
    let bands = add(1, mul(usize::from(config.levels), 3)?)?;
    for tile in tiles {
        for plane in &tile.planes {
            if !plane.partial || plane.rounded_bits != 0 || plane.bands.len() != bands {
                return Err(unsupported());
            }
            for (index, band) in plane.bands.iter().enumerate() {
                if band.partial {
                    return Err(unsupported());
                }
                if config.levels != 0 {
                    match band.quant {
                        Quantization::Uniform(value) => {
                            quant_step(value)?;
                        }
                        Quantization::Adaptive { gain, .. } if index < 4 && gain != 0 => return Err(unsupported()),
                        Quantization::Adaptive { .. } => {}
                    }
                }
            }
        }
    }
    Ok(())
}

struct Header<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> Header<'a> {
    fn marker(&mut self, wanted: u16, wanted_size: usize) -> Result<&'a [u8], DevelopError> {
        let marker = be16(self.src, self.at)?;
        if marker != wanted {
            return Err(if Some(marker) == wanted.checked_add(0x10) { unsupported() } else { corrupt() });
        }
        let size = usize::from(be16(self.src, add(self.at, 2)?)?);
        if size != wanted_size {
            return Err(unsupported());
        }
        let start = add(self.at, 4)?;
        let end = add(start, size)?;
        let body = self.src.get(start..end).ok_or_else(corrupt)?;
        self.at = end;
        Ok(body)
    }
}

fn layout(sample: &[u8], config: &Coding) -> Result<Vec<Tile>, DevelopError> {
    let (width, height) = (config.width as usize, config.height as usize);
    let (tw, th) = (config.tile_width as usize, config.tile_height as usize);
    if [width, height, tw, th].iter().any(|v| *v == 0 || !v.is_multiple_of(2)) || tw > width || th > height {
        return Err(corrupt());
    }
    let columns = width.div_ceil(tw);
    let rows = height.div_ceil(th);
    let tile_count = mul(columns, rows)?;
    if tile_count > MAX_TILES {
        return Err(DevelopError::InvalidDimensions);
    }
    let header_size = config.header_size as usize;
    if header_size > MAX_HEADER_BYTES {
        return Err(DevelopError::InvalidDimensions);
    }
    let header = sample.get(..header_size).ok_or_else(corrupt)?;
    let mut reader = Header { src: header, at: 0 };
    let mut data_at = header_size;
    let modern = config.version == 0x200;
    let band_count = add(1, mul(usize::from(config.levels), 3)?)?;
    let mut tiles = Vec::with_capacity(tile_count);
    for index in 0..tile_count {
        let body = reader.marker(if modern { 0xff11 } else { 0xff01 }, if modern { 16 } else { 8 })?;
        let tile_size = be32(body, 0)? as usize;
        if usize::from(be16(body, 4)?) != index {
            return Err(corrupt());
        }
        let tile_end = add(data_at, tile_size)?;
        if tile_end > sample.len() {
            return Err(corrupt());
        }
        let qp = if modern {
            let length = be32(body, 8)? as usize;
            let padding = usize::from(be16(body, 12)?);
            if be16(body, 6)? != 0x4000 || be16(body, 14)? != 0 || length == 0 || padding > 7 {
                return Err(unsupported());
            }
            let end = add(data_at, length)?;
            if end > tile_end {
                return Err(corrupt());
            }
            let data = data_at..end;
            data_at = add(end, padding)?;
            if data_at > tile_end {
                return Err(corrupt());
            }
            Some(data)
        } else {
            None
        };
        let mut planes = Vec::with_capacity(usize::from(config.planes));
        for p in 0..u32::from(config.planes) {
            let body = reader.marker(if modern { 0xff12 } else { 0xff02 }, 8)?;
            let size = be32(body, 0)? as usize;
            let flags = be32(body, 4)?;
            if flags >> 28 != p {
                return Err(corrupt());
            }
            let plane_end = add(data_at, size)?;
            if plane_end > tile_end {
                return Err(corrupt());
            }
            let mut bands = Vec::with_capacity(band_count);
            for b in 0..band_count {
                let body = reader.marker(if modern { 0xff13 } else { 0xff03 }, if modern { 16 } else { 8 })?;
                let band_size = be32(body, 0)? as usize;
                let band_flags = be32(body, 4)?;
                if (band_flags >> 28) as usize != b {
                    return Err(corrupt());
                }
                let band_end = add(data_at, band_size)?;
                if band_end > plane_end {
                    return Err(corrupt());
                }
                let padding = if modern { usize::from(be16(body, 12)?) } else { (band_flags & 0x7ffff) as usize };
                if padding > band_size || (modern && padding > 7) {
                    return Err(corrupt());
                }
                let quant = if modern {
                    if be16(body, 14)? != 0 {
                        return Err(unsupported());
                    }
                    Quantization::Adaptive { base: be32(body, 8)?, gain: band_flags & 0x07ff_ffff }
                } else {
                    Quantization::Uniform(((band_flags >> 19) & 0xff) as u8)
                };
                bands.push(Band { data: data_at..band_end, padding, quant, partial: band_flags & 0x0800_0000 != 0 });
                data_at = band_end;
            }
            if data_at != plane_end {
                return Err(corrupt());
            }
            planes.push(Plane { partial: flags & 0x0800_0000 != 0, rounded_bits: ((flags >> 25) & 3) as u8, bands });
        }
        if data_at != tile_end {
            return Err(corrupt());
        }
        let x = mul(index.checked_rem(columns).ok_or_else(corrupt)?, tw)?;
        let y = mul(index.checked_div(columns).ok_or_else(corrupt)?, th)?;
        tiles.push(Tile { x, y, width: tw.min(sub(width, x)?), height: th.min(sub(height, y)?), planes, qp });
    }
    if header.get(reader.at..).is_none_or(|rest| rest.iter().any(|&byte| byte != 0)) {
        return Err(corrupt());
    }
    if data_at != sample.len() {
        return Err(corrupt());
    }
    Ok(tiles)
}

fn band_bytes<'a>(sample: &'a [u8], band: &Band) -> Result<&'a [u8], DevelopError> {
    let end = sub(band.data.end, band.padding)?;
    if end < band.data.start {
        return Err(corrupt());
    }
    sample.get(band.data.start..end).ok_or_else(corrupt)
}

fn tile_qp(sample: &[u8], tile: &Tile) -> Result<Option<QpMap>, DevelopError> {
    match &tile.qp {
        Some(range) => {
            let bytes = sample.get(range.clone()).ok_or_else(corrupt)?;
            Ok(Some(qp_map(bytes, tile.width.div_ceil(16), tile.height.div_ceil(4))?))
        }
        None => Ok(None),
    }
}

/// Uniform quantisation step for quantValue 4..=44 (LightCraft's measured integer sequence).
fn quant_step(value: u8) -> Result<i32, DevelopError> {
    if !(4..=44).contains(&value) {
        return Err(unsupported());
    }
    let scale = *[40i32, 45, 51, 57, 64, 72].get(usize::from(value % 6)).ok_or_else(corrupt)?;
    Ok((scale << (value / 6)) / 64)
}

fn wavelet_plane(sample: &[u8], plane: &Plane, width: usize, height: usize, edges: TileEdges, qp: Option<&QpMap>) -> Result<Vec<i32>, DevelopError> {
    let mut dimensions = vec![(width, height)];
    let mut high_dimensions = Vec::new();
    for _ in 0..3 {
        let &(w, h) = dimensions.last().ok_or_else(corrupt)?;
        let (lw, hw) = crx_wavelet::sizes(w, edges.left, edges.right);
        let (lh, hh) = crx_wavelet::sizes(h, edges.top, edges.bottom);
        dimensions.push((lw, lh));
        high_dimensions.push([(hw, lh), (lw, hh), (hw, hh)]);
    }
    let mut shapes = vec![*dimensions.last().ok_or_else(corrupt)?];
    for high in high_dimensions.iter().rev() {
        shapes.extend_from_slice(high);
    }
    let mut bands = Vec::with_capacity(10);
    for (i, (&(w, h), band)) in shapes.iter().zip(&plane.bands).enumerate() {
        let bytes = band_bytes(sample, band)?;
        let mut data = if i == 0 {
            lossless_plane(bytes, w, h)?
        } else {
            // Some high-frequency bands carry a redundant right column beyond synthesis support
            // (LightCraft: LH2 in the M50/R100 fixtures); accept up to 2 extra columns only from a
            // complete, aligned entropy stream, then crop each row.
            let mut decoded = None;
            for extra in 0..=2usize {
                let coded_width = add(w, extra)?;
                if let Ok(values) = high_frequency_band(bytes, coded_width, h) {
                    if extra == 0 {
                        decoded = Some(values);
                    } else {
                        let mut cropped = Vec::new();
                        cropped.try_reserve_exact(mul(w, h)?).map_err(|_| DevelopError::InvalidDimensions)?;
                        for row in values.chunks_exact(coded_width) {
                            cropped.extend_from_slice(row.get(..w).ok_or_else(corrupt)?);
                        }
                        decoded = Some(cropped);
                    }
                    break;
                }
            }
            decoded.ok_or_else(corrupt)?
        };
        for (position, value) in data.iter_mut().enumerate() {
            let step = match band.quant {
                Quantization::Uniform(q) => quant_step(q)?,
                Quantization::Adaptive { base, gain } => {
                    let x = position.checked_rem(w).ok_or_else(corrupt)?;
                    let y = position.checked_div(w).ok_or_else(corrupt)?;
                    adaptive_step(qp.ok_or_else(corrupt)?, i, x, y, base, gain)?
                }
            };
            *value = value.checked_mul(step).ok_or_else(corrupt)?;
        }
        bands.push(WaveletBand { width: w, height: h, data });
    }
    crx_wavelet::reconstruct(&bands, width, height, 3, edges)
}

fn adaptive_step(map: &QpMap, band: usize, x: usize, y: usize, base: u32, gain: u32) -> Result<i32, DevelopError> {
    let step = if gain == 0 {
        u64::from(base)
    } else {
        let (row, column) = if (4..=6).contains(&band) {
            (mul(y, 2)?, x / 2)
        } else if (7..=9).contains(&band) {
            (y, x / 4)
        } else {
            return Err(unsupported());
        };
        if row >= map.height || column >= map.width {
            return Err(corrupt());
        }
        let first = *map.data.get(add(mul(row, map.width)?, column)?).ok_or_else(corrupt)?;
        let value = if band < 7 {
            // An odd-height map leaves the last row of the coarser bands without a partner.
            let second_row = add(row, 1)?.min(sub(map.height, 1)?);
            let second = *map.data.get(add(mul(second_row, map.width)?, column)?).ok_or_else(corrupt)?;
            first.checked_add(second).ok_or_else(corrupt)? / 2
        } else {
            first
        };
        // QP rows are averaged before the integer step lookup.
        let quant = u8::try_from(value.checked_sub(124).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
        let q = u64::try_from(quant_step(quant)?).map_err(|_| corrupt())?;
        u64::from(base).saturating_add(u64::from(gain).saturating_mul(q) / 8)
    };
    i32::try_from(step.max(1)).map_err(|_| corrupt())
}

struct Bits<'a> {
    src: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    /// The next 32 bits, MSB first, when at least 40 bits remain before the end of the stream.
    fn peek32(&self) -> Option<u32> {
        let start = self.at / 8;
        let bytes = self.src.get(start..start.checked_add(5)?)?;
        let word = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
        u32::try_from((word >> 8u32.saturating_sub((self.at % 8) as u32)) & 0xffff_ffff).ok()
    }

    fn advance(&mut self, count: usize) -> Result<(), DevelopError> {
        self.at = add(self.at, count)?;
        Ok(())
    }

    fn take(&mut self, count: u32) -> Result<u32, DevelopError> {
        if count > 24 {
            return Err(corrupt());
        }
        if count == 0 {
            return Ok(0);
        }
        if let Some(window) = self.peek32() {
            self.advance(count as usize)?;
            return Ok(window >> 32u32.saturating_sub(count));
        }
        // Near the end of the stream: bit by bit, so truncation is reported exactly.
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self.src.get(self.at / 8).ok_or_else(corrupt)?;
            value = (value << 1) | u32::from((byte >> 7u32.saturating_sub((self.at % 8) as u32)) & 1);
            self.advance(1)?;
        }
        Ok(value)
    }

    fn rice(&mut self, k: u32) -> Result<u32, DevelopError> {
        if let Some(window) = self.peek32().filter(|&w| w != 0) {
            let zeros = window.leading_zeros();
            self.advance(add(zeros as usize, 1)?)?;
            return if k <= 20 { Ok((zeros << k) | self.take(k)?) } else { Err(corrupt()) };
        }
        let mut zeros = 0u32;
        while self.take(1)? == 0 {
            zeros = zeros.saturating_add(1);
            if zeros > 41 {
                return Err(corrupt());
            }
        }
        if zeros == 41 {
            self.take(21)
        } else if k <= 20 {
            Ok((zeros << k) | self.take(k)?)
        } else {
            Err(corrupt())
        }
    }

    /// T.87 limited-length code: 23 zeros, the terminating 1, then an 8-bit symbol.
    fn qp_rice(&mut self, k: u32) -> Result<u32, DevelopError> {
        let mut zeros = 0u32;
        while self.take(1)? == 0 {
            zeros = zeros.saturating_add(1);
            if zeros > QP_ESCAPE_ZEROS {
                return Err(corrupt());
            }
        }
        if zeros == QP_ESCAPE_ZEROS {
            self.take(8)
        } else if k <= 20 {
            Ok((zeros << k) | self.take(k)?)
        } else {
            Err(corrupt())
        }
    }

    /// The stream must end within its last byte with zero alignment bits.
    fn finish(&self) -> Result<(), DevelopError> {
        let bits = mul(self.src.len(), 8)?;
        let remaining = bits.checked_sub(self.at).ok_or_else(corrupt)?;
        if remaining > 7 {
            return Err(corrupt());
        }
        if let Some(&last) = self.src.last()
            && u32::from(last) & (1u32 << remaining).saturating_sub(1) != 0
        {
            return Err(corrupt());
        }
        Ok(())
    }
}

fn signed(symbol: u32) -> i32 {
    ((symbol >> 1) as i32) ^ (((symbol & 1) as i32).wrapping_neg())
}

fn update_rice(mut k: u32, symbol: u32) -> u32 {
    if symbol >= 3u32.checked_shl(k).unwrap_or(u32::MAX) {
        k = k.saturating_add(1);
        if symbol >= 3u32.checked_shl(k).unwrap_or(u32::MAX) {
            k = k.saturating_add(1);
        }
    } else if k > 0 && symbol < 1 << k.saturating_sub(1) {
        k = k.saturating_sub(1);
    }
    k.min(20)
}

fn median(left: i32, above: i32, corner: i32) -> i32 {
    if corner >= left.max(above) {
        left.min(above)
    } else if corner <= left.min(above) {
        left.max(above)
    } else {
        left.saturating_add(above).saturating_sub(corner)
    }
}

fn run(bits: &mut Bits<'_>, index: &mut usize, remaining: usize) -> Result<usize, DevelopError> {
    let mut length = 1usize;
    while length < remaining {
        let exponent = u32::from(*RUN_EXPONENT.get(*index).ok_or_else(corrupt)?);
        if bits.take(1)? == 0 {
            length = add(length, bits.take(exponent)? as usize)?;
            *index = index.saturating_sub(1);
            if length > remaining {
                return Err(corrupt());
            }
            break;
        }
        length = add(length, 1usize << exponent)?;
        if length <= remaining {
            *index = add(*index, 1)?.min(RUN_EXPONENT.len().saturating_sub(1));
        }
    }
    Ok(length.min(remaining))
}

/// Bounded row buffer.
fn row(width: usize) -> Result<Vec<i32>, DevelopError> {
    zeroed(width)
}

fn plane_count(width: usize, height: usize) -> Result<usize, DevelopError> {
    let count = mul(width, height)?;
    if width == 0 || count == 0 || count > MAX_SAMPLES / 4 {
        return Err(DevelopError::InvalidDimensions);
    }
    Ok(count)
}

fn lossless_plane(src: &[u8], width: usize, height: usize) -> Result<Vec<i32>, DevelopError> {
    plane_count(width, height)?;
    // Grows per decoded row: a truncated stream fails before the whole plane is reserved.
    let mut output = Vec::new();
    let mut previous = row(width)?;
    let mut current = row(width)?;
    let (mut k, mut run_index) = (0u32, 0usize);
    let mut bits = Bits { src, at: 0 };
    for y in 0..height {
        let mut x = 0usize;
        let mut left = *previous.first().ok_or_else(corrupt)?;
        while x < width {
            let mut interrupted = false;
            let above = *previous.get(x).ok_or_else(corrupt)?;
            let right = previous.get(add(x, 1)?).copied().unwrap_or(above);
            if add(x, 1)? < width && left == above && left == right && bits.take(1)? != 0 {
                let length = run(&mut bits, &mut run_index, sub(width, x)?)?;
                let end = add(x, length)?;
                current.get_mut(x..end).ok_or_else(corrupt)?.fill(left);
                x = end;
                if x == width {
                    continue;
                }
                // A run interruption always uses regular prediction from above; no second run flag.
                interrupted = true;
            }
            let above = *previous.get(x).ok_or_else(corrupt)?;
            let corner = if x == 0 { above } else { *previous.get(sub(x, 1)?).ok_or_else(corrupt)? };
            let right = previous.get(add(x, 1)?).copied().unwrap_or(above);
            let symbol = bits.rice(k)?;
            let prediction = if interrupted { above } else { median(left, above, corner) };
            let value = prediction.checked_add(signed(symbol)).filter(|v| (-1_048_576..=1_048_575).contains(v)).ok_or_else(corrupt)?;
            *current.get_mut(x).ok_or_else(corrupt)? = value;
            left = value;
            let estimate = if y > 0 && add(x, 1)? < width {
                symbol.checked_add(right.abs_diff(above).checked_mul(2).ok_or_else(corrupt)?).ok_or_else(corrupt)? / 2
            } else {
                symbol
            };
            k = update_rice(k, estimate);
            x = add(x, 1)?;
        }
        output.try_reserve(width).map_err(|_| DevelopError::InvalidDimensions)?;
        output.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
    }
    bits.finish()?;
    Ok(output)
}

fn high_frequency_band(src: &[u8], width: usize, height: usize) -> Result<Vec<i32>, DevelopError> {
    plane_count(width, height)?;
    let mut output = Vec::new();
    let (mut previous, mut current) = (row(width)?, row(width)?);
    let (mut previous_k, mut current_k) = (row(width)?, row(width)?);
    let (mut k, mut run_index) = (0u32, 0usize);
    let mut bits = Bits { src, at: 0 };
    for y in 0..height {
        let (mut x, mut left) = (0usize, 0i32);
        while x < width {
            let above = *previous.get(x).ok_or_else(corrupt)?;
            let right = previous.get(add(x, 1)?).copied().unwrap_or(above);
            let mut nonzero = false;
            if add(x, 1)? < width && left == 0 && above == 0 && right == 0 {
                if bits.take(1)? != 0 {
                    let length = run(&mut bits, &mut run_index, sub(width, x)?)?;
                    let end = add(x, length)?;
                    current.get_mut(x..end).ok_or_else(corrupt)?.fill(0);
                    // The upper-row Rice context of a zero run is zero; k is unchanged across it.
                    current_k.get_mut(x..end).ok_or_else(corrupt)?.fill(0);
                    left = 0;
                    x = end;
                    if x == width {
                        continue;
                    }
                }
                nonzero = true;
            }
            let coded = bits.rice(k)?;
            // A zero-run flag 0 or its interruption proves the coefficient is nonzero.
            let symbol = coded.checked_add(u32::from(nonzero)).ok_or_else(corrupt)?;
            let value = signed(symbol);
            if !(-1_048_576..=1_048_575).contains(&value) {
                return Err(corrupt());
            }
            *current.get_mut(x).ok_or_else(corrupt)? = value;
            left = value;
            k = update_rice(k, coded);
            if y > 0 && add(x, 1)? < width {
                let upper_right_k = u32::try_from(*previous_k.get(add(x, 1)?).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
                if upper_right_k > k.saturating_add(1) {
                    k = k.saturating_add(1);
                }
            }
            *current_k.get_mut(x).ok_or_else(corrupt)? = i32::try_from(k).map_err(|_| corrupt())?;
            x = add(x, 1)?;
        }
        output.try_reserve(width).map_err(|_| DevelopError::InvalidDimensions)?;
        output.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
        std::mem::swap(&mut current_k, &mut previous_k);
    }
    bits.finish()?;
    Ok(output)
}

fn qp_map(src: &[u8], width: usize, height: usize) -> Result<QpMap, DevelopError> {
    let count = mul(width, height)?;
    if width == 0 || count == 0 || count > MAX_SAMPLES / 32 {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut data = Vec::new();
    data.try_reserve_exact(count).map_err(|_| DevelopError::InvalidDimensions)?;
    let (mut previous, mut current) = (row(width)?, row(width)?);
    let mut bits = Bits { src, at: 0 };
    let mut k = 0u32;
    for y in 0..height {
        let mut left = *previous.first().ok_or_else(corrupt)?;
        for x in 0..width {
            let above = *previous.get(x).ok_or_else(corrupt)?;
            let corner = if x == 0 { above } else { *previous.get(sub(x, 1)?).ok_or_else(corrupt)? };
            let right = previous.get(add(x, 1)?).copied().unwrap_or(above);
            // The map opens with its first symbol as a 32-bit literal; later ones are Rice coded.
            let symbol = if x == 0 && y == 0 { (bits.take(16)? << 16) | bits.take(16)? } else { bits.qp_rice(k)? };
            let value = median(left, above, corner).checked_add(signed(symbol)).ok_or_else(corrupt)?;
            if !(131..=168).contains(&value) {
                return Err(unsupported());
            }
            *current.get_mut(x).ok_or_else(corrupt)? = value;
            left = value;
            let estimate = if y > 0 && add(x, 1)? < width {
                symbol.checked_add(right.abs_diff(above).checked_mul(2).ok_or_else(corrupt)?).ok_or_else(corrupt)? / 2
            } else {
                symbol
            };
            k = update_rice(k, estimate);
        }
        data.extend_from_slice(&current);
        std::mem::swap(&mut current, &mut previous);
    }
    bits.finish()?;
    Ok(QpMap { width, height, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coding() -> Coding {
        Coding {
            version: 0x200,
            width: 16,
            height: 16,
            tile_width: 16,
            tile_height: 16,
            bit_depth: 14,
            planes: 4,
            cfa_pattern: 0,
            encoding: 0,
            levels: 3,
            header_size: 872,
            median_bit_depth: None,
        }
    }

    fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
        out.extend(code.to_be_bytes());
        out.extend(8u16.to_be_bytes());
        out.extend(size.to_be_bytes());
        out.extend(flags.to_be_bytes());
    }

    /// Version 0x200 adaptive-QP C-RAW, 16x16, flat field (LightCraft's hand-built stream): every
    /// sample reconstructs to the midpoint 8192; every truncation is a typed refusal; the measured
    /// quantisation steps hold.
    #[test]
    fn adaptive_v200_craw_known_answer_and_truncation() {
        let mut sample = Vec::new();
        sample.extend(0xff11u16.to_be_bytes());
        sample.extend(16u16.to_be_bytes());
        sample.extend(60u32.to_be_bytes());
        sample.extend(0x4000u32.to_be_bytes());
        sample.extend(5u32.to_be_bytes());
        sample.extend(3u16.to_be_bytes());
        sample.extend(0u16.to_be_bytes());
        let gains = [0u32, 0, 0, 0, 4, 4, 8, 8, 8, 16];
        for plane in 0..4u32 {
            marker(&mut sample, 0xff12, 13, (plane << 28) | 0x0800_0000);
            for (band, &gain) in gains.iter().enumerate() {
                sample.extend(0xff13u16.to_be_bytes());
                sample.extend(16u16.to_be_bytes());
                sample.extend(if band >= 7 { 2u32 } else { 1 }.to_be_bytes());
                sample.extend((((band as u32) << 28) | gain).to_be_bytes());
                sample.extend(u32::from(band < 4).to_be_bytes());
                sample.extend([0u8; 4]);
            }
        }
        sample.extend([0u8; 4]);
        sample.extend([0, 0, 1, 0x2c, 0x94, 0, 0, 0]);
        for _ in 0..4 {
            sample.extend([0x80; 4]);
            sample.extend([0xf0; 3]);
            for _ in 0..3 {
                sample.extend([0xff, 0xf8]);
            }
        }
        assert_eq!(decode(&sample, &coding()).unwrap(), vec![8192; 256]);
        for end in 0..sample.len() {
            assert!(decode(&sample[..end], &coding()).is_err(), "prefix {end}");
        }
        for (quant, expected) in [(4, 1), (10, 2), (22, 8), (36, 40), (44, 102)] {
            assert_eq!(quant_step(quant).unwrap(), expected);
        }
        assert_eq!(quant_step(45).err(), Some(DevelopError::UnsupportedCompression));
        let mut huge = coding();
        (huge.width, huge.height, huge.tile_width, huge.tile_height) = (65_536, 2, 65_536, 2);
        assert_eq!(decode(&[], &huge).err(), Some(DevelopError::InvalidDimensions));
    }
}
