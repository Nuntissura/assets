//! Blend functions, fidelity tags and straight-alpha stack evaluation for one pixel sample.
//!
//! Scope: pure scalar maths on already-resolved samples. Colours are STRAIGHT (non-premultiplied)
//! RGB in `[0, 1]`; the composite never converts profiles or transfer curves (STU-COL-163), so
//! `BlendSpace` only DECLARES what the caller's numbers mean and is recorded in the receipt
//! (STU-CMP-021). Every operator carries a fidelity class; the lowest class used travels with
//! the receipt so approximations are never invisible (STU-CMP-020).
use std::fmt;

/// Declared meaning of the sample numbers handed to the compositor (STU-CMP-021).
/// Photoshop's default blends gamma-encoded document values; linear-light compositing is a
/// document choice. No conversion happens here: the caller supplies buffers already in this space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendSpace {
    /// Gamma-encoded (display-referred) numbers blended as stored.
    Encoded,
    /// Linear-light numbers blended as stored.
    Linear,
}

impl BlendSpace {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Encoded => "encoded",
            Self::Linear => "linear",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "encoded" => Some(Self::Encoded),
            "linear" => Some(Self::Linear),
            _ => None,
        }
    }
}

/// The host resolves the document's blending-space profile through prism and passes the validated
/// transfer classification here; composite itself never reads or converts profiles.
impl From<hsk_studio_prism::Transfer> for BlendSpace {
    fn from(transfer: hsk_studio_prism::Transfer) -> Self {
        match transfer {
            hsk_studio_prism::Transfer::LinearLight => Self::Linear,
            hsk_studio_prism::Transfer::Encoded => Self::Encoded,
        }
    }
}

/// Fidelity class of one operator, best to worst. "Exact" means the cited published formula
/// is implemented verbatim; it is NOT a claim of Adobe output parity (see `ModeInfo::reference`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fidelity {
    /// Published standard formula implemented verbatim.
    Exact,
    /// Parameters fitted to observed reference output (no operator is in this class yet).
    Fitted,
    /// Formula from non-Adobe community documentation or a deterministic stand-in; oracle pending.
    Approximate,
    /// No usable formula: evaluation refuses with a typed error, never a silent fallback.
    Unsupported,
}

impl Fidelity {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Exact => 0,
            Self::Fitted => 1,
            Self::Approximate => 2,
            Self::Unsupported => 3,
        }
    }
    pub const fn key(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fitted => "fitted",
            Self::Approximate => "approximate",
            Self::Unsupported => "unsupported",
        }
    }
    /// The less faithful of the two classes (what a receipt carries).
    pub const fn lowest_of(self, other: Self) -> Self {
        if other.rank() > self.rank() { other } else { self }
    }
}

/// Where a blend mode may be set (STU-RAS-155).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
    Layer,
    GroupOnly,
    ToolOnly,
    /// Compositing function not recovered; needs governed spec enrichment (STU-RAS-039).
    Unspecified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Structural,
    Normal,
    Stochastic,
    Separable,
    NonSeparable,
    WholePixel,
    Unspecified,
}

/// `StudioBlendMode` (STU-RAS-154 members 1..=30, STU-RAS-039 members 31..=35) with canonical
/// discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum BlendMode {
    PassThrough = 1,
    Normal = 2,
    Dissolve = 3,
    Darken = 4,
    Multiply = 5,
    ColourBurn = 6,
    LinearBurn = 7,
    Lighten = 8,
    Screen = 9,
    ColourDodge = 10,
    LinearDodgeAdd = 11,
    Overlay = 12,
    SoftLight = 13,
    HardLight = 14,
    VividLight = 15,
    LinearLight = 16,
    PinLight = 17,
    Difference = 18,
    Exclusion = 19,
    Hue = 20,
    Saturation = 21,
    Colour = 22,
    Luminosity = 23,
    Behind = 24,
    Clear = 25,
    HardMix = 26,
    LighterColour = 27,
    DarkerColour = 28,
    Subtract = 29,
    Divide = 30,
    Average = 31,
    Negation = 32,
    Reflect = 33,
    Glow = 34,
    Erase = 35,
}

