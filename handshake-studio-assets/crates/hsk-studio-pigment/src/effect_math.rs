//! Single owner (CX-CAP-001) of the layer-effect maths of effects waves 1 and 2 (STU-RAS-040,
//! 156-158): drop shadow, colour overlay, solid stroke (wave 1), inner shadow, outer glow and inner
//! glow (wave 2, colour source only). Brief: work-folder `builders/briefs/lane-effects-wave-1.md`;
//! envelope design: `diagnostics/adjustments-advanced-blending.md`.
//! render-cpu and render-gpu (WGSL tested against this module) consume the matte and the layer
//! composition order defined here; composite lowers `StudioEffectStack` instances to [`EffectOp`].
//!
//! Scope: scalar reference maths on small planes of STRAIGHT colours in `[0, 1]` in the plan's blend
//! space (STU-COL-163: nothing here converts profiles or transfer curves). Photoshop does not
//! publish its effect maths, so every operator is class `Approximate` and the parity questions are
//! on the D5 oracle list (cases 12-17, see [`EffectKind::facts`]). Non-finite or out-of-range
//! parameters are a typed refusal and are not clamped in (`tests/effects.rs`
//! `effect_limits_validation_and_budget`).
//!
//! Layer composition (Studio rule, Approximate, D5 case 14). For one layer with effects, bottom to
//! top: (1) every [`EffectOrder::BehindContent`] effect in list order, (2) the layer content with
//! `opacity * fill_opacity`, (3) every [`EffectOrder::OverContent`] effect in list order. Each effect
//! is composited with its own blend mode against the running result. Every effect fades with the
//! layer opacity and not with the fill opacity (`tests/effects.rs`
//! `colour_overlay_clips_to_alpha_and_ignores_fill`). The effect matte is derived from the
//! layer's SHAPE alpha (content alpha after the layer mask, before fill); which alpha Photoshop uses
//! when a layer mask exists is UNVERIFIED.
//!
//! Bounded work (V4-01/02): sizes and offsets are limited up front ([`MAX_EFFECT_SIZE_PX`],
//! [`MAX_EFFECT_DISTANCE_PX`]); reference planes are at most [`MAX_REFERENCE_SIDE`] per side and the
//! neighbourhood work (`pixels * window`) is checked against [`REFERENCE_WORK_BUDGET`] with checked
//! arithmetic before any allocation or loop (`tests/effects.rs` `effect_limits_validation_and_budget`).
#![deny(clippy::arithmetic_side_effects)]
use crate::blend_math::{
    Applicability, BlendError, Exactness, PixelSite, Rgba, StudioBlendMode, composite_over,
};
use crate::coverage_math::{feather_kernel, feather_support};
use serde::Serialize;
use std::fmt;

/// Windowed evaluation of the matte and the layer composition for an output window of an
/// arbitrarily large layer (bit-identical to the reference below; see the module doc there).
pub mod window;

/// Satin, bevel & emboss and gradient overlay (lane SMB-PS-ADJUST-STYLES).
pub mod style;
pub use style::{
    BevelEmbossOp, BevelPart, BevelShape, BevelStroke, BevelStyle, BevelTechnique,
    GradientOverlayOp, GradientStopOp, GradientStyle, MAX_GRADIENT_STOPS, MAX_PATTERN_SIDE, NoPatterns,
    PatternOverlayOp, PatternPixels, PatternRef, SatinOp,
};

/// Class of every effect operator in this module (see module doc).
pub const EFFECT_EXACTNESS: Exactness = Exactness::Approximate;
/// Largest blur size, spread distance or stroke size in pixels. STU-RAS-157 declares no hard maximum
/// (`unknown`); the largest shipped value observed is 213 px (inner glow size), so this is an
/// engineering bound recorded as a spec gap: larger values are refused with a typed error.
pub const MAX_EFFECT_SIZE_PX: f32 = 1_000.0;
/// Largest shadow distance in pixels (the shipped inner-shadow distance of 30000 px, STU-RAS-157
/// observed range; distance only moves the matte and costs no neighbourhood work).
pub const MAX_EFFECT_DISTANCE_PX: f32 = 30_000.0;
/// Effect instances one layer may carry (STU-RAS-156 requires at least 10 per kind; shipped styles
/// reach 8 strokes; the hard maximum is `unknown`, so this is an engineering bound).
pub const MAX_EFFECTS_PER_LAYER: usize = 64;
/// Effect instances one envelope may carry in total (the running aggregate, V4-02).
pub const MAX_EFFECTS_PER_PLAN: usize = 4_096;
/// Largest side of a reference plane.
pub const MAX_REFERENCE_SIDE: u32 = 64;
/// Budget for the reference neighbourhood work: `pixels * window area` visits per operation.
pub const REFERENCE_WORK_BUDGET: u64 = 100_000_000;

/// A recognised effect feature that this slice refuses instead of approximating (typed; see
/// `tests/effects_wave2.rs` `glow_refusals_envelope_admission_and_budget`). Each has a stable key
/// that admission reports as the refused operator.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectFeature {
    /// A glow filled from a gradient (colour source only).
    GlowGradient,
    /// A glow whose range is not the full 100 % (Photoshop's range semantics are UNVERIFIED, D5 16).
    GlowRange,
    /// A glow with jitter above 0 (Adobe-side evidence says jitter only acts on gradients).
    GlowJitter,
    /// The Precise glow technique (Photoshop uses a distance transform there; Softer only).
    GlowTechniquePrecise,
    /// Chisel hard / chisel soft bevel techniques (rendered since lane SMB-PS-ADJUST-STYLES-2; the
    /// key is kept for stored receipts and is no longer produced by pigment).
    BevelChisel,
    /// Stroke emboss (rendered since lane SMB-PS-ADJUST-STYLES-2; key kept, no longer produced).
    BevelStrokeEmboss,
    /// Bevel soften above 0 (rendered since lane SMB-PS-ADJUST-STYLES-2; key kept, no longer
    /// produced).
    BevelSoften,
    /// More gradient stops than [`MAX_GRADIENT_STOPS`].
    GradientStops,
    /// A pattern overlay whose pattern pixels the caller did not supply (lane SMB-PS-ADJUST-STYLES-2).
    PatternUnresolved,
}
impl EffectFeature {
    pub const fn key(self) -> &'static str {
        match self {
            Self::GlowGradient => "glow_gradient",
            Self::GlowRange => "glow_range",
            Self::GlowJitter => "glow_jitter",
            Self::GlowTechniquePrecise => "glow_technique_precise",
            Self::BevelChisel => "bevel_chisel",
            Self::BevelStrokeEmboss => "bevel_stroke_emboss",
            Self::BevelSoften => "bevel_soften",
            Self::GradientStops => "gradient_stops",
            Self::PatternUnresolved => "pattern_unresolved",
        }
    }
}

