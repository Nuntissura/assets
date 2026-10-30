//! Lowering of the Folio per-layer effect stack (lane SMB-AE-EFFECT-STACK-1; parity AE-17) into
//! [`crate::composition::SceneContent::Effected`].
//!
//! At read time ([`read_effects`]) the entries that run are kept in stack order: disabled entries
//! are dropped ([STU-FX-003] "individually enable/disable-able"); when any enabled entry is soloed,
//! only the enabled soloed entries run (Folio `solo`, Studio reading). Each value becomes a motion
//! `Property` through the transform-stream keyframe conversion ([STU-FX-011b] "an effect parameter
//! exposed to motion is the same typed field, sampled per frame"). A stack on a layer without a
//! content rect of its own (a null layer, an adjustment layer, a collapsed precomposition) is
//! refused [`COMPOSITION_EFFECT_CONTENT`] (adjustment-layer effects, STU-CMP-015, are not part of
//! this lane).
//!
//! At a tick ([`CompositionTemporalSource::effected`]) every value is sampled (an absent value is
//! its row default), checked against its declared hard bounds again (a Bezier keyframe may
//! overshoot; refused `InvalidState` with field `effect_param_out_of_range`, never clamped,
//! [STU-FX-104]), and lowered to surface pixels: a `point_2d` is a fraction of the layer size
//! ([STU-FX-112]) measured from the layer origin, the rect is the content's plan rect (a static
//! subtree's surface, else its bounds). The layer's mask paths are folded into the content as
//! layer-space coverage over that rect ([STU-CMP-010] step 2 "Combine the layer's masks ... and
//! multiply it into the layer's alpha", before step 3 "Effects"), so a layer with an effect stack
//! carries no composition-space mask matte. Options and values whose behaviour this lane does not
//! implement are refused [`COMPOSITION_EFFECT_OPTION`], never approximated: generator blending modes
//! other than `None` / `Normal`, Circle `Edge` other than `None` and a non-zero `Feather Inner
//! Edge`, a non-zero `Ramp Scatter`, a non-zero Checkerboard feather.
//!
//! Studio readings (UNVERIFIED, D5): Twirl `Twirl Radius` is percent of the larger layer side;
//! Spherize `Radius` and Circle `Radius` / `Feather Outer Edge` and Checkerboard `Width` / `Height`
//! are layer pixels; Offset moves the layer centre to `Shift Center To`.
//!
//! Lane SMB-AE-EFFECT-FAMILIES-2 (blur, glow and colour families; entry blend mode and mask). The
//! lowering maps each row onto pigment's kernels (`composition::effect_stack` module docs); a value
//! whose kernel pigment does not have is refused [`COMPOSITION_EFFECT_KERNEL`], a value outside a
//! pigment kernel's domain [`COMPOSITION_EFFECT_OPTION`]. Studio readings (UNVERIFIED, D5):
//! - Gaussian Blur `Blurriness` is pigment's blur radius in layer pixels (`sigma = radius / 2`),
//!   at most pigment's `MAX_BLUR_RADIUS_PX` (1000; above: kernel refusal). Fast Box Blur `Blur
//!   Radius` rounds half up to the box half-width and `Iterations` half up to the pass count
//!   (pigment `box_kernel`, at most `MAX_BOX_KERNEL_TAPS` taps; above: kernel refusal). `Blur
//!   Dimensions` / `Glow Dimensions` `Horizontal` / `Vertical` run pigment's one-axis pass.
//! - Directional Blur (pigment `directional_blur_window`): `Direction` is measured clockwise from
//!   screen-up, so pigment's angle (from +x towards screen-up) is `90 - Direction`; `Blur Length`
//!   is the segment length in layer pixels. Radial Blur (pigment `radial_blur_window`): `Amount`
//!   1:1, `Center` a layer point, `Type` Spin | Zoom; `Antialiasing (Best Quality)` and `Random
//!   Seed` do not change pigment's kernel (no quality modes, no dither). Find Edges (pigment
//!   `find_edges_window`): the `invert` row, `Blend With Original` mixes the original back in.
//! - Glow: `Glow Threshold` `v` is the fraction `v / 255`; `Glow Radius` is the blur radius;
//!   `Glow Operation` maps to the pigment blend mode of the same name (`Add` = linear dodge (add));
//!   `None`, `Dissolve` and the stencil / silhouette / luminescent / alpha-add operations, `Arbitrary
//!   Map` colours and a `Color Phase` other than its default are refused.
//! - Colour: Levels rows are the composite (RGB) record (the per-channel records are not
//!   recovered: identity) and `Channel:` selects the panel view only; the clip options are no-ops
//!   on the unit-range stack. Hue/Saturation `Channel Control` selects the panel view only; with
//!   `colorize` the colorize rows replace the master rows; hues wrap into `-180..180`. Exposure
//!   linearises encoded values by pigment's `x^2.2` stand-in unless the plan is linear light or
//!   `bypass_linear_light_conversion` is set; `Individual Channels` runs one exposure per channel.
//!   Tint is pigment's gradient map black -> `Map Black To`, white -> `Map White To` mixed by
//!   `Amount to Tint`; Tritone the map shadows (0), midtones (0.5), highlights (1) mixed with the
//!   original by `Blend With Original`. Invert `RGB`, `Red`, `Green`, `Blue` run pigment's invert
//!   on those channels; the other channel spaces and `Alpha` are refused.
//! - Colour parameters (lane SMB-R43-FOLLOWUPS-1, round finding F-R43-4; [STU-FX-118] "Every
//!   `color` parameter carries a required `StudioColorProfile` reference and is evaluated in the
//!   layer's declared working space"): every colour a kind reads (Tint, Tritone, Glow `A & B
//!   Colors`, Gradient Ramp, Circle, Checkerboard) is taken from the profile its Folio
//!   `colour_profiles` reference names (explicit null = the document's `working_space_rgb`) into
//!   the plan's colour tag. The values pass unchanged when that profile is the plan's own: the
//!   document's `blending_space` in an encoded plan, or a profile whose ICC bytes hash to the
//!   tag's `profile_sha256`. Otherwise Prism's `WideEngine` converts the RGB channels (relative
//!   colorimetric, no black-point compensation; Studio reading, UNVERIFIED D5) from the
//!   referenced profile's ICC bytes into the plan profile's, both supplied by the caller through
//!   [`EffectColourProfiles`] ([`CompositionTemporalSource::with_effect_colour_profiles`]); alpha
//!   is kept. A parameter without a reference, a reference the caller supplied no bytes for, a
//!   plan profile whose bytes do not hash to the tag, or a conversion Prism refuses (a profile
//!   that is not RGB, a channel outside `0..1`) is refused [`COMPOSITION_EFFECT_OPTION`] when the
//!   effect is applied, with the layer id `{layer}/effects/{effect_id}/{param_key}` naming the
//!   parameter. Prism's Observe frame of the conversion is not forwarded (the frame lowering
//!   reports its own outcome).
//! - Entry semantics: a blend mode other than normal ([STU-FX-008]) and an effect mask
//!   ([STU-FX-004], the coverage of the named layer mask alone in `add` mode with its own inversion,
//!   feather, opacity and expansion, whatever its mode on the layer) wrap the entry's effect in
//!   [`crate::composition::SceneEffect::Composite`]. Dissolve and the modes pigment's `blend_rgb`
//!   does not define are refused.
#![deny(clippy::arithmetic_side_effects)]
use super::{CompLayer, CompositionInput, CompositionTemporalSource, Content, keyed_property, unsupported};
use crate::CompositeError;
use crate::composition::{
    ChannelAdjustment, ColourEffect, EffectedContent, EntryComposite, GeneratorComposite, GlowColours, GlowEffect, GlowLooping,
    GlowOriginal, SceneContent, SceneEffect, SceneEffectEntry,
};
use crate::temporal::TemporalError;
use crate::temporal::motion::sample;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{
    EFFECT_PARAM_OUT_OF_RANGE, ProfileBinding, StudioCompositionEffect, StudioCompositionEffectKind, StudioCompositionMaskMode,
};
use hsk_studio_motion::Property;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_prism::{Intent, ProfileInput, WideEngine, WideSource, WideTransformRequest, profile_hash};
use std::fmt;
use std::sync::Arc;
use hsk_studio_pigment::adjust_math::{
    AdjustmentOp, ColourBalance, Exposure, GradientMap, GradientMapStop, HueSaturation, Levels, LevelsRecord,
};
use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode, blend_rgb};
use hsk_studio_pigment::blur_math::{BlurAxis, BlurScope, LayerBlur, MAX_BLUR_RADIUS_PX, RadialBlurKind, box_kernel};
use hsk_studio_pigment::transform_math::Affine2;
use std::collections::BTreeMap;