/// Static facts about one mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub mode: BlendMode,
    pub key: &'static str,
    pub family: Family,
    pub fidelity: Fidelity,
    pub applicability: Applicability,
    pub reference: &'static str,
}

const fn row(
    mode: BlendMode,
    key: &'static str,
    family: Family,
    fidelity: Fidelity,
    applicability: Applicability,
    reference: &'static str,
) -> ModeInfo {
    ModeInfo {
        mode,
        key,
        family,
        fidelity,
        applicability,
        reference,
    }
}

const W3C_NORMAL: &str = "w3c-compositing-1 s5.1 source-over (Normal)";
const W3C_SEP: &str = "w3c-compositing-1 s10.1 separable";
const W3C_NONSEP: &str = "w3c-compositing-1 s10.2 non-separable";
const COMMUNITY: &str =
    "community-documented formula, not Adobe-published; Photoshop oracle pending";
const NO_FORMULA: &str =
    "compositing function not recovered (STU-RAS-039); refused until governed spec enrichment";

/// All 35 modes in discriminant order (`MODE_TABLE[d - 1].mode as u16 == d`).
pub const MODE_TABLE: [ModeInfo; 35] = {
    use Applicability::{GroupOnly, Layer, ToolOnly, Unspecified};
    use BlendMode::*;
    use Fidelity::{Approximate, Exact, Unsupported};
    use Family::{NonSeparable, Normal as FNormal, Separable, Stochastic, Structural, WholePixel};
    [
        row(PassThrough, "pass_through", Structural, Exact, GroupOnly,
            "w3c-compositing-1 s8.2 non-isolated group, no knockout; Photoshop mapping UNVERIFIED"),
        row(Normal, "normal", FNormal, Exact, Layer, W3C_NORMAL),
        row(Dissolve, "dissolve", Stochastic, Approximate, Layer,
            "pseudo-random alpha threshold; Photoshop RNG proprietary, deterministic coordinate hash used"),
        row(Darken, "darken", Separable, Exact, Layer, W3C_SEP),
        row(Multiply, "multiply", Separable, Exact, Layer, W3C_SEP),
        row(ColourBurn, "colour_burn", Separable, Exact, Layer, W3C_SEP),
        row(LinearBurn, "linear_burn", Separable, Approximate, Layer, COMMUNITY),
        row(Lighten, "lighten", Separable, Exact, Layer, W3C_SEP),
        row(Screen, "screen", Separable, Exact, Layer, W3C_SEP),
        row(ColourDodge, "colour_dodge", Separable, Exact, Layer, W3C_SEP),
        row(LinearDodgeAdd, "linear_dodge_add", Separable, Approximate, Layer, COMMUNITY),
        row(Overlay, "overlay", Separable, Exact, Layer, W3C_SEP),
        row(SoftLight, "soft_light", Separable, Exact, Layer,
            "w3c-compositing-1 s10.1 soft-light; Photoshop may use a different variant (oracle pending)"),
        row(HardLight, "hard_light", Separable, Exact, Layer, W3C_SEP),
        row(VividLight, "vivid_light", Separable, Approximate, Layer, COMMUNITY),
        row(LinearLight, "linear_light", Separable, Approximate, Layer, COMMUNITY),
        row(PinLight, "pin_light", Separable, Approximate, Layer, COMMUNITY),
        row(Difference, "difference", Separable, Exact, Layer, W3C_SEP),
        row(Exclusion, "exclusion", Separable, Exact, Layer, W3C_SEP),
        row(Hue, "hue", NonSeparable, Exact, Layer, W3C_NONSEP),
        row(Saturation, "saturation", NonSeparable, Exact, Layer, W3C_NONSEP),
        row(Colour, "colour", NonSeparable, Exact, Layer, W3C_NONSEP),
        row(Luminosity, "luminosity", NonSeparable, Exact, Layer, W3C_NONSEP),
        row(Behind, "behind", FNormal, Exact, ToolOnly,
            "tool-only (STU-RAS-155): paints only transparent areas; not a layer blend"),
        row(Clear, "clear", FNormal, Exact, ToolOnly,
            "tool-only (STU-RAS-155): paints to transparency; not a layer blend"),
        row(HardMix, "hard_mix", Separable, Approximate, Layer, COMMUNITY),
        row(LighterColour, "lighter_colour", WholePixel, Approximate, Layer,
            "pick whole pixel by channel-sum compare; Adobe wording UNVERIFIED; oracle pending"),
        row(DarkerColour, "darker_colour", WholePixel, Approximate, Layer,
            "pick whole pixel by channel-sum compare; Adobe wording UNVERIFIED; oracle pending"),
        row(Subtract, "subtract", Separable, Approximate, Layer, COMMUNITY),
        row(Divide, "divide", Separable, Approximate, Layer, COMMUNITY),
        row(Average, "average", Family::Unspecified, Unsupported, Unspecified, NO_FORMULA),
        row(Negation, "negation", Family::Unspecified, Unsupported, Unspecified, NO_FORMULA),
        row(Reflect, "reflect", Family::Unspecified, Unsupported, Unspecified, NO_FORMULA),
        row(Glow, "glow", Family::Unspecified, Unsupported, Unspecified, NO_FORMULA),
        row(Erase, "erase", Family::Unspecified, Unsupported, Unspecified, NO_FORMULA),
    ]
};