/// Typed failure of an effect operation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectError {
    /// A colour, opacity, spread or plane sample was non-finite or outside its range, or an
    /// angle, distance or size was non-finite or negative.
    OutOfDomain,
    /// Shadow angle finite but outside `[-180, 180]` degrees.
    AngleOutOfRange,
    /// Shadow distance finite and non-negative but above [`MAX_EFFECT_DISTANCE_PX`].
    DistanceOutOfRange,
    /// Blur or stroke size finite and non-negative but above [`MAX_EFFECT_SIZE_PX`].
    SizeOutOfRange,
    /// The effect blend mode is not a layer-applicable mode.
    ModeNotApplicable(StudioBlendMode),
    /// Plane side zero or above [`MAX_REFERENCE_SIDE`], or buffer lengths disagree.
    PlaneShape,
    /// The neighbourhood work would exceed [`REFERENCE_WORK_BUDGET`].
    OverBudget,
    /// Size arithmetic overflowed.
    Overflow,
    /// The compositing step refused.
    Blend(BlendError),
    /// A recognised feature this slice refuses (see [`EffectFeature`]).
    Unsupported(EffectFeature),
}
impl EffectError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::OutOfDomain => "effect_out_of_domain",
            Self::AngleOutOfRange => "effect_angle_out_of_range",
            Self::DistanceOutOfRange => "effect_distance_out_of_range",
            Self::SizeOutOfRange => "effect_size_out_of_range",
            Self::ModeNotApplicable(_) => "effect_mode_not_applicable",
            Self::PlaneShape => "effect_plane_shape",
            Self::OverBudget => "effect_over_budget",
            Self::Overflow => "effect_overflow",
            Self::Blend(e) => e.code(),
            Self::Unsupported(_) => "effect_unsupported",
        }
    }
}
impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModeNotApplicable(m) => write!(f, "{}:{}", self.code(), m.key()),
            Self::Blend(e) => write!(f, "{e}"),
            Self::Unsupported(feature) => write!(f, "{}:{}", self.code(), feature.key()),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for EffectError {}
impl From<BlendError> for EffectError {
    fn from(e: BlendError) -> Self {
        Self::Blend(e)
    }
}

/// The effect kinds of waves 1 and 2, with STU-RAS-157 keys.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum EffectKind {
    DropShadow,
    ColourOverlay,
    Stroke,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    // Lane SMB-PS-ADJUST-STYLES (module `style`).
    Satin,
    BevelEmboss,
    GradientOverlay,
    // Lane SMB-PS-ADJUST-STYLES-2 (module `style`).
    PatternOverlay,
}

/// Static facts of one kind: key, formula-provenance class and reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectFacts {
    pub kind: EffectKind,
    pub key: &'static str,
    pub exactness: Exactness,
    pub reference: &'static str,
}
impl EffectKind {
    pub const ALL: [Self; 10] = [
        Self::DropShadow,
        Self::ColourOverlay,
        Self::Stroke,
        Self::InnerShadow,
        Self::OuterGlow,
        Self::InnerGlow,
        Self::Satin,
        Self::BevelEmboss,
        Self::GradientOverlay,
        Self::PatternOverlay,
    ];
    pub const fn facts(self) -> EffectFacts {
        let (key, reference) = match self {
            Self::DropShadow => (
                "drop_shadow",
                "matte = layer shape alpha; spread_px = spread*size grown as a hard disc dilation, blur = size - spread_px by a Gaussian (sigma = coverage_math::feather_sigma), then offset (-cos a, sin a)*distance rounded to whole pixels, then optional knockout alpha*(1-layer alpha). Structure from Krita libs/image/layerstyles/kis_ls_drop_shadow_filter.cpp (spread_size = (spread*size+50)/100, blur_size = size - spread_size, spread then blur then offset then knockout; behaviour reference only, GPL, no code copied); Adobe community evidence that spread has no visible effect at size 0. Photoshop sign convention, radius-to-sigma (Krita uses 0.3r+0.3), sub-pixel offset UNVERIFIED (D5 12)",
            ),
            Self::ColourOverlay => (
                "colour_overlay",
                "colour composited with the effect mode and opacity over the content, matte = layer shape alpha, full strength regardless of fill (STU-RAS-115); Photoshop interplay with fill, masks and blend-interior options UNVERIFIED (D5 14)",
            ),
            Self::Stroke => (
                "stroke",
                "solid stroke from a Euclidean distance field of the layer alpha thresholded at 0.5: outside = dilated minus alpha, inside = alpha minus eroded, centre = both halves; one-pixel linear ramp for anti-aliasing; corners are round; Photoshop stroke corner shape and anti-aliasing UNVERIFIED (D5 13)",
            ),
            Self::InnerShadow => (
                "inner_shadow",
                "matte = 1 - layer alpha (beyond the plane counts as transparent, so 1), choke_px = choke*size grown as a hard disc, blur = size - choke_px by a Gaussian, offset like the drop shadow, then clipped to the layer alpha (multiplied by it), composited over the content. Structure from Krita kis_ls_drop_shadow_filter.cpp (same pipeline for inner shadow, outer and inner glow; the choke uses the spread slot; behaviour reference only, GPL, no code copied); Adobe: choke trims the matte before the blur, Tuts+: choke 100 % gives a hard-edged inner stroke. Direction convention, choke model, clip by alpha vs threshold UNVERIFIED (D5 15)",
            ),
            Self::OuterGlow => (
                "outer_glow",
                "colour glow behind the content: the drop-shadow matte chain with zero offset and no knockout (spread_px = spread*size grown, blur = the rest); technique Softer only. Krita pipeline as for the drop shadow; Precise is a distance transform in Photoshop (ImageMagick forum report on a Photoshop layer-styles port) and is refused, as are gradient fill, range other than 100 % and jitter. Softer is not a plain blur in Photoshop (same report), so Approximate; UNVERIFIED (D5 16)",
            ),
            Self::InnerGlow => (
                "inner_glow",
                "colour glow over the content, clipped to the layer alpha: source edge = the inner-shadow chain with zero offset (inverted matte, choke grown, blurred); source centre = 1 - that edge matte (Krita inverts the selection again after the blur for a centre source); technique Softer only; gradient, range other than 100 % and jitter refused. Tuts+: edge radiates inward from the outline, centre radiates from the middle; choke 100 % is a hard-edged inner stroke. UNVERIFIED (D5 17)",
            ),
            Self::Satin => (
                "satin",
                "|B(x - o) - B(x + o)| of the Gaussian-blurred shape alpha B (size) at the whole-pixel shadow offset o (angle, distance), inverted when invert, times the alpha (effect_math::style); two offset blurred copies as in Krita's satin (behaviour reference, GPL, no code copied); Photoshop combination UNVERIFIED (D5)",
            ),
            Self::BevelEmboss => (
                "bevel_emboss",
                "smooth technique: height depth/100 * size * Gaussian(alpha, size); chisel hard: signed edge distance capped at size (slope depth/100), chisel soft: that field blurred by size/4; central-difference normal, Lambert shading against the flat sin(altitude), highlight (s - s0)/(1 - s0) and shadow (s0 - s)/s0 as two ops; inner/outer/emboss/pillow regions, stroke emboss on the layer's stroke matte (field of shape union stroke); soften blurs the part matte (effect_math::style); Photoshop maths UNVERIFIED (D5)",
            ),
            Self::GradientOverlay => (
                "gradient_overlay",
                "gradient over the layer bounds (linear box-projection length, radial and diamond half the larger side, angle one turn, reflected), linear stop interpolation, matte = alpha x stop opacity (effect_math::style); Photoshop geometry and smoothness UNVERIFIED (D5)",
            ),
            Self::PatternOverlay => (
                "pattern_overlay",
                "pattern pixels (a plan tile source) tiled from the layer origin when linked (else the document origin), shifted by the phase and scaled by the scale fraction, nearest-pixel sampling at pixel centres; colour = pattern colour, matte = alpha x pattern alpha (effect_math::style); Photoshop resampling and phase convention UNVERIFIED (D5)",
            ),
        };
        EffectFacts {
            kind: self,
            key,
            exactness: EFFECT_EXACTNESS,
            reference,
        }
    }
    pub const fn key(self) -> &'static str {
        self.facts().key
    }
}