/// An effect stack on a layer without a content rect of its own (module docs).
pub const COMPOSITION_EFFECT_CONTENT: &str = "composition_effect_content";
/// An effect option or value whose behaviour is not implemented (module docs).
pub const COMPOSITION_EFFECT_OPTION: &str = "composition_effect_option";
/// An effect value whose kernel pigment does not provide (lane SMB-AE-EFFECT-FAMILIES-2).
pub const COMPOSITION_EFFECT_KERNEL: &str = "composition_effect_kernel";
/// Glow `Color Phase` default as recorded ([STU-FX-131]); other phases are refused.
const GLOW_PHASE_DEFAULT: f64 = 0.003906;

/// The ICC profiles colour parameters are converted with (module docs): the plan profile (its
/// bytes must hash to the plan colour tag's `profile_sha256`) and the document profiles that
/// colour parameters reference, by `SCPF` id. The actor attributes Prism's conversions.
#[derive(Clone)]
pub struct EffectColourProfiles {
    actor: ActorContext,
    plan_profile_id: String,
    plan_icc: Vec<u8>,
    profiles: BTreeMap<String, Vec<u8>>,
}
impl EffectColourProfiles {
    /// The plan profile `plan_profile_id` (an `SCPF` id) with its ICC bytes.
    pub fn new(actor: ActorContext, plan_profile_id: impl Into<String>, plan_icc: Vec<u8>) -> Self {
        Self { actor, plan_profile_id: plan_profile_id.into(), plan_icc, profiles: BTreeMap::new() }
    }
    /// Adds (or replaces) the ICC bytes of document profile `profile_id`.
    #[must_use]
    pub fn with_profile(mut self, profile_id: impl Into<String>, icc: Vec<u8>) -> Self {
        self.profiles.insert(profile_id.into(), icc);
        self
    }
}
impl fmt::Debug for EffectColourProfiles {
    /// Ids and byte counts only (profile bytes are not diagnostic text).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let profiles: BTreeMap<&str, usize> = self.profiles.iter().map(|(k, v)| (k.as_str(), v.len())).collect();
        f.debug_struct("EffectColourProfiles")
            .field("plan_profile_id", &self.plan_profile_id)
            .field("plan_icc_bytes", &self.plan_icc.len())
            .field("profiles", &profiles)
            .finish_non_exhaustive()
    }
}

