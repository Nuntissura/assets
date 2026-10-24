//! Raster tonal diagnostics (lane SMB-PS-INFO-HISTOGRAM-1; STU-RAS-049, STU-RAS-163): histogram per
//! channel and luminosity with bin and clip counts, averaged eyedropper samples for the seven
//! closed sample sizes, selection bounds, and colour-model readouts of a sampled colour. Pure,
//! deterministic and bounded; the host (the harness viewer's Info panel) calls these over the
//! displayed composite and exposes every number to the model surface.
//!
//! Input is an 8-bit RGBA plane with straight alpha placed at a document origin ([`Rgba8Plane`]),
//! the form of the viewer's rendered composite (sRGB-encoded samples).
//!
//! Studio readings, declared (Photoshop behaviour UNVERIFIED, D5):
//! - histogram bins split the 0..=255 sample range evenly: value `v` falls in bin `v * bins / 256`;
//!   `clip_low` / `clip_high` count samples equal to 0 / 255; pixels with alpha 0 carry no colour
//!   and are counted as `transparent`, never binned (the alpha channel bins every pixel);
//! - luminosity is `round((299 R + 587 G + 114 B) / 1000)` ([`LUMINOSITY_WEIGHTS`], Rec. 601);
//! - a sample of size `n x n` averages the in-bounds pixels of the square centred on the pixel
//!   (`point` = that pixel); colour is averaged weighted by alpha (transparent pixels do not pull
//!   the colour), alpha plainly; a centre outside the plane reads nothing (`None`);
//! - readouts: RGB 0..=255, HSB (degrees, percent, percent) and CIELab D65 of the triple read as
//!   sRGB through Prism's colour maths (`rgb_to_hsb`, `srgb_to_lab`), and CMYK percent by the
//!   unmanaged device formula `K = 1 - max(R, G, B)`, `C = (1 - R - K) / (1 - K)` (no CMYK profile
//!   is involved; a profiled CMYK readout is a declared gap).
//!
//! Bounds (V4-01/02): plane dimensions, byte length and pixel count are checked before any work;
//! bin counts are `1..=256`; coverage planes must match their bounds.
#![deny(clippy::arithmetic_side_effects)]

use crate::selection::CoveragePlane;
use crate::{Error, Rect};

/// Largest plane edge accepted (pixels).
pub const MAX_DIAG_EDGE: u32 = 65_536;
/// Largest plane accepted (pixels).
pub const MAX_DIAG_PIXELS: u64 = 64 * 1024 * 1024;
/// Largest bin count (one bin per 8-bit value).
pub const MAX_BINS: usize = 256;
/// Luminosity weights (per mille of R, G, B; Rec. 601). Photoshop's weights UNVERIFIED (D5).
pub const LUMINOSITY_WEIGHTS: [u32; 3] = [299, 587, 114];

/// An 8-bit RGBA plane (straight alpha) whose top-left pixel is the document pixel `(x, y)`.
#[derive(Clone, Copy, Debug)]
pub struct Rgba8Plane<'a> {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub data: &'a [u8],
}

impl Rgba8Plane<'_> {
    /// Checks the dimensions, the pixel budget and the byte length.
    pub fn validate(&self) -> Result<u64, Error> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::InvalidInput);
        }
        if self.width > MAX_DIAG_EDGE || self.height > MAX_DIAG_EDGE {
            return Err(Error::BudgetExceeded);
        }
        let pixels = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or(Error::Overflow)?;
        if pixels > MAX_DIAG_PIXELS {
            return Err(Error::BudgetExceeded);
        }
        let bytes = pixels.checked_mul(4).ok_or(Error::Overflow)?;
        if u64::try_from(self.data.len()).map_err(|_| Error::Overflow)? != bytes {
            return Err(Error::MalformedStride);
        }
        Ok(pixels)
    }

    /// The plane's rectangle in document pixels.
    pub fn rect(&self) -> Rect {
        Rect { x: self.x, y: self.y, width: self.width, height: self.height }
    }

    /// The RGBA bytes of the document pixel `(x, y)`, `None` outside the plane.
    pub fn pixel(&self, x: i64, y: i64) -> Option<[u8; 4]> {
        let (px, py) = (x.checked_sub(self.x)?, y.checked_sub(self.y)?);
        let (px, py) = (u64::try_from(px).ok()?, u64::try_from(py).ok()?);
        if px >= u64::from(self.width) || py >= u64::from(self.height) {
            return None;
        }
        let at = usize::try_from(py.checked_mul(u64::from(self.width))?.checked_add(px)?.checked_mul(4)?).ok()?;
        let p = self.data.get(at..at.checked_add(4)?)?;
        Some([p[0], p[1], p[2], p[3]])
    }
}

