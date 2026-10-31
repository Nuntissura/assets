//! Grayscale -> Bitmap (STU-COL-119 bitmap option record; MT-31175, MT-31176).
//!
//! Input: an 8- or 16-bit grayscale plane (0 = black, full scale = white), borrowed and never
//! written. Output: a packed 1-bit plane, 1 = black (the PSD bitmap convention), most significant
//! bit first, each row padded to a whole byte. Tone `v` is the sample over full scale.
//!
//! Methods (Studio readings; Photoshop parity UNVERIFIED, D5):
//! * `half_threshold`: white iff `100 v >= bitmap.threshold` (percent; required, its default is
//!   UNKNOWN; any finite value, no clamp).
//! * `pattern_dither`: ordered dither with the 8 x 8 Bayer matrix `B`: white iff
//!   `v > (B[y mod 8][x mod 8] + 0.5) / 64`.
//! * `diffusion_dither`: Floyd-Steinberg error diffusion (7/16 right, 3/16 below-left, 5/16 below,
//!   1/16 below-right), threshold 0.5, every row left to right, one thread: the output is a pure
//!   function of the input.
//! * `halftone_screen`: the pixel centre in inches (`(x + 0.5) / resolution`) is rotated by
//!   `-bitmap.angle`, scaled by `bitmap.frequency` (lines per inch) and reduced to cell coordinates
//!   `(u, w)` in `[-1, 1)`. Spot functions `s(u, w)` in `[-1, 1]`: round `1 - (u^2 + w^2)`, diamond
//!   `1 - (|u| + |w|)`, ellipse `1 - (0.6 u^2 + 1.4 w^2)`, line `1 - 2|w|`, square
//!   `1 - 2 max(|u|, |w|)`, cross `1 - 2 min(|u|, |w|)`. The spot value orders the pixels of a
//!   cell: with darkness `d = 1 - v`, black iff `d > 0` and the fraction of the cell whose spot
//!   value is `>= s(u, w)` (sampled on a 256 x 256 grid of the cell) is `<= d`, so coverage tracks
//!   tone for every shape. Frequency, angle and shape are required (defaults UNKNOWN).
//! * `custom_pattern`: a caller tile of 8-bit thresholds repeated from the origin: white iff
//!   `v > (t + 0.5) / 256`. A missing name or tile is `pattern_missing`.
//!
//! The requested `bitmap.resolution` must equal the source resolution: a different value is
//! `bitmap_resample_not_in_prism` (resampling is pigment's). Refusals precede the output
//! allocation.
#![deny(clippy::arithmetic_side_effects)]

use super::{check_plane, token_enum, ModeError, BITMAP_ANGLE, BITMAP_FREQUENCY, BITMAP_RESOLUTION, BITMAP_THRESHOLD};

/// Most cells of a custom pattern tile (Studio bound, V4-02).
pub const MAX_PATTERN_TILE_CELLS: usize = 1 << 20;

token_enum! {
    /// STU-COL-119 `method`.
    BitmapMethod {
        HalfThreshold => "half_threshold",
        PatternDither => "pattern_dither",
        DiffusionDither => "diffusion_dither",
        HalftoneScreen => "halftone_screen",
        CustomPattern => "custom_pattern",
    }
}

impl Default for BitmapMethod {
    /// STU-COL-119: default `diffusion_dither`.
    fn default() -> Self {
        Self::DiffusionDither
    }
}

token_enum! {
    /// STU-COL-119 halftone `shape` (six values).
    HalftoneShape {
        Round => "round",
        Diamond => "diamond",
        Ellipse => "ellipse",
        Line => "line",
        Square => "square",
        Cross => "cross",
    }
}

/// STU-COL-119 bitmap option record: the six spec fields plus `threshold` (the table's
/// `bitmap.threshold` row). Method-specific fields are kept, inert, when the method does not use
/// them and are returned unchanged in [`BitmapConversion::options`]. `None` = UNKNOWN / unset.
#[derive(Clone, Debug, PartialEq)]
pub struct BitmapOptions {
    pub method: BitmapMethod,
    /// ppi; default 72.0 ([`BITMAP_RESOLUTION`]).
    pub resolution: f64,
    /// `custom_pattern` only: the caller's pattern reference.
    pub pattern_name: Option<String>,
    /// `halftone_screen` only; lines per inch.
    pub frequency: Option<f64>,
    /// `halftone_screen` only; degrees.
    pub angle: Option<f64>,
    /// `halftone_screen` only. The spec declares no default.
    pub shape: Option<HalftoneShape>,
    /// `half_threshold` only; percent.
    pub threshold: Option<f64>,
}

