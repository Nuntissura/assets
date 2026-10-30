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
#![deny(clippy::arithmetic_side_effects)]
use super::{CompLayer, CompositionTemporalSource, Content, keyed_property, unsupported};
use crate::CompositeError;
use crate::composition::{EffectedContent, GeneratorComposite, SceneContent, SceneEffect, SceneEffectEntry};
use crate::temporal::TemporalError;
use crate::temporal::motion::sample;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{EFFECT_PARAM_OUT_OF_RANGE, StudioCompositionEffect, StudioCompositionEffectKind};
use hsk_studio_motion::Property;
use hsk_studio_pigment::transform_math::Affine2;
use std::collections::BTreeMap;

/// An effect stack on a layer without a content rect of its own (module docs).
pub const COMPOSITION_EFFECT_CONTENT: &str = "composition_effect_content";
/// An effect option or value whose behaviour is not implemented (module docs).
pub const COMPOSITION_EFFECT_OPTION: &str = "composition_effect_option";

/// One running entry's streams.
#[derive(Clone, Debug)]
pub(super) struct EffectStream {
    effect_id: String,
    kind: StudioCompositionEffectKind,
    mix: f64,
    values: BTreeMap<String, Property>,
}

/// The running entries of layer `id` (module docs).
pub(super) fn read_effects(id: &str, effects: &[StudioCompositionEffect], content: &Content) -> Result<Vec<EffectStream>, TemporalError> {
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
            let values = e.values.iter().map(|(k, v)| Ok((k.clone(), keyed_property(id, v)?))).collect::<Result<_, TemporalError>>()?;
            Ok(EffectStream { effect_id: e.effect_id.clone(), kind: e.filter_kind, mix: e.opacity / 100.0, values })
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
    fn colour(&self, key: &str) -> Result<[f64; 4], TemporalError> {
        let v = self.value(key)?;
        Ok([v[0], v[1], v[2], v[3]])
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
    /// The generators' `Blending Mode` (option 1 `None`, option 2 `Normal`).
    fn composite(&self) -> Result<GeneratorComposite, TemporalError> {
        match self.option("blending_mode")? {
            1 => Ok(GeneratorComposite::Replace),
            2 => Ok(GeneratorComposite::Over),
            _ => Err(unsupported(self.id, COMPOSITION_EFFECT_OPTION)),
        }
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
        })
    }
}

impl CompositionTemporalSource {
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
            effects.push(SceneEffectEntry { effect_id: e.effect_id.clone(), effect: at.effect()?, mix: e.mix });
        }
        Ok((SceneContent::Effected(Box::new(EffectedContent { content, rect, mask, effects })), l.masks.is_some()))
    }
}
