//! Decoded-CFA source plane, normalisation, bilinear reflect demosaic and the develop stages
//! (STU-CON-043..045). Scalar oracle only: written products, no FMA, checked arithmetic (stage A
//! skips its per-product refusals only where `stage_a_is_total` shows they cannot fire).
use crate::DevelopError;
use crate::convert::{BandLimits, ConversionStats, ProfileConverter, pixel_limit};
use crate::par::{Threads, band_rows, for_each_band, run_indexed};
use crate::recipe::{
    DEMOSAIC_ALGORITHM, DevelopRecipe, ENGINE_VERSION, MATH_TOKEN, PROCESS_VERSION,
    PreparedRecipe,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::{Grid, Rect};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bayer {
    Rggb,
    Bggr,
    Grbg,
    Gbrg,
}

impl Bayer {
    /// `[row][col]` channel indices of the 2x2 cell, R=0 G=1 B=2.
    pub(crate) const fn cell(self) -> [[u8; 2]; 2] {
        match self {
            Self::Rggb => [[0, 1], [1, 2]],
            Self::Bggr => [[2, 1], [1, 0]],
            Self::Grbg => [[1, 0], [2, 1]],
            Self::Gbrg => [[1, 2], [0, 1]],
        }
    }
}

/// Fixed X-Trans 6x6 rows from STU-CON-043: GGRGGB GGBGGR BRGRBG GGBGGR GGRGGB RBGBRG.
pub(crate) const XTRANS: [[u8; 6]; 6] = [
    [1, 1, 0, 1, 1, 2],
    [1, 1, 2, 1, 1, 0],
    [2, 0, 1, 0, 2, 1],
    [1, 1, 2, 1, 1, 0],
    [1, 1, 0, 1, 1, 2],
    [0, 2, 1, 2, 0, 1],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfaPattern {
    Bayer(Bayer),
    XTrans,
}

impl CfaPattern {
    const fn period(self) -> usize {
        match self {
            Self::Bayer(_) => 2,
            Self::XTrans => 6,
        }
    }
    const fn channel(self, col: usize, row: usize) -> u8 {
        match self {
            Self::Bayer(b) => b.cell()[row][col],
            Self::XTrans => XTRANS[row][col],
        }
    }
}

/// Pattern plus phase. `phase` is relative to `sensor_origin`; callers must not double-apply a
/// previously shifted active-area phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cfa {
    pub pattern: CfaPattern,
    pub phase_x: u8,
    pub phase_y: u8,
}

/// Admission limits. `SPEC_V1` is the STU-CON-043/045 initial subset; `LARGE` is an explicit
/// caller opt-in for full sensor frames (the receipt records which one ran).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub min_bayer_side: u32,
    pub min_xtrans_side: u32,
    pub max_side: u32,
    pub max_stride_bytes: u32,
    pub max_plane_bytes: u32,
    pub max_tile_edge: u32,
    pub max_tiles: u32,
    pub max_output_bytes: u64,
}

impl Limits {
    pub const SPEC_V1: Self = Self {
        min_bayer_side: 2,
        min_xtrans_side: 6,
        max_side: 256,
        max_stride_bytes: 1024,
        max_plane_bytes: 262_144,
        max_tile_edge: 64,
        max_tiles: 16,
        max_output_bytes: 16_777_216,
    };
    pub const LARGE: Self = Self {
        min_bayer_side: 2,
        min_xtrans_side: 6,
        max_side: 16_384,
        max_stride_bytes: 65_536,
        max_plane_bytes: 1 << 30,
        max_tile_edge: 256,
        max_tiles: 1 << 16,
        max_output_bytes: 1 << 32,
    };
    pub fn name(&self) -> &'static str {
        if *self == Self::SPEC_V1 {
            "spec_v1"
        } else if *self == Self::LARGE {
            "large"
        } else {
            "custom"
        }
    }

    /// Caller-supplied limits may tighten the presets but can never reopen a panic: Bayer sides
    /// below 2 or X-Trans sides below 6 have no reflect period, and zero caps admit nothing.
    pub fn validate(&self) -> Result<(), DevelopError> {
        if self.min_bayer_side < 2
            || self.min_xtrans_side < 6
            || self.max_side < self.min_bayer_side
            || self.max_tile_edge == 0
            || self.max_tiles == 0
            || self.max_stride_bytes < 4
            || self.max_plane_bytes == 0
            || self.max_output_bytes == 0
        {
            return Err(DevelopError::InvalidLimits);
        }
        Ok(())
    }
}

/// Everything needed to admit one decoded plane.
#[derive(Clone, Debug)]
pub struct PlaneInit {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub sensor_origin: (i64, i64),
    pub cfa: Cfa,
    pub black: [u16; 3],
    pub white: [u16; 3],
    pub source_revision: u64,
    /// u16le row-major, `stride_bytes` per row; padding is hashed but never a sample.
    pub bytes: Vec<u8>,
}

/// Immutable validated CFA plane. Develop only ever borrows it.
#[derive(Clone, Debug)]
pub struct CfaPlane {
    width: u32,
    height: u32,
    stride_bytes: u32,
    sensor_origin: (i64, i64),
    cfa: Cfa,
    black: [u16; 3],
    white: [u16; 3],
    source_revision: u64,
    bytes: Vec<u8>,
    col_phase: Vec<u8>,
    row_phase: Vec<u8>,
    /// Channel of a site by `[row phase][column phase]` (period <= 6); built once at admission.
    lut: [[u8; 6]; 6],
    sha256: [u8; 32],
    limits: Limits,
}

impl CfaPlane {
    /// Admit a plane under the STU-CON-043 initial limits.
    pub fn new(init: PlaneInit) -> Result<Self, DevelopError> {
        Self::with_limits(init, Limits::SPEC_V1)
    }

