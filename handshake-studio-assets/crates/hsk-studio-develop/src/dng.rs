//! DNG container decode (own reader written from the public Adobe DNG specification): TIFF IFD
//! walk, CFA raw IFD selection, strip/tile layout, uncompressed 8/16-bit samples or lossless JPEG
//! (compression 7, via `ljpeg`; any other compression is `UnsupportedCompression`), optional
//! LinearizationTable, black/white levels, active area, colour tags. The decoded frame stays
//! immutable; `cfa_plane` cuts a bounded window into the STU-CON-043 plane type.
//!
//! Pattern anchor: the DNG CFAPattern is anchored at the stored raw image origin (TIFF/EP). An
//! ActiveArea whose origin is not a multiple of the pattern period is refused rather than guessed
//! (UNVERIFIED which anchor Adobe applies there).
// V4-01: this module decodes untrusted bytes; every integer operation is checked, saturating or
// bounded by an explicit validation above it.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::color::{self, CameraSolution, DngColor, Mat3};
use crate::ljpeg;
use crate::pipeline::{Cfa, CfaPattern, CfaPlane, Limits, PlaneInit, XTRANS};
use crate::recipe::{DevelopRecipe, ProfileBinding, as_shot_gain};
use crate::tiffep::{Entry, Ifd, Tiff};
use sha2::{Digest, Sha256};

pub const MAX_DNG_PIXELS: u64 = 1 << 28;
const MAX_BLOCKS: usize = 1 << 20;
const MAX_BLOCK_DIM: u32 = 65_535;

mod tag {
    pub const NEW_SUBFILE_TYPE: u16 = 254;
    pub const WIDTH: u16 = 256;
    pub const HEIGHT: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const CFA_REPEAT_PATTERN_DIM: u16 = 33421;
    pub const CFA_PATTERN: u16 = 33422;
    pub const DNG_VERSION: u16 = 50706;
    pub const DNG_BACKWARD_VERSION: u16 = 50707;
    pub const UNIQUE_CAMERA_MODEL: u16 = 50708;
    pub const CFA_PLANE_COLOR: u16 = 50710;
    pub const CFA_LAYOUT: u16 = 50711;
    pub const LINEARIZATION_TABLE: u16 = 50712;
    pub const BLACK_LEVEL_REPEAT_DIM: u16 = 50713;
    pub const BLACK_LEVEL: u16 = 50714;
    pub const BLACK_LEVEL_DELTA_H: u16 = 50715;
    pub const BLACK_LEVEL_DELTA_V: u16 = 50716;
    pub const WHITE_LEVEL: u16 = 50717;
    pub const ORIENTATION: u16 = 274;
    pub const DEFAULT_SCALE: u16 = 50718;
    pub const OPCODE_LIST_1: u16 = 51008;
    pub const OPCODE_LIST_2: u16 = 51009;
    pub const OPCODE_LIST_3: u16 = 51022;
    pub const DEFAULT_USER_CROP: u16 = 51125;
    pub const PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
    pub const PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50939;
    pub const PROFILE_LOOK_TABLE_DATA: u16 = 50982;
    pub const MASKED_AREAS: u16 = 50830;
    pub const DEFAULT_CROP_ORIGIN: u16 = 50719;
    pub const DEFAULT_CROP_SIZE: u16 = 50720;
    pub const COLOR_MATRIX_1: u16 = 50721;
    pub const COLOR_MATRIX_2: u16 = 50722;
    pub const CAMERA_CALIBRATION_1: u16 = 50723;
    pub const CAMERA_CALIBRATION_2: u16 = 50724;
    pub const ANALOG_BALANCE: u16 = 50727;
    pub const AS_SHOT_NEUTRAL: u16 = 50728;
    pub const AS_SHOT_WHITE_XY: u16 = 50729;
    pub const BASELINE_EXPOSURE: u16 = 50730;
    pub const CALIBRATION_ILLUMINANT_1: u16 = 50778;
    pub const CALIBRATION_ILLUMINANT_2: u16 = 50779;
    pub const ACTIVE_AREA: u16 = 50829;
    pub const FORWARD_MATRIX_1: u16 = 50964;
    pub const FORWARD_MATRIX_2: u16 = 50965;
}

const PHOTOMETRIC_CFA: u32 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveArea {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
}

/// Window into the stored raw frame, in raw image coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneWindow {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Out-of-range handling when cutting a plane. Real sensors report noise below black, so a
/// clamp is available, but it is explicit and counted; `Refuse` follows STU-CON-043 exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangePolicy {
    Refuse,
    ClampReported,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowReport {
    pub clamped_below: u64,
    pub clamped_above: u64,
    /// A fractional BlackLevel was rounded to the nearest integer.
    pub black_rounded: bool,
    /// Per-site black levels differed inside one CFA channel and were normalised to the
    /// channel's first-site black.
    pub black_site_adjusted: bool,
}