/// A histogram channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistogramChannel {
    Luminosity,
    Red,
    Green,
    Blue,
    Alpha,
}

impl HistogramChannel {
    pub const ALL: [HistogramChannel; 5] = [Self::Luminosity, Self::Red, Self::Green, Self::Blue, Self::Alpha];
    pub const fn code(self) -> &'static str {
        match self {
            Self::Luminosity => "luminosity",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
        }
    }
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.code() == code)
    }
}

/// One channel's bin counts and statistics (over its binned samples).
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelHistogram {
    pub channel: HistogramChannel,
    pub bins: Vec<u64>,
    /// Samples equal to 0 / 255.
    pub clip_low: u64,
    pub clip_high: u64,
    /// Samples binned.
    pub count: u64,
    /// Mean, standard deviation (population) and median of the 8-bit sample values; 0 when empty.
    pub mean: f64,
    pub std_dev: f64,
    pub median: u8,
}

/// The histogram of a plane or of a covered region of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bin_count: usize,
    /// Pixels in the region (covered pixels for a coverage region).
    pub pixels: u64,
    /// Region pixels with alpha 0 (not binned in the colour channels).
    pub transparent: u64,
    /// In [`HistogramChannel::ALL`] order.
    pub channels: Vec<ChannelHistogram>,
}

impl Histogram {
    pub fn channel(&self, c: HistogramChannel) -> Option<&ChannelHistogram> {
        self.channels.iter().find(|h| h.channel == c)
    }
}

/// The 8-bit luminosity of an RGB triple ([`LUMINOSITY_WEIGHTS`]).
pub fn luminosity(r: u8, g: u8, b: u8) -> u8 {
    let [wr, wg, wb] = LUMINOSITY_WEIGHTS;
    let sum = wr.saturating_mul(u32::from(r)).saturating_add(wg.saturating_mul(u32::from(g))).saturating_add(wb.saturating_mul(u32::from(b))).saturating_add(500);
    u8::try_from(sum.checked_div(1000).unwrap_or(0)).unwrap_or(u8::MAX)
}

fn check_coverage(c: &CoveragePlane) -> Result<(), Error> {
    let area = usize::try_from(c.bounds.area()).map_err(|_| Error::Overflow)?;
    if c.data.len() != area {
        return Err(Error::MalformedStride);
    }
    Ok(())
}