/// Where an effect sits relative to the layer content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum EffectOrder {
    /// Composited onto the running result before the layer content (outer effects).
    BehindContent,
    /// Composited onto the running result after the layer content (overlays and strokes).
    OverContent,
}

/// Stroke position relative to the layer edge (STU-RAS-157 `position`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum StrokePosition {
    Inside,
    Outside,
    Centre,
}

/// Glow technique (STU-RAS-157 `technique`). Only `Softer` renders; `Precise` is refused as
/// [`EffectFeature::GlowTechniquePrecise`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum GlowTechnique {
    Softer,
    Precise,
}
/// Where an inner glow starts (STU-RAS-157 `source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum GlowSource {
    Edge,
    Centre,
}
/// What fills a glow. Only `Colour` renders (the op's `colour`); `Gradient` is refused as
/// [`EffectFeature::GlowGradient`], so a gradient glow is not drawn as a flat colour.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum GlowFill {
    Colour,
    Gradient,
}

/// Drop shadow. `angle_deg` is the absolute angle (the builder resolves Global Light into it),
/// `spread` is the fraction of `size_px` that is hard, `size_px` the total soft size.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DropShadowOp {
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub angle_deg: f32,
    pub distance_px: f32,
    pub spread: f32,
    pub size_px: f32,
    pub layer_knocks_out: bool,
}
impl DropShadowOp {
    /// A shadow with zero offset, spread and size that does not knock out.
    pub const fn new(colour: [f32; 3], mode: StudioBlendMode, opacity: f32) -> Self {
        Self {
            colour,
            mode,
            opacity,
            angle_deg: 0.0,
            distance_px: 0.0,
            spread: 0.0,
            size_px: 0.0,
            layer_knocks_out: false,
        }
    }
    pub const fn with_offset(mut self, angle_deg: f32, distance_px: f32) -> Self {
        self.angle_deg = angle_deg;
        self.distance_px = distance_px;
        self
    }
    pub const fn with_spread(mut self, spread: f32) -> Self {
        self.spread = spread;
        self
    }
    pub const fn with_size(mut self, size_px: f32) -> Self {
        self.size_px = size_px;
        self
    }
    pub const fn with_layer_knocks_out(mut self, knocks_out: bool) -> Self {
        self.layer_knocks_out = knocks_out;
        self
    }
}

/// Colour overlay.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ColourOverlayOp {
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
}
impl ColourOverlayOp {
    pub const fn new(colour: [f32; 3], mode: StudioBlendMode, opacity: f32) -> Self {
        Self {
            colour,
            mode,
            opacity,
        }
    }
}

/// Solid stroke.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct StrokeOp {
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub size_px: f32,
    pub position: StrokePosition,
}
impl StrokeOp {
    pub const fn new(
        colour: [f32; 3],
        mode: StudioBlendMode,
        opacity: f32,
        size_px: f32,
        position: StrokePosition,
    ) -> Self {
        Self {
            colour,
            mode,
            opacity,
            size_px,
            position,
        }
    }
}

/// Inner shadow: the shadow cast INSIDE the layer shape. `choke` is the fraction of `size_px` that
/// is hard (the inner counterpart of the drop shadow `spread`); the offset convention is the drop
/// shadow's. Composited over the content, clipped to the layer alpha.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct InnerShadowOp {
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub angle_deg: f32,
    pub distance_px: f32,
    pub choke: f32,
    pub size_px: f32,
}
impl InnerShadowOp {
    /// A shadow with zero offset, choke and size.
    pub const fn new(colour: [f32; 3], mode: StudioBlendMode, opacity: f32) -> Self {
        Self {
            colour,
            mode,
            opacity,
            angle_deg: 0.0,
            distance_px: 0.0,
            choke: 0.0,
            size_px: 0.0,
        }
    }
    pub const fn with_offset(mut self, angle_deg: f32, distance_px: f32) -> Self {
        self.angle_deg = angle_deg;
        self.distance_px = distance_px;
        self
    }
    pub const fn with_choke(mut self, choke: f32) -> Self {
        self.choke = choke;
        self
    }
    pub const fn with_size(mut self, size_px: f32) -> Self {
        self.size_px = size_px;
        self
    }
}

/// Outer glow, composited BEHIND the content. `range` is the fraction `(0, 1]` and `jitter` the
/// fraction `[0, 1]` of the source parameters; only `range == 1`, `jitter == 0`, `Softer` and a
/// `Colour` fill render, everything else is refused with an [`EffectFeature`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct OuterGlowOp {
    pub colour: [f32; 3],
    pub fill: GlowFill,
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub spread: f32,
    pub size_px: f32,
    pub technique: GlowTechnique,
    pub range: f32,
    pub jitter: f32,
}
impl OuterGlowOp {
    /// A Softer colour glow with zero spread and size, full range, no jitter.
    pub const fn new(colour: [f32; 3], mode: StudioBlendMode, opacity: f32) -> Self {
        Self {
            colour,
            fill: GlowFill::Colour,
            mode,
            opacity,
            spread: 0.0,
            size_px: 0.0,
            technique: GlowTechnique::Softer,
            range: 1.0,
            jitter: 0.0,
        }
    }
    pub const fn with_spread(mut self, spread: f32) -> Self {
        self.spread = spread;
        self
    }
    pub const fn with_size(mut self, size_px: f32) -> Self {
        self.size_px = size_px;
        self
    }
    pub const fn with_fill(mut self, fill: GlowFill) -> Self {
        self.fill = fill;
        self
    }
    pub const fn with_technique(mut self, technique: GlowTechnique) -> Self {
        self.technique = technique;
        self
    }
    pub const fn with_range(mut self, range: f32) -> Self {
        self.range = range;
        self
    }
    pub const fn with_jitter(mut self, jitter: f32) -> Self {
        self.jitter = jitter;
        self
    }
}