#[derive(Clone, Debug)]
pub struct DngRaw {
    pub width: u32,
    pub height: u32,
    pub bits_per_sample: u16,
    pub cfa_pattern: CfaPattern,
    pub active_area: ActiveArea,
    pub default_crop: Option<PlaneWindow>,
    pub black_repeat: (u32, u32),
    pub black_level: Vec<f64>,
    /// BlackLevelDeltaH: one offset per active-area column (empty when absent).
    pub black_delta_h: Vec<f64>,
    /// BlackLevelDeltaV: one offset per active-area row (empty when absent).
    pub black_delta_v: Vec<f64>,
    pub white_level: u32,
    pub linearized: bool,
    pub color: DngColor,
    pub unique_camera_model: Option<String>,
    /// SHA-256 of the complete container bytes.
    pub container_sha256: [u8; 32],
    /// Features present in the file that this reader does not apply (never silently dropped):
    /// OpcodeList1-3, DefaultScale, DefaultUserCrop, ProfileHueSatMap/LookTable, MaskedAreas,
    /// non-identity Orientation. See [`DngRaw::require_complete`].
    pub unapplied: Vec<&'static str>,
    samples: Vec<u16>,
}

/// Whether BaselineExposure (EV, camera-vendor rendering offset) is carried into the recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineExposure {
    Ignore,
    Apply,
}

fn first_u32(tiff: &Tiff<'_>, e: &Entry) -> Result<u32, DevelopError> {
    tiff.u32s(e)?.first().copied().ok_or(DevelopError::InvalidContainer)
}

struct Lookup<'a> {
    raw: &'a Ifd,
    root: &'a Ifd,
}

impl<'a> Lookup<'a> {
    /// Raw-IFD-scoped tags never fall back to IFD0 (a raw SubIFD must not inherit the preview's
    /// layout, levels or CFA description); colour, version and model tags may.
    fn get(&self, tag: u16) -> Option<&'a Entry> {
        let raw_scoped = matches!(
            tag,
            256..=259 | 262 | 273 | 277..=279 | 322..=325 | 339 | 33421 | 33422 | 50710..=50717
                | 50719 | 50720 | 50829 | 50830
        );
        let found = self.raw.find(tag);
        if raw_scoped {
            found
        } else {
            found.or_else(|| self.root.find(tag))
        }
    }
}

fn matrix(tiff: &Tiff<'_>, entry: Option<&Entry>) -> Result<Option<Mat3>, DevelopError> {
    match entry {
        None => Ok(None),
        Some(e) => {
            let v = tiff.f64s(e)?;
            let m: Mat3 = v.try_into().map_err(|_| DevelopError::UnsupportedContainer)?; // 3-plane only
            Ok(Some(m))
        }
    }
}

fn triple(tiff: &Tiff<'_>, entry: Option<&Entry>) -> Result<Option<[f64; 3]>, DevelopError> {
    match entry {
        None => Ok(None),
        Some(e) => {
            let v = tiff.f64s(e)?;
            Ok(Some(v.try_into().map_err(|_| DevelopError::UnsupportedContainer)?))
        }
    }
}

fn parse_color(tiff: &Tiff<'_>, l: &Lookup<'_>) -> Result<DngColor, DevelopError> {
    let mut color = DngColor::empty();
    color.color_matrix = [
        matrix(tiff, l.get(tag::COLOR_MATRIX_1))?,
        matrix(tiff, l.get(tag::COLOR_MATRIX_2))?,
    ];
    color.forward_matrix = [
        matrix(tiff, l.get(tag::FORWARD_MATRIX_1))?,
        matrix(tiff, l.get(tag::FORWARD_MATRIX_2))?,
    ];
    color.camera_calibration = [
        matrix(tiff, l.get(tag::CAMERA_CALIBRATION_1))?,
        matrix(tiff, l.get(tag::CAMERA_CALIBRATION_2))?,
    ];
    for (slot, t) in color
        .calibration_illuminant
        .iter_mut()
        .zip([tag::CALIBRATION_ILLUMINANT_1, tag::CALIBRATION_ILLUMINANT_2])
    {
        if let Some(e) = l.get(t) {
            *slot = Some(u16::try_from(first_u32(tiff, e)?).map_err(|_| DevelopError::InvalidContainer)?);
        }
    }
    if let Some(ab) = triple(tiff, l.get(tag::ANALOG_BALANCE))? {
        color.analog_balance = ab;
    }
    color.as_shot_neutral = triple(tiff, l.get(tag::AS_SHOT_NEUTRAL))?;
    if let Some(e) = l.get(tag::AS_SHOT_WHITE_XY) {
        let v = tiff.f64s(e)?;
        color.as_shot_white_xy = Some(v.try_into().map_err(|_| DevelopError::UnsupportedContainer)?);
    }
    if let Some(e) = l.get(tag::BASELINE_EXPOSURE) {
        color.baseline_exposure = *tiff.f64s(e)?.first().ok_or(DevelopError::InvalidContainer)?;
    }
    Ok(color)
}