/// The histogram of `plane` with `bins` bins per channel; with `coverage`, only pixels whose
/// coverage is above 0 count (coverage is in document pixels; uncovered and off-plane pixels are
/// skipped).
pub fn histogram(plane: &Rgba8Plane<'_>, coverage: Option<&CoveragePlane>, bins: usize) -> Result<Histogram, Error> {
    plane.validate()?;
    if bins == 0 || bins > MAX_BINS {
        return Err(Error::InvalidInput);
    }
    if let Some(c) = coverage {
        check_coverage(c)?;
    }
    let mut counts = [[0u64; 256]; 5];
    let (mut pixels, mut transparent) = (0u64, 0u64);
    let width = usize::try_from(plane.width).map_err(|_| Error::Overflow)?;
    for (i, p) in plane.data.chunks_exact(4).enumerate() {
        if let Some(c) = coverage {
            let col = i64::try_from(i.checked_rem(width).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?;
            let row = i64::try_from(i.checked_div(width).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?;
            let (x, y) = (plane.x.checked_add(col).ok_or(Error::Overflow)?, plane.y.checked_add(row).ok_or(Error::Overflow)?);
            if c.at(x, y) == 0 {
                continue;
            }
        }
        pixels = pixels.saturating_add(1);
        let a = usize::from(p[3]);
        counts[4][a] = counts[4][a].saturating_add(1);
        if p[3] == 0 {
            transparent = transparent.saturating_add(1);
            continue;
        }
        for (k, v) in [luminosity(p[0], p[1], p[2]), p[0], p[1], p[2]].into_iter().enumerate() {
            let v = usize::from(v);
            counts[k][v] = counts[k][v].saturating_add(1);
        }
    }
    let channels = HistogramChannel::ALL.into_iter().zip(counts.iter()).map(|(channel, values)| channel_stats(channel, values, bins)).collect::<Result<Vec<_>, Error>>()?;
    Ok(Histogram { bin_count: bins, pixels, transparent, channels })
}

fn channel_stats(channel: HistogramChannel, values: &[u64; 256], bins: usize) -> Result<ChannelHistogram, Error> {
    let mut out = vec![0u64; bins];
    let (mut count, mut sum) = (0u64, 0f64);
    for (v, n) in values.iter().enumerate() {
        let bin = v.checked_mul(bins).and_then(|n| n.checked_div(256)).ok_or(Error::Overflow)?;
        out[bin] = out[bin].saturating_add(*n);
        count = count.saturating_add(*n);
        sum += (v as f64) * (*n as f64);
    }
    let (mean, std_dev, median) = if count == 0 {
        (0.0, 0.0, 0)
    } else {
        let mean = sum / count as f64;
        let var = values.iter().enumerate().map(|(v, n)| (v as f64 - mean).powi(2) * (*n as f64)).sum::<f64>() / count as f64;
        // The lower median: the smallest value whose cumulative count reaches half (rounded up).
        let half = count.div_ceil(2);
        let mut seen = 0u64;
        let median = values.iter().position(|n| {
            seen = seen.saturating_add(*n);
            seen >= half
        });
        (mean, var.sqrt(), u8::try_from(median.unwrap_or(0)).unwrap_or(u8::MAX))
    };
    Ok(ChannelHistogram { channel, bins: out, clip_low: values[0], clip_high: values[255], count, mean, std_dev, median })
}

/// The eyedropper / sampler sample size: averaged square neighbourhoods (STU-RAS-163).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SampleSize {
    #[default]
    Point,
    S3,
    S5,
    S11,
    S31,
    S51,
    S101,
}

impl SampleSize {
    pub const ALL: [SampleSize; 7] = [Self::Point, Self::S3, Self::S5, Self::S11, Self::S31, Self::S51, Self::S101];
    /// The square's edge in pixels.
    pub const fn edge(self) -> u32 {
        match self {
            Self::Point => 1,
            Self::S3 => 3,
            Self::S5 => 5,
            Self::S11 => 11,
            Self::S31 => 31,
            Self::S51 => 51,
            Self::S101 => 101,
        }
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::S3 => "3x3",
            Self::S5 => "5x5",
            Self::S11 => "11x11",
            Self::S31 => "31x31",
            Self::S51 => "51x51",
            Self::S101 => "101x101",
        }
    }
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }
}

/// An averaged sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Averaged straight RGB (0..=255 scale, alpha-weighted) and alpha (0..=255 scale).
    pub rgb: [f64; 3],
    pub alpha: f64,
    /// The average rounded to 8 bits.
    pub rgba8: [u8; 4],
    /// In-bounds pixels averaged.
    pub pixels: u32,
}

fn round8(v: f64) -> u8 {
    // In range by construction (averages of 8-bit values); the clamp guards rounding only.
    v.round().clamp(0.0, 255.0) as u8
}

/// The `size` sample centred on the document pixel `(x, y)` (module docs); `None` when the centre
/// lies outside the plane.
pub fn sample(plane: &Rgba8Plane<'_>, x: i64, y: i64, size: SampleSize) -> Result<Option<Sample>, Error> {
    plane.validate()?;
    if plane.pixel(x, y).is_none() {
        return Ok(None);
    }
    let r = i64::from(size.edge().checked_div(2).unwrap_or(0));
    let (mut weighted, mut alpha, mut pixels) = ([0f64; 3], 0f64, 0u32);
    for yy in y.saturating_sub(r)..=y.saturating_add(r) {
        for xx in x.saturating_sub(r)..=x.saturating_add(r) {
            let Some(p) = plane.pixel(xx, yy) else { continue };
            let a = f64::from(p[3]);
            for (w, c) in weighted.iter_mut().zip(p) {
                *w += f64::from(c) * a;
            }
            alpha += a;
            pixels = pixels.saturating_add(1);
        }
    }
    let rgb = if alpha > 0.0 { weighted.map(|w| w / alpha) } else { [0.0; 3] };
    let mean_alpha = alpha / f64::from(pixels.max(1));
    Ok(Some(Sample { rgb, alpha: mean_alpha, rgba8: [round8(rgb[0]), round8(rgb[1]), round8(rgb[2]), round8(mean_alpha)], pixels }))
}