impl EffectColourProfiles {
    /// `rgb` in profile `source_id` (ICC `source`) converted by Prism into the plan profile, whose
    /// bytes must hash to `tag` (module docs). `Err(Some)` = canceled, `Err(None)` = refused.
    fn to_plan(
        &self,
        source_id: &str,
        source: &[u8],
        tag: ColourTag,
        rgb: [f64; 3],
        cancel: &CancellationToken,
    ) -> Result<[f64; 3], Option<TemporalError>> {
        let plan_sha = profile_hash(&self.plan_icc);
        if plan_sha != tag.profile_sha256 {
            return Err(None);
        }
        let source_id = DomainId::parse(source_id).map_err(|_| None)?;
        let plan_id = DomainId::parse(&self.plan_profile_id).map_err(|_| None)?;
        #[allow(clippy::cast_possible_truncation)]
        let pixels = rgb.map(|c| c as f32);
        let request = WideTransformRequest {
            source: WideSource::Profile(ProfileInput { profile_id: &source_id, bytes: source, expected_sha256: profile_hash(source) }),
            destination: ProfileInput { profile_id: &plan_id, bytes: &self.plan_icc, expected_sha256: plan_sha },
            pixels: &pixels,
            intent: Intent::RelativeColorimetric,
            black_point_compensation: false,
            revision: 0,
            expected_revision: 0,
            correlation_id: 1,
            resource_id: &source_id,
            actor: &self.actor,
        };
        let mut engine = WideEngine::new(self.actor.clone(), 1).map_err(|_| None)?;
        let out = engine.transform(&request, cancel, &mut Discard).map_err(|f| {
            (f.error == hsk_studio_prism::WideError::Canceled).then_some(TemporalError::Composite(CompositeError::Canceled))
        })?;
        match out.pixels.as_slice() {
            [r, g, b] => Ok([f64::from(*r), f64::from(*g), f64::from(*b)]),
            _ => Err(None),
        }
    }
}

/// Prism's Observe frames of the colour conversions are not forwarded (module docs).
struct Discard;
impl hsk_studio_observe::SinkPort for Discard {
    fn try_send(&mut self, _: hsk_studio_observe::DeliveryClass, _: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
        Ok(())
    }
}

/// One running entry's streams.
#[derive(Clone, Debug)]
pub(super) struct EffectStream {
    effect_id: String,
    kind: StudioCompositionEffectKind,
    mix: f64,
    /// [STU-FX-008] entry blend mode.
    mode: StudioBlendMode,
    /// [STU-FX-004] the layer mask this entry is masked by.
    mask_id: Option<String>,
    /// The plan's colour tag (its blend space drives the Exposure linearisation).
    tag: ColourTag,
    values: BTreeMap<String, Property>,
    /// [STU-FX-118] profile reference per colour parameter (Folio `colour_profiles`).
    colour_refs: BTreeMap<String, ProfileBinding>,
    /// The document's `blending_space` and `working_space_rgb` bindings.
    blending: ProfileBinding,
    working_rgb: ProfileBinding,
    /// The caller's profile bytes ([`CompositionTemporalSource::with_effect_colour_profiles`]).
    colour_profiles: Option<Arc<EffectColourProfiles>>,
}