fn parse_cfa(tiff: &Tiff<'_>, l: &Lookup<'_>) -> Result<CfaPattern, DevelopError> {
    if let Some(e) = l.get(tag::CFA_PLANE_COLOR)
        && tiff.bytes(e)? != [0u8, 1, 2]
    {
        return Err(DevelopError::UnsupportedPattern);
    }
    if let Some(e) = l.get(tag::CFA_LAYOUT)
        && first_u32(tiff, e)? != 1
    {
        return Err(DevelopError::UnsupportedPattern);
    }
    let dim = tiff.u32s(l.get(tag::CFA_REPEAT_PATTERN_DIM).ok_or(DevelopError::InvalidContainer)?)?;
    let pattern = tiff.bytes(l.get(tag::CFA_PATTERN).ok_or(DevelopError::InvalidContainer)?)?;
    match dim.as_slice() {
        [2, 2] if pattern.len() == 4 => {
            use crate::pipeline::Bayer::*;
            Ok(CfaPattern::Bayer(match pattern {
                [0, 1, 1, 2] => Rggb,
                [2, 1, 1, 0] => Bggr,
                [1, 0, 2, 1] => Grbg,
                [1, 2, 0, 1] => Gbrg,
                _ => return Err(DevelopError::UnsupportedPattern),
            }))
        }
        [6, 6] if pattern.len() == 36 => {
            let same = (0..36).all(|i| pattern[i] == XTRANS[i / 6][i % 6]);
            if same {
                Ok(CfaPattern::XTrans)
            } else {
                Err(DevelopError::UnsupportedPattern)
            }
        }
        [_, _] => Err(DevelopError::UnsupportedPattern),
        _ => Err(DevelopError::InvalidContainer),
    }
}

fn read_sample(bytes: &[u8], bits: u16, big_endian: bool) -> u16 {
    if bits == 8 {
        u16::from(bytes[0])
    } else if big_endian {
        u16::from_be_bytes([bytes[0], bytes[1]])
    } else {
        u16::from_le_bytes([bytes[0], bytes[1]])
    }
}

struct Layout {
    /// Block size in samples (strips: full width x rows; tiles: tile size).
    block_w: u32,
    block_h: u32,
    across: u32,
    down: u32,
    offsets: Vec<u32>,
    counts: Vec<u32>,
    tiled: bool,
}