/// Tight bounds of the pixels with coverage above 0 and their count; `None` when nothing is covered.
pub fn coverage_bounds(coverage: &CoveragePlane) -> Result<Option<(Rect, u64)>, Error> {
    check_coverage(coverage)?;
    let width = usize::try_from(coverage.bounds.width).map_err(|_| Error::Overflow)?;
    if width == 0 {
        return Ok(None);
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y, mut n) = (usize::MAX, usize::MAX, 0usize, 0usize, 0u64);
    for (i, v) in coverage.data.iter().enumerate() {
        if *v == 0 {
            continue;
        }
        let (cx, cy) = (i.checked_rem(width).ok_or(Error::Overflow)?, i.checked_div(width).ok_or(Error::Overflow)?);
        min_x = min_x.min(cx);
        min_y = min_y.min(cy);
        max_x = max_x.max(cx);
        max_y = max_y.max(cy);
        n = n.saturating_add(1);
    }
    if n == 0 {
        return Ok(None);
    }
    let off = |v: usize| i64::try_from(v).map_err(|_| Error::Overflow);
    let span = |a: usize, b: usize| u32::try_from(b.checked_sub(a).and_then(|d| d.checked_add(1)).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow);
    let rect = Rect {
        x: coverage.bounds.x.checked_add(off(min_x)?).ok_or(Error::Overflow)?,
        y: coverage.bounds.y.checked_add(off(min_y)?).ok_or(Error::Overflow)?,
        width: span(min_x, max_x)?,
        height: span(min_y, max_y)?,
    };
    Ok(Some((rect, n)))
}

/// The colour models of the readout (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColourModel {
    #[default]
    Rgb,
    Hsb,
    Lab,
    /// Unmanaged device CMYK (no profile).
    Cmyk,
}

impl ColourModel {
    pub const ALL: [ColourModel; 4] = [Self::Rgb, Self::Hsb, Self::Lab, Self::Cmyk];
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Hsb => "hsb",
            Self::Lab => "lab",
            Self::Cmyk => "cmyk",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Hsb => "HSB",
            Self::Lab => "Lab",
            Self::Cmyk => "CMYK (unmanaged)",
        }
    }
    /// Component names, in readout order.
    pub const fn components(self) -> &'static [&'static str] {
        match self {
            Self::Rgb => &["R", "G", "B"],
            Self::Hsb => &["H", "S", "B"],
            Self::Lab => &["L", "a", "b"],
            Self::Cmyk => &["C", "M", "Y", "K"],
        }
    }
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code() == code)
    }
}

/// The readout of an RGB colour (0..=255 scale, sRGB-encoded) in `model` (module docs). Refuses
/// non-finite or out-of-range components.
pub fn readout(rgb: [f64; 3], model: ColourModel) -> Result<Vec<f64>, Error> {
    if !rgb.iter().all(|v| v.is_finite() && (0.0..=255.0).contains(v)) {
        return Err(Error::InvalidInput);
    }
    let unit = rgb.map(|v| v / 255.0);
    Ok(match model {
        ColourModel::Rgb => rgb.to_vec(),
        ColourModel::Hsb => {
            let [h, s, b] = hsk_studio_prism::rgb_to_hsb(unit);
            vec![h, s * 100.0, b * 100.0]
        }
        ColourModel::Lab => hsk_studio_prism::srgb_to_lab(unit).to_vec(),
        ColourModel::Cmyk => {
            let k = 1.0 - unit[0].max(unit[1]).max(unit[2]);
            let ink = |c: f64| if k >= 1.0 { 0.0 } else { (1.0 - c - k) / (1.0 - k) * 100.0 };
            vec![ink(unit[0]), ink(unit[1]), ink(unit[2]), k * 100.0]
        }
    })
}
