//! Lane SMB-DEVELOP-CONTROLS-2, colour families: point colour, grayscale mix and colour grading.
//! Spec basis: STU-RAW-115 (`point_colours` list, eight grayscale mixer bands, `convert_to_grayscale`)
//! and STU-RAW-116 (colour grading). All three work on the sRGB-encoded value like the rest of
//! `adjust.rs`, are scalar binary64 (no FMA, no SIMD), have identity defaults (the spec says no
//! default is recoverable for the sparse ones), and every numeric mapping is this crate's own choice
//! and UNVERIFIED against Lightroom (oracle session D5).
//!
//! * point colour: a source colour (encoded RGB 0..1) and three range sliders select pixels by a
//!   product of smooth hue / saturation / value falloffs; hue shift, saturation and luminance
//!   sliders (+-100) then act with that weight like a colour-mixer band;
//! * grayscale mix: when `convert` is set the pixel becomes the encoded luma scaled by the hue-band
//!   weighted mix (`1 + sum w_i b_i/100 * saturation`), so greys are unaffected by the bands;
//! * colour grading: shadows / midtones / highlights / global wheels (hue degrees, saturation,
//!   luminance) with a `blending` overlap width and a `balance` shift of the region crossover.
use crate::DevelopError;
use crate::adjust::{band_weights, count, hsv_to_rgb, rgb_to_hsv, smoothstep};

/// Most point colours one recipe may carry.
pub const MAX_POINT_COLOURS: usize = 16;
/// Hue rotation of a point colour at a hue slider of +-100 (degrees).
pub const POINT_HUE_DEGREES: f64 = 30.0;
/// Encoded offset along the wheel's chroma vector at wheel saturation 100.
pub const GRADING_CHROMA: f64 = 0.25;
/// Encoded offset at wheel luminance +-100.
pub const GRADING_LUMINANCE: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointColour {
    /// Sampled colour, encoded sRGB components in 0..1.
    pub source: [f64; 3],
    pub hue_shift: f64,
    pub saturation: f64,
    pub luminance: f64,
    /// 0..100 (default 50): half-width of the selection, 1..60 degrees of hue.
    pub hue_range: f64,
    /// 0..100 (default 50): half-width in saturation, 0.01..0.51.
    pub saturation_range: f64,
    /// 0..100 (default 50): half-width in value, 0.01..0.51.
    pub luminance_range: f64,
}