    pub fn with_limits(init: PlaneInit, limits: Limits) -> Result<Self, DevelopError> {
        limits.validate()?;
        let PlaneInit {
            width,
            height,
            stride_bytes,
            sensor_origin,
            cfa,
            black,
            white,
            source_revision,
            bytes,
        } = init;
        let min_side = match cfa.pattern {
            CfaPattern::Bayer(_) => limits.min_bayer_side,
            CfaPattern::XTrans => limits.min_xtrans_side,
        };
        if !(min_side..=limits.max_side).contains(&width)
            || !(min_side..=limits.max_side).contains(&height)
        {
            return Err(DevelopError::InvalidDimensions);
        }
        let min_stride = width.checked_mul(2).ok_or(DevelopError::InvalidStride)?;
        if stride_bytes % 2 != 0 || stride_bytes < min_stride || stride_bytes > limits.max_stride_bytes
        {
            return Err(DevelopError::InvalidStride);
        }
        let total = stride_bytes
            .checked_mul(height)
            .ok_or(DevelopError::InvalidLength)?;
        if total > limits.max_plane_bytes || usize::try_from(total) != Ok(bytes.len()) {
            return Err(DevelopError::InvalidLength);
        }
        let period = cfa.pattern.period();
        if usize::from(cfa.phase_x) >= period || usize::from(cfa.phase_y) >= period {
            return Err(DevelopError::InvalidPhase);
        }
        if (0..3).any(|i| white[i] <= black[i]) {
            return Err(DevelopError::InvalidLevels);
        }
        // Keep `sensor_origin + index + phase` overflow-free for every site.
        let reach_x = i64::from(width) + 8;
        let reach_y = i64::from(height) + 8;
        if sensor_origin.0.checked_add(reach_x).is_none()
            || sensor_origin.1.checked_add(reach_y).is_none()
        {
            return Err(DevelopError::Overflow);
        }
        let p = period as i64;
        let col_phase: Vec<u8> = (0..i64::from(width))
            .map(|x| (sensor_origin.0 + x + i64::from(cfa.phase_x)).rem_euclid(p) as u8)
            .collect();
        let row_phase: Vec<u8> = (0..i64::from(height))
            .map(|y| (sensor_origin.1 + y + i64::from(cfa.phase_y)).rem_euclid(p) as u8)
            .collect();
        let mut lut = [[0u8; 6]; 6];
        for (row_phase_index, row) in lut.iter_mut().enumerate().take(period) {
            for (col_phase_index, slot) in row.iter_mut().enumerate().take(period) {
                *slot = cfa.pattern.channel(col_phase_index, row_phase_index);
            }
        }
        let plane = Self {
            width,
            height,
            stride_bytes,
            sensor_origin,
            cfa,
            black,
            white,
            source_revision,
            sha256: Sha256::digest(&bytes).into(),
            bytes,
            col_phase,
            row_phase,
            lut,
            limits,
        };
        for y in 0..height as usize {
            let lut_row = &plane.lut[usize::from(plane.row_phase[y])];
            for (x, s) in plane.row_samples(y).enumerate() {
                let ch = usize::from(lut_row[usize::from(plane.col_phase[x])]);
                if s < black[ch] || s > white[ch] {
                    return Err(DevelopError::SampleOutOfRange);
                }
            }
        }
        Ok(plane)
    }

