//! Straight-alpha sample math: W3C Compositing and Blending Level 1 general source-over formula with
//! the separable (10.1) and non-separable (10.2) blend functions. Arithmetic is space-agnostic: the
//! caller's plan declares the blending space and this module never converts between spaces
//! (STU-COL-163).
//!
//! Basis: every supported function is the published W3C/PDF function. That is NOT proof of Adobe
//! parity (Photoshop may differ, e.g. soft light variants); parity stays NOT_PROVEN until an
//! Adobe-produced oracle exists. Values outside `[0, 1]` (HDR, linear light) go through the same
//! arithmetic unclamped, except where a formula clamps itself (dodge/burn minima, ClipColor); the
//! formulas are only specified on `[0, 1]`, so that extension is stated here, not verified.
use crate::RenderError;

/// Blend members with a recovered, source-verified function. Discriminants are the canonical
/// `StudioBlendMode` values of STU-RAS-154; every other member is rejected, never mapped to Normal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum BlendMode {
    Normal = 2,
    Darken = 4,
    Multiply = 5,
    ColourBurn = 6,
    Lighten = 8,
    Screen = 9,
    ColourDodge = 10,
    Overlay = 12,
    SoftLight = 13,
    HardLight = 14,
    Difference = 18,
    Exclusion = 19,
    Hue = 20,
    Saturation = 21,
    Colour = 22,
    Luminosity = 23,
}

impl BlendMode {
    pub const SUPPORTED: [BlendMode; 16] = [
        Self::Normal,
        Self::Darken,
        Self::Multiply,
        Self::ColourBurn,
        Self::Lighten,
        Self::Screen,
        Self::ColourDodge,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::Difference,
        Self::Exclusion,
        Self::Hue,
        Self::Saturation,
        Self::Colour,
        Self::Luminosity,
    ];
    /// Canonical STU-RAS-154 discriminant.
    pub const fn code(self) -> u16 {
        self as u16
    }
    pub const fn name(self) -> &'static str {
        match blend_mode_name(self as u16) {
            Some(name) => name,
            None => "unknown",
        }
    }
    /// True when the function acts on each channel independently (W3C 10.1); false for the four
    /// component modes that act on the whole RGB triple (W3C 10.2).
    pub const fn is_separable(self) -> bool {
        !matches!(
            self,
            Self::Hue | Self::Saturation | Self::Colour | Self::Luminosity
        )
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
fn soft_light(cb: f64, cs: f64) -> f64 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}
fn colour_dodge(cb: f64, cs: f64) -> f64 {
    if cb == 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}
fn colour_burn(cb: f64, cs: f64) -> f64 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

/// How a mode is evaluated: one channel at a time (W3C 10.1) or on the whole triple (10.2).
enum Kind {
    Separable(fn(f64, f64) -> f64),
    Component(fn([f64; 3], [f64; 3]) -> [f64; 3]),
}

fn kind(mode: BlendMode) -> Kind {
    match mode {
        BlendMode::Normal => Kind::Separable(|_, cs| cs),
        BlendMode::Multiply => Kind::Separable(multiply),
        BlendMode::Screen => Kind::Separable(screen),
        BlendMode::Darken => Kind::Separable(f64::min),
        BlendMode::Lighten => Kind::Separable(f64::max),
        BlendMode::Difference => Kind::Separable(|cb, cs| (cb - cs).abs()),
        BlendMode::Exclusion => Kind::Separable(|cb, cs| cb + cs - 2.0 * cb * cs),
        BlendMode::ColourDodge => Kind::Separable(colour_dodge),
        BlendMode::ColourBurn => Kind::Separable(colour_burn),
        BlendMode::HardLight => Kind::Separable(hard_light),
        BlendMode::SoftLight => Kind::Separable(soft_light),
        // Overlay(Cb, Cs) = HardLight(Cs, Cb): the backdrop decides multiply versus screen.
        BlendMode::Overlay => Kind::Separable(|cb, cs| hard_light(cs, cb)),
        BlendMode::Hue => Kind::Component(|cb, cs| set_lum(set_sat(cs, sat(cb)), lum(cb))),
        BlendMode::Saturation => Kind::Component(|cb, cs| set_lum(set_sat(cb, sat(cs)), lum(cb))),
        BlendMode::Colour => Kind::Component(|cb, cs| set_lum(cs, lum(cb))),
        BlendMode::Luminosity => Kind::Component(|cb, cs| set_lum(cb, lum(cs))),
    }
}

fn lum(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}
fn clip_colour(c: [f64; 3]) -> [f64; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out
}
fn set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - lum(c);
    clip_colour(c.map(|v| v + d))
}
fn sat(c: [f64; 3]) -> f64 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}
fn set_sat(c: [f64; 3], s: f64) -> [f64; 3] {
    let mut idx = [0_usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
    let [lo, mid, hi] = idx;
    let mut out = [0.0; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out
}

/// Blend function `B(Cb, Cs)` on an RGB triple (W3C 10.1 per channel, 10.2 whole-triple).
pub fn blend_rgb(mode: BlendMode, cb: [f64; 3], cs: [f64; 3]) -> [f64; 3] {
    match kind(mode) {
        Kind::Separable(f) => std::array::from_fn(|i| f(cb[i], cs[i])),
        Kind::Component(f) => f(cb, cs),
    }
}

/// Source-over with a blend function on straight (non-premultiplied) RGBA, evaluated in binary64
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
    let cb = [backdrop[0], backdrop[1], backdrop[2]].map(f64::from);
    let cs = [source[0], source[1], source[2]].map(f64::from);
    let b = blend_rgb(mode, cb, cs);
    let mut out = [0.0_f32; 4];
    for i in 0..3 {
        out[i] =
            (((1.0 - a_s) * ab * cb[i] + (1.0 - ab) * a_s * cs[i] + a_s * ab * b[i]) / ao) as f32;
    }
    out[3] = ao as f32;
    out
}