/// Inner glow, composited OVER the content and clipped to the layer alpha; same refusals as
/// [`OuterGlowOp`]. `choke` is the fraction of `size_px` that is hard.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct InnerGlowOp {
    pub colour: [f32; 3],
    pub fill: GlowFill,
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub choke: f32,
    pub size_px: f32,
    pub source: GlowSource,
    pub technique: GlowTechnique,
    pub range: f32,
    pub jitter: f32,
}
impl InnerGlowOp {
    /// A Softer colour glow from the edge with zero choke and size, full range, no jitter.
    pub const fn new(colour: [f32; 3], mode: StudioBlendMode, opacity: f32) -> Self {
        Self {
            colour,
            fill: GlowFill::Colour,
            mode,
            opacity,
            choke: 0.0,
            size_px: 0.0,
            source: GlowSource::Edge,
            technique: GlowTechnique::Softer,
            range: 1.0,
            jitter: 0.0,
        }
    }
    pub const fn with_choke(mut self, choke: f32) -> Self {
        self.choke = choke;
        self
    }
    pub const fn with_size(mut self, size_px: f32) -> Self {
        self.size_px = size_px;
        self
    }
    pub const fn with_source(mut self, source: GlowSource) -> Self {
        self.source = source;
        self
    }
    pub const fn with_fill(mut self, fill: GlowFill) -> Self {
        self.fill = fill;
        self
    }
    pub const fn with_technique(mut self, technique: GlowTechnique) -> Self {
        self.technique = technique;
        self
    }
    pub const fn with_range(mut self, range: f32) -> Self {
        self.range = range;
        self
    }
    pub const fn with_jitter(mut self, jitter: f32) -> Self {
        self.jitter = jitter;
        self
    }
}

/// One effect instance of the envelope (waves 1 and 2, and the styles of `style`). The gradient
/// overlay carries its stops inline (at most [`MAX_GRADIENT_STOPS`]) so the op stays `Copy`, which
/// makes it the largest variant; ops are few per layer ([`MAX_EFFECTS_PER_LAYER`]).
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum EffectOp {
    DropShadow(DropShadowOp),
    ColourOverlay(ColourOverlayOp),
    Stroke(StrokeOp),
    InnerShadow(InnerShadowOp),
    OuterGlow(OuterGlowOp),
    InnerGlow(InnerGlowOp),
    Satin(SatinOp),
    BevelEmboss(BevelEmbossOp),
    GradientOverlay(GradientOverlayOp),
    PatternOverlay(PatternOverlayOp),
}

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
/// Non-finite or negative is a domain error; finite and above `max` is the given limit error.
fn bounded(v: f32, max: f32, over: EffectError) -> Result<(), EffectError> {
    if !v.is_finite() || v < 0.0 {
        Err(EffectError::OutOfDomain)
    } else if v > max {
        Err(over)
    } else {
        Ok(())
    }
}
fn angle_ok(v: f32) -> Result<(), EffectError> {
    if !v.is_finite() {
        Err(EffectError::OutOfDomain)
    } else if !(-180.0..=180.0).contains(&v) {
        Err(EffectError::AngleOutOfRange)
    } else {
        Ok(())
    }
}

impl EffectOp {
    pub const fn kind(&self) -> EffectKind {
        match self {
            Self::DropShadow(_) => EffectKind::DropShadow,
            Self::ColourOverlay(_) => EffectKind::ColourOverlay,
            Self::Stroke(_) => EffectKind::Stroke,
            Self::InnerShadow(_) => EffectKind::InnerShadow,
            Self::OuterGlow(_) => EffectKind::OuterGlow,
            Self::InnerGlow(_) => EffectKind::InnerGlow,
            Self::Satin(_) => EffectKind::Satin,
            Self::BevelEmboss(_) => EffectKind::BevelEmboss,
            Self::GradientOverlay(_) => EffectKind::GradientOverlay,
            Self::PatternOverlay(_) => EffectKind::PatternOverlay,
        }
    }
    /// Fixed by the kind: the drop shadow and outer glow sit behind the content, overlays, strokes,
    /// the inner shadow and the inner glow over it. List
    /// order is the render order within each placement (STU-RAS-156). Whether Photoshop's fixed
    /// kind order may differ from a stored list order is a spec question recorded in the lane notes.
    pub const fn placement(&self) -> EffectOrder {
        match self {
            Self::DropShadow(_) | Self::OuterGlow(_) => EffectOrder::BehindContent,
            Self::ColourOverlay(_)
            | Self::Stroke(_)
            | Self::InnerShadow(_)
            | Self::InnerGlow(_)
            | Self::Satin(_)
            | Self::BevelEmboss(_)
            | Self::GradientOverlay(_)
            | Self::PatternOverlay(_) => EffectOrder::OverContent,
        }
    }
    pub const fn mode(&self) -> StudioBlendMode {
        match self {
            Self::DropShadow(o) => o.mode,
            Self::ColourOverlay(o) => o.mode,
            Self::Stroke(o) => o.mode,
            Self::InnerShadow(o) => o.mode,
            Self::OuterGlow(o) => o.mode,
            Self::InnerGlow(o) => o.mode,
            Self::Satin(o) => o.mode,
            Self::BevelEmboss(o) => o.mode,
            Self::GradientOverlay(o) => o.mode,
            Self::PatternOverlay(o) => o.mode,
        }
    }
    pub const fn opacity(&self) -> f32 {
        match self {
            Self::DropShadow(o) => o.opacity,
            Self::ColourOverlay(o) => o.opacity,
            Self::Stroke(o) => o.opacity,
            Self::InnerShadow(o) => o.opacity,
            Self::OuterGlow(o) => o.opacity,
            Self::InnerGlow(o) => o.opacity,
            Self::Satin(o) => o.opacity,
            Self::BevelEmboss(o) => o.opacity,
            Self::GradientOverlay(o) => o.opacity,
            Self::PatternOverlay(o) => o.opacity,
        }
    }
    /// The uniform colour of the effect. A gradient overlay has a colour per pixel
    /// (`style::gradient_at`); this returns its first stop's colour, which only parameter
    /// validation reads.
    pub const fn colour(&self) -> [f32; 3] {
        match self {
            Self::DropShadow(o) => o.colour,
            Self::ColourOverlay(o) => o.colour,
            Self::Stroke(o) => o.colour,
            Self::InnerShadow(o) => o.colour,
            Self::OuterGlow(o) => o.colour,
            Self::InnerGlow(o) => o.colour,
            Self::Satin(o) => o.colour,
            Self::BevelEmboss(o) => o.colour,
            Self::GradientOverlay(o) => o.stops[0].colour,
            // A pattern has a colour per pixel (its pattern pixels); none is uniform.
            Self::PatternOverlay(_) => [0.0; 3],
        }
    }
    /// Parameter validation shared by the envelope and the reference.
    pub fn validate(&self) -> Result<(), EffectError> {
        if !self.colour().iter().all(|c| unit(*c)) || !unit(self.opacity()) {
            return Err(EffectError::OutOfDomain);
        }
        if self.mode().facts().applicability != Applicability::Layer {
            return Err(EffectError::ModeNotApplicable(self.mode()));
        }
        match self {
            Self::DropShadow(o) => {
                angle_ok(o.angle_deg)?;
                bounded(
                    o.distance_px,
                    MAX_EFFECT_DISTANCE_PX,
                    EffectError::DistanceOutOfRange,
                )?;
                bounded(o.size_px, MAX_EFFECT_SIZE_PX, EffectError::SizeOutOfRange)?;
                if !unit(o.spread) {
                    return Err(EffectError::OutOfDomain);
                }
            }
            Self::ColourOverlay(_) => {}
            Self::Stroke(o) => {
                bounded(o.size_px, MAX_EFFECT_SIZE_PX, EffectError::SizeOutOfRange)?;
            }
            Self::InnerShadow(o) => {
                angle_ok(o.angle_deg)?;
                bounded(
                    o.distance_px,
                    MAX_EFFECT_DISTANCE_PX,
                    EffectError::DistanceOutOfRange,
                )?;
                bounded(o.size_px, MAX_EFFECT_SIZE_PX, EffectError::SizeOutOfRange)?;
                if !unit(o.choke) {
                    return Err(EffectError::OutOfDomain);
                }
            }
            Self::OuterGlow(o) => {
                bounded(o.size_px, MAX_EFFECT_SIZE_PX, EffectError::SizeOutOfRange)?;
                if !unit(o.spread) {
                    return Err(EffectError::OutOfDomain);
                }
                glow_ok(o.fill, o.technique, o.range, o.jitter)?;
            }
            Self::InnerGlow(o) => {
                bounded(o.size_px, MAX_EFFECT_SIZE_PX, EffectError::SizeOutOfRange)?;
                if !unit(o.choke) {
                    return Err(EffectError::OutOfDomain);
                }
                glow_ok(o.fill, o.technique, o.range, o.jitter)?;
            }
            Self::Satin(o) => style::validate_satin(o)?,
            Self::BevelEmboss(o) => style::validate_bevel(o)?,
            Self::GradientOverlay(o) => style::validate_gradient(o)?,
            Self::PatternOverlay(o) => style::validate_pattern(o)?,
        }
        Ok(())
    }
}