impl Default for BitmapOptions {
    fn default() -> Self {
        Self {
            method: BitmapMethod::default(),
            resolution: 72.0,
            pattern_name: None,
            frequency: None,
            angle: None,
            shape: None,
            threshold: None,
        }
    }
}

fn opt_num(v: Option<f64>) -> String {
    v.map_or_else(|| "\"UNKNOWN\"".to_owned(), |x| format!("{x:?}"))
}

impl BitmapOptions {
    /// Every field, inert ones included; unset numbers are `"UNKNOWN"`, an unset name or shape
    /// is `null`. The pattern name is the caller's reference and is written as given (escaped).
    pub fn to_json(&self) -> String {
        let name = self.pattern_name.as_ref().map_or_else(|| "null".to_owned(), |n| json_string(n));
        let shape = self.shape.map_or_else(|| "null".to_owned(), |s| format!("\"{}\"", s.token()));
        format!(
            "{{\"method\":\"{}\",\"resolution\":{:?},\"pattern_name\":{},\"frequency\":{},\"angle\":{},\"shape\":{},\"threshold\":{}}}",
            self.method.token(),
            self.resolution,
            name,
            opt_num(self.frequency),
            opt_num(self.angle),
            shape,
            opt_num(self.threshold),
        )
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A borrowed grayscale plane, row-major, no padding.
#[derive(Clone, Copy, Debug)]
pub enum GrayPlane<'a> {
    Eight { width: u32, height: u32, data: &'a [u8] },
    Sixteen { width: u32, height: u32, data: &'a [u16] },
}

impl GrayPlane<'_> {
    fn dims(&self) -> (u32, u32, usize) {
        match self {
            Self::Eight { width, height, data } => (*width, *height, data.len()),
            Self::Sixteen { width, height, data } => (*width, *height, data.len()),
        }
    }
    fn tone(&self, i: usize) -> f64 {
        match self {
            Self::Eight { data, .. } => f64::from(data[i]) / 255.0,
            Self::Sixteen { data, .. } => f64::from(data[i]) / 65_535.0,
        }
    }
}

/// A caller pattern tile of 8-bit thresholds, row-major.
#[derive(Clone, Copy, Debug)]
pub struct PatternTile<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// A packed 1-bit plane: 1 = black, MSB first, `stride` bytes per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitmapPlane {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl BitmapPlane {
    /// Whether pixel `(x, y)` is black; `None` outside the plane.
    pub fn is_black(&self, x: u32, y: u32) -> Option<bool> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let row = usize::try_from(y).ok()?.checked_mul(self.stride)?;
        let xi = usize::try_from(x).ok()?;
        let byte = self.data.get(row.checked_add(xi.wrapping_div(8))?)?;
        Some(byte & 0x80_u8.wrapping_shr(xi.wrapping_rem(8) as u32) != 0)
    }
    /// Count of black pixels.
    pub fn black_count(&self) -> u64 {
        let mut n = 0_u64;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.is_black(x, y) == Some(true) {
                    n = n.saturating_add(1);
                }
            }
        }
        n
    }
}

/// The result: the plane, its resolution and the options exactly as given.
#[derive(Clone, Debug, PartialEq)]
pub struct BitmapConversion {
    pub plane: BitmapPlane,
    pub resolution: f64,
    pub options: BitmapOptions,
}

const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// The 8 x 8 Bayer index of pixel `(x, y)` (0..64).
pub(crate) fn bayer8(x: usize, y: usize) -> u8 {
    BAYER8[y.wrapping_rem(8)][x.wrapping_rem(8)]
}