impl BlendMode {
    pub const fn discriminant(self) -> u16 {
        self as u16
    }
    pub fn info(self) -> ModeInfo {
        MODE_TABLE[usize::from(self as u16) - 1]
    }
    pub fn key(self) -> &'static str {
        self.info().key
    }
}

impl TryFrom<u16> for BlendMode {
    type Error = BlendError;
    fn try_from(value: u16) -> Result<Self, BlendError> {
        MODE_TABLE
            .iter()
            .find(|row| row.mode as u16 == value)
            .map(|row| row.mode)
            .ok_or(BlendError::UnknownDiscriminant(value))
    }
}

/// Typed capability / domain failures. None of them degrades to Normal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendError {
    /// `pass_through` applies to groups only.
    GroupOnly(BlendMode),
    /// `behind` / `clear` are tool modes, not layer blends.
    ToolOnly(BlendMode),
    /// No recovered compositing function.
    Unsupported(BlendMode),
    UnknownDiscriminant(u16),
    /// A colour channel, alpha or opacity was non-finite or outside `[0, 1]`.
    OutOfDomain,
    /// Group nesting exceeded `MAX_STACK_DEPTH`.
    TooDeep,
}

impl BlendError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::GroupOnly(_) => "blend_group_only",
            Self::ToolOnly(_) => "blend_tool_only",
            Self::Unsupported(_) => "blend_unsupported",
            Self::UnknownDiscriminant(_) => "blend_unknown_discriminant",
            Self::OutOfDomain => "blend_out_of_domain",
            Self::TooDeep => "blend_too_deep",
        }
    }
}

impl fmt::Display for BlendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GroupOnly(m) | Self::ToolOnly(m) | Self::Unsupported(m) => {
                write!(f, "{}:{}", self.code(), m.key())
            }
            Self::UnknownDiscriminant(d) => write!(f, "{}:{d}", self.code()),
            Self::OutOfDomain | Self::TooDeep => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for BlendError {}

fn in_unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

fn check_rgb(c: [f32; 3]) -> Result<(), BlendError> {
    if c.iter().all(|v| in_unit(*v)) {
        Ok(())
    } else {
        Err(BlendError::OutOfDomain)
    }
}

// ---- separable functions B(Cb, Cs), W3C Compositing 1 s10.1 (b = backdrop, s = source) ----

fn multiply(b: f32, s: f32) -> f32 {
    b * s
}
fn screen(b: f32, s: f32) -> f32 {
    b + s - b * s
}
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        multiply(b, 2.0 * s)
    } else {
        screen(b, 2.0 * s - 1.0)
    }
}
fn overlay(b: f32, s: f32) -> f32 {
    hard_light(s, b)
}
fn colour_dodge(b: f32, s: f32) -> f32 {
    if b == 0.0 {
        0.0
    } else if s == 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
}
fn colour_burn(b: f32, s: f32) -> f32 {
    if b == 1.0 {
        1.0
    } else if s == 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}
fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}
fn difference(b: f32, s: f32) -> f32 {
    (b - s).abs()
}
fn exclusion(b: f32, s: f32) -> f32 {
    b + s - 2.0 * b * s
}
fn darken(b: f32, s: f32) -> f32 {
    b.min(s)
}
fn lighten(b: f32, s: f32) -> f32 {
    b.max(s)
}

// ---- community-documented functions (Approximate); a = backdrop, b = source ----

fn linear_burn(a: f32, b: f32) -> f32 {
    (a + b - 1.0).max(0.0)
}
fn linear_dodge_add(a: f32, b: f32) -> f32 {
    (a + b).min(1.0)
}
fn linear_light(a: f32, b: f32) -> f32 {
    (a + 2.0 * b - 1.0).clamp(0.0, 1.0)
}
fn vivid_light(a: f32, b: f32) -> f32 {
    if b < 0.5 {
        if b == 0.0 {
            if a >= 1.0 { 1.0 } else { 0.0 }
        } else {
            (1.0 - (1.0 - a) / (2.0 * b)).clamp(0.0, 1.0)
        }
    } else if b >= 1.0 {
        if a <= 0.0 { 0.0 } else { 1.0 }
    } else {
        (a / (2.0 * (1.0 - b))).clamp(0.0, 1.0)
    }
}
fn pin_light(a: f32, b: f32) -> f32 {
    if b < 0.5 {
        a.min(2.0 * b)
    } else {
        a.max(2.0 * b - 1.0)
    }
}
fn hard_mix(a: f32, b: f32) -> f32 {
    if a + b >= 1.0 { 1.0 } else { 0.0 }
}
fn subtract(a: f32, b: f32) -> f32 {
    (a - b).max(0.0)
}
fn divide(a: f32, b: f32) -> f32 {
    if b == 0.0 {
        if a == 0.0 { 0.0 } else { 1.0 }
    } else {
        (a / b).min(1.0)
    }
}

fn separable_fn(mode: BlendMode) -> Option<fn(f32, f32) -> f32> {
    use BlendMode::*;
    Some(match mode {
        Darken => darken,
        Multiply => multiply,
        ColourBurn => colour_burn,
        LinearBurn => linear_burn,
        Lighten => lighten,
        Screen => screen,
        ColourDodge => colour_dodge,
        LinearDodgeAdd => linear_dodge_add,
        Overlay => overlay,
        SoftLight => soft_light,
        HardLight => hard_light,
        VividLight => vivid_light,
        LinearLight => linear_light,
        PinLight => pin_light,
        Difference => difference,
        Exclusion => exclusion,
        HardMix => hard_mix,
        Subtract => subtract,
        Divide => divide,
        _ => return None,
    })
}

// ---- non-separable helpers, W3C Compositing 1 s10.2 ----

/// `Lum(C) = 0.3 R + 0.59 G + 0.11 B`.
pub fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}
fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c.iter().copied().fold(f32::INFINITY, f32::min);
    let x = c.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}
/// `SetLum(C, l)`.
pub fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}
/// `Sat(C) = max - min`.
pub fn sat(c: [f32; 3]) -> f32 {
    let n = c.iter().copied().fold(f32::INFINITY, f32::min);
    let x = c.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    x - n
}
/// `SetSat(C, s)`: min channel -> 0, max -> s, mid scaled; all zero when max == min.
pub fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| c[*a].total_cmp(&c[*b]));
    let (imin, imid, imax) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
        out[imax] = s;
    }
    out
}

fn non_separable(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Colour => set_lum(cs, lum(cb)),
        _ => set_lum(cb, lum(cs)),
    }
}