    /// The `width` samples of row `y` (padding excluded); the caller guarantees `y < height`.
    fn row_samples(&self, y: usize) -> impl Iterator<Item = u16> + '_ {
        let start = y * self.stride_bytes as usize;
        self.bytes[start..start + self.width as usize * 2]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stride_bytes(&self) -> u32 {
        self.stride_bytes
    }
    pub fn sensor_origin(&self) -> (i64, i64) {
        self.sensor_origin
    }
    pub fn cfa(&self) -> Cfa {
        self.cfa
    }
    pub fn black(&self) -> [u16; 3] {
        self.black
    }
    pub fn white(&self) -> [u16; 3] {
        self.white
    }
    pub fn source_revision(&self) -> u64 {
        self.source_revision
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// SHA-256 over every supplied byte including row padding.
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Raw sample at `(x, y)`; the caller guarantees bounds.
    pub fn sample(&self, x: usize, y: usize) -> u16 {
        let i = y * self.stride_bytes as usize + x * 2;
        u16::from_le_bytes([self.bytes[i], self.bytes[i + 1]])
    }

    /// CFA channel index (R=0, G=1, B=2) at a full-plane site.
    pub fn channel(&self, x: usize, y: usize) -> u8 {
        self.lut[usize::from(self.row_phase[y])][usize::from(self.col_phase[x])]
    }
}

fn reflect(coordinate: i64, extent: usize) -> usize {
    let period = 2 * (extent as i64 - 1);
    let remainder = coordinate.rem_euclid(period);
    if remainder >= extent as i64 {
        (period - remainder) as usize
    } else {
        remainder as usize
    }
}

/// Normalised measured sample per site: binary64((s-black)/(white-black)) then one binary32
/// rounding; a nonzero value rounding to zero refuses. Rows are independent, so bands of rows run
/// on up to `threads` workers; every site is computed by the same expression whatever the count.
///
/// The per-site black level and denominator come from per-row-phase tables, which leaves a
/// branch-free inner loop (the refusal is accumulated and raised after the row). The compiler may
/// vectorise it with plain IEEE binary64 division and binary32 conversion, which are correctly
/// rounded per lane, so the result is bit-identical to the scalar definition (no `unsafe`, no
/// arch intrinsics, no FMA; pinned by `tests/develop_digest.rs`
/// `scalar_output_is_byte_identical_to_the_pre_rewrite_oracle`). This is not an execution route.
fn normalize(
    plane: &CfaPlane,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<Vec<f32>, DevelopError> {
    let (w, h) = (plane.width as usize, plane.height as usize);
    let denominator: [f64; 3] = std::array::from_fn(|ch| f64::from(plane.white[ch] - plane.black[ch]));
    let period = plane.cfa.pattern.period();
    let tables: Vec<(Vec<u16>, Vec<f64>)> = (0..period)
        .map(|row_phase| {
            let lut_row = &plane.lut[row_phase];
            (0..w)
                .map(|x| {
                    let ch = usize::from(lut_row[usize::from(plane.col_phase[x])]);
                    (plane.black[ch], denominator[ch])
                })
                .unzip()
        })
        .collect();
    let mut norm = vec![0.0f32; w * h];
    let rows = band_rows(w);
    for_each_band(&mut norm, rows * w, threads, cancel, |band, out| {
        for (offset, out_row) in out.chunks_exact_mut(w).enumerate() {
            let y = band * rows + offset;
            let (black, den) = &tables[usize::from(plane.row_phase[y])];
            let start = y * plane.stride_bytes as usize;
            let samples = plane.bytes[start..start + w * 2].chunks_exact(2);
            let mut underflow = false;
            for (((slot, sample), black), den) in out_row.iter_mut().zip(samples).zip(black).zip(den) {
                let numerator = f64::from(u16::from_le_bytes([sample[0], sample[1]]) - *black);
                let value = numerator / *den;
                let rounded = value as f32;
                underflow |= (value != 0.0) & (rounded == 0.0);
                *slot = rounded;
            }
            if underflow {
                return Err(DevelopError::NormalizationUnderflow);
            }
        }
        Ok(())
    })?;
    Ok(norm)
}

/// Reference demosaic of one site with reflect indexing (used for sites within two samples of an
/// edge, where the 5x5 window leaves the plane). Summation order is fixed: rows, then columns.
fn demosaic_at_reflect(plane: &CfaPlane, norm: &[f32], x: usize, y: usize) -> Result<[f32; 3], DevelopError> {
    let (w, h) = (plane.width as usize, plane.height as usize);
    let own = plane.channel(x, y);
    let mut out = [0.0f32; 3];
    for channel in 0..3u8 {
        if channel == own {
            out[usize::from(channel)] = norm[y * w + x];
            continue;
        }
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for radius in [1i64, 2] {
            for dy in -radius..=radius {
                let ry = reflect(y as i64 + dy, h);
                for dx in -radius..=radius {
                    let rx = reflect(x as i64 + dx, w);
                    if plane.channel(rx, ry) == channel {
                        sum += norm[ry * w + rx];
                        count += 1;
                    }
                }
            }
            if count > 0 {
                break;
            }
        }
        if count == 0 {
            return Err(DevelopError::MissingColourSupport);
        }
        out[usize::from(channel)] = sum / (count as f32);
    }
    Ok(out)
}

/// Contributing neighbours of one phase class, as linear offsets into the normalised plane in the
/// reference summation order. `None` for the own channel and for a channel with no support.
struct Stencil {
    own: u8,
    offsets: [Option<Box<[isize]>>; 3],
}

/// Bilinear reflect demosaic over a normalised plane. Sites at least two samples from every edge
/// never reflect, so their neighbours depend only on the (row phase, column phase) class: the
/// offsets are built once per class and the sum runs in exactly the reference order (same f32
/// additions, same count), which keeps the output byte-identical to [`demosaic_at_reflect`].
struct Demosaic<'a> {
    plane: &'a CfaPlane,
    norm: &'a [f32],
    width: usize,
    height: usize,
    period: usize,
    classes: Vec<Stencil>,
}

impl<'a> Demosaic<'a> {
    fn new(plane: &'a CfaPlane, norm: &'a [f32]) -> Self {
        let width = plane.width as usize;
        let period = plane.cfa.pattern.period();
        let p = period as isize;
        let mut classes = Vec::with_capacity(period * period);
        for row_phase in 0..period {
            for col_phase in 0..period {
                let own = plane.lut[row_phase][col_phase];
                let offsets = std::array::from_fn(|channel| {
                    if channel == usize::from(own) {
                        return None;
                    }
                    for radius in [1isize, 2] {
                        let mut found = Vec::new();
                        for dy in -radius..=radius {
                            for dx in -radius..=radius {
                                let neighbour_row = (row_phase as isize + dy).rem_euclid(p) as usize;
                                let neighbour_col = (col_phase as isize + dx).rem_euclid(p) as usize;
                                if usize::from(plane.lut[neighbour_row][neighbour_col]) == channel {
                                    found.push(dy * width as isize + dx);
                                }
                            }
                        }
                        if !found.is_empty() {
                            return Some(found.into_boxed_slice());
                        }
                    }
                    None
                });
                classes.push(Stencil { own, offsets });
            }
        }
        Self {
            plane,
            norm,
            width,
            height: plane.height as usize,
            period,
            classes,
        }
    }

