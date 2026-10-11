//! Sample conventions: how raw 1/8/16/32-bit channel values become numbers, and the sRGB transfer
//! pair used when a document carries no usable profile.
//!
//! UNVERIFIED: whether a 16-bit PSD stores `0..=65535` (full range) or Photoshop's internal
//! `0..=32768` range. Both interpretations are implemented behind [`SixteenBitScale`]; the default
//! is the full range. 32-bit samples are IEEE-754 floats taken as stored (assumed linear light,
//! also UNVERIFIED), and may exceed 1.0.

/// Interpretation of 16-bit samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SixteenBitScale {
    /// `raw / 65535`.
    #[default]
    Full65535,
    /// `raw / 32768` (Photoshop's internal 15-bit-plus-one range); raw values above 32768 map
    /// above 1.0 and are kept.
    Photoshop32768,
}

impl SixteenBitScale {
    pub fn divisor(self) -> f64 {
        match self {
            Self::Full65535 => 65535.0,
            Self::Photoshop32768 => 32768.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Full65535 => "full_65535",
            Self::Photoshop32768 => "photoshop_32768",
        }
    }
}

/// Raw sample (the u32 holds the bit pattern for 32-bit floats) to a number in document units
/// (`0.0..=1.0` for integer depths; unbounded for floats and the 32768 scale).
pub fn sample_to_unit(raw: u32, depth: u16, scale: SixteenBitScale) -> f32 {
    match depth {
        8 => (f64::from(raw) / 255.0) as f32,
        16 => (f64::from(raw) / scale.divisor()) as f32,
        32 => f32::from_bits(raw),
        _ => raw as f32,
    }
}

/// Inverse of [`sample_to_unit`]: nearest representable sample, clamped to the depth's range.
pub fn unit_to_sample(value: f32, depth: u16, scale: SixteenBitScale) -> u32 {
    match depth {
        8 => (f64::from(value).clamp(0.0, 1.0) * 255.0).round() as u32,
        16 => (f64::from(value) * scale.divisor()).round().clamp(0.0, 65535.0) as u32,
        32 => value.to_bits(),
        _ => u32::from(value >= 0.5),
    }
}

/// IEC 61966-2-1 decoding, extended symmetrically outside `0..=1`.
pub fn srgb_to_linear(x: f32) -> f32 {
    let x = f64::from(x);
    let a = x.abs();
    let y = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    y.copysign(x) as f32
}

/// IEC 61966-2-1 encoding, extended symmetrically outside `0..=1`.
pub fn linear_to_srgb(y: f32) -> f32 {
    let y = f64::from(y);
    let a = y.abs();
    let x = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    x.copysign(y) as f32
}
