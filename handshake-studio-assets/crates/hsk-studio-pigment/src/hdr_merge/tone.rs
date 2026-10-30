//! Global tone-mapping of a 32-bit linear frame to 16- or 8-bit display code values ([STU-RAS-033]
//! "tone-mapping between 32/16/8-bit").
//!
//! Operator (Studio reading, declared): exposure `2^exposure_ev`, then the global operator of
//! Reinhard, Stark, Shirley and Ferwerda, "Photographic Tone Reproduction for Digital Images",
//! SIGGRAPH 2002, eq. 4, on luminance `Y` (Rec.709 weights, Prism's `grade::LUMA_REC709`, the
//! linear-sRGB working space): `Yd = Y (1 + Y / Lwhite^2) / (1 + Y)` (`Lwhite = None` is the
//! limit `Y / (1 + Y)`), colour scaled by `Yd / Y` so hue is kept (negative results and `Y <= 0`
//! become 0); no automatic key (the exposure is explicit). The display encoding is Prism's
//! [`DisplayEncoding::encode`] (sRGB piecewise, BT.1886 or gamma 2.2; it clamps to `[0, 1]`), never
//! an ad hoc gamma; code values are `round(v * (2^depth - 1))`, alpha quantised linearly.
//! Photoshop's HDR Toning / Lightroom's HDR rendering are UNVERIFIED (D5).
use super::{DepthCapability, DepthRole, HDR_MAX_EXPOSURE_STOPS, HdrError, HdrLimits, admit, check_cancel, check_frame_samples, frame_area};
use crate::Rect;
use crate::retouch::RgbaPlane;
use hsk_studio_accord::CancellationToken;
use hsk_studio_prism::DisplayEncoding;
use hsk_studio_prism::grade::LUMA_REC709;

/// [`tone_map`]: 32-bit source, 16- or 8-bit result.
pub const HDR_TONE_MAP_CAPABILITY: DepthCapability = DepthCapability {
    command: "hdr_tone_map",
    source_depths: &[32],
    target_depths: &[8, 16],
};

/// Tone-mapping options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneMapOptions {
    /// Exposure before compression, in stops (`|ev| <= 30`).
    pub exposure_ev: f64,
    /// Luminance mapped to display white; `None` = never reaches white.
    pub white: Option<f64>,
    /// Display encoding: [`DisplayEncoding::Srgb`], [`DisplayEncoding::Bt1886`] or
    /// [`DisplayEncoding::Gamma22`] (PQ is an HDR encoding and is refused).
    pub encoding: DisplayEncoding,
    /// 8 or 16 ([`HDR_TONE_MAP_CAPABILITY`]).
    pub target_depth: u8,
}
impl Default for ToneMapOptions {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            white: None,
            encoding: DisplayEncoding::Srgb,
            target_depth: 16,
        }
    }
}

/// Tone-mapped display code values, RGBA, `0..=2^depth - 1` per channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TonedPlane {
    pub bounds: Rect,
    pub depth: u8,
    pub data: Vec<[u16; 4]>,
}

/// Tone-map a 32-bit linear frame (module docs). `source_depth` is the depth of the document the
/// frame belongs to; anything but 32 is refused typed ([STU-RAS-149]).
pub fn tone_map(
    frame: &RgbaPlane,
    source_depth: u8,
    options: &ToneMapOptions,
    limits: &HdrLimits,
    cancel: &CancellationToken,
) -> Result<TonedPlane, HdrError> {
    HDR_TONE_MAP_CAPABILITY.admit(DepthRole::Source, source_depth)?;
    HDR_TONE_MAP_CAPABILITY.admit(DepthRole::Target, options.target_depth)?;
    let ev = options.exposure_ev;
    if !(ev.is_finite() && ev.abs() <= HDR_MAX_EXPOSURE_STOPS) {
        return Err(HdrError::Parameter);
    }
    if options.white.is_some_and(|w| !(w.is_finite() && w > 0.0)) {
        return Err(HdrError::Parameter);
    }
    if !matches!(
        options.encoding,
        DisplayEncoding::Srgb | DisplayEncoding::Bt1886 | DisplayEncoding::Gamma22
    ) {
        return Err(HdrError::Parameter);
    }
    let area = frame_area(frame, 0)?;
    let bytes = (area as u64).checked_mul(8).ok_or(HdrError::Overflow)?;
    admit(bytes, area as u64, limits)?;
    check_frame_samples(frame, 0)?;
    check_cancel(cancel)?;

    let scale = ev.exp2();
    let top = if options.target_depth == 8 { 255.0 } else { 65535.0 };
    let quantise = |v: f32| (f64::from(v.clamp(0.0, 1.0)) * top).round() as u16;
    let luma = LUMA_REC709.map(f64::from);
    let width = frame.bounds.width as usize;
    let mut data = Vec::with_capacity(area);
    for (p, px) in frame.data.iter().enumerate() {
        if p % width == 0 {
            check_cancel(cancel)?;
        }
        let rgb = [
            f64::from(px[0]) * scale,
            f64::from(px[1]) * scale,
            f64::from(px[2]) * scale,
        ];
        let y = luma[0] * rgb[0] + luma[1] * rgb[1] + luma[2] * rgb[2];
        let k = if y > 0.0 {
            let yd = match options.white {
                None => y / (1.0 + y),
                Some(w) => y * (1.0 + y / (w * w)) / (1.0 + y),
            };
            yd / y
        } else {
            0.0
        };
        // The SDR encodings clamp to [0, 1] anyway; clamping first keeps huge values finite.
        let linear = rgb.map(|c| (c * k).clamp(0.0, 1.0) as f32);
        let encoded = options
            .encoding
            .encode(linear)
            .map_err(|_| HdrError::NonFinite { index: 0 })?;
        data.push([
            quantise(encoded[0]),
            quantise(encoded[1]),
            quantise(encoded[2]),
            quantise(px[3]),
        ]);
    }
    Ok(TonedPlane {
        bounds: frame.bounds,
        depth: options.target_depth,
        data,
    })
}
