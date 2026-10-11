//! Decoded-CFA source plane, normalisation, bilinear reflect demosaic and the develop stages
//! (STU-CON-043..045). Scalar oracle only: written products, no FMA, checked arithmetic.
use crate::DevelopError;
use crate::convert::ProfileConverter;
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
/// rounding; a nonzero value rounding to zero refuses.
fn normalize(plane: &CfaPlane, cancel: &CancellationToken) -> Result<Vec<f32>, DevelopError> {
    let (w, h) = (plane.width as usize, plane.height as usize);
    let denominator: [f64; 3] = std::array::from_fn(|ch| f64::from(plane.white[ch] - plane.black[ch]));
    let mut norm = Vec::with_capacity(w * h);
    for y in 0..h {
        cancel.check()?;
        let lut_row = &plane.lut[usize::from(plane.row_phase[y])];
        for (x, sample) in plane.row_samples(y).enumerate() {
            let ch = usize::from(lut_row[usize::from(plane.col_phase[x])]);
            let numerator = f64::from(sample - plane.black[ch]);
            let value = numerator / denominator[ch];
            let rounded = value as f32;
            if value != 0.0 && rounded == 0.0 {
                return Err(DevelopError::NormalizationUnderflow);
            }
            norm.push(rounded);
        }
    }
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
pub fn demosaic_normalized(
    plane: &CfaPlane,
    cancel: &CancellationToken,
) -> Result<Vec<[f32; 3]>, DevelopError> {
    cancel.check()?;
    let norm = normalize(plane, cancel)?;
    let demosaic = Demosaic::new(plane, &norm);
    let (w, h) = (plane.width as usize, plane.height as usize);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        cancel.check()?;
        for x in 0..w {
            out.push(demosaic.pixel(x, y)?);
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// inspected both, equal) or "prism_admitted" (converted through the admitted Prism route).
    pub profile_route: &'static str,
    /// Number of converter calls (Prism transforms of at most 64 pixels each).
    pub profile_calls: u32,
    pub limits: &'static str,
    pub source_bounds: (u32, u32, u32, u32),
    pub output: Rect,
    pub tiles: u32,
    pub pixels: u64,
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

/// Stage A: demosaiced sample -> as-shot gain -> calibration matrix -> working-profile RGB (f32).
#[inline]
fn working_pixel(prepared: &PreparedRecipe, demosaiced: [f32; 3]) -> Result<[f32; 3], DevelopError> {
    let mut balanced = [0.0f64; 3];
    for i in 0..3 {
        balanced[i] = mul_checked(prepared.gain[i], f64::from(demosaiced[i]))?;
    }
    let m = &prepared.matrix;
    let mut working = [0.0f32; 3];
    for row in 0..3 {
        let p0 = mul_checked(m[row * 3], balanced[0])?;
        let p1 = mul_checked(m[row * 3 + 1], balanced[1])?;
        let p2 = mul_checked(m[row * 3 + 2], balanced[2])?;
        let sum = (p0 + p1) + p2;
        working[row] = to_f32_checked(sum)?;
    }
    Ok(working)
}

/// Stage C (after any working -> output profile conversion): exposure, then curves, one f32 round.
fn finish_pixel(prepared: &PreparedRecipe, converted: [f32; 3]) -> Result<[f32; 3], DevelopError> {
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

/// Run the develop stages over the crop and hand tiles to `sink` after ALL tiles computed
/// (atomic: any refusal, cancel or sink error leaves no receipt; the plane is only borrowed).
pub fn develop(
    request: &DevelopRequest<'_>,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
) -> Result<DevelopReceipt, DevelopError> {
    develop_with(request, None, cancel, sink)
}

/// [`develop`] with an optional working -> output profile converter (Prism admitted route).
/// A recipe whose working and output profiles differ requires a converter whose bindings match
/// the recipe exactly; with a converter, both profiles are inspected (admitted, linear-light)
/// before any pixel is computed, and identical profiles skip the transform only after that.
pub fn develop_with(
    request: &DevelopRequest<'_>,
    mut converter: Option<&mut dyn ProfileConverter>,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(DevelopedTile) -> Result<(), SinkError>,
) -> Result<DevelopReceipt, DevelopError> {
    cancel.check()?;
    let plane = request.plane;
    if plane.source_revision != request.expected_source_revision {
        return Err(DevelopError::StaleSource);
    }
    let prepared = request.recipe.prepare()?;
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
                "prism_admitted"
            } else {
                "identity_inspected"
            }
        }
    };
    let limits = plane.limits;
    let (x0, y0, x1, y1) = request
        .recipe
        .crop
        .source_bounds(plane.width, plane.height)?;
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

    let norm = normalize(plane, cancel)?;
    let demosaic = Demosaic::new(plane, &norm);
    let identity_finish =
        prepared.exposure_gain == 1.0 && prepared.curves.iter().all(Option::is_none);
    let tile_width = grid.tile_width as usize;
    let tile_pixels = tile_width * grid.tile_height as usize;
    let mut tiles = Vec::with_capacity(tile_count as usize);
    let mut working: Vec<[f32; 3]> = Vec::new();
    for row in row_first..=row_last {
        for column in col_first..=col_last {
            cancel.check()?;
            let bounds = grid
                .bounds(checked_i64(column)?, checked_i64(row)?)
                .map_err(|_| DevelopError::Overflow)?;
            let developed = bounds
                .intersect(output)
                .map_err(|_| DevelopError::Overflow)?
                .ok_or(DevelopError::EmptyCrop)?;
            let (developed_w, developed_h) = (developed.width as usize, developed.height as usize);
            let mut colour = vec![0u8; tile_pixels * 12];
            let mut alpha = vec![0u8; tile_pixels * 4];
            let mut coverage = vec![0u8; tile_pixels];
            // Stage A for the whole developed region (row-major), then the optional conversion.
            working.clear();
            working.reserve(developed_w * developed_h);
            let source_x = x0 as usize + (developed.x - ox) as usize;
            let source_y = y0 as usize + (developed.y - oy) as usize;
            for sy in source_y..source_y + developed_h {
                for sx in source_x..source_x + developed_w {
                    working.push(working_pixel(&prepared, demosaic.pixel(sx, sy)?)?);
                }
            }
            if prepared.profile_transform
                && let Some(c) = converter.as_deref_mut()
            {
                working = c.convert(&working, cancel)?;
                if working.len() != developed_w * developed_h {
                    return Err(DevelopError::InvalidLength);
                }
            }
            let first_x = (developed.x - bounds.x) as usize;
            let first_y = (developed.y - bounds.y) as usize;
            for (dy, source_row) in working.chunks_exact(developed_w).enumerate() {
                let first = (first_y + dy) * tile_width + first_x;
                let colour_row = &mut colour[first * 12..(first + developed_w) * 12];
                for (cell, w) in colour_row.chunks_exact_mut(12).zip(source_row) {
                    let rgb = if identity_finish {
                        finish_pixel_identity(*w)?
                    } else {
                        finish_pixel(&prepared, *w)?
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
            tiles.push(DevelopedTile {
                bounds,
                developed,
                colour,
                alpha,
                coverage,
            });
        }
    }

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
        limits: limits.name(),
        source_bounds: (x0, y0, x1, y1),
        output,
        tiles: delivered,
        pixels: u64::from(crop_w) * u64::from(crop_h),
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
pub fn develop_to_image(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    cancel: &CancellationToken,
) -> Result<(DevelopedImage, DevelopReceipt), DevelopError> {
    let edge = plane.limits.max_tile_edge;
    let mut tiles = Vec::new();
    let receipt = develop(
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
        cancel,
        &mut |tile| {
            tiles.push(tile);
            Ok(())
        },
    )?;
    let (width, height) = (receipt.output.width, receipt.output.height);
    let mut rgb = vec![0.0f32; width as usize * height as usize * 3];
    for tile in &tiles {
        let d = tile.developed;
        let row_pixels = d.width as usize;
        for y in 0..d.height as usize {
            let tx = (d.x - tile.bounds.x) as usize;
            let ty = (d.y - tile.bounds.y) as usize + y;
            let from = (ty * edge as usize + tx) * 12;
            let to = ((d.y as usize + y) * width as usize + d.x as usize) * 3;
            let source = tile.colour[from..from + row_pixels * 12].chunks_exact(4);
            for (slot, b) in rgb[to..to + row_pixels * 3].iter_mut().zip(source) {
                *slot = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
        }
    }
    Ok((DevelopedImage { width, height, rgb }, receipt))
}