fn layout(tiff: &Tiff<'_>, l: &Lookup<'_>, width: u32, height: u32) -> Result<Layout, DevelopError> {
    let ceil = |a: u32, b: u32| a.div_ceil(b);
    let layout = if let Some(tw) = l.get(tag::TILE_WIDTH) {
        let block_w = first_u32(tiff, tw)?;
        let block_h = first_u32(tiff, l.get(tag::TILE_LENGTH).ok_or(DevelopError::InvalidContainer)?)?;
        if block_w == 0
            || block_h == 0
            || block_w > MAX_BLOCK_DIM
            || block_h > MAX_BLOCK_DIM
            || u64::from(block_w).saturating_mul(u64::from(block_h)) > MAX_DNG_PIXELS
        {
            return Err(DevelopError::InvalidContainer);
        }
        Layout {
            block_w,
            block_h,
            across: ceil(width, block_w),
            down: ceil(height, block_h),
            offsets: tiff.u32s(l.get(tag::TILE_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?,
            counts: tiff.u32s(l.get(tag::TILE_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?,
            tiled: true,
        }
    } else {
        let rows = match l.get(tag::ROWS_PER_STRIP) {
            Some(e) => first_u32(tiff, e)?.min(height),
            None => height,
        };
        if rows == 0 {
            return Err(DevelopError::InvalidContainer);
        }
        Layout {
            block_w: width,
            block_h: rows,
            across: 1,
            down: ceil(height, rows),
            offsets: tiff.u32s(l.get(tag::STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?,
            counts: tiff.u32s(l.get(tag::STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?,
            tiled: false,
        }
    };
    let blocks = (layout.across as usize).saturating_mul(layout.down as usize);
    if blocks > MAX_BLOCKS || layout.offsets.len() != blocks || layout.counts.len() != blocks {
        return Err(DevelopError::InvalidContainer);
    }
    Ok(layout)
}

/// Decode a DNG container into an immutable raw frame.
pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let (tiff, first) = Tiff::parse(bytes)?;
    let ifds = tiff.walk(first)?;
    let root = ifds.first().ok_or(DevelopError::InvalidContainer)?;

    let version = tiff.bytes(root.find(tag::DNG_VERSION).ok_or(DevelopError::UnsupportedContainer)?)?;
    if version.len() != 4 || version[0] != 1 {
        return Err(DevelopError::UnsupportedContainer);
    }
    if let Some(e) = root.find(tag::DNG_BACKWARD_VERSION) {
        let backward = tiff.bytes(e)?;
        if backward.len() != 4 || backward > &[1u8, 7, 1, 0][..] {
            return Err(DevelopError::UnsupportedContainer);
        }
    }

    let mut linear_raw = false;
    let mut found = None;
    for ifd in &ifds {
        let photometric = match ifd.find(tag::PHOTOMETRIC) {
            Some(e) => first_u32(&tiff, e)?,
            None => continue,
        };
        let subfile = match ifd.find(tag::NEW_SUBFILE_TYPE) {
            Some(e) => first_u32(&tiff, e)?,
            None => 0,
        };
        if photometric == PHOTOMETRIC_LINEAR_RAW {
            linear_raw = true;
        }
        if photometric == PHOTOMETRIC_CFA && subfile == 0 {
            found = Some(ifd);
            break;
        }
    }
    let raw_ifd = match found {
        Some(ifd) => ifd,
        None if linear_raw => return Err(DevelopError::UnsupportedContainer),
        None => return Err(DevelopError::InvalidContainer),
    };
    let l = Lookup { raw: raw_ifd, root };
    let req = |t: u16| l.get(t).ok_or(DevelopError::InvalidContainer);

    let width = first_u32(&tiff, req(tag::WIDTH)?)?;
    let height = first_u32(&tiff, req(tag::HEIGHT)?)?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    if let Some(e) = l.get(tag::SAMPLES_PER_PIXEL)
        && first_u32(&tiff, e)? != 1
    {
        return Err(DevelopError::UnsupportedContainer);
    }
    if let Some(e) = l.get(tag::SAMPLE_FORMAT)
        && first_u32(&tiff, e)? != 1
    {
        return Err(DevelopError::UnsupportedContainer);
    }
    let bits = u16::try_from(first_u32(&tiff, req(tag::BITS_PER_SAMPLE)?)?)
        .map_err(|_| DevelopError::InvalidContainer)?;
    let compression = match l.get(tag::COMPRESSION) {
        Some(e) => first_u32(&tiff, e)?,
        None => 1,
    };
    let lossless_jpeg = match compression {
        1 => false,
        7 => true,
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    let bits_ok = if lossless_jpeg {
        (2..=16).contains(&bits)
    } else {
        bits == 8 || bits == 16
    };
    if !bits_ok {
        return Err(DevelopError::UnsupportedContainer);
    }
    let cfa_pattern = parse_cfa(&tiff, &l)?;

    // Strip / tile payloads: uncompressed byte-aligned samples in file byte order, or one
    // lossless-JPEG stream per block (components interleave along the block row).
    let layout = layout(&tiff, &l, width, height)?;
    let bytes_per_sample = u64::from(bits.div_ceil(8));
    // Rows in block row `block_row` (the last strip may be short; tiles are always full height).
    let block_height = |block_row: u32| {
        if layout.tiled {
            layout.block_h
        } else {
            layout.block_h.min(height.saturating_sub(block_row.saturating_mul(layout.block_h)))
        }
    };
    let block_index = |block_row: u32, block_col: u32| -> Result<usize, DevelopError> {
        block_row
            .checked_mul(layout.across)
            .and_then(|v| v.checked_add(block_col))
            .map(|v| v as usize)
            .ok_or(DevelopError::InvalidContainer)
    };
    // Checked payload size of one uncompressed block (hostile tile sizes must not wrap).
    let uncompressed_len = |block_row: u32| -> Result<u64, DevelopError> {
        u64::from(layout.block_w)
            .checked_mul(u64::from(block_height(block_row)))
            .and_then(|v| v.checked_mul(bytes_per_sample))
            .ok_or(DevelopError::InvalidContainer)
    };
    // Every block must lie inside the file before the frame is allocated, so a tiny file cannot
    // force a large allocation.
    for block_row in 0..layout.down {
        for block_col in 0..layout.across {
            let index = block_index(block_row, block_col)?;
            let stream = tiff.region(u64::from(layout.offsets[index]), u64::from(layout.counts[index]))?;
            if !lossless_jpeg && (stream.len() as u64) < uncompressed_len(block_row)? {
                return Err(DevelopError::InvalidContainer);
            }
        }
    }
    // The declared frame must be supported by the payload actually present, in every compression
    // (review R5: a ~300-byte lossless-JPEG file declaring a 2^28-sample frame forced a 512 MiB
    // allocation). Lossless JPEG spends at least one bit per sample, uncompressed data exactly
    // `bytes_per_sample`; the payload is the sum of the block byte counts capped by the file
    // length, so blocks that all point at the same bytes add nothing.
    let payload = layout
        .counts
        .iter()
        .map(|count| u64::from(*count))
        .fold(0u64, u64::saturating_add)
        .min(bytes.len() as u64);
    let needed = if lossless_jpeg {
        pixels.div_ceil(8)
    } else {
        pixels.saturating_mul(bytes_per_sample)
    };
    if needed > payload {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut samples = vec![0u16; usize::try_from(pixels).map_err(|_| DevelopError::InvalidDimensions)?];
    for block_row in 0..layout.down {
        for block_col in 0..layout.across {
            let index = block_index(block_row, block_col)?;
            let block_h = block_height(block_row);
            let block_w = layout.block_w as usize;
            let decoded: Vec<u16>;
            let uncompressed: &[u8];
            if lossless_jpeg {
                let stream = tiff.region(u64::from(layout.offsets[index]), u64::from(layout.counts[index]))?;
                let max_samples = block_w
                    .checked_mul(block_h as usize)
                    .ok_or(DevelopError::InvalidDimensions)?;
                let image = ljpeg::decode(stream, max_samples)?;
                // The stream carries the block's samples in raster order. Its SOF3 frame may split
                // them differently (e.g. MLV App: a 1920x818 strip as 3840 samples x 409 lines x 1
                // component, two block rows per JPEG row; or 2 components of half width): accepted
                // when the decoded sample count is exactly the block's and JPEG rows and block rows
                // tile each other in whole rows. Anything else is refused, never re-shaped.
                let jpeg_row = (image.width as usize)
                    .checked_mul(image.components)
                    .ok_or(DevelopError::InvalidContainer)?;
                let jpeg_total = jpeg_row
                    .checked_mul(image.height as usize)
                    .ok_or(DevelopError::InvalidContainer)?;
                let whole_rows = jpeg_row != 0 && (jpeg_row.is_multiple_of(block_w) || block_w.is_multiple_of(jpeg_row));
                if jpeg_total != max_samples || image.samples.len() != max_samples || !whole_rows {
                    return Err(DevelopError::InvalidContainer);
                }
                // A decoded value above the stream's own precision means a corrupt stream.
                let shift = 16u32
                    .checked_sub(u32::from(image.precision))
                    .ok_or(DevelopError::InvalidContainer)?;
                let ceiling = u32::from(u16::MAX)
                    .checked_shr(shift)
                    .ok_or(DevelopError::InvalidContainer)?;
                if image.samples.iter().any(|v| u32::from(*v) > ceiling) {
                    return Err(DevelopError::InvalidContainer);
                }
                decoded = image.samples;
                uncompressed = &[];
            } else {
                let expected = uncompressed_len(block_row)?;
                if u64::from(layout.counts[index]) < expected {
                    return Err(DevelopError::InvalidContainer);
                }
                decoded = Vec::new();
                uncompressed = tiff.region(u64::from(layout.offsets[index]), expected)?;
            }
            let x0 = block_col.saturating_mul(layout.block_w);
            let y0 = block_row.saturating_mul(layout.block_h);
            // Columns of this block that fall inside the frame (edge blocks are padded).
            let cols = layout.block_w.min(width.saturating_sub(x0)) as usize;
            for by in 0..block_h {
                let y = y0.saturating_add(by);
                if y >= height {
                    break;
                }
                let row_at = (by as usize).checked_mul(block_w).ok_or(DevelopError::Overflow)?;
                let dst = (y as usize)
                    .checked_mul(width as usize)
                    .and_then(|v| v.checked_add(x0 as usize))
                    .ok_or(DevelopError::Overflow)?;
                let out = dst
                    .checked_add(cols)
                    .and_then(|end| samples.get_mut(dst..end))
                    .ok_or(DevelopError::InvalidContainer)?;
                if lossless_jpeg {
                    let src = row_at
                        .checked_add(cols)
                        .and_then(|end| decoded.get(row_at..end))
                        .ok_or(DevelopError::InvalidContainer)?;
                    out.copy_from_slice(src);
                } else {
                    let bps = bytes_per_sample as usize;
                    let start = row_at.checked_mul(bps).ok_or(DevelopError::Overflow)?;
                    let src = cols
                        .checked_mul(bps)
                        .and_then(|len| start.checked_add(len))
                        .and_then(|end| uncompressed.get(start..end))
                        .ok_or(DevelopError::InvalidContainer)?;
                    for (slot, sample) in out.iter_mut().zip(src.chunks_exact(bps)) {
                        *slot = read_sample(sample, bits, tiff.is_big_endian());
                    }
                }
            }
        }
    }

    // Optional linearization table: refuses a raw value beyond the table (no implicit clamp).
    let mut linearized = false;
    if let Some(e) = l.get(tag::LINEARIZATION_TABLE) {
        let table = tiff.u32s(e)?;
        for s in samples.iter_mut() {
            let mapped = table.get(usize::from(*s)).ok_or(DevelopError::SampleOutOfRange)?;
            *s = u16::try_from(*mapped).map_err(|_| DevelopError::InvalidContainer)?;
        }
        linearized = true;
    }

    let black_repeat = match l.get(tag::BLACK_LEVEL_REPEAT_DIM) {
        Some(e) => match tiff.u32s(e)?.as_slice() {
            [r, c] if *r > 0 && *c > 0 && *r <= 16 && *c <= 16 => (*r, *c),
            _ => return Err(DevelopError::InvalidContainer),
        },
        None => (1, 1),
    };
    let black_level = match l.get(tag::BLACK_LEVEL) {
        Some(e) => tiff.f64s(e)?,
        None => vec![0.0],
    };
    if black_level.len() != (black_repeat.0 as usize).saturating_mul(black_repeat.1 as usize) {
        return Err(DevelopError::InvalidContainer);
    }
    let white_level = match l.get(tag::WHITE_LEVEL) {
        Some(e) => first_u32(&tiff, e)?,
        None if !linearized => 1u32
            .checked_shl(u32::from(bits))
            .and_then(|v| v.checked_sub(1))
            .ok_or(DevelopError::InvalidContainer)?,
        None => return Err(DevelopError::InvalidContainer),
    };
    if white_level == 0 || white_level > u32::from(u16::MAX) {
        return Err(DevelopError::UnsupportedContainer);
    }

    let active_area = match l.get(tag::ACTIVE_AREA) {
        Some(e) => match tiff.u32s(e)?.as_slice() {
            [top, left, bottom, right] => ActiveArea {
                top: *top,
                left: *left,
                bottom: *bottom,
                right: *right,
            },
            _ => return Err(DevelopError::InvalidContainer),
        },
        None => ActiveArea {
            top: 0,
            left: 0,
            bottom: height,
            right: width,
        },
    };
    if active_area.top >= active_area.bottom
        || active_area.left >= active_area.right
        || active_area.bottom > height
        || active_area.right > width
    {
        return Err(DevelopError::InvalidDimensions);
    }
    let period = match cfa_pattern {
        CfaPattern::Bayer(_) => 2,
        CfaPattern::XTrans => 6,
    };
    if !active_area.top.is_multiple_of(period) || !active_area.left.is_multiple_of(period) {
        return Err(DevelopError::UnsupportedActiveArea);
    }

    // BlackLevelDeltaH / DeltaV: per active-area column / row offsets added to the repeating
    // BlackLevel (DNG spec). Lengths must match the active area exactly.
    let delta = |t: u16, len: u32| -> Result<Vec<f64>, DevelopError> {
        match l.get(t) {
            None => Ok(Vec::new()),
            Some(e) => {
                let v = tiff.f64s(e)?;
                if v.len() == len as usize {
                    Ok(v)
                } else {
                    Err(DevelopError::InvalidContainer)
                }
            }
        }
    };
    let black_delta_h = delta(tag::BLACK_LEVEL_DELTA_H, active_area.right.saturating_sub(active_area.left))?;
    let black_delta_v = delta(tag::BLACK_LEVEL_DELTA_V, active_area.bottom.saturating_sub(active_area.top))?;

    let default_crop = match (l.get(tag::DEFAULT_CROP_ORIGIN), l.get(tag::DEFAULT_CROP_SIZE)) {
        (Some(o), Some(s)) => {
            let (o, s) = (tiff.f64s(o)?, tiff.f64s(s)?);
            let whole = |v: f64| (v >= 0.0 && v.fract() == 0.0 && v <= 65_535.0).then_some(v as u32);
            match (o.as_slice(), s.as_slice()) {
                ([ox, oy], [sw, sh]) => {
                    let (ox, oy, sw, sh) = (
                        whole(*ox).ok_or(DevelopError::UnsupportedContainer)?,
                        whole(*oy).ok_or(DevelopError::UnsupportedContainer)?,
                        whole(*sw).ok_or(DevelopError::UnsupportedContainer)?,
                        whole(*sh).ok_or(DevelopError::UnsupportedContainer)?,
                    );
                    let window = PlaneWindow {
                        x: active_area.left.saturating_add(ox),
                        y: active_area.top.saturating_add(oy),
                        width: sw,
                        height: sh,
                    };
                    if sw == 0
                        || sh == 0
                        || window.x.saturating_add(sw) > active_area.right
                        || window.y.saturating_add(sh) > active_area.bottom
                    {
                        return Err(DevelopError::InvalidDimensions);
                    }
                    Some(window)
                }
                _ => return Err(DevelopError::InvalidContainer),
            }
        }
        _ => None,
    };

    let color = parse_color(&tiff, &l)?;
    let unique_camera_model = match l.get(tag::UNIQUE_CAMERA_MODEL) {
        Some(e) => Some(tiff.ascii(e)?),
        None => None,
    };
    let mut unapplied: Vec<&'static str> = [
        (tag::OPCODE_LIST_1, "opcode_list_1"),
        (tag::OPCODE_LIST_2, "opcode_list_2"),
        (tag::OPCODE_LIST_3, "opcode_list_3"),
        (tag::DEFAULT_USER_CROP, "default_user_crop"),
        (tag::PROFILE_HUE_SAT_MAP_DATA_1, "profile_hue_sat_map_1"),
        (tag::PROFILE_HUE_SAT_MAP_DATA_2, "profile_hue_sat_map_2"),
        (tag::PROFILE_LOOK_TABLE_DATA, "profile_look_table"),
        (tag::MASKED_AREAS, "masked_areas"),
    ]
    .into_iter()
    .filter_map(|(t, name)| l.get(t).map(|_| name))
    .collect();
    if let Some(e) = l.get(tag::DEFAULT_SCALE)
        && tiff.f64s(e)? != [1.0, 1.0]
    {
        unapplied.push("default_scale");
    }
    if let Some(e) = l.get(tag::ORIENTATION)
        && first_u32(&tiff, e)? != 1
    {
        unapplied.push("orientation");
    }
    Ok(DngRaw {
        width,
        height,
        bits_per_sample: bits,
        cfa_pattern,
        active_area,
        default_crop,
        black_repeat,
        black_level,
        black_delta_h,
        black_delta_v,
        white_level,
        linearized,
        color,
        unique_camera_model,
        unapplied,
        container_sha256: Sha256::digest(bytes).into(),
        samples,
    })
}

/// A whole stored CFA frame produced by a proprietary container decoder (`cr2`, ...). The frame
/// carries no colour, black 0 and a placeholder white (`white_level`, default
/// [`VendorFrame::full_scale`]); every level, crop or pattern
/// fact the vendor decoder did not read from the file is named in `unapplied`, so
/// [`DngRaw::require_complete`] refuses it and nothing reads as an authored value.
pub(crate) struct VendorFrame {
    pub width: u32,
    pub height: u32,
    pub bits: u16,
    pub cfa_pattern: CfaPattern,
    pub unique_camera_model: Option<String>,
    pub unapplied: Vec<&'static str>,
    pub samples: Vec<u16>,
    /// Largest code value the frame may hold (every sample is checked against it).
    pub white_level: u32,
}

impl VendorFrame {
    /// Full-scale white of `bits` (`2^bits - 1`), `None` outside 1..=16 bits.
    pub fn full_scale(bits: u16) -> Option<u32> {
        1u32.checked_shl(u32::from(bits))
            .and_then(|v| v.checked_sub(1))
            .filter(|v| (1..=u32::from(u16::MAX)).contains(v))
    }
}

impl DngRaw {
    /// Build the immutable raw frame from a vendor decoder's output; `container` is the complete
    /// file (hashed exactly as for DNG). Dimensions, bit depth and sample count are re-validated.
    pub(crate) fn from_vendor(frame: VendorFrame, container: &[u8]) -> Result<Self, DevelopError> {
        let pixels = u64::from(frame.width).saturating_mul(u64::from(frame.height));
        if frame.width == 0
            || frame.height == 0
            || frame.width > 65_535
            || frame.height > 65_535
            || pixels > MAX_DNG_PIXELS
            || frame.samples.len() as u64 != pixels
        {
            return Err(DevelopError::InvalidDimensions);
        }
        let white_level = frame.white_level;
        if !(1..=u32::from(u16::MAX)).contains(&white_level) || !(1..=16).contains(&frame.bits) {
            return Err(DevelopError::UnsupportedContainer);
        }
        if frame.samples.iter().any(|v| u32::from(*v) > white_level) {
            return Err(DevelopError::InvalidContainer);
        }
        Ok(Self {
            width: frame.width,
            height: frame.height,
            bits_per_sample: frame.bits,
            cfa_pattern: frame.cfa_pattern,
            active_area: ActiveArea {
                top: 0,
                left: 0,
                bottom: frame.height,
                right: frame.width,
            },
            default_crop: None,
            black_repeat: (1, 1),
            black_level: vec![0.0],
            black_delta_h: Vec::new(),
            black_delta_v: Vec::new(),
            white_level,
            linearized: false,
            color: DngColor::empty(),
            unique_camera_model: frame.unique_camera_model,
            container_sha256: Sha256::digest(container).into(),
            unapplied: frame.unapplied,
            samples: frame.samples,
        })
    }

    /// Raw sample after the optional linearization table (before black subtraction).
    pub fn sample(&self, x: u32, y: u32) -> u16 {
        self.samples[(y as usize).saturating_mul(self.width as usize).saturating_add(x as usize)]
    }

    fn channel_at(&self, x: u32, y: u32) -> u8 {
        match self.cfa_pattern {
            CfaPattern::Bayer(b) => b.cell()[(y % 2) as usize][(x % 2) as usize],
            CfaPattern::XTrans => XTRANS[(y % 6) as usize][(x % 6) as usize],
        }
    }

    fn black_at(&self, x: u32, y: u32) -> f64 {
        let (rr, rc) = self.black_repeat;
        // Callers pass sites inside the active area and the repeat dims are 1..=16 (validated at decode).
        let dy = y.saturating_sub(self.active_area.top);
        let dx = x.saturating_sub(self.active_area.left);
        let row = dy.checked_rem(rr).unwrap_or(0);
        let col = dx.checked_rem(rc).unwrap_or(0);
        let mut black = self.black_level[row.saturating_mul(rc).saturating_add(col) as usize];
        if let Some(d) = self.black_delta_h.get(dx as usize) {
            black += d;
        }
        if let Some(d) = self.black_delta_v.get(dy as usize) {
            black += d;
        }
        black
    }

    /// Default crop window (raw coordinates) when authored, else the whole active area.
    pub fn default_window(&self) -> PlaneWindow {
        self.default_crop.unwrap_or(PlaneWindow {
            x: self.active_area.left,
            y: self.active_area.top,
            width: self.active_area.right.saturating_sub(self.active_area.left),
            height: self.active_area.bottom.saturating_sub(self.active_area.top),
        })
    }

    /// Cut a window into a CFA plane (u16le, stride = 2*width, phase 0, sensor origin = window
    /// origin so the pattern stays anchored at the stored frame origin).
    pub fn cfa_plane(
        &self,
        window: PlaneWindow,
        policy: RangePolicy,
        source_revision: u64,
        limits: Limits,
    ) -> Result<(CfaPlane, WindowReport), DevelopError> {
        let a = self.active_area;
        if window.width == 0
            || window.height == 0
            || window.x < a.left
            || window.y < a.top
            || u64::from(window.x).saturating_add(u64::from(window.width)) > u64::from(a.right)
            || u64::from(window.y).saturating_add(u64::from(window.height)) > u64::from(a.bottom)
        {
            return Err(DevelopError::InvalidDimensions);
        }
        let mut report = WindowReport::default();
        let mut channel_black: [Option<i64>; 3] = [None; 3];
        let white = self.white_level as i64;
        let mut bytes = Vec::with_capacity(
            (window.width as usize)
                .saturating_mul(window.height as usize)
                .saturating_mul(2),
        );
        for y in window.y..window.y.saturating_add(window.height) {
            for x in window.x..window.x.saturating_add(window.width) {
                let ch = usize::from(self.channel_at(x, y));
                let site_black = self.black_at(x, y);
                let rounded = site_black.round();
                if rounded != site_black {
                    report.black_rounded = true;
                }
                let site_black = rounded as i64;
                let channel = *channel_black[ch].get_or_insert(site_black);
                if site_black != channel {
                    report.black_site_adjusted = true;
                }
                // Authored BlackLevel / BlackLevelDelta values are only checked for finiteness at
                // decode, so the rounded black can sit at an i64 limit: shifts that cannot be
                // represented are a typed refusal, never an overflow (panic in debug, wrap in release).
                let shift = site_black
                    .checked_sub(channel)
                    .ok_or(DevelopError::InvalidLevels)?;
                let mut value = i64::from(self.sample(x, y))
                    .checked_sub(shift)
                    .ok_or(DevelopError::InvalidLevels)?;
                if value < channel {
                    match policy {
                        RangePolicy::Refuse => return Err(DevelopError::SampleOutOfRange),
                        RangePolicy::ClampReported => {
                            report.clamped_below = report.clamped_below.saturating_add(1);
                            value = channel;
                        }
                    }
                } else if value > white {
                    match policy {
                        RangePolicy::Refuse => return Err(DevelopError::SampleOutOfRange),
                        RangePolicy::ClampReported => {
                            report.clamped_above = report.clamped_above.saturating_add(1);
                            value = white;
                        }
                    }
                }
                bytes.extend_from_slice(&(value as u16).to_le_bytes());
            }
        }
        let black = [
            channel_black[0].ok_or(DevelopError::MissingColourSupport)?,
            channel_black[1].ok_or(DevelopError::MissingColourSupport)?,
            channel_black[2].ok_or(DevelopError::MissingColourSupport)?,
        ];
        let to_u16 = |v: i64| u16::try_from(v).map_err(|_| DevelopError::InvalidLevels);
        let plane = CfaPlane::with_limits(
            PlaneInit {
                width: window.width,
                height: window.height,
                stride_bytes: window.width.saturating_mul(2),
                sensor_origin: (i64::from(window.x), i64::from(window.y)),
                cfa: Cfa {
                    pattern: self.cfa_pattern,
                    phase_x: 0,
                    phase_y: 0,
                },
                black: [to_u16(black[0])?, to_u16(black[1])?, to_u16(black[2])?],
                white: [self.white_level as u16; 3],
                source_revision,
                bytes,
            },
            limits,
        )?;
        Ok((plane, report))
    }

    pub fn solve_colour(&self) -> Result<CameraSolution, DevelopError> {
        color::solve(&self.color)
    }

    /// Native 1.0 recipe from the DNG colour tags: as-shot neutral + `xyz_d50_to_profile *
    /// camera_to_xyz_d50 * inverse(gain)` so the recipe's own as-shot gain stage is not applied
    /// twice. `xyz_d50_to_profile` must be the inverse of the working profile's colorant matrix
    /// (see `color::xyz_d50_to_linear_srgb`). BaselineExposure is applied as the recipe exposure
    /// only when `baseline` says so; either way it stays readable in `color`.
    pub fn recipe(
        &self,
        xyz_d50_to_profile: &Mat3,
        profile: ProfileBinding,
        baseline: BaselineExposure,
    ) -> Result<(DevelopRecipe, CameraSolution), DevelopError> {
        let solution = self.solve_colour()?;
        let recipe = recipe_from_solution(&self.color, &solution, xyz_d50_to_profile, profile, baseline)?;
        Ok((recipe, solution))
    }

    /// Raw-form white balance (STU-RAW-110): the recipe for an explicit kelvin / tint instead of the
    /// as-shot white. The target white's chromaticity ([`crate::adjust::white_xy`]) replaces
    /// AsShotNeutral in the DNG colour solution, so the camera neutral, the iterated CM/FM
    /// interpolation and the camera -> XYZ matrix are those of that illuminant. The recipe carries
    /// `WbMode::Temperature` and the request in `adjust.absolute_white_balance`. Kelvin 2000..25000
    /// (Planckian locus), tint -150..150; the tint scale is UNVERIFIED against Adobe.
    pub fn recipe_for_white(
        &self,
        xyz_d50_to_profile: &Mat3,
        profile: ProfileBinding,
        baseline: BaselineExposure,
        kelvin: f64,
        tint: f64,
    ) -> Result<(DevelopRecipe, CameraSolution), DevelopError> {
        recipe_for_white_point(&self.color, xyz_d50_to_profile, profile, baseline, kelvin, tint)
    }

    /// Strict gate: refuse when the file carries features this reader does not apply.
    pub fn require_complete(&self) -> Result<(), DevelopError> {
        match self.unapplied.first() {
            Some(name) => Err(DevelopError::UnsupportedContribution(name)),
            None => Ok(()),
        }
    }
}

fn recipe_from_solution(
    colour: &DngColor,
    solution: &CameraSolution,
    xyz_d50_to_profile: &Mat3,
    profile: ProfileBinding,
    baseline: BaselineExposure,
) -> Result<DevelopRecipe, DevelopError> {
    let gain = as_shot_gain(solution.neutral)?;
    let to_profile = color::mul(xyz_d50_to_profile, &solution.camera_to_xyz_d50);
    let matrix = color::mul(
        &to_profile,
        &color::diag([1.0 / gain[0], 1.0 / gain[1], 1.0 / gain[2]]),
    );
    let mut recipe = DevelopRecipe::native(solution.neutral, matrix, profile);
    if baseline == BaselineExposure::Apply {
        recipe.exposure_stops = colour.baseline_exposure;
    }
    Ok(recipe)
}

/// [`DngRaw::recipe_for_white`] on bare DNG colour tags (usable without a decoded file).
pub fn recipe_for_white_point(
    colour: &DngColor,
    xyz_d50_to_profile: &Mat3,
    profile: ProfileBinding,
    baseline: BaselineExposure,
    kelvin: f64,
    tint: f64,
) -> Result<(DevelopRecipe, CameraSolution), DevelopError> {
    let xy = crate::adjust::white_xy(kelvin, tint)?;
    let mut target = colour.clone();
    target.as_shot_neutral = None;
    target.as_shot_white_xy = Some(xy);
    let solution = color::solve(&target)?;
    let mut recipe = recipe_from_solution(colour, &solution, xyz_d50_to_profile, profile, baseline)?;
    recipe.white_balance_mode = crate::recipe::WbMode::Temperature;
    recipe.adjust.absolute_white_balance = Some(crate::adjust::AbsoluteWhiteBalance { kelvin, tint });
    Ok((recipe, solution))
}