/// Glow-only parameters: domain first (`range` in `(0, 1]`, `jitter` in `[0, 1]`), then the typed
/// refusals of the features this slice does not render.
fn glow_ok(
    fill: GlowFill,
    technique: GlowTechnique,
    range: f32,
    jitter: f32,
) -> Result<(), EffectError> {
    if !range.is_finite() || range <= 0.0 || range > 1.0 || !unit(jitter) {
        return Err(EffectError::OutOfDomain);
    }
    if fill == GlowFill::Gradient {
        return Err(EffectError::Unsupported(EffectFeature::GlowGradient));
    }
    if technique == GlowTechnique::Precise {
        return Err(EffectError::Unsupported(
            EffectFeature::GlowTechniquePrecise,
        ));
    }
    if range != 1.0 {
        return Err(EffectError::Unsupported(EffectFeature::GlowRange));
    }
    if jitter != 0.0 {
        return Err(EffectError::Unsupported(EffectFeature::GlowJitter));
    }
    Ok(())
}

/// `(spread_px, blur_px)` of a drop shadow, outer glow, inner shadow or inner glow (pass the choke
/// as `spread` for the inner ones): `spread_px = spread * size` is grown as a hard disc,
/// `blur_px = size - spread_px` is blurred (Krita `kis_ls_drop_shadow_filter.cpp` split, without its
/// integer rounding). Spread 1 is a hard matte grown by the full size.
pub fn shadow_split(size_px: f32, spread: f32) -> (f32, f32) {
    let spread_px = (f64::from(size_px) * f64::from(spread)) as f32;
    let blur_px = (size_px - spread_px).max(0.0);
    (spread_px, blur_px)
}

/// Whole-pixel shadow offset `(dx, dy) = round(distance * (-cos angle, sin angle))`, y down,
/// rounding half away from zero. Angle 180 and distance 2 give `(2, 0)`. Photoshop's sign
/// convention and its handling of fractional offsets are UNVERIFIED (D5 case 12).
pub fn shadow_offset(angle_deg: f32, distance_px: f32) -> Result<(i32, i32), EffectError> {
    angle_ok(angle_deg)?;
    bounded(
        distance_px,
        MAX_EFFECT_DISTANCE_PX,
        EffectError::DistanceOutOfRange,
    )?;
    let rad = f64::from(angle_deg).to_radians();
    let d = f64::from(distance_px);
    let dx = (-(d * rad.cos())).round();
    let dy = (d * rad.sin()).round();
    // Both are bounded by MAX_EFFECT_DISTANCE_PX, far inside i32.
    Ok((dx as i32, dy as i32))
}

/// Pixels of margin around the layer bounds that the effect reads and writes: the matte at a pixel
/// depends only on layer alpha within this many pixels (a backend extends its working rectangle by
/// it per side). Drop shadow, inner shadow: offset + spread (choke) growth + blur kernel half-width;
/// outer and inner glow: the same without offset; overlay: 0; stroke: the distance window (`size`,
/// or `size / 2` for the centre position).
pub fn effect_support_px(op: &EffectOp) -> Result<u32, EffectError> {
    op.validate()?;
    let ceil = |v: f32| v.ceil() as u32;
    match op {
        EffectOp::DropShadow(o) => {
            let (spread_px, blur_px) = shadow_split(o.size_px, o.spread);
            let blur = feather_support(blur_px).map_err(|_| EffectError::SizeOutOfRange)?;
            ceil(o.distance_px)
                .checked_add(ceil(spread_px))
                .and_then(|v| v.checked_add(blur))
                .ok_or(EffectError::Overflow)
        }
        EffectOp::InnerShadow(o) => grown_support(o.size_px, o.choke, ceil(o.distance_px)),
        EffectOp::OuterGlow(o) => grown_support(o.size_px, o.spread, 0),
        EffectOp::InnerGlow(o) => grown_support(o.size_px, o.choke, 0),
        EffectOp::ColourOverlay(_) | EffectOp::GradientOverlay(_) | EffectOp::PatternOverlay(_) => Ok(0),
        EffectOp::Satin(o) => style::satin_support(o),
        EffectOp::BevelEmboss(o) => style::bevel_support(o),
        EffectOp::Stroke(o) => Ok(match o.position {
            StrokePosition::Centre => ceil(o.size_px / 2.0),
            _ => ceil(o.size_px),
        }),
    }
}