    #[inline]
    fn pixel(&self, x: usize, y: usize) -> Result<[f32; 3], DevelopError> {
        if x < 2 || y < 2 || x + 2 >= self.width || y + 2 >= self.height {
            return demosaic_at_reflect(self.plane, self.norm, x, y);
        }
        let class = usize::from(self.plane.row_phase[y]) * self.period + usize::from(self.plane.col_phase[x]);
        let stencil = &self.classes[class];
        let site = y * self.width + x;
        let mut out = [0.0f32; 3];
        for (channel, slot) in out.iter_mut().enumerate() {
            if channel == usize::from(stencil.own) {
                *slot = self.norm[site];
                continue;
            }
            let offsets = stencil.offsets[channel]
                .as_deref()
                .ok_or(DevelopError::MissingColourSupport)?;
            let mut sum = 0.0f32;
            for offset in offsets {
                sum += self.norm[site.wrapping_add_signed(*offset)];
            }
            *slot = sum / (offsets.len() as f32);
        }
        Ok(out)
    }
}

/// Full-plane normalised demosaic (scalar oracle). Crop and tile boundaries never change it.
/// Runs on `Threads::auto()`; the result does not depend on the thread count.
pub fn demosaic_normalized(
    plane: &CfaPlane,
    cancel: &CancellationToken,
) -> Result<Vec<[f32; 3]>, DevelopError> {
    demosaic_normalized_threads(plane, Threads::auto(), cancel)
}

/// [`demosaic_normalized`] on an explicit number of worker threads (bands of rows; cancellation is
/// checked between bands and a cancelled run returns no pixels).
pub fn demosaic_normalized_threads(
    plane: &CfaPlane,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<Vec<[f32; 3]>, DevelopError> {
    cancel.check()?;
    let norm = normalize(plane, threads, cancel)?;
    let demosaic = Demosaic::new(plane, &norm);
    let (w, h) = (plane.width as usize, plane.height as usize);
    let mut out = vec![[0.0f32; 3]; w * h];
    let rows = band_rows(w);
    for_each_band(&mut out, rows * w, threads, cancel, |band, pixels| {
        for (offset, row) in pixels.chunks_exact_mut(w).enumerate() {
            let y = band * rows + offset;
            for (x, slot) in row.iter_mut().enumerate() {
                *slot = demosaic.pixel(x, y)?;
            }
        }
        Ok(())
    })?;
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionHint {
    Scalar,
    Sse2,
    Auto,
}

impl ExecutionHint {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Sse2 => "sse2",
            Self::Auto => "auto",
        }
    }
}

/// Returned by a sink callback to refuse a tile; develop returns `SinkRejected`, no receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkError;

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopedTile {
    /// Complete grid tile bounds in document pixels.
    pub bounds: Rect,
    /// Part of the tile covered by the mapped crop.
    pub developed: Rect,
    /// `rgb_f32le` linear light, tile-sized, 12 bytes per pixel; zero outside the crop.
    pub colour: Vec<u8>,
    /// Straight `f32le` alpha, tile-sized; 1.0 inside the crop only.
    pub alpha: Vec<u8>,
    /// `coverage_u8`: 255 inside the crop, 0 outside, so a consumer can keep preimage bytes.
    pub coverage: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopReceipt {
    pub source_sha256: [u8; 32],
    pub source_revision: u64,
    pub recipe_revision: u64,
    pub process_version: (u32, u32),
    pub engine_version: (u32, u32),
    pub math_token: &'static str,
    pub demosaic_algorithm: &'static str,
    pub requested_hint: ExecutionHint,
    /// Actual route; never claims SIMD.
    pub route: &'static str,
    /// "identity_uninspected" (no converter, profiles equal), "identity_inspected" (converter
    /// inspected both, equal) or "prism_admitted_wide" (both profiles inspected through the
    /// admitted Prism route, pixels converted by a Prism admitted wide job).
    pub profile_route: &'static str,
    /// Number of bands the converter converted (one per tile; a tile above the converter's
    /// `BandLimits::band_pixels` is several).
    pub profile_calls: u32,
    /// Plan-cache use (one hit or miss per converter job) and destination clip report of the
    /// converter (default when none ran).
    pub profile_stats: ConversionStats,
    pub limits: &'static str,
    pub source_bounds: (u32, u32, u32, u32),
    pub output: Rect,
    pub tiles: u32,
    pub pixels: u64,
    /// Whole-frame geometry stage (lens corrections, perspective, crop angle/flips) when it ran
    /// (`develop_to_image` only); `None` otherwise.
    pub geometry: Option<crate::geometry::GeometryReport>,
}

pub struct DevelopRequest<'a> {
    pub plane: &'a CfaPlane,
    pub recipe: &'a DevelopRecipe,
    pub recipe_revision: u64,
    pub expected_source_revision: u64,
    pub grid: Grid,
    /// Document pixel of the crop's top-left source pixel.
    pub output_origin: (i64, i64),
    pub execution_hint: ExecutionHint,
}

fn mul_checked(a: f64, b: f64) -> Result<f64, DevelopError> {
    let product = a * b;
    if !product.is_finite() {
        return Err(DevelopError::StageOverflow);
    }
    if product == 0.0 && a != 0.0 && b != 0.0 {
        return Err(DevelopError::StageUnderflow);
    }
    Ok(product)
}

fn to_f32_checked(value: f64) -> Result<f32, DevelopError> {
    let rounded = value as f32;
    if !value.is_finite() || rounded.is_infinite() {
        return Err(DevelopError::StageOverflow);
    }
    if rounded == 0.0 && value != 0.0 {
        return Err(DevelopError::StageUnderflow);
    }
    Ok(rounded)
}

/// True when no stage-A product can overflow or round to zero from nonzero operands, so the
/// per-product refusals of [`mul_checked`] cannot fire. Demosaiced samples are 0 or a finite f32
/// of magnitude 2^-149..3.4e38; with every gain and coefficient zero or within 1e-100..1e100 the
/// gain products stay within 1.4e-145..3.4e138 and the matrix products within 1.4e-245..3.4e238,
/// far inside binary64 (a zero operand gives an exact zero, which is permitted).
fn stage_a_is_total(prepared: &PreparedRecipe) -> bool {
    prepared
        .gain
        .iter()
        .chain(prepared.matrix.iter())
        .all(|v| *v == 0.0 || (1e-100..=1e100).contains(&v.abs()))
}

/// Stage A: demosaiced sample -> as-shot gain -> calibration matrix -> working-profile RGB (f32).
/// `total` is [`stage_a_is_total`]; the f32 rounding of each row keeps its overflow/underflow refusal.
#[inline]
fn working_pixel(
    prepared: &PreparedRecipe,
    total: bool,
    demosaiced: [f32; 3],
) -> Result<[f32; 3], DevelopError> {
    let m = &prepared.matrix;
    let mut working = [0.0f32; 3];
    if total {
        let balanced: [f64; 3] = std::array::from_fn(|i| prepared.gain[i] * f64::from(demosaiced[i]));
        for row in 0..3 {
            let sum = ((m[row * 3] * balanced[0]) + (m[row * 3 + 1] * balanced[1]))
                + (m[row * 3 + 2] * balanced[2]);
            working[row] = to_f32_checked(sum)?;
        }
        return Ok(working);
    }
    let mut balanced = [0.0f64; 3];
    for i in 0..3 {
        balanced[i] = mul_checked(prepared.gain[i], f64::from(demosaiced[i]))?;
    }
    for row in 0..3 {
        let p0 = mul_checked(m[row * 3], balanced[0])?;
        let p1 = mul_checked(m[row * 3 + 1], balanced[1])?;
        let p2 = mul_checked(m[row * 3 + 2], balanced[2])?;
        let sum = (p0 + p1) + p2;
        working[row] = to_f32_checked(sum)?;
    }
    Ok(working)
}

/// Stage C with adjustments: exposure, white balance shift + basic tone + parametric regions, point
/// curves, then vibrance/saturation/mixer, one f32 round (see `adjust.rs`).
fn finish_pixel_adjusted(prepared: &PreparedRecipe, converted: [f32; 3]) -> Result<[f32; 3], DevelopError> {
    let mut v = [0.0f64; 3];
    for (slot, c) in v.iter_mut().zip(converted) {
        *slot = mul_checked(f64::from(c), prepared.exposure_gain)?;
    }
    v = prepared.adjust.pre(v)?;
    for (channel, slot) in v.iter_mut().enumerate() {
        if let Some(curve) = &prepared.curves[0] {
            *slot = curve.eval(*slot)?;
        }
        if let Some(curve) = &prepared.curves[1 + channel] {
            *slot = curve.eval(*slot)?;
        }
    }
    v = prepared.adjust.colour(v)?;
    let mut out = [0.0f32; 3];
    for (slot, c) in out.iter_mut().zip(v) {
        *slot = to_f32_checked(c)?;
    }
    Ok(out)
}

/// Stage C (after any working -> output profile conversion): exposure, then curves, one f32 round.
fn finish_pixel(prepared: &PreparedRecipe, converted: [f32; 3]) -> Result<[f32; 3], DevelopError> {
    if prepared.adjust.pixel_active() {
        return finish_pixel_adjusted(prepared, converted);
    }
    let mut out = [0.0f32; 3];
    for channel in 0..3 {
        let mut v = mul_checked(f64::from(converted[channel]), prepared.exposure_gain)?;
        if let Some(curve) = &prepared.curves[0] {
            v = curve.eval(v)?;
        }
        if let Some(curve) = &prepared.curves[1 + channel] {
            v = curve.eval(v)?;
        }
        out[channel] = to_f32_checked(v)?;
    }
    Ok(out)
}

/// Stage C when exposure is exactly 1.0 and no curve runs: multiplying a finite f32 by 1.0 in
/// binary64 and rounding back is the identity, so only the finiteness refusal remains.
#[inline]
fn finish_pixel_identity(converted: [f32; 3]) -> Result<[f32; 3], DevelopError> {
    if converted.iter().all(|v| v.is_finite()) {
        Ok(converted)
    } else {
        Err(DevelopError::StageOverflow)
    }
}

fn checked_i64(value: i128) -> Result<i64, DevelopError> {
    i64::try_from(value).map_err(|_| DevelopError::Overflow)
}

/// One grid tile's bounds and the part of it inside the mapped crop.
#[derive(Clone, Copy)]
struct TileGeometry {
    bounds: Rect,
    developed: Rect,
}

impl TileGeometry {
    fn pixels(&self) -> usize {
        self.developed.width as usize * self.developed.height as usize
    }
}

/// One converter job: a window of consecutive tiles, each cut into bands of at most
/// `BandLimits::band_pixels` pixels (one band per tile unless a tile is larger).
struct BandWindow {
    /// Indices of the tiles the window covers.
    tiles: std::ops::Range<usize>,
    /// Pixels of every band of the window, in band order.
    bands: Vec<usize>,
    /// Index in `bands` of each covered tile's first band.
    first_band: Vec<usize>,
}

/// The window that starts at tile `start`: tiles are added while the job stays within
/// `limits.max_bands` (at least one tile). A single tile that alone needs more bands than that is
/// Prism's typed `pixel_limit` refusal, raised before the band list is built.
fn band_window(
    geometries: &[TileGeometry],
    start: usize,
    limits: BandLimits,
) -> Result<BandWindow, DevelopError> {
    let band_pixels = limits.band_pixels.max(1);
    let max_bands = limits.max_bands.max(1);
    let mut bands = Vec::new();
    let mut first_band = Vec::new();
    let mut end = start;
    while let Some(geometry) = geometries.get(end) {
        let pixels = geometry.pixels();
        let count = pixels.div_ceil(band_pixels);
        if count > max_bands {
            return Err(pixel_limit());
        }
        if end > start && bands.len().saturating_add(count) > max_bands {
            break;
        }
        first_band.push(bands.len());
        let mut left = pixels;
        while left > 0 {
            let band = left.min(band_pixels);
            bands.push(band);
            left -= band;
        }
        end += 1;
    }
    Ok(BandWindow {
        tiles: start..end,
        bands,
        first_band,
    })
}

/// Everything a tile job reads; all of it is immutable, so workers share it freely.
struct TileJob<'a> {
    demosaic: &'a Demosaic<'a>,
    prepared: &'a PreparedRecipe,
    stage_a_total: bool,
    identity_finish: bool,
    grid: Grid,
    output: Rect,
    /// Source pixel of the crop's top-left corner.
    source_origin: (usize, usize),
    tile_width: usize,
    tile_pixels: usize,
    first_column: i128,
    first_row: i128,
    columns: usize,
}