/// The running entries of layer `id` (module docs).
pub(super) fn read_effects(
    id: &str,
    effects: &[StudioCompositionEffect],
    content: &Content,
    input: &CompositionInput<'_>,
) -> Result<Vec<EffectStream>, TemporalError> {
    let doc = input.snapshot.document();
    let solo = effects.iter().any(|e| e.enabled && e.solo);
    let running: Vec<&StudioCompositionEffect> = effects.iter().filter(|e| e.enabled && (!solo || e.solo)).collect();
    if running.is_empty() {
        return Ok(Vec::new());
    }
    if matches!(content, Content::None | Content::Adjustment(_) | Content::Precomp { collapse: true, .. }) {
        return Err(unsupported(id, COMPOSITION_EFFECT_CONTENT));
    }
    running
        .into_iter()
        .map(|e| {
            let mode = StudioBlendMode::try_from(e.blend_mode).map_err(|_| unsupported(id, COMPOSITION_EFFECT_OPTION))?;
            if mode == StudioBlendMode::Dissolve || blend_rgb(mode, [0.5; 3], [0.5; 3]).is_err() {
                return Err(unsupported(id, COMPOSITION_EFFECT_OPTION));
            }
            let values = e.values.iter().map(|(k, v)| Ok((k.clone(), keyed_property(id, v)?))).collect::<Result<_, TemporalError>>()?;
            Ok(EffectStream {
                effect_id: e.effect_id.clone(),
                kind: e.filter_kind,
                mix: e.opacity / 100.0,
                mode,
                mask_id: e.mask_id.clone(),
                tag: input.space,
                values,
                colour_refs: e.colour_profiles.iter().map(|(k, p)| (k.clone(), p.colour_profile_id.clone())).collect(),
                blending: doc.blending_space.clone(),
                working_rgb: doc.working_space_rgb.clone(),
                colour_profiles: None,
            })
        })
        .collect()
}

/// Sampled values of one entry at a tick, in layer terms.
struct At<'a> {
    id: &'a str,
    e: &'a EffectStream,
    tick: i64,
    cancel: &'a CancellationToken,
    /// Layer size in pixels and the layer origin in surface pixels.
    size: [f64; 2],
    origin: [f64; 2],
}

/// Hue in degrees wrapped into `-180..180` (pigment's Hue/Saturation domain).
fn wrapped_hue(deg: f64) -> f64 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}
#[allow(clippy::cast_possible_truncation)]
fn rgb32(c: [f64; 4]) -> [f32; 3] {
    [c[0] as f32, c[1] as f32, c[2] as f32]
}
/// One adjustment over every channel.
fn all(op: AdjustmentOp) -> ChannelAdjustment {
    ChannelAdjustment { op, channels: [true; 3] }
}