/// `offset + ceil(grown) + blur half-width` of a grown-then-blurred matte.
fn grown_support(size_px: f32, grow: f32, offset: u32) -> Result<u32, EffectError> {
    let (grown_px, blur_px) = shadow_split(size_px, grow);
    let blur = feather_support(blur_px).map_err(|_| EffectError::SizeOutOfRange)?;
    offset
        .checked_add(grown_px.ceil() as u32)
        .and_then(|v| v.checked_add(blur))
        .ok_or(EffectError::Overflow)
}

/// A straight-RGBA reference plane of at most [`MAX_REFERENCE_SIDE`] x [`MAX_REFERENCE_SIDE`]
/// pixels, row-major, every sample in `[0, 1]`.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectPlane {
    width: u32,
    height: u32,
    rgb: Vec<[f32; 3]>,
    alpha: Vec<f32>,
}
impl EffectPlane {
    pub fn new(
        width: u32,
        height: u32,
        rgb: Vec<[f32; 3]>,
        alpha: Vec<f32>,
    ) -> Result<Self, EffectError> {
        if width == 0 || height == 0 || width > MAX_REFERENCE_SIDE || height > MAX_REFERENCE_SIDE {
            return Err(EffectError::PlaneShape);
        }
        let len = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(w, h)| w.checked_mul(h))
            .ok_or(EffectError::Overflow)?;
        if rgb.len() != len || alpha.len() != len {
            return Err(EffectError::PlaneShape);
        }
        if !rgb.iter().all(|p| p.iter().all(|c| unit(*c))) || !alpha.iter().all(|a| unit(*a)) {
            return Err(EffectError::OutOfDomain);
        }
        Ok(Self {
            width,
            height,
            rgb,
            alpha,
        })
    }
    /// A fully transparent plane.
    pub fn transparent(width: u32, height: u32) -> Result<Self, EffectError> {
        let len = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(w, h)| w.checked_mul(h))
            .ok_or(EffectError::Overflow)?;
        if width > MAX_REFERENCE_SIDE || height > MAX_REFERENCE_SIDE {
            return Err(EffectError::PlaneShape);
        }
        Self::new(width, height, vec![[0.0; 3]; len], vec![0.0; len])
    }
    pub const fn width(&self) -> u32 {
        self.width
    }
    pub const fn height(&self) -> u32 {
        self.height
    }
    pub fn alpha(&self) -> &[f32] {
        &self.alpha
    }
    pub fn rgb(&self) -> &[[f32; 3]] {
        &self.rgb
    }
    /// The pixel at `(x, y)`, `None` outside the plane.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        let g = Grid::of(self);
        let i = g.idx(i64::from(x), i64::from(y))?;
        Some(Rgba::new(*self.rgb.get(i)?, *self.alpha.get(i)?))
    }
}

/// Plane geometry with checked indexing (all arithmetic in `i64`, bounded by the plane side).
#[derive(Clone, Copy)]
struct Grid {
    w: i64,
    h: i64,
}
impl Grid {
    fn of(p: &EffectPlane) -> Self {
        Self {
            w: i64::from(p.width),
            h: i64::from(p.height),
        }
    }
    fn idx(self, x: i64, y: i64) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        usize::try_from(y.checked_mul(self.w)?.checked_add(x)?).ok()
    }
    fn get(self, v: &[f32], x: i64, y: i64) -> Option<f32> {
        v.get(self.idx(x, y)?).copied()
    }
    fn len(self) -> Result<usize, EffectError> {
        let n = self.w.checked_mul(self.h).ok_or(EffectError::Overflow)?;
        usize::try_from(n).map_err(|_| EffectError::Overflow)
    }
}

/// `out(x) = max over y of src(y) * ramp(dist(x, y))` with `ramp = clamp(r + 0.5 - d, 0, 1)`, where
/// `d` is the distance from the centre of pixel `x` to the unit square of pixel `y` (a pixel covers
/// itself fully). For a binary source this is the disc-dilated region with a one-pixel linear
/// anti-aliasing ramp; samples beyond the plane read `outside`. The window is `ceil(r)` per side.
fn dilate(src: &[f32], g: Grid, r: f32, outside: f32) -> Result<Vec<f32>, EffectError> {
    let k = i64::from(r.ceil() as u32);
    let side = k
        .checked_mul(2)
        .and_then(|v| v.checked_add(1))
        .ok_or(EffectError::Overflow)?;
    let work =
        g.w.checked_mul(g.h)
            .and_then(|v| v.checked_mul(side))
            .and_then(|v| v.checked_mul(side))
            .and_then(|v| u64::try_from(v).ok())
            .ok_or(EffectError::Overflow)?;
    if work > REFERENCE_WORK_BUDGET {
        return Err(EffectError::OverBudget);
    }
    dilate_unbudgeted(src, g, r, outside)
}

/// [`dilate`] without the reference work budget (a windowed caller charges its own meter first).
fn dilate_unbudgeted(src: &[f32], g: Grid, r: f32, outside: f32) -> Result<Vec<f32>, EffectError> {
    let k = i64::from(r.ceil() as u32);
    let lo = k.checked_neg().ok_or(EffectError::Overflow)?;
    let mut out = vec![0.0f32; g.len()?];
    for y in 0..g.h {
        for x in 0..g.w {
            let mut best = 0.0f32;
            for oy in lo..=k {
                for ox in lo..=k {
                    let sx = x.checked_add(ox).ok_or(EffectError::Overflow)?;
                    let sy = y.checked_add(oy).ok_or(EffectError::Overflow)?;
                    let v = g.get(src, sx, sy).unwrap_or(outside);
                    if v <= best {
                        continue;
                    }
                    let cov = if ox == 0 && oy == 0 {
                        1.0
                    } else {
                        let ddx = (ox.unsigned_abs() as f32 - 0.5).max(0.0);
                        let ddy = (oy.unsigned_abs() as f32 - 0.5).max(0.0);
                        (r + 0.5 - ddx.hypot(ddy)).clamp(0.0, 1.0)
                    };
                    best = best.max(v * cov);
                }
            }
            if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                *slot = best;
            }
        }
    }
    Ok(out)
}

/// Separable convolution with `kernel` (odd length), samples beyond the plane read `outside` (the
/// plane is the layer bounds plus its effect margin, so beyond it the layer is transparent: the
/// matte of a shadow or outer glow reads 0 there, the inverted matte of an inner effect reads 1).
/// f64 accumulation.
fn blur_ext(src: &[f32], g: Grid, kernel: &[f32], outside: f32) -> Result<Vec<f32>, EffectError> {
    let half = i64::try_from(kernel.len() / 2).map_err(|_| EffectError::Overflow)?;
    let pass = |input: &[f32], horizontal: bool| -> Result<Vec<f32>, EffectError> {
        let mut out = vec![0.0f32; g.len()?];
        for y in 0..g.h {
            for x in 0..g.w {
                let mut acc = 0.0f64;
                for (i, tap) in kernel.iter().enumerate() {
                    let off = i64::try_from(i)
                        .ok()
                        .and_then(|i| i.checked_sub(half))
                        .ok_or(EffectError::Overflow)?;
                    let (sx, sy) = if horizontal {
                        (x.checked_add(off).ok_or(EffectError::Overflow)?, y)
                    } else {
                        (x, y.checked_add(off).ok_or(EffectError::Overflow)?)
                    };
                    acc += f64::from(*tap) * f64::from(g.get(input, sx, sy).unwrap_or(outside));
                }
                if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                    *slot = acc as f32;
                }
            }
        }
        Ok(out)
    };
    let h = pass(src, true)?;
    pass(&h, false)
}