/// The blend function `B(Cb, Cs)` on straight colours in `[0, 1]`.
///
/// `Normal` and `Dissolve` return the source colour (dissolve acts on alpha, see
/// `composite_over`). Structural / tool-only / unrecovered modes return typed errors.
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> Result<[f32; 3], BlendError> {
    use BlendMode::*;
    match mode {
        PassThrough => return Err(BlendError::GroupOnly(mode)),
        Behind | Clear => return Err(BlendError::ToolOnly(mode)),
        Average | Negation | Reflect | Glow | Erase => return Err(BlendError::Unsupported(mode)),
        _ => {}
    }
    check_rgb(cb)?;
    check_rgb(cs)?;
    let raw = match mode {
        Normal | Dissolve => cs,
        Hue | Saturation | Colour | Luminosity => non_separable(mode, cb, cs),
        DarkerColour => {
            if cs.iter().sum::<f32>() < cb.iter().sum::<f32>() {
                cs
            } else {
                cb
            }
        }
        LighterColour => {
            if cs.iter().sum::<f32>() > cb.iter().sum::<f32>() {
                cs
            } else {
                cb
            }
        }
        _ => {
            let f = separable_fn(mode).ok_or(BlendError::Unsupported(mode))?;
            [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])]
        }
    };
    Ok([
        raw[0].clamp(0.0, 1.0),
        raw[1].clamp(0.0, 1.0),
        raw[2].clamp(0.0, 1.0),
    ])
}

/// Straight (non-premultiplied) colour with alpha (STU-CMP-022: alpha convention is explicit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub rgb: [f32; 3],
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self {
        rgb: [0.0; 3],
        a: 0.0,
    };
    pub const fn new(rgb: [f32; 3], a: f32) -> Self {
        Self { rgb, a }
    }
    pub fn premultiplied(self) -> [f32; 4] {
        [
            self.rgb[0] * self.a,
            self.rgb[1] * self.a,
            self.rgb[2] * self.a,
            self.a,
        ]
    }
    /// Zero alpha yields `TRANSPARENT` (colour is undefined there).
    pub fn from_premultiplied(p: [f32; 4]) -> Self {
        if p[3] <= 0.0 {
            return Self::TRANSPARENT;
        }
        Self {
            rgb: [
                (p[0] / p[3]).clamp(0.0, 1.0),
                (p[1] / p[3]).clamp(0.0, 1.0),
                (p[2] / p[3]).clamp(0.0, 1.0),
            ],
            a: p[3].clamp(0.0, 1.0),
        }
    }
    fn check(self) -> Result<(), BlendError> {
        check_rgb(self.rgb)?;
        if in_unit(self.a) {
            Ok(())
        } else {
            Err(BlendError::OutOfDomain)
        }
    }
}

/// Pixel coordinate plus seed; used only by the deterministic `Dissolve` stand-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PixelSite {
    pub x: i64,
    pub y: i64,
    pub seed: u64,
}

fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic uniform value in `[0, 1)` for a site (24 mantissa bits).
pub fn site_unit(site: PixelSite) -> f32 {
    let h = mix64(
        site.seed
            ^ (site.x as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
            ^ (site.y as u64).wrapping_mul(0xA076_1D64_78BD_642F),
    );
    (h >> 40) as f32 / 16_777_216.0
}

/// W3C source-over with blend, straight alpha in and out (Compositing 1 s5.1, s9.1.4):
/// `Cs' = (1-ab) Cs + ab B(Cb,Cs)`, `ao = as + ab(1-as)`, `co = as Cs' + (1-as) ab Cb`, `Cr = co/ao`.
/// `opacity` scales the source alpha. Mode errors surface even when the source is invisible.
pub fn composite_over(
    mode: BlendMode,
    backdrop: Rgba,
    source: Rgba,
    opacity: f32,
    site: PixelSite,
) -> Result<Rgba, BlendError> {
    backdrop.check()?;
    source.check()?;
    if !in_unit(opacity) {
        return Err(BlendError::OutOfDomain);
    }
    let blended = blend_rgb(mode, backdrop.rgb, source.rgb)?;
    let mut a_s = source.a * opacity;
    if mode == BlendMode::Dissolve {
        a_s = if site_unit(site) < a_s { 1.0 } else { 0.0 };
    }
    let a_b = backdrop.a;
    let a_o = a_s + a_b * (1.0 - a_s);
    if a_o <= 0.0 {
        return Ok(Rgba::TRANSPARENT);
    }
    let mut rgb = [0.0f32; 3];
    for (i, out) in rgb.iter_mut().enumerate() {
        let cs_prime = (1.0 - a_b) * source.rgb[i] + a_b * blended[i];
        let co = a_s * cs_prime + (1.0 - a_s) * a_b * backdrop.rgb[i];
        *out = (co / a_o).clamp(0.0, 1.0);
    }
    Ok(Rgba { rgb, a: a_o })
}

/// Maximum group nesting accepted by `evaluate_stack`.
pub const MAX_STACK_DEPTH: usize = 32;

/// One entry of a single-pixel stack, listed bottom-first.
#[derive(Clone, Debug, PartialEq)]
pub enum StackNode {
    Leaf {
        source: Rgba,
        mode: BlendMode,
        opacity: f32,
    },
    /// `mode == PassThrough` composites children directly onto the running backdrop (non-isolated);
    /// any other layer mode renders the children on a transparent backdrop first (isolated) and
    /// then composites the group result with that mode.
    Group {
        children: Vec<StackNode>,
        mode: BlendMode,
        opacity: f32,
    },
}

/// One distinct operator used by an evaluation and its effective fidelity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorTag {
    pub key: &'static str,
    pub fidelity: Fidelity,
}