impl TileJob<'_> {
    /// Geometry of tile `index` (row-major over the grid tiles that touch the crop).
    fn geometry(&self, index: usize) -> Result<TileGeometry, DevelopError> {
        let column = self.first_column + (index % self.columns) as i128;
        let row = self.first_row + (index / self.columns) as i128;
        let bounds = self
            .grid
            .bounds(checked_i64(column)?, checked_i64(row)?)
            .map_err(|_| DevelopError::Overflow)?;
        let developed = bounds
            .intersect(self.output)
            .map_err(|_| DevelopError::Overflow)?
            .ok_or(DevelopError::EmptyCrop)?;
        Ok(TileGeometry { bounds, developed })
    }

    /// Stage A over the developed region of a tile into `working` (row-major).
    fn stage_a(
        &self,
        geometry: &TileGeometry,
        working: &mut Vec<[f32; 3]>,
    ) -> Result<(), DevelopError> {
        let developed = geometry.developed;
        let (developed_w, developed_h) = (developed.width as usize, developed.height as usize);
        working.clear();
        working.reserve(developed_w * developed_h);
        let source_x = self.source_origin.0 + (developed.x - self.output.x) as usize;
        let source_y = self.source_origin.1 + (developed.y - self.output.y) as usize;
        for sy in source_y..source_y + developed_h {
            for sx in source_x..source_x + developed_w {
                working.push(working_pixel(
                    self.prepared,
                    self.stage_a_total,
                    self.demosaic.pixel(sx, sy)?,
                )?);
            }
        }
        Ok(())
    }

    /// Stage C: exposure and curves per pixel, then the tile's colour, alpha and coverage planes.
    fn stage_c(
        &self,
        geometry: &TileGeometry,
        working: &[[f32; 3]],
    ) -> Result<DevelopedTile, DevelopError> {
        let TileGeometry { bounds, developed } = *geometry;
        let developed_w = developed.width as usize;
        let mut colour = vec![0u8; self.tile_pixels * 12];
        let mut alpha = vec![0u8; self.tile_pixels * 4];
        let mut coverage = vec![0u8; self.tile_pixels];
        let first_x = (developed.x - bounds.x) as usize;
        let first_y = (developed.y - bounds.y) as usize;
        for (dy, source_row) in working.chunks_exact(developed_w).enumerate() {
            let first = (first_y + dy) * self.tile_width + first_x;
            let colour_row = &mut colour[first * 12..(first + developed_w) * 12];
            for (cell, w) in colour_row.chunks_exact_mut(12).zip(source_row) {
                let rgb = if self.identity_finish {
                    finish_pixel_identity(*w)?
                } else {
                    finish_pixel(self.prepared, *w)?
                };
                for (slot, value) in cell.chunks_exact_mut(4).zip(rgb) {
                    slot.copy_from_slice(&value.to_le_bytes());
                }
            }
            for cell in alpha[first * 4..(first + developed_w) * 4].chunks_exact_mut(4) {
                cell.copy_from_slice(&1.0f32.to_le_bytes());
            }
            coverage[first..first + developed_w].fill(255);
        }
        Ok(DevelopedTile {
            bounds,
            developed,
            colour,
            alpha,
            coverage,
        })
    }
}