/// `out(x, y) = src(x - dx, y - dy)`, samples from beyond the plane read `outside`.
fn shift_ext(
    src: &[f32],
    g: Grid,
    dx: i32,
    dy: i32,
    outside: f32,
) -> Result<Vec<f32>, EffectError> {
    let mut out = vec![0.0f32; g.len()?];
    for y in 0..g.h {
        for x in 0..g.w {
            let sx = x.checked_sub(i64::from(dx)).ok_or(EffectError::Overflow)?;
            let sy = y.checked_sub(i64::from(dy)).ok_or(EffectError::Overflow)?;
            let v = g.get(src, sx, sy).unwrap_or(outside);
            if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                *slot = v;
            }
        }
    }
    Ok(out)
}

/// The matte chain shared by the drop shadow, the inner shadow and both glows: grow `matte` by
/// `grown_px` as a hard disc, blur by `blur_px`, move by `offset`; samples beyond the plane read
/// `outside`.
fn matte_chain(
    matte: &[f32],
    g: Grid,
    (grown_px, blur_px): (f32, f32),
    (dx, dy): (i32, i32),
    outside: f32,
) -> Result<Vec<f32>, EffectError> {
    let grown = if grown_px > 0.0 {
        dilate(matte, g, grown_px, outside)?
    } else {
        matte.to_vec()
    };
    let kernel = feather_kernel(blur_px).map_err(|_| EffectError::SizeOutOfRange)?;
    let blurred = blur_ext(&grown, g, &kernel, outside)?;
    shift_ext(&blurred, g, dx, dy, outside)
}

/// One half of a stroke: `outside` = region dilated by `w` minus the alpha, otherwise the alpha
/// minus the region eroded by `w`. The region is `alpha >= 0.5`; beyond the plane is outside.
fn stroke_half(alpha: &[f32], g: Grid, w: f32, outside: bool) -> Result<Vec<f32>, EffectError> {
    stroke_half_by(alpha, g, w, outside, dilate)
}

/// The dilation primitive of a stroke half.
type Dilate = fn(&[f32], Grid, f32, f32) -> Result<Vec<f32>, EffectError>;

/// [`stroke_half`] with the dilation `grow` (the budgeted reference one or the unbudgeted one of a
/// windowed caller).
fn stroke_half_by(
    alpha: &[f32],
    g: Grid,
    w: f32,
    outside: bool,
    grow: Dilate,
) -> Result<Vec<f32>, EffectError> {
    if w <= 0.0 {
        return Ok(vec![0.0; g.len()?]);
    }
    let region: Vec<f32> = alpha
        .iter()
        .map(|a| if *a >= 0.5 { 1.0 } else { 0.0 })
        .collect();
    if outside {
        let grown = grow(&region, g, w, 0.0)?;
        Ok(grown
            .iter()
            .zip(alpha)
            .map(|(d, a)| d * (1.0 - a))
            .collect())
    } else {
        let complement: Vec<f32> = region.iter().map(|r| 1.0 - r).collect();
        let grown = grow(&complement, g, w, 1.0)?;
        Ok(grown.iter().zip(alpha).map(|(d, a)| d * a).collect())
    }
}

/// The matte of one effect (coverage in `[0, 1]` before colour, mode and opacity) for a layer whose
/// shape alpha is `plane.alpha()`. Reference definition of the neighbourhood part of every wave 1
/// effect; backends are tested against it.
///
/// - Drop shadow: grow by `spread_px`, blur by `blur_px` ([`shadow_split`]), move by
///   [`shadow_offset`], then with `layer_knocks_out` multiply by `1 - alpha`.
/// - Colour overlay: the alpha.
/// - Stroke: see [`EffectKind::facts`]; size 0 is an empty matte.
/// - Outer glow: the drop-shadow chain with zero offset and no knockout.
/// - Inner shadow: the chain on `1 - alpha` (choke as the grown share of `size`, beyond the plane
///   reads 1), then multiplied by the alpha.
/// - Inner glow: the inner-shadow chain with zero offset; source edge multiplies by the alpha,
///   source centre multiplies `1 - chain` by the alpha.
pub fn reference_matte(plane: &EffectPlane, op: &EffectOp) -> Result<Vec<f32>, EffectError> {
    reference_matte_with(plane, op, &NoPatterns)
}