impl At<'_> {
    fn value(&self, key: &str) -> Result<Vec<f64>, TemporalError> {
        let spec = self.e.kind.param(key).ok_or_else(|| unsupported(self.id, COMPOSITION_EFFECT_OPTION))?;
        let n = spec.kind.components();
        let v = sample(self.id, self.e.values.get(key), n, self.tick, self.cancel)?.unwrap_or_else(|| spec.default.to_vec());
        if !spec.accepts(&v) {
            return Err(TemporalError::InvalidState { layer_id: self.id.to_owned(), field: EFFECT_PARAM_OUT_OF_RANGE });
        }
        Ok(v)
    }
    fn one(&self, key: &str) -> Result<f64, TemporalError> {
        Ok(self.value(key)?.first().copied().unwrap_or(0.0))
    }
    fn point(&self, key: &str) -> Result<[f64; 2], TemporalError> {
        let v = self.value(key)?;
        Ok([self.origin[0] + v[0] * self.size[0], self.origin[1] + v[1] * self.size[1]])
    }
    /// A colour parameter in the plan's colour tag, from its [STU-FX-118] profile reference
    /// (module docs).
    fn colour(&self, key: &str) -> Result<[f64; 4], TemporalError> {
        let v = self.value(key)?;
        let rgba = [v[0], v[1], v[2], v[3]];
        let param = format!("{}/effects/{}/{key}", self.id, self.e.effect_id);
        let refused = || unsupported(&param, COMPOSITION_EFFECT_OPTION);
        let profile = match self.e.colour_refs.get(key) {
            Some(ProfileBinding::Assigned(p)) => p,
            Some(ProfileBinding::Unassigned(())) => match &self.e.working_rgb {
                ProfileBinding::Assigned(w) => w,
                ProfileBinding::Unassigned(()) => return Err(refused()),
            },
            None => return Err(refused()),
        };
        // The document's blending profile is the encoded plan's own: already plan units.
        if self.e.tag.blend_space == BlendSpace::Encoded && matches!(&self.e.blending, ProfileBinding::Assigned(b) if b == profile) {
            return Ok(rgba);
        }
        let profiles = self.e.colour_profiles.as_deref().ok_or_else(refused)?;
        let icc = profiles.profiles.get(profile).ok_or_else(refused)?;
        if profile_hash(icc) == self.e.tag.profile_sha256 {
            return Ok(rgba);
        }
        let rgb = profiles.to_plan(profile, icc, self.e.tag, [rgba[0], rgba[1], rgba[2]], self.cancel).map_err(|e| match e {
            Some(c) => c,
            None => refused(),
        })?;
        Ok([rgb[0], rgb[1], rgb[2], rgba[3]])
    }
    fn zero(&self, key: &str) -> Result<(), TemporalError> {
        if self.one(key)?.abs() > 0.0 { Err(unsupported(self.id, COMPOSITION_EFFECT_OPTION)) } else { Ok(()) }
    }
    /// An enumeration value: one of the row's option indices (checked by `value`).
    fn option(&self, key: &str) -> Result<u16, TemporalError> {
        let v = self.one(key)?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = v as u16;
        Ok(i)
    }
    /// A boolean row (`0` / `1`, checked by `value`).
    fn flag(&self, key: &str) -> Result<bool, TemporalError> {
        Ok(self.one(key)? > 0.5)
    }
    /// The generators' `Blending Mode` (option 1 `None`, option 2 `Normal`).
    fn composite(&self) -> Result<GeneratorComposite, TemporalError> {
        match self.option("blending_mode")? {
            1 => Ok(GeneratorComposite::Replace),
            2 => Ok(GeneratorComposite::Over),
            _ => Err(unsupported(self.id, COMPOSITION_EFFECT_OPTION)),
        }
    }
    fn refused(&self) -> TemporalError {
        unsupported(self.id, COMPOSITION_EFFECT_OPTION)
    }
    fn no_kernel(&self) -> TemporalError {
        unsupported(self.id, COMPOSITION_EFFECT_KERNEL)
    }
    /// `Blur Dimensions` / `Glow Dimensions`: `None` = both axes, else pigment's one-axis pass.
    fn axis(&self, key: &str) -> Result<Option<BlurAxis>, TemporalError> {
        Ok(match self.option(key)? {
            2 => Some(BlurAxis::Horizontal),
            3 => Some(BlurAxis::Vertical),
            _ => None,
        })
    }
    /// pigment's Gaussian taps of `radius` layer pixels (module docs).
    fn gaussian(&self, radius: f64) -> Result<Vec<f32>, TemporalError> {
        if !(radius.is_finite() && (0.0..=f64::from(MAX_BLUR_RADIUS_PX)).contains(&radius)) {
            return Err(self.no_kernel());
        }
        #[allow(clippy::cast_possible_truncation)]
        let r = radius as f32;
        LayerBlur::new(BlurScope::Layer, r).taps().map_err(|_| self.no_kernel())
    }
    /// A colour entry whose adjustments pigment accepts (else an option refusal).
    fn colour_effect(&self, ops: Vec<ChannelAdjustment>, blend: f64) -> Result<SceneEffect, TemporalError> {
        if ops.iter().any(|o| o.op.prepare().is_err()) || !(0.0..=1.0).contains(&blend) {
            return Err(self.refused());
        }
        Ok(SceneEffect::Colour(Box::new(ColourEffect { ops, blend })))
    }

    fn effect(&self) -> Result<SceneEffect, TemporalError> {
        Ok(match self.e.kind {
            StudioCompositionEffectKind::Twirl => SceneEffect::Twirl {
                angle_deg: self.one("angle")?,
                radius_px: self.one("twirl_radius")? / 100.0 * self.size[0].max(self.size[1]),
                centre: self.point("twirl_center")?,
            },
            StudioCompositionEffectKind::Spherize => {
                SceneEffect::Spherize { radius_px: self.one("radius")?, centre: self.point("center_of_sphere")? }
            }
            StudioCompositionEffectKind::Offset => {
                let to = self.point("shift_center_to")?;
                let centre = [self.origin[0] + self.size[0] / 2.0, self.origin[1] + self.size[1] / 2.0];
                SceneEffect::Offset { shift: [to[0] - centre[0], to[1] - centre[1]], blend: self.one("blend_with_original")? }
            }
            StudioCompositionEffectKind::PolarCoordinates => SceneEffect::PolarCoordinates {
                interpolation: self.one("interpolation")?,
                rect_to_polar: self.option("type_of_conversion")? == 1,
            },
            StudioCompositionEffectKind::GradientRamp => {
                self.zero("ramp_scatter")?;
                SceneEffect::GradientRamp {
                    start: self.point("start_of_ramp")?,
                    start_colour: self.colour("start_color")?,
                    end: self.point("end_of_ramp")?,
                    end_colour: self.colour("end_color")?,
                    radial: self.option("ramp_shape")? == 2,
                    blend: self.one("blend_with_original")?,
                }
            }
            StudioCompositionEffectKind::Circle => {
                if self.option("edge")? != 1 {
                    return Err(unsupported(self.id, COMPOSITION_EFFECT_OPTION));
                }
                self.zero("feather_inner_edge")?;
                SceneEffect::Circle {
                    centre: self.point("center")?,
                    radius_px: self.one("radius")?,
                    feather_px: self.one("feather_outer_edge")?,
                    colour: self.colour("color")?,
                    opacity: self.one("opacity")? / 100.0,
                    composite: self.composite()?,
                }
            }
            StudioCompositionEffectKind::Checkerboard => {
                self.zero("feather_width")?;
                self.zero("feather_height")?;
                let anchor = self.point("anchor")?;
                let cell = match self.option("size_from")? {
                    1 => {
                        let corner = self.point("corner")?;
                        [(corner[0] - anchor[0]).abs(), (corner[1] - anchor[1]).abs()]
                    }
                    2 => {
                        let w = self.one("width")?;
                        [w, w]
                    }
                    _ => [self.one("width")?, self.one("height")?],
                };
                SceneEffect::Checkerboard {
                    anchor,
                    cell,
                    colour: self.colour("color")?,
                    opacity: self.one("opacity")? / 100.0,
                    composite: self.composite()?,
                }
            }
            StudioCompositionEffectKind::GaussianBlur => SceneEffect::Blur {
                taps: self.gaussian(self.one("blurriness")?)?,
                axis: self.axis("blur_dimensions")?,
                repeat_edges: self.flag("repeat_edge_pixels")?,
            },
            StudioCompositionEffectKind::FastBoxBlur => {
                // Hard bounds 0..30000 and 1..50 (checked by `value`): finite, non-negative, in u32.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (half, passes) = ((self.one("blur_radius")? + 0.5).floor() as u32, (self.one("iterations")? + 0.5).floor() as u32);
                let taps = box_kernel(half, passes).map_err(|_| self.no_kernel())?;
                SceneEffect::Blur { taps, axis: self.axis("blur_dimensions")?, repeat_edges: self.flag("repeat_edge_pixels")? }
            }
            StudioCompositionEffectKind::DirectionalBlur => SceneEffect::DirectionalBlur {
                angle_deg: 90.0 - self.one("direction")?,
                length_px: self.one("blur_length")?,
            },
            StudioCompositionEffectKind::RadialBlur => {
                // Sampled so a value outside its hard bounds is refused like every row.
                self.option("antialiasing_best_quality")?;
                self.one("random_seed")?;
                SceneEffect::RadialBlur {
                    centre: self.point("center")?,
                    amount: self.one("amount")?,
                    kind: if self.option("type")? == 2 { RadialBlurKind::Zoom } else { RadialBlurKind::Spin },
                }
            }
            StudioCompositionEffectKind::FindEdges => {
                SceneEffect::FindEdges { invert: self.flag("invert")?, blend: self.one("blend_with_original")? }
            }
            StudioCompositionEffectKind::Glow => self.glow()?,
            StudioCompositionEffectKind::ColorBalance => {
                let triple = |a: &str, b: &str, c: &str| -> Result<[f32; 3], TemporalError> {
                    #[allow(clippy::cast_possible_truncation)]
                    let v = [self.one(a)? as f32, self.one(b)? as f32, self.one(c)? as f32];
                    Ok(v)
                };
                let op = AdjustmentOp::ColourBalance(ColourBalance {
                    shadows: triple("shadow_red_balance", "shadow_green_balance", "shadow_blue_balance")?,
                    midtones: triple("midtone_red_balance", "midtone_green_balance", "midtone_blue_balance")?,
                    highlights: triple("highlight_red_balance", "highlight_green_balance", "highlight_blue_balance")?,
                    preserve_luminosity: self.flag("preserve_luminosity")?,
                });
                self.colour_effect(vec![all(op)], 0.0)?
            }
            StudioCompositionEffectKind::Exposure => {
                let linearize_gamma = if self.flag("bypass_linear_light_conversion")? {
                    1.0
                } else {
                    match self.e.tag.blend_space {
                        BlendSpace::LinearLight => 1.0,
                        BlendSpace::Encoded => 2.2,
                    }
                };
                let op = |prefix: &str| -> Result<AdjustmentOp, TemporalError> {
                    let (e, o, g) = (format!("{prefix}exposure"), format!("{prefix}offset"), format!("{prefix}gamma_correction"));
                    #[allow(clippy::cast_possible_truncation)]
                    let exposure = Exposure {
                        exposure: self.one(&e)? as f32,
                        offset: self.one(&o)? as f32,
                        gamma: self.one(&g)? as f32,
                        linearize_gamma,
                    };
                    Ok(AdjustmentOp::Exposure(exposure))
                };
                let ops = if self.option("channels")? == 1 {
                    vec![all(op("")?)]
                } else {
                    vec![
                        ChannelAdjustment { op: op("red_")?, channels: [true, false, false] },
                        ChannelAdjustment { op: op("green_")?, channels: [false, true, false] },
                        ChannelAdjustment { op: op("blue_")?, channels: [false, false, true] },
                    ]
                };
                self.colour_effect(ops, 0.0)?
            }
            StudioCompositionEffectKind::HueSaturation => {
                let colorize = self.flag("colorize")?;
                let prefix = if colorize { "colorize_" } else { "master_" };
                #[allow(clippy::cast_possible_truncation)]
                let op = AdjustmentOp::HueSaturation(HueSaturation {
                    hue: wrapped_hue(self.one(&format!("{prefix}hue"))?) as f32,
                    saturation: self.one(&format!("{prefix}saturation"))? as f32,
                    lightness: self.one(&format!("{prefix}lightness"))? as f32,
                    colorize,
                });
                self.colour_effect(vec![all(op)], 0.0)?
            }
            StudioCompositionEffectKind::Levels => {
                // Sampled so a value outside its hard bounds is refused like every row.
                self.option("channel")?;
                self.option("clip_to_output_black")?;
                self.option("clip_to_output_white")?;
                #[allow(clippy::cast_possible_truncation)]
                let composite = LevelsRecord {
                    input_black: self.one("input_black")? as f32,
                    input_white: self.one("input_white")? as f32,
                    gamma: self.one("gamma")? as f32,
                    output_black: self.one("output_black")? as f32,
                    output_white: self.one("output_white")? as f32,
                };
                let id = LevelsRecord::IDENTITY;
                self.colour_effect(vec![all(AdjustmentOp::Levels(Levels { composite, red: id, green: id, blue: id }))], 0.0)?
            }
            StudioCompositionEffectKind::Tint => {
                let stops = vec![
                    GradientMapStop { position: 0.0, colour: rgb32(self.colour("map_black_to")?) },
                    GradientMapStop { position: 1.0, colour: rgb32(self.colour("map_white_to")?) },
                ];
                let amount = self.one("amount_to_tint")? / 100.0;
                self.colour_effect(vec![all(AdjustmentOp::GradientMap(GradientMap { stops, reverse: false }))], 1.0 - amount)?
            }
            StudioCompositionEffectKind::Tritone => {
                let stops = vec![
                    GradientMapStop { position: 0.0, colour: rgb32(self.colour("shadows")?) },
                    GradientMapStop { position: 0.5, colour: rgb32(self.colour("midtones")?) },
                    GradientMapStop { position: 1.0, colour: rgb32(self.colour("highlights")?) },
                ];
                let blend = self.one("blend_with_original")? / 100.0;
                self.colour_effect(vec![all(AdjustmentOp::GradientMap(GradientMap { stops, reverse: false }))], blend)?
            }
            StudioCompositionEffectKind::Invert => {
                let channels = match self.option("channel")? {
                    1 => [true; 3],
                    2 => [true, false, false],
                    3 => [false, true, false],
                    4 => [false, false, true],
                    _ => return Err(self.refused()),
                };
                let blend = self.one("blend_with_original")? / 100.0;
                self.colour_effect(vec![ChannelAdjustment { op: AdjustmentOp::Invert, channels }], blend)?
            }
        })
    }

    /// Glow (module docs).
    fn glow(&self) -> Result<SceneEffect, TemporalError> {
        let original = match self.option("composite_original")? {
            1 => GlowOriginal::OnTop,
            2 => GlowOriginal::Behind,
            _ => GlowOriginal::None,
        };
        let mode = match self.option("glow_operation")? {
            2 => StudioBlendMode::Normal,
            3 => StudioBlendMode::LinearDodgeAdd,
            4 => StudioBlendMode::Multiply,
            6 => StudioBlendMode::Screen,
            7 => StudioBlendMode::Overlay,
            8 => StudioBlendMode::SoftLight,
            9 => StudioBlendMode::HardLight,
            10 => StudioBlendMode::Darken,
            11 => StudioBlendMode::Lighten,
            12 => StudioBlendMode::Difference,
            13 => StudioBlendMode::Hue,
            14 => StudioBlendMode::Saturation,
            15 => StudioBlendMode::Colour,
            16 => StudioBlendMode::Luminosity,
            17 => StudioBlendMode::ColourDodge,
            18 => StudioBlendMode::ColourBurn,
            19 => StudioBlendMode::Exclusion,
            _ => return Err(self.refused()),
        };
        let colours = match self.option("glow_colors")? {
            1 => None,
            2 => {
                #[allow(clippy::float_cmp)]
                let default_phase = self.one("color_phase")? == GLOW_PHASE_DEFAULT;
                if !default_phase {
                    return Err(self.refused());
                }
                let looping = match self.option("color_looping")? {
                    1 => GlowLooping::SawtoothAb,
                    2 => GlowLooping::SawtoothBa,
                    3 => GlowLooping::TriangleAba,
                    _ => GlowLooping::TriangleBab,
                };
                Some(GlowColours {
                    a: self.colour("color_a")?,
                    b: self.colour("color_b")?,
                    looping,
                    loops: self.one("color_loops")?,
                    midpoint: self.one("a_b_midpoint")?,
                })
            }
            _ => return Err(self.refused()),
        };
        Ok(SceneEffect::Glow(Box::new(GlowEffect {
            based_on_alpha: self.option("glow_based_on")? == 1,
            threshold: self.one("glow_threshold")? / 255.0,
            taps: self.gaussian(self.one("glow_radius")?)?,
            axis: self.axis("glow_dimensions")?,
            intensity: self.one("glow_intensity")?,
            original,
            mode,
            colours,
        })))
    }
}