/// Run the develop stages over the crop and hand tiles to `sink` after ALL tiles computed
/// (atomic: any refusal, cancel or sink error leaves no receipt; the plane is only borrowed).
/// Tiles are computed on `Threads::auto()` workers; the output does not depend on the count.
pub fn develop(
    request: &DevelopRequest<'_>,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
) -> Result<DevelopReceipt, DevelopError> {
    develop_with_threads(request, None, Threads::auto(), cancel, sink)
}

/// [`develop`] with an optional working -> output profile converter (admitted inspection, Prism
/// admitted wide job). A recipe whose working and output profiles differ requires a converter whose
/// bindings match the recipe exactly; with a converter, both profiles are inspected (admitted,
/// linear-light) before any pixel is computed, and identical profiles skip the transform only
/// after that.
pub fn develop_with(
    request: &DevelopRequest<'_>,
    converter: Option<&mut dyn ProfileConverter>,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
) -> Result<DevelopReceipt, DevelopError> {
    develop_with_threads(request, converter, Threads::auto(), cancel, sink)
}

/// [`develop_with`] on an explicit number of worker threads. Determinism contract: tiles, receipt
/// and refusals are identical for every thread count (including `Threads::single()`, which spawns
/// nothing); proven by `tests/develop_parallel.rs` (`output_is_identical_for_every_thread_count`,
/// `lowest_failing_tile_decides_the_error_for_every_thread_count`). Tiles are independent, so
/// workers take tiles from a shared counter. A converter prepares one thread-shared job per
/// window of tiles (all tiles unless the converter's `BandLimits::max_bands` is exceeded) before
/// any tile is computed; each worker converts its own tile's bands between stage A and stage C
/// (`converter_bands_run_once_each_on_the_tile_workers`). Cancellation is checked between
/// normalisation row bands and between tiles, and the token is passed to every converter band
/// (`cancellation_returns_canceled_and_no_partial_output` covers the first two and a cancel raised
/// from inside a stand-in converter band; with Prism the band itself checks the token per 64-pixel
/// chunk, see `convert.rs`); when several tiles fail, the lowest tile's error is returned, as in a
/// serial run.
pub fn develop_with_threads(
    request: &DevelopRequest<'_>,
    converter: Option<&mut dyn ProfileConverter>,
    threads: Threads,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
) -> Result<DevelopReceipt, DevelopError> {
    develop_core(request, converter, threads, cancel, sink, CoreMode::Tiles)
}

/// What [`develop_core`] runs: the tile API (whole-frame stages refused), the crop for the
/// whole-frame image API, or the uncropped linear frame (stage A only) the geometry stage resamples.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CoreMode {
    Tiles,
    WholeFrame,
    LinearFull,
}