/// Evaluation inputs that are part of the document, not preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendContext {
    pub space: BlendSpace,
    pub site: PixelSite,
}

/// Receipt of one stack evaluation (STU-CMP-021: blend space recorded; lowest fidelity carried).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlendReceipt {
    pub space: BlendSpace,
    pub lowest: Fidelity,
    pub operators: Vec<OperatorTag>,
}

impl BlendReceipt {
    pub fn new(space: BlendSpace) -> Self {
        Self {
            space,
            lowest: Fidelity::Exact,
            operators: Vec::new(),
        }
    }
    pub fn record(&mut self, key: &'static str, fidelity: Fidelity) {
        self.lowest = self.lowest.lowest_of(fidelity);
        let tag = OperatorTag { key, fidelity };
        if !self.operators.contains(&tag) {
            self.operators.push(tag);
        }
    }
}

fn fold_nodes(
    nodes: &[StackNode],
    mut backdrop: Rgba,
    ctx: &BlendContext,
    receipt: &mut BlendReceipt,
    depth: usize,
) -> Result<Rgba, BlendError> {
    if depth > MAX_STACK_DEPTH {
        return Err(BlendError::TooDeep);
    }
    for node in nodes {
        backdrop = match node {
            StackNode::Leaf {
                source,
                mode,
                opacity,
            } => {
                let out = composite_over(*mode, backdrop, *source, *opacity, ctx.site)?;
                let info = mode.info();
                receipt.record(info.key, info.fidelity);
                out
            }
            StackNode::Group {
                children,
                mode,
                opacity,
            } => {
                if !in_unit(*opacity) {
                    return Err(BlendError::OutOfDomain);
                }
                if *mode == BlendMode::PassThrough {
                    let after = fold_nodes(children, backdrop, ctx, receipt, depth + 1)?;
                    if *opacity >= 1.0 {
                        receipt.record("group_pass_through", Fidelity::Exact);
                        after
                    } else {
                        receipt.record("group_pass_through_opacity", Fidelity::Approximate);
                        let before = backdrop.premultiplied();
                        let after_p = after.premultiplied();
                        let mut mixed = [0.0f32; 4];
                        for (i, out) in mixed.iter_mut().enumerate() {
                            *out = before[i] * (1.0 - opacity) + after_p[i] * opacity;
                        }
                        Rgba::from_premultiplied(mixed)
                    }
                } else {
                    let inner =
                        fold_nodes(children, Rgba::TRANSPARENT, ctx, receipt, depth + 1)?;
                    receipt.record("group_isolated", Fidelity::Exact);
                    let out = composite_over(*mode, backdrop, inner, *opacity, ctx.site)?;
                    let info = mode.info();
                    receipt.record(info.key, info.fidelity);
                    out
                }
            }
        };
    }
    Ok(backdrop)
}

/// Evaluate a bottom-first stack of leaves and groups on one pixel.
pub fn evaluate_stack(
    nodes: &[StackNode],
    backdrop: Rgba,
    ctx: &BlendContext,
) -> Result<(Rgba, BlendReceipt), BlendError> {
    backdrop.check()?;
    let mut receipt = BlendReceipt::new(ctx.space);
    let out = fold_nodes(nodes, backdrop, ctx, &mut receipt, 0)?;
    Ok((out, receipt))
}