/// [STU-FX-004] the coverage over `rect` of layer `l`'s mask `mask_id` alone (module docs).
fn entry_mask(l: &CompLayer, id: &str, mask_id: &str, tick: i64, cancel: &CancellationToken, rect: Rect) -> Result<Vec<f32>, TemporalError> {
    let lm = l.masks.as_ref().ok_or_else(|| unsupported(id, COMPOSITION_EFFECT_OPTION))?;
    let index = lm.masks.iter().position(|m| m.mask_id == mask_id).ok_or_else(|| unsupported(id, COMPOSITION_EFFECT_OPTION))?;
    let (m, shape) = lm.at(id, tick, cancel)?.into_iter().nth(index).ok_or_else(|| unsupported(id, COMPOSITION_EFFECT_OPTION))?;
    let alone = [(crate::mask_paths::MaskAtTick { mode: StudioCompositionMaskMode::Add, ..m }, shape)];
    let o = l.content.origin();
    let coverage = crate::mask_paths::mask_coverage_shaped(&alone, &Affine2::translate(o[0], o[1]), rect, super::mask_plane_bound(rect.area()))
        .map_err(|r| unsupported(&format!("{id}/masks/{mask_id}"), r.code))?;
    let n = usize::try_from(rect.area()).map_err(|_| CompositeError::Overflow)?;
    // An open path covers nothing.
    Ok(coverage.unwrap_or_else(|| vec![0.0; n]))
}