/// [`reference_matte`] with the pattern pixels a pattern overlay reads (lane
/// SMB-PS-ADJUST-STYLES-2): its matte is the alpha times the pattern pixel's alpha (without the
/// pixels: `Unsupported(PatternUnresolved)`).
pub fn reference_matte_with(
    plane: &EffectPlane,
    op: &EffectOp,
    patterns: &dyn PatternPixels,
) -> Result<Vec<f32>, EffectError> {
    op.validate()?;
    let g = Grid::of(plane);
    let alpha = plane.alpha();
    match op {
        EffectOp::ColourOverlay(_) => Ok(alpha.to_vec()),
        EffectOp::Satin(o) => style::satin_matte(alpha, g, o),
        EffectOp::BevelEmboss(o) => {
            // the reference plane's neighbourhood work, before any pass (the windowed evaluator
            // charges its own meter instead)
            let work = u64::try_from(g.len()?)
                .ok()
                .and_then(|n| n.checked_mul(style::bevel_work_per_px(o).ok()?))
                .ok_or(EffectError::Overflow)?;
            if work > REFERENCE_WORK_BUDGET {
                return Err(EffectError::OverBudget);
            }
            style::bevel_matte(alpha, g, o)
        }
        EffectOp::PatternOverlay(o) => {
            let pixels = style::pattern_pixels(o, patterns)?;
            let mut out = alpha.to_vec();
            let bounds = crate::Rect { x: 0, y: 0, width: plane.width, height: plane.height };
            for y in 0..g.h {
                for x in 0..g.w {
                    let p = o.sample_at(x, y, bounds, pixels).ok_or(EffectError::PlaneShape)?;
                    if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                        *slot *= p[3];
                    }
                }
            }
            Ok(out)
        }
        EffectOp::GradientOverlay(o) => {
            let mut out = alpha.to_vec();
            let bounds = (0, 0, plane.width, plane.height);
            for y in 0..g.h {
                for x in 0..g.w {
                    let (_, opacity) = style::gradient_at(o, x, y, bounds);
                    if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                        *slot *= opacity;
                    }
                }
            }
            Ok(out)
        }
        EffectOp::DropShadow(o) => {
            let split = shadow_split(o.size_px, o.spread);
            let offset = shadow_offset(o.angle_deg, o.distance_px)?;
            let mut matte = matte_chain(alpha, g, split, offset, 0.0)?;
            if o.layer_knocks_out {
                for (m, a) in matte.iter_mut().zip(alpha) {
                    *m *= 1.0 - a;
                }
            }
            Ok(matte)
        }
        EffectOp::OuterGlow(o) => {
            matte_chain(alpha, g, shadow_split(o.size_px, o.spread), (0, 0), 0.0)
        }
        EffectOp::InnerShadow(o) => {
            let offset = shadow_offset(o.angle_deg, o.distance_px)?;
            let inverted: Vec<f32> = alpha.iter().map(|a| 1.0 - a).collect();
            let matte = matte_chain(&inverted, g, shadow_split(o.size_px, o.choke), offset, 1.0)?;
            Ok(matte.iter().zip(alpha).map(|(m, a)| m * a).collect())
        }
        EffectOp::InnerGlow(o) => {
            let inverted: Vec<f32> = alpha.iter().map(|a| 1.0 - a).collect();
            let edge = matte_chain(&inverted, g, shadow_split(o.size_px, o.choke), (0, 0), 1.0)?;
            Ok(edge
                .iter()
                .zip(alpha)
                .map(|(m, a)| match o.source {
                    GlowSource::Edge => m * a,
                    GlowSource::Centre => (1.0 - m) * a,
                })
                .collect())
        }
        EffectOp::Stroke(o) => match o.position {
            StrokePosition::Outside => stroke_half(alpha, g, o.size_px, true),
            StrokePosition::Inside => stroke_half(alpha, g, o.size_px, false),
            StrokePosition::Centre => {
                let half = o.size_px / 2.0;
                let out = stroke_half(alpha, g, half, true)?;
                let inn = stroke_half(alpha, g, half, false)?;
                Ok(out
                    .iter()
                    .zip(&inn)
                    .map(|(a, b)| (a + b).min(1.0))
                    .collect())
            }
        },
    }
}

/// Layer parameters of a reference composition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerParams {
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub fill_opacity: f32,
}
impl LayerParams {
    /// Normal mode, opacity 1, fill 1.
    pub const fn plain() -> Self {
        Self {
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
        }
    }
}

/// Reference composition of one layer with effects over `backdrop` (same size as `content`): the
/// order and fade rules of the module doc. Content is composited with `opacity * fill_opacity`
/// (render-cpu's fold of fill, class Approximate); effects with `effect opacity * layer opacity`.
pub fn render_layer_reference(
    backdrop: &EffectPlane,
    content: &EffectPlane,
    layer: LayerParams,
    effects: &[EffectOp],
) -> Result<EffectPlane, EffectError> {
    render_layer_reference_with(backdrop, content, layer, effects, &NoPatterns)
}

/// [`render_layer_reference`] with the pattern pixels its pattern overlays read (lane
/// SMB-PS-ADJUST-STYLES-2; the colour of a pattern overlay at a pixel is the pattern pixel's
/// colour, its geometry the reference plane as for the gradient overlay).
pub fn render_layer_reference_with(
    backdrop: &EffectPlane,
    content: &EffectPlane,
    layer: LayerParams,
    effects: &[EffectOp],
    patterns: &dyn PatternPixels,
) -> Result<EffectPlane, EffectError> {
    if backdrop.width != content.width || backdrop.height != content.height {
        return Err(EffectError::PlaneShape);
    }
    if !unit(layer.opacity) || !unit(layer.fill_opacity) {
        return Err(EffectError::OutOfDomain);
    }
    if layer.mode.facts().applicability != Applicability::Layer {
        return Err(EffectError::ModeNotApplicable(layer.mode));
    }
    let mut mattes = Vec::with_capacity(effects.len());
    for op in effects {
        mattes.push(reference_matte_with(content, op, patterns)?);
    }
    let plane_rect = crate::Rect { x: 0, y: 0, width: content.width, height: content.height };
    let g = Grid::of(content);
    let mut rgb = backdrop.rgb.clone();
    let mut alpha = backdrop.alpha.clone();
    for y in 0..g.h {
        for x in 0..g.w {
            let i = g.idx(x, y).ok_or(EffectError::PlaneShape)?;
            let site = PixelSite { x, y, seed: 0 };
            let mut px = Rgba::new(
                *rgb.get(i).ok_or(EffectError::PlaneShape)?,
                *alpha.get(i).ok_or(EffectError::PlaneShape)?,
            );
            let apply = |px: Rgba, k: usize| -> Result<Rgba, EffectError> {
                let (Some(op), Some(m)) = (effects.get(k), mattes.get(k)) else {
                    return Err(EffectError::PlaneShape);
                };
                let colour = match op {
                    EffectOp::GradientOverlay(o) => {
                        style::gradient_at(o, x, y, (0, 0, content.width, content.height)).0
                    }
                    EffectOp::PatternOverlay(o) => {
                        let p = o
                            .sample_at(x, y, plane_rect, style::pattern_pixels(o, patterns)?)
                            .ok_or(EffectError::PlaneShape)?;
                        [p[0], p[1], p[2]]
                    }
                    _ => op.colour(),
                };
                let src = Rgba::new(colour, m.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0));
                Ok(composite_over(
                    op.mode(),
                    px,
                    src,
                    op.opacity() * layer.opacity,
                    site,
                )?)
            };
            for (k, op) in effects.iter().enumerate() {
                if op.placement() == EffectOrder::BehindContent {
                    px = apply(px, k)?;
                }
            }
            let src = Rgba::new(
                *content.rgb.get(i).ok_or(EffectError::PlaneShape)?,
                *content.alpha.get(i).ok_or(EffectError::PlaneShape)?,
            );
            px = composite_over(
                layer.mode,
                px,
                src,
                layer.opacity * layer.fill_opacity,
                site,
            )?;
            for (k, op) in effects.iter().enumerate() {
                if op.placement() == EffectOrder::OverContent {
                    px = apply(px, k)?;
                }
            }
            if let (Some(c), Some(a)) = (rgb.get_mut(i), alpha.get_mut(i)) {
                *c = px.rgb;
                *a = px.a;
            }
        }
    }
    EffectPlane::new(content.width, content.height, rgb, alpha)
}

/// Reference render of `content` with ONE effect over a transparent backdrop, layer Normal at
/// opacity 1 and fill 1.
pub fn render_reference(content: &EffectPlane, op: &EffectOp) -> Result<EffectPlane, EffectError> {
    let backdrop = EffectPlane::transparent(content.width, content.height)?;
    render_layer_reference(&backdrop, content, LayerParams::plain(), &[*op])
}