fn spot(shape: HalftoneShape, u: f64, w: f64) -> f64 {
    match shape {
        HalftoneShape::Round => 1.0 - (u * u + w * w),
        HalftoneShape::Diamond => 1.0 - (u.abs() + w.abs()),
        HalftoneShape::Ellipse => 1.0 - (0.6 * u * u + 1.4 * w * w),
        HalftoneShape::Line => 1.0 - 2.0 * w.abs(),
        HalftoneShape::Square => 1.0 - 2.0 * u.abs().max(w.abs()),
        HalftoneShape::Cross => 1.0 - 2.0 * u.abs().min(w.abs()),
    }
}

/// Cell samples per axis for the spot-value ranking.
const SPOT_GRID: usize = 256;

/// The spot values of a `SPOT_GRID` x `SPOT_GRID` grid of cell centres, ascending.
fn spot_samples(shape: HalftoneShape) -> Vec<f64> {
    let step = 2.0 / SPOT_GRID as f64;
    let mut out = Vec::with_capacity(SPOT_GRID.saturating_mul(SPOT_GRID));
    for i in 0..SPOT_GRID {
        for j in 0..SPOT_GRID {
            out.push(spot(shape, (i as f64 + 0.5) * step - 1.0, (j as f64 + 0.5) * step - 1.0));
        }
    }
    out.sort_by(f64::total_cmp);
    out
}

/// Fraction of the cell whose spot value is `>= s`.
fn coverage(ranks: &[f64], s: f64) -> f64 {
    let below = ranks.partition_point(|v| *v < s);
    ranks.len().saturating_sub(below) as f64 / ranks.len().max(1) as f64
}

enum Plan<'a> {
    Threshold(f64),
    Pattern,
    Diffusion,
    Halftone { frequency: f64, cos: f64, sin: f64, shape: HalftoneShape, ranks: Vec<f64> },
    Custom(PatternTile<'a>, usize, usize),
}

fn plan<'a>(options: &BitmapOptions, pattern: Option<PatternTile<'a>>) -> Result<Plan<'a>, ModeError> {
    Ok(match options.method {
        BitmapMethod::HalfThreshold => {
            let t = options.threshold.ok_or(ModeError::ParameterRequired(BITMAP_THRESHOLD.key))?;
            Plan::Threshold(BITMAP_THRESHOLD.check(t)?)
        }
        BitmapMethod::PatternDither => Plan::Pattern,
        BitmapMethod::DiffusionDither => Plan::Diffusion,
        BitmapMethod::HalftoneScreen => {
            let f = options.frequency.ok_or(ModeError::ParameterRequired(BITMAP_FREQUENCY.key))?;
            let f = BITMAP_FREQUENCY.check(f)?;
            if f <= 0.0 {
                return Err(ModeError::InvalidParameter(BITMAP_FREQUENCY.key));
            }
            let a = options.angle.ok_or(ModeError::ParameterRequired(BITMAP_ANGLE.key))?;
            let a = BITMAP_ANGLE.check(a)?.to_radians();
            let shape = options.shape.ok_or(ModeError::ParameterRequired("bitmap.shape"))?;
            Plan::Halftone { frequency: f, cos: a.cos(), sin: a.sin(), shape, ranks: spot_samples(shape) }
        }
        BitmapMethod::CustomPattern => {
            let (Some(_), Some(tile)) = (options.pattern_name.as_ref(), pattern) else {
                return Err(ModeError::PatternMissing);
            };
            if tile.width == 0 || tile.height == 0 {
                return Err(ModeError::PatternInvalid);
            }
            let (tw, th) = (usize::try_from(tile.width).map_err(|_| ModeError::PatternInvalid)?, usize::try_from(tile.height).map_err(|_| ModeError::PatternInvalid)?);
            let cells = tw.checked_mul(th).ok_or(ModeError::PatternInvalid)?;
            if cells > MAX_PATTERN_TILE_CELLS || cells != tile.data.len() {
                return Err(ModeError::PatternInvalid);
            }
            Plan::Custom(tile, tw, th)
        }
    })
}