impl CompositionTemporalSource {
    /// Attaches the caller's ICC profiles that colour parameters are converted with (module docs)
    /// to every effect entry of every composition this source reads. Without them only colours in
    /// the plan's own profile (the document's blending space in an encoded plan) are accepted.
    #[must_use]
    pub fn with_effect_colour_profiles(mut self, profiles: Arc<EffectColourProfiles>) -> Self {
        for comp in self.comps.values_mut() {
            for l in &mut comp.layers {
                for e in &mut l.effects {
                    e.colour_profiles = Some(Arc::clone(&profiles));
                }
            }
        }
        self
    }

    /// `content` under layer `l`'s effect stack at `tick` (module docs); the flag says whether the
    /// layer's masks were folded into it.
    pub(super) fn effected(l: &CompLayer, content: SceneContent, tick: i64, cancel: &CancellationToken) -> Result<(SceneContent, bool), TemporalError> {
        if l.effects.is_empty() {
            return Ok((content, false));
        }
        let id = l.layer_id.as_str();
        let bounds = l.content.bounds().ok_or_else(|| unsupported(id, COMPOSITION_EFFECT_CONTENT))?;
        let rect = match &l.content {
            Content::Static { content, .. } => content.rect,
            _ => bounds,
        };
        #[allow(clippy::cast_precision_loss)]
        let origin = [bounds.x as f64 - rect.x as f64, bounds.y as f64 - rect.y as f64];
        let size = [f64::from(bounds.width), f64::from(bounds.height)];
        let mask = match &l.masks {
            None => None,
            Some(lm) => {
                let o = l.content.origin();
                lm.coverage(id, tick, cancel, &Affine2::translate(o[0], o[1]), rect, super::mask_plane_bound(rect.area()))?
            }
        };
        let mut effects = Vec::with_capacity(l.effects.len());
        for e in &l.effects {
            cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
            let at = At { id, e, tick, cancel, size, origin };
            let mut effect = at.effect()?;
            if e.mode != StudioBlendMode::Normal || e.mask_id.is_some() {
                let mask = e.mask_id.as_deref().map(|m| entry_mask(l, id, m, tick, cancel, rect)).transpose()?;
                effect = SceneEffect::Composite(Box::new(EntryComposite { effect, mode: e.mode, mask }));
            }
            effects.push(SceneEffectEntry { effect_id: e.effect_id.clone(), effect, mix: e.mix });
        }
        Ok((SceneContent::Effected(Box::new(EffectedContent { content, rect, mask, effects })), l.masks.is_some()))
    }
}