/// Shared body of the tile API and the whole-frame image API. Stages that need the whole frame
/// (clarity) are refused here as `unsupported_contribution:clarity_whole_frame` unless
/// `whole_frame` is set, which only [`develop_to_image_threads`] does (it runs them afterwards).
fn develop_core(
    request: &DevelopRequest<'_>,
    mut converter: Option<&mut dyn ProfileConverter>,
    threads: Threads,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
    mode: CoreMode,
) -> Result<DevelopReceipt, DevelopError> {
    cancel.check()?;
    let plane = request.plane;
    if plane.source_revision != request.expected_source_revision {
        return Err(DevelopError::StaleSource);
    }
    let prepared = request.recipe.prepare()?;
    if mode == CoreMode::Tiles {
        if prepared.adjust.optics.is_active() {
            return Err(DevelopError::UnsupportedContribution("lens_corrections_whole_frame"));
        }
        if prepared.adjust.geometry.is_some() || request.recipe.crop.needs_geometry() {
            return Err(DevelopError::UnsupportedContribution("transform_whole_frame"));
        }
        if prepared.adjust.effects.is_active() {
            return Err(DevelopError::UnsupportedContribution("effects_whole_frame"));
        }
        if prepared.adjust.clarity != 0.0 {
            return Err(DevelopError::UnsupportedContribution("clarity_whole_frame"));
        }
        if let Some(name) = prepared.adjust.detail.first_active() {
            return Err(DevelopError::UnsupportedContribution(name));
        }
        if !prepared.adjust.local.is_empty() {
            return Err(DevelopError::UnsupportedContribution("mask_group_whole_frame"));
        }
    }
    if request.execution_hint == ExecutionHint::Sse2 {
        return Err(DevelopError::UnsupportedExecution);
    }
    let profile_route = match converter.as_deref_mut() {
        None if !prepared.profile_transform => "identity_uninspected",
        None => return Err(DevelopError::UnsupportedProfileTransform),
        Some(c) => {
            if c.source() != &request.recipe.working_profile
                || c.destination() != &request.recipe.output_profile
            {
                return Err(DevelopError::InvalidProfile);
            }
            c.inspect(cancel)?;
            if prepared.profile_transform {
                "prism_admitted_wide"
            } else {
                "identity_inspected"
            }
        }
    };
    let limits = plane.limits;
    let (x0, y0, x1, y1) = if mode == CoreMode::LinearFull {
        request.recipe.crop.source_bounds(plane.width, plane.height)?;
        (0, 0, plane.width, plane.height)
    } else {
        request.recipe.crop.source_bounds(plane.width, plane.height)?
    };
    let (crop_w, crop_h) = (x1 - x0, y1 - y0);
    let (ox, oy) = request.output_origin;
    let output = Rect {
        x: ox,
        y: oy,
        width: crop_w,
        height: crop_h,
    };
    let (out_end_x, out_end_y) = output.end().map_err(|_| DevelopError::Overflow)?;

    let grid = request.grid;
    if grid.tile_width == 0
        || grid.tile_height == 0
        || grid.tile_width > limits.max_tile_edge
        || grid.tile_height > limits.max_tile_edge
    {
        return Err(DevelopError::InvalidGrid);
    }
    let (tw, th) = (i128::from(grid.tile_width), i128::from(grid.tile_height));
    let col_first = (i128::from(ox) - i128::from(grid.origin_x)).div_euclid(tw);
    let col_last = (i128::from(out_end_x) - 1 - i128::from(grid.origin_x)).div_euclid(tw);
    let row_first = (i128::from(oy) - i128::from(grid.origin_y)).div_euclid(th);
    let row_last = (i128::from(out_end_y) - 1 - i128::from(grid.origin_y)).div_euclid(th);
    let tile_count = (col_last - col_first + 1) * (row_last - row_first + 1);
    if tile_count > i128::from(limits.max_tiles) {
        return Err(DevelopError::TooManyTiles);
    }
    let bytes_per_tile = u64::from(grid.tile_width) * u64::from(grid.tile_height) * 17;
    if (tile_count as u64).saturating_mul(bytes_per_tile) > limits.max_output_bytes {
        return Err(DevelopError::OutputTooLarge);
    }

    let norm = normalize(plane, threads, cancel)?;
    let demosaic = Demosaic::new(plane, &norm);
    let tile_width = grid.tile_width as usize;
    let job = TileJob {
        demosaic: &demosaic,
        prepared: &prepared,
        stage_a_total: stage_a_is_total(&prepared),
        identity_finish: mode == CoreMode::LinearFull
            || (prepared.exposure_gain == 1.0
                && prepared.curves.iter().all(Option::is_none)
                && !prepared.adjust.pixel_active()),
        grid,
        output,
        source_origin: (x0 as usize, y0 as usize),
        tile_width,
        tile_pixels: tile_width * grid.tile_height as usize,
        first_column: col_first,
        first_row: row_first,
        columns: (col_last - col_first + 1) as usize,
    };
    let tile_total = tile_count as usize;
    let tiles: Vec<DevelopedTile> = match converter
        .as_deref_mut()
        .filter(|_| prepared.profile_transform)
    {
        // No conversion: each tile is one job (stage A then stage C on the worker's scratch).
        None => run_indexed(tile_total, threads, cancel, Vec::new, |working, index| {
            let geometry = job.geometry(index)?;
            job.stage_a(&geometry, working)?;
            job.stage_c(&geometry, working)
        })?,
        // Conversion: each tile is one job on a worker (stage A, the converter's bands in place on
        // the worker's scratch, stage C). The converter job is `Sync` and shared by all workers;
        // the lowest failing tile decides the error exactly as in a serial run, and a geometry
        // failure is held back until every tile before it succeeded.
        Some(convert) => {
            let limits = convert.band_limits();
            let band_pixels = limits.band_pixels.max(1);
            let mut geometries = Vec::new();
            let mut geometry_failure = None;
            for index in 0..tile_total {
                match job.geometry(index) {
                    Ok(geometry) => geometries.push(geometry),
                    Err(error) => {
                        geometry_failure = Some(error);
                        break;
                    }
                }
            }
            let mut tiles = Vec::with_capacity(geometries.len());
            let mut start = 0usize;
            while start < geometries.len() {
                let window = band_window(&geometries, start, limits)?;
                // Everything the conversion needs is charged and allocated here, before any pixel.
                let converting = convert.prepare(&window.bands, cancel)?;
                let converted = run_indexed(window.tiles.len(), threads, cancel, Vec::new, |working, offset| {
                    let geometry = &geometries[window.tiles.start + offset];
                    job.stage_a(geometry, working)?;
                    let first = window.first_band[offset];
                    for (k, band) in working.chunks_mut(band_pixels).enumerate() {
                        converting.convert_band(first + k, band, cancel)?;
                    }
                    job.stage_c(geometry, working)
                })?;
                // An aborted run (error or cancellation above) drops the job unfinished.
                converting.finish(cancel)?;
                tiles.extend(converted);
                start = window.tiles.end;
            }
            if let Some(error) = geometry_failure {
                return Err(error);
            }
            tiles
        }
    };

    let delivered = tiles.len() as u32;
    for tile in tiles {
        cancel.check()?;
        sink(tile).map_err(|_| DevelopError::SinkRejected)?;
    }
    Ok(DevelopReceipt {
        source_sha256: plane.sha256,
        source_revision: plane.source_revision,
        recipe_revision: request.recipe_revision,
        process_version: PROCESS_VERSION,
        engine_version: ENGINE_VERSION,
        math_token: MATH_TOKEN,
        demosaic_algorithm: DEMOSAIC_ALGORITHM,
        requested_hint: request.execution_hint,
        route: "scalar",
        profile_route,
        profile_calls: converter.as_deref().map_or(0, |c| c.calls()),
        profile_stats: converter.as_deref().map_or_else(ConversionStats::default, |c| c.stats()),
        limits: limits.name(),
        source_bounds: (x0, y0, x1, y1),
        output,
        tiles: delivered,
        pixels: u64::from(crop_w) * u64::from(crop_h),
        geometry: None,
    })
}

/// Contiguous linear-light RGB result (3 x f32 per pixel, row-major) for tools and viewers.
#[derive(Clone, Debug, PartialEq)]
pub struct DevelopedImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<f32>,
}

/// Convenience over [`develop`]: whole crop into one image at output origin (0, 0), tiled at the
/// plane limits' tile edge. Same atomic/refusal behaviour; the receipt is returned unchanged.
/// Runs on `Threads::auto()`; the image does not depend on the thread count.
pub fn develop_to_image(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    cancel: &CancellationToken,
) -> Result<(DevelopedImage, DevelopReceipt), DevelopError> {
    develop_to_image_threads(plane, recipe, recipe_revision, Threads::auto(), cancel)
}

