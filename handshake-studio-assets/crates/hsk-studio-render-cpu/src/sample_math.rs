//! Straight-alpha sample math: W3C Compositing and Blending Level 1 general source-over formula with
//! the separable blend functions of section 10.1. Arithmetic is space-agnostic: the caller's plan
//! declares the blending space and this module never converts between spaces (STU-COL-163).
//!
//! Values outside `[0, 1]` (HDR, linear light) go through the same arithmetic unclamped; the
//! formulas are only specified on `[0, 1]`, so that extension is stated here, not asserted as
//! verified against any external editor.
use crate::RenderError;

/// Blend members with a recovered, source-verified function. Discriminants are the canonical
/// `StudioBlendMode` values of STU-RAS-154; every other member is rejected, never mapped to Normal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum BlendMode {
    Normal = 2,
    Darken = 4,
    Multiply = 5,
    Lighten = 8,
    Screen = 9,
    Overlay = 12,
    Difference = 18,
}

impl BlendMode {
    pub const SUPPORTED: [BlendMode; 7] = [
        Self::Normal,
        Self::Darken,
        Self::Multiply,
        Self::Lighten,
        Self::Screen,
        Self::Overlay,
        Self::Difference,
    ];
    /// Canonical STU-RAS-154 discriminant.
    pub const fn code(self) -> u16 {
        self as u16
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Darken => "darken",
            Self::Multiply => "multiply",
            Self::Lighten => "lighten",
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::Difference => "difference",
        }
    }
}

/// Canonical name for a STU-RAS-154 / STU-RAS-039 discriminant, so a capability error can name the
/// mode it refused. `None` for a code outside 1..=35.
pub const fn blend_mode_name(code: u16) -> Option<&'static str> {
    Some(match code {
        1 => "pass_through",
        2 => "normal",
        3 => "dissolve",
        4 => "darken",
        5 => "multiply",
        6 => "colour_burn",
        7 => "linear_burn",
        8 => "lighten",
        9 => "screen",
        10 => "colour_dodge",
        11 => "linear_dodge_add",
        12 => "overlay",
        13 => "soft_light",
        14 => "hard_light",
        15 => "vivid_light",
        16 => "linear_light",
        17 => "pin_light",
        18 => "difference",
        19 => "exclusion",
        20 => "hue",
        21 => "saturation",
        22 => "colour",
        23 => "luminosity",
        24 => "behind",
        25 => "clear",
        26 => "hard_mix",
        27 => "lighter_colour",
        28 => "darker_colour",
        29 => "subtract",
        30 => "divide",
        31 => "average",
        32 => "negation",
        33 => "reflect",
        34 => "glow",
        35 => "erase",
        _ => return None,
    })
}

impl TryFrom<u16> for BlendMode {
    type Error = RenderError;
    fn try_from(code: u16) -> Result<Self, RenderError> {
        Self::SUPPORTED
            .into_iter()
            .find(|m| m.code() == code)
            .ok_or(RenderError::UnsupportedBlend(code))
    }
}

fn multiply(cb: f64, cs: f64) -> f64 {
    cb * cs
}
fn screen(cb: f64, cs: f64) -> f64 {
    cb + cs - cb * cs
}
/// HardLight(Cb, Cs) of W3C 10.1: multiply by 2*Cs below the midpoint, screen by 2*Cs-1 above.
fn hard_light(cb: f64, cs: f64) -> f64 {
    if cs <= 0.5 {
        multiply(cb, 2.0 * cs)
    } else {
        screen(cb, 2.0 * cs - 1.0)
    }
}

/// Separable blend function `B(Cb, Cs)` on one channel.
pub fn blend_channel(mode: BlendMode, cb: f64, cs: f64) -> f64 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => multiply(cb, cs),
        BlendMode::Screen => screen(cb, cs),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::Difference => (cb - cs).abs(),
        // Overlay(Cb, Cs) = HardLight(Cs, Cb): the backdrop decides multiply versus screen.
        BlendMode::Overlay => hard_light(cs, cb),
    }
}

/// Source-over with a separable blend on straight (non-premultiplied) RGBA, evaluated in binary64
/// and rounded once to binary32. Inputs must be finite; the executor validates them.
///
/// `as = source.a * opacity * coverage`; `ao = as + ab*(1-as)`;
/// `C = ((1-as)*ab*Cb + (1-ab)*as*Cs + as*ab*B(Cb,Cs)) / ao`.
/// `as <= 0` returns the backdrop bit-for-bit; `ao <= 0` returns transparent black.
pub fn composite_straight(
    mode: BlendMode,
    backdrop: [f32; 4],
    source: [f32; 4],
    opacity: f32,
    coverage: f32,
) -> [f32; 4] {
    let ab = f64::from(backdrop[3]);
    let a_s = f64::from(source[3]) * f64::from(opacity) * f64::from(coverage);
    if a_s <= 0.0 {
        return backdrop;
    }
    let ao = a_s + ab * (1.0 - a_s);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0_f32; 4];
    for i in 0..3 {
        let cb = f64::from(backdrop[i]);
        let cs = f64::from(source[i]);
        let b = blend_channel(mode, cb, cs);
        out[i] = (((1.0 - a_s) * ab * cb + (1.0 - ab) * a_s * cs + a_s * ab * b) / ao) as f32;
    }
    out[3] = ao as f32;
    out
}