impl PointColour {
    pub fn new(source: [f64; 3]) -> Self {
        Self {
            source,
            hue_shift: 0.0,
            saturation: 0.0,
            luminance: 0.0,
            hue_range: 50.0,
            saturation_range: 50.0,
            luminance_range: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GrayscaleMix {
    pub convert: bool,
    /// Eight bands in `BAND_NAMES` order, +-100.
    pub bands: [f64; 8],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wheel {
    /// Degrees, 0..360 (360 excluded is not required: the value is taken modulo nothing, only range-checked).
    pub hue: f64,
    /// 0..100.
    pub saturation: f64,
    /// -100..100.
    pub luminance: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColourGrading {
    pub shadows: Wheel,
    pub midtones: Wheel,
    pub highlights: Wheel,
    pub global: Wheel,
    /// 0..100 (default 50): width of the overlap between regions.
    pub blending: f64,
    /// -100..100: moves the crossovers toward highlights (+) or shadows (-).
    pub balance: f64,
}

impl Default for ColourGrading {
    fn default() -> Self {
        Self {
            shadows: Wheel::default(),
            midtones: Wheel::default(),
            highlights: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

fn range(value: f64, lo: f64, hi: f64) -> Result<f64, DevelopError> {
    if !value.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(lo..=hi).contains(&value) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(value)
}

struct PointPrepared {
    source: (f64, f64, f64),
    hue: f64,
    sat: f64,
    lum: f64,
    hue_half: f64,
    sat_half: f64,
    lum_half: f64,
}

struct WheelPrepared {
    chroma: [f64; 3],
    luminance: f64,
}

struct GradingPrepared {
    shadows: Option<WheelPrepared>,
    midtones: Option<WheelPrepared>,
    highlights: Option<WheelPrepared>,
    global: Option<WheelPrepared>,
    low: f64,
    high: f64,
    width: f64,
}

/// Validated, derived colour families (point colour, grayscale mix, colour grading).
#[derive(Default)]
pub(crate) struct ColourExtras {
    points: Vec<PointPrepared>,
    gray: Option<[f64; 8]>,
    grading: Option<GradingPrepared>,
    luma_diff: [f64; 2],
}

fn wheel(w: &Wheel) -> Result<Option<WheelPrepared>, DevelopError> {
    let hue = range(w.hue, 0.0, 360.0)?;
    let sat = range(w.saturation, 0.0, 100.0)? / 100.0;
    let lum = count(w.luminance)? / 100.0;
    if sat == 0.0 && lum == 0.0 {
        return Ok(None);
    }
    let t = hsv_to_rgb(hue, 1.0, 1.0);
    let mean = (t[0] + t[1] + t[2]) / 3.0;
    Ok(Some(WheelPrepared {
        chroma: [
            (t[0] - mean) * sat * GRADING_CHROMA,
            (t[1] - mean) * sat * GRADING_CHROMA,
            (t[2] - mean) * sat * GRADING_CHROMA,
        ],
        luminance: lum * GRADING_LUMINANCE,
    }))
}

impl ColourExtras {
    /// Validates everything (even a disabled family); `colour_on` gates points and grading,
    /// `gray_on` the grayscale mix.
    pub(crate) fn prepare(
        points: &[PointColour],
        gray: &GrayscaleMix,
        grading: &ColourGrading,
        luma: [f64; 3],
        colour_on: bool,
        gray_on: bool,
    ) -> Result<Self, DevelopError> {
        if points.len() > MAX_POINT_COLOURS {
            return Err(DevelopError::InvalidAdjustment);
        }
        let mut prepared_points = Vec::new();
        for p in points {
            for c in p.source {
                range(c, 0.0, 1.0)?;
            }
            let (hue, sat, lum) = (
                count(p.hue_shift)? / 100.0,
                count(p.saturation)? / 100.0,
                count(p.luminance)? / 100.0,
            );
            let hr = range(p.hue_range, 0.0, 100.0)?;
            let sr = range(p.saturation_range, 0.0, 100.0)?;
            let lr = range(p.luminance_range, 0.0, 100.0)?;
            if hue == 0.0 && sat == 0.0 && lum == 0.0 {
                continue;
            }
            let (h, s, v) = rgb_to_hsv(p.source);
            prepared_points.push(PointPrepared {
                source: (if s < 0.02 { f64::NAN } else { h }, s, v),
                hue,
                sat,
                lum,
                hue_half: 1.0 + hr / 100.0 * 59.0,
                sat_half: 0.01 + sr / 100.0 * 0.5,
                lum_half: 0.01 + lr / 100.0 * 0.5,
            });
        }
        let mut bands = [0.0; 8];
        for (slot, b) in bands.iter_mut().zip(gray.bands) {
            *slot = count(b)? / 100.0;
        }
        let blending = range(grading.blending, 0.0, 100.0)? / 100.0;
        let balance = count(grading.balance)? / 100.0;
        let wheels = [
            wheel(&grading.shadows)?,
            wheel(&grading.midtones)?,
            wheel(&grading.highlights)?,
            wheel(&grading.global)?,
        ];
        let [shadows, midtones, highlights, global] = wheels;
        let grading_active = shadows.is_some() || midtones.is_some() || highlights.is_some() || global.is_some();
        let sum = luma[0] + luma[1] + luma[2];
        Ok(Self {
            points: if colour_on { prepared_points } else { Vec::new() },
            gray: (gray_on && gray.convert).then_some(bands),
            grading: (colour_on && grading_active).then_some(GradingPrepared {
                shadows,
                midtones,
                highlights,
                global,
                low: 0.25 + balance * 0.2,
                high: 0.75 + balance * 0.2,
                width: 0.05 + 0.3 * blending,
            }),
            luma_diff: [luma[1] / sum, luma[2] / sum],
        })
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.points.is_empty() || self.gray.is_some() || self.grading.is_some()
    }

    fn luma(&self, e: &[f64; 3]) -> f64 {
        (e[0] + self.luma_diff[0] * (e[1] - e[0])) + self.luma_diff[1] * (e[2] - e[0])
    }

    /// Point colour, grayscale mix, colour grading, in that order, on an encoded value in place.
    pub(crate) fn apply(&self, e: &mut [f64; 3]) {
        let non_negative = e.iter().all(|c| *c >= 0.0);
        if non_negative && !self.points.is_empty() {
            let (h, s, v) = rgb_to_hsv(*e);
            let (mut dh, mut sf, mut dl) = (0.0, 1.0, 0.0);
            for p in &self.points {
                let fh = if p.source.0.is_nan() {
                    1.0
                } else {
                    let d = (h - p.source.0).abs();
                    1.0 - smoothstep(d.min(360.0 - d) / p.hue_half)
                };
                let fs = 1.0 - smoothstep((s - p.source.1).abs() / p.sat_half);
                let fv = 1.0 - smoothstep((v - p.source.2).abs() / p.lum_half);
                let w = (fh * fs) * fv;
                dh += w * p.hue * POINT_HUE_DEGREES;
                sf += w * p.sat;
                dl += w * p.lum;
            }
            let s2 = (s * sf.max(0.0)).clamp(0.0, 1.0);
            // The luminance slider scales value, weighted by saturation like the mixer bands.
            *e = hsv_to_rgb(h + dh, s2, v * (1.0 + 0.5 * dl * s2).max(0.0));
        }
        if let Some(bands) = &self.gray {
            let y = self.luma(e);
            let factor = if e.iter().all(|c| *c >= 0.0) {
                let (h, s, _) = rgb_to_hsv(*e);
                let w = band_weights(h);
                let mix: f64 = w.iter().zip(bands).map(|(wi, b)| wi * b).sum();
                (1.0 + mix * s).max(0.0)
            } else {
                1.0
            };
            *e = [y * factor; 3];
        }
        if let Some(g) = &self.grading {
            let y = self.luma(e).clamp(0.0, 1.0);
            let ss = |t: f64| smoothstep(t);
            let ws = 1.0 - ss((y - (g.low - g.width)) / (2.0 * g.width));
            let wh = ss((y - (g.high - g.width)) / (2.0 * g.width));
            let wm = (1.0 - ws - wh).max(0.0);
            let total = ws + wm + wh;
            let regions = [(&g.shadows, ws / total), (&g.midtones, wm / total), (&g.highlights, wh / total), (&g.global, 1.0)];
            for (wheel, weight) in regions {
                if let Some(w) = wheel {
                    for (c, chroma) in e.iter_mut().zip(w.chroma) {
                        *c += weight * (chroma + w.luminance);
                    }
                }
            }
        }
    }
}