/// [`develop_to_image`] on an explicit number of worker threads (tiles, then bands of tile rows
/// copied into the image).
pub fn develop_to_image_threads(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(DevelopedImage, DevelopReceipt), DevelopError> {
    let prepared = recipe.prepare()?;
    let geometric = prepared.adjust.optics.is_active() || prepared.adjust.geometry.is_some() || recipe.crop.needs_geometry();
    let mode = if geometric { CoreMode::LinearFull } else { CoreMode::WholeFrame };
    let (rgb, mut receipt) = develop_frame(plane, recipe, recipe_revision, threads, cancel, mode)?;
    let mut image = DevelopedImage { width: receipt.output.width, height: receipt.output.height, rgb };
    if geometric {
        image = geometry_stage(image, &mut receipt, recipe, &prepared, threads, cancel)?;
    }
    postprocess(&mut image, recipe, threads, cancel)?;
    Ok((image, receipt))
}

/// Develop the whole crop (or, in `LinearFull`, the uncropped stage-A frame) into one RGB buffer.
fn develop_frame(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    threads: Threads,
    cancel: &CancellationToken,
    mode: CoreMode,
) -> Result<(Vec<f32>, DevelopReceipt), DevelopError> {
    let edge = plane.limits.max_tile_edge;
    let mut tiles = Vec::new();
    let receipt = develop_core(
        &DevelopRequest {
            plane,
            recipe,
            recipe_revision,
            expected_source_revision: plane.source_revision,
            grid: Grid {
                origin_x: 0,
                origin_y: 0,
                tile_width: edge,
                tile_height: edge,
            },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        None,
        threads,
        cancel,
        &mut |tile| {
            tiles.push(tile);
            Ok(())
        },
        mode,
    )?;
    let (width, height) = (receipt.output.width as usize, receipt.output.height as usize);
    let edge = edge as usize;
    let columns = width.div_ceil(edge);
    if tiles.len() != columns * height.div_ceil(edge) {
        return Err(DevelopError::InvalidLength);
    }
    let mut rgb = vec![0.0f32; width * height * 3];
    // The grid is anchored at (0, 0) like the output, so tile row `r` holds output rows
    // `r * edge ..`: one band of the image per tile row, each written by exactly one worker.
    for_each_band(&mut rgb, edge * width * 3, threads, cancel, |tile_row, band| {
        let first_row = tile_row * edge;
        for tile in &tiles[tile_row * columns..(tile_row + 1) * columns] {
            let d = tile.developed;
            let row_pixels = d.width as usize;
            for y in 0..d.height as usize {
                let tx = (d.x - tile.bounds.x) as usize;
                let ty = (d.y - tile.bounds.y) as usize + y;
                let from = (ty * edge + tx) * 12;
                let to = ((d.y as usize + y - first_row) * width + d.x as usize) * 3;
                let source = tile.colour[from..from + row_pixels * 12].chunks_exact(4);
                for (slot, b) in band[to..to + row_pixels * 3].iter_mut().zip(source) {
                    *slot = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
            }
        }
        Ok(())
    })?;
    Ok((rgb, receipt))
}

/// Whole-frame geometry: resample the uncropped linear frame (lens map, perspective, crop angle and
/// flips; `geometry::apply`), cut the crop, then run stage C (exposure, tone, curves, colour) per
/// pixel exactly as the tiles would have. The receipt is rewritten to describe the crop.
fn geometry_stage(
    image: DevelopedImage,
    receipt: &mut DevelopReceipt,
    recipe: &DevelopRecipe,
    prepared: &PreparedRecipe,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<DevelopedImage, DevelopError> {
    let (warped, report) = crate::geometry::apply(
        &image.rgb,
        image.width,
        image.height,
        &prepared.adjust.optics,
        prepared.adjust.geometry.as_ref(),
        &recipe.crop,
        prepared.adjust.luma,
        threads,
        cancel,
    )?;
    let (x0, y0, x1, y1) = recipe.crop.source_bounds(image.width, image.height)?;
    let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let full_w = image.width as usize;
    let mut rgb = vec![0.0f32; cw * ch * 3];
    let identity = prepared.exposure_gain == 1.0 && prepared.curves.iter().all(Option::is_none) && !prepared.adjust.pixel_active();
    let rows = band_rows(cw);
    for_each_band(&mut rgb, rows * cw * 3, threads, cancel, |band_index, band| {
        for (k, px) in band.chunks_exact_mut(3).enumerate() {
            let (x, y) = (x0 as usize + k % cw, y0 as usize + band_index * rows + k / cw);
            let at = (y * full_w + x) * 3;
            let value = [warped[at], warped[at + 1], warped[at + 2]];
            let out = if identity { finish_pixel_identity(value)? } else { finish_pixel(prepared, value)? };
            px.copy_from_slice(&out);
        }
        Ok(())
    })?;
    receipt.source_bounds = (x0, y0, x1, y1);
    receipt.output = Rect { x: 0, y: 0, width: x1 - x0, height: y1 - y0 };
    receipt.pixels = u64::from(x1 - x0) * u64::from(y1 - y0);
    receipt.geometry = Some(report);
    Ok(DevelopedImage { width: x1 - x0, height: y1 - y0, rgb })
}

/// Whole-frame stages after the tiles: clarity (when `enable_detail` and the slider is nonzero),
/// then the local corrections (when `enable_mask_group_based_corrections`).
fn postprocess(
    image: &mut DevelopedImage,
    recipe: &DevelopRecipe,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(), DevelopError> {
    let prepared = recipe.prepare()?;
    cancel.check()?;
    let (width, height) = (image.width, image.height);
    let mut clarity_step = |rgb: &mut [f32]| -> Result<(), DevelopError> {
        if prepared.adjust.clarity == 0.0 {
            return Ok(());
        }
        crate::adjust::clarity(rgb, width, height, recipe.adjust.presence.clarity, prepared.adjust.luma)
    };
    prepared
        .adjust
        .detail
        .apply(&mut image.rgb, width, height, prepared.adjust.luma, &mut clarity_step)?;
    crate::local::apply(
        &mut image.rgb,
        image.width,
        image.height,
        &prepared.adjust.local,
        prepared.adjust.luma,
        threads,
        cancel,
    )?;
    if prepared.adjust.effects.is_active() {
        cancel.check()?;
        prepared.adjust.effects.apply(&mut image.rgb, image.width, image.height, prepared.adjust.luma)?;
    }
    Ok(())
}