/// Convert `source` (at `source_resolution` ppi) to a 1-bit plane (module docs). `pattern` is the
/// caller tile for `custom_pattern` and ignored otherwise.
pub fn grayscale_to_bitmap(
    source: &GrayPlane<'_>,
    source_resolution: f64,
    options: &BitmapOptions,
    pattern: Option<PatternTile<'_>>,
) -> Result<BitmapConversion, ModeError> {
    let (width, height, len) = source.dims();
    let pixels = check_plane(width, height, 1, len)?;
    let resolution = BITMAP_RESOLUTION.check(options.resolution)?;
    if resolution <= 0.0 {
        return Err(ModeError::InvalidParameter(BITMAP_RESOLUTION.key));
    }
    let source_resolution = BITMAP_RESOLUTION.check(source_resolution)?;
    if resolution != source_resolution {
        return Err(ModeError::BitmapResampleNotInPrism);
    }
    let plan = plan(options, pattern)?;
    let (w, h) = (usize::try_from(width).map_err(|_| ModeError::PixelBudgetExceeded)?, usize::try_from(height).map_err(|_| ModeError::PixelBudgetExceeded)?);
    let stride = w.div_ceil(8);
    let bytes = stride.checked_mul(h).ok_or(ModeError::PixelBudgetExceeded)?;
    debug_assert!(bytes <= pixels);
    let mut data = vec![0_u8; bytes];
    let mut set_black = |x: usize, y: usize| {
        // In range by construction: x < w, y < h.
        if let Some(b) = y.checked_mul(stride).and_then(|r| r.checked_add(x.wrapping_div(8))).and_then(|i| data.get_mut(i)) {
            *b |= 0x80_u8.wrapping_shr(x.wrapping_rem(8) as u32);
        }
    };
    let index = |x: usize, y: usize| y.wrapping_mul(w).wrapping_add(x);
    match plan {
        Plan::Diffusion => {
            // Two error rows (current, next); width + 2 entries so x - 1 and x + 1 stay in range.
            let row_len = w.checked_add(2).ok_or(ModeError::PixelBudgetExceeded)?;
            let mut cur = vec![0.0_f64; row_len];
            let mut next = vec![0.0_f64; row_len];
            for y in 0..h {
                for x in 0..w {
                    let k = x.wrapping_add(1);
                    let v = source.tone(index(x, y)) + cur[k];
                    let out = if v >= 0.5 { 1.0 } else { 0.0 };
                    if out == 0.0 {
                        set_black(x, y);
                    }
                    let e = v - out;
                    cur[k.wrapping_add(1)] += e * 7.0 / 16.0;
                    next[x] += e * 3.0 / 16.0;
                    next[k] += e * 5.0 / 16.0;
                    next[k.wrapping_add(1)] += e / 16.0;
                }
                std::mem::swap(&mut cur, &mut next);
                next.iter_mut().for_each(|e| *e = 0.0);
            }
        }
        other => {
            let inch = 1.0 / resolution;
            for y in 0..h {
                for x in 0..w {
                    let v = source.tone(index(x, y));
                    let white = match &other {
                        Plan::Threshold(t) => v * 100.0 >= *t,
                        Plan::Pattern => v > (f64::from(bayer8(x, y)) + 0.5) / 64.0,
                        Plan::Halftone { frequency, cos, sin, shape, ranks } => {
                            let (px, py) = ((x as f64 + 0.5) * inch, (y as f64 + 0.5) * inch);
                            let cx = (px * cos + py * sin) * frequency;
                            let cy = (py * cos - px * sin) * frequency;
                            let (u, ww) = (2.0 * (cx - cx.floor()) - 1.0, 2.0 * (cy - cy.floor()) - 1.0);
                            let d = 1.0 - v;
                            !(d > 0.0 && coverage(ranks, spot(*shape, u, ww)) <= d)
                        }
                        Plan::Custom(tile, tw, th) => {
                            let t = tile.data[y.wrapping_rem(*th).wrapping_mul(*tw).wrapping_add(x.wrapping_rem(*tw))];
                            v > (f64::from(t) + 0.5) / 256.0
                        }
                        Plan::Diffusion => unreachable!("handled above"),
                    };
                    if !white {
                        set_black(x, y);
                    }
                }
            }
        }
    }
    Ok(BitmapConversion { plane: BitmapPlane { width, height, stride, data }, resolution, options: options.clone() })
}
