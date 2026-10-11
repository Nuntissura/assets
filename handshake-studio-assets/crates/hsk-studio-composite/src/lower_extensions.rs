//! Conversion of Folio raster records (`StudioRasterRecords`, decision record
//! `diagnostics/adjustments-advanced-blending.md`) into the values of pigment's additive extension
//! envelope (`PlanExtensions`). Composite adds no formula and no class here: every value is a
//! unit conversion or a typed refusal, and every class composite reports is read from pigment
//! (`ExtOperator::key()` / `intrinsic()`, `AdjustmentOp::prepare`).
//!
//! Units: Folio stores levels `normalized_0_1`, curve points and Blend-If ranges `levels_0_255`
//! (STU-RAS-152), mask density in percent and feather in pixels. Pigment takes fractions
//! (`level / 255`) and a density fraction (`percent / 100`).
//!
//! Refusals are codes, never silent drops: `hue_saturation_bands`, `threshold_colours`,
//! `exposure_use_legacy`, `channel_blend_unsupported`, `layer_effects_not_rendered`.
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Parent, Payload, StudioAdjustment, StudioAdjustmentChannel,
    StudioAdvancedBlending, StudioBlendChannel, StudioColor, StudioCurve, StudioEffectStack,
    StudioKnockout, StudioLayer, StudioLevelsRecord, StudioRasterRecords,
};
use hsk_studio_pigment::adjust_math::{
    BrightnessContrast, Curves, Exposure, HueSaturation, Levels, LevelsRecord,
};
use hsk_studio_pigment::blend_math::{BlendSpace, Exactness};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry};
use hsk_studio_pigment::render_plan::{ExtOperator, Knockout};
use std::collections::BTreeMap;

pub use hsk_studio_pigment::adjust_math::AdjustmentOp;

/// Refusal code: a hue band with a non-zero shift (pigment carries master values only).
pub const REFUSE_HUE_BANDS: &str = "hue_saturation_bands";
/// Refusal code: a threshold with a non-default false or true colour.
pub const REFUSE_THRESHOLD_COLOURS: &str = "threshold_colours";
/// Refusal code: exposure `use_legacy` has no pigment counterpart.
pub const REFUSE_EXPOSURE_LEGACY: &str = "exposure_use_legacy";
/// Refusal code: a per-channel blend enablement other than all channels on.
pub const REFUSE_CHANNEL_BLEND: &str = "channel_blend_unsupported";
/// Refusal code: an enabled effect stack with at least one enabled instance (effects wave 1 pending).
pub const REFUSE_EFFECTS: &str = "layer_effects_not_rendered";
/// Refusal code: an adjustment layer whose record is absent.
pub const REFUSE_ADJUSTMENT_RECORD: &str = "adjustment_record_missing";

/// Lowering-report key: one curve point whose corner flag pigment cannot carry.
pub const NOTE_CURVE_CORNER_DROPPED: &str = "curves_corner_flag_dropped";
/// Lowering-report key: exposure linearisation exponent chosen from the plan blend space.
pub const NOTE_EXPOSURE_LINEARIZE: &str = "exposure_linearize_gamma";

/// One recorded approximation of the lowering (never a silent drop). `exactness` is the class the
/// approximation costs; the lowering ledger carries it under `key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweringNote {
    pub layer_id: String,
    pub key: &'static str,
    pub exactness: Exactness,
    pub detail: String,
}

/// Is this layer an adjustment layer (STU-RAS-007: kind `adjustment`, payload a primitive
/// reference to `hsk.studio.adjustment@1` naming its own layer id)?
pub(crate) fn is_adjustment_layer(l: &StudioLayer) -> bool {
    l.kind == "adjustment"
        && matches!(&l.payload, Payload::PrimitiveReference { schema_id, object_id }
            if schema_id == ADJUSTMENT_SCHEMA && *object_id == l.layer_id)
}

pub(crate) struct ConvertedAdjustment {
    pub op: AdjustmentOp,
    pub notes: Vec<(&'static str, Exactness, String)>,
}

/// The converted adjustment of `layer_id` (the one function evaluation and lowering share, so
/// they refuse the same cases with the same code).
pub(crate) fn adjustment_for(
    records: &StudioRasterRecords,
    layer_id: &str,
    space: BlendSpace,
) -> Result<ConvertedAdjustment, &'static str> {
    let a = records
        .layer(layer_id)
        .and_then(|r| r.adjustment.as_ref())
        .ok_or(REFUSE_ADJUSTMENT_RECORD)?;
    convert_adjustment(a, space)
}

fn levels_record(r: &StudioLevelsRecord) -> LevelsRecord {
    LevelsRecord {
        input_black: r.input_black as f32,
        input_white: r.input_white as f32,
        gamma: r.gamma as f32,
        output_black: r.output_black as f32,
        output_white: r.output_white as f32,
    }
}

fn curve_points(
    curves: &[StudioCurve],
    channel: StudioAdjustmentChannel,
    notes: &mut Vec<(&'static str, Exactness, String)>,
) -> Vec<[f32; 2]> {
    let Some(curve) = curves.iter().find(|c| c.channel == channel) else {
        return Vec::new();
    };
    let mut points = Vec::with_capacity(curve.points.len());
    for (n, p) in curve.points.iter().enumerate() {
        if p.corner {
            // Pigment has no corner support: the spline is smooth through this point.
            notes.push((
                NOTE_CURVE_CORNER_DROPPED,
                Exactness::Approximate,
                format!("{channel:?} point {n}"),
            ));
        }
        points.push([(p.input / 255.0) as f32, (p.output / 255.0) as f32]);
    }
    points
}

fn default_colour(c: &Option<StudioColor>, white: bool) -> bool {
    // Absent = the normative default (STU-RAS-151: false black, true white); black and white are
    // the same in every RGB profile, so an explicit default value counts as default.
    let v = if white { 1.0 } else { 0.0 };
    c.as_ref().is_none_or(|c| c.r == v && c.g == v && c.b == v)
}

/// Convert one Folio adjustment into pigment's `AdjustmentOp`, or name the refusal.
pub(crate) fn convert_adjustment(
    a: &StudioAdjustment,
    space: BlendSpace,
) -> Result<ConvertedAdjustment, &'static str> {
    let mut notes = Vec::new();
    let op = match a {
        StudioAdjustment::Levels { records } => {
            let pick = |channel| {
                records
                    .iter()
                    .find(|r| r.channel == channel)
                    .map_or(LevelsRecord::IDENTITY, levels_record)
            };
            AdjustmentOp::Levels(Levels {
                composite: pick(StudioAdjustmentChannel::Composite),
                red: pick(StudioAdjustmentChannel::Red),
                green: pick(StudioAdjustmentChannel::Green),
                blue: pick(StudioAdjustmentChannel::Blue),
            })
        }
        StudioAdjustment::Curves { curves } => AdjustmentOp::Curves(Curves {
            composite: curve_points(curves, StudioAdjustmentChannel::Composite, &mut notes),
            red: curve_points(curves, StudioAdjustmentChannel::Red, &mut notes),
            green: curve_points(curves, StudioAdjustmentChannel::Green, &mut notes),
            blue: curve_points(curves, StudioAdjustmentChannel::Blue, &mut notes),
        }),
        StudioAdjustment::HueSaturation {
            master,
            colorize,
            colorization,
            bands,
        } => {
            // A band with an all-zero shift is the identity (PSD files carry six default bands);
            // any other band has no pigment counterpart.
            if bands
                .iter()
                .any(|b| b.shift.hue != 0.0 || b.shift.saturation != 0.0 || b.shift.lightness != 0.0)
            {
                return Err(REFUSE_HUE_BANDS);
            }
            let s = if *colorize { colorization } else { master };
            AdjustmentOp::HueSaturation(HueSaturation {
                hue: s.hue as f32,
                saturation: s.saturation as f32,
                lightness: s.lightness as f32,
                colorize: *colorize,
            })
        }
        StudioAdjustment::BrightnessContrast {
            brightness,
            contrast,
            use_legacy,
        } => AdjustmentOp::BrightnessContrast(BrightnessContrast {
            brightness: *brightness as f32,
            contrast: *contrast as f32,
            use_legacy: *use_legacy,
        }),
        StudioAdjustment::Exposure {
            exposure,
            offset,
            gamma,
            use_legacy,
        } => {
            if *use_legacy {
                return Err(REFUSE_EXPOSURE_LEGACY);
            }
            // psd-tools stand-in for the document transfer curve (Photoshop UNVERIFIED, D5 6).
            let linearize_gamma = match space {
                BlendSpace::LinearLight => 1.0,
                BlendSpace::Encoded => 2.2,
            };
            notes.push((
                NOTE_EXPOSURE_LINEARIZE,
                Exactness::Approximate,
                format!("{}={linearize_gamma}", space.key()),
            ));
            AdjustmentOp::Exposure(Exposure {
                exposure: *exposure as f32,
                offset: *offset as f32,
                gamma: *gamma as f32,
                linearize_gamma,
            })
        }
        StudioAdjustment::Invert => AdjustmentOp::Invert,
        StudioAdjustment::Threshold {
            level,
            false_colour,
            true_colour,
        } => {
            if !default_colour(false_colour, false) || !default_colour(true_colour, true) {
                return Err(REFUSE_THRESHOLD_COLOURS);
            }
            AdjustmentOp::Threshold {
                level: (*level / 255.0) as f32,
            }
        }
        StudioAdjustment::Posterize { levels } => AdjustmentOp::Posterize { levels: *levels },
    };
    Ok(ConvertedAdjustment { op, notes })
}

fn channel(c: StudioBlendChannel) -> BlendChannel {
    match c {
        StudioBlendChannel::Gray => BlendChannel::Gray,
        StudioBlendChannel::Red => BlendChannel::Red,
        StudioBlendChannel::Green => BlendChannel::Green,
        StudioBlendChannel::Blue => BlendChannel::Blue,
    }
}

/// Blend-If entries as pigment ranges: `[black_start, black_end, white_start, white_end] / 255`,
/// `source_black ++ source_white` and `destination_black ++ destination_white`. Entries equal to
/// the identity are dropped (they change nothing).
pub(crate) fn blend_if(b: &StudioAdvancedBlending) -> Vec<BlendRangeEntry> {
    let f = |v: f64| (v / 255.0) as f32;
    b.blend_ranges
        .iter()
        .map(|r| BlendRangeEntry {
            channel: channel(r.channel),
            source: [
                f(r.source_black[0]),
                f(r.source_black[1]),
                f(r.source_white[0]),
                f(r.source_white[1]),
            ],
            destination: [
                f(r.destination_black[0]),
                f(r.destination_black[1]),
                f(r.destination_white[0]),
                f(r.destination_white[1]),
            ],
        })
        .filter(|e| *e != BlendRangeEntry::identity(e.channel))
        .collect()
}

/// Knockout of `layer_id`: `Enabled` is `Shallow` (pigment refuses it until implemented);
/// `Inherited` takes the state of the nearest containing layer that is not itself `Inherited`;
/// `Disabled`, `Unknown` and an unresolved inheritance are `None`. Photoshop's inheritance rule is
/// UNVERIFIED (D5 11).
pub(crate) fn resolve_knockout(
    layers: &BTreeMap<&str, &StudioLayer>,
    records: &StudioRasterRecords,
    layer_id: &str,
) -> Knockout {
    let mut id = layer_id;
    // Folio rejects containment cycles; the bound only guards a hand-built snapshot.
    for _ in 0..=layers.len() {
        let state = records
            .layer(id)
            .and_then(|r| r.advanced_blending.as_ref())
            .map(|a| a.knockout);
        match state {
            Some(StudioKnockout::Enabled) => return Knockout::Shallow,
            Some(StudioKnockout::Inherited) => {}
            Some(StudioKnockout::Disabled | StudioKnockout::Unknown) | None => {
                return Knockout::None;
            }
        }
        match layers.get(id).map(|l| &l.parent) {
            Some(Parent::Node(p)) => id = p.node_id.as_str(),
            _ => return Knockout::None,
        }
    }
    Knockout::None
}

fn effects_enabled(s: &StudioEffectStack) -> bool {
    s.enabled && s.effects.iter().any(|e| e.enabled)
}

/// Mask parameters of a layer as pigment units. Only meaningful when the layer has an enabled
/// coverage mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MaskParams {
    pub density: f32,
    pub feather_px: f32,
    pub inverted: bool,
}

/// Everything the raster records say about one layer besides its adjustment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayerFeatures {
    pub mask: Option<MaskParams>,
    pub blend_if: Vec<BlendRangeEntry>,
    pub knockout: Knockout,
    effects: bool,
    channel_blend_default: bool,
}

impl LayerFeatures {
    pub(crate) fn of(
        layers: &BTreeMap<&str, &StudioLayer>,
        records: &StudioRasterRecords,
        layer_id: &str,
    ) -> Self {
        let rec = records.layer(layer_id);
        let blending = rec.and_then(|r| r.advanced_blending.as_ref());
        Self {
            mask: rec.and_then(|r| r.mask.as_ref()).map(|m| MaskParams {
                density: (m.density / 100.0) as f32,
                feather_px: m.feather as f32,
                inverted: m.inverted,
            }),
            blend_if: blending.map(blend_if).unwrap_or_default(),
            knockout: resolve_knockout(layers, records, layer_id),
            effects: rec
                .and_then(|r| r.effects.as_ref())
                .is_some_and(effects_enabled),
            channel_blend_default: blending
                .is_none_or(|b| b.channel_blend.red && b.channel_blend.green && b.channel_blend.blue),
        }
    }

    /// The refusal code when the layer uses something the envelope cannot carry.
    pub(crate) fn refusal(&self) -> Option<&'static str> {
        if self.effects {
            Some(REFUSE_EFFECTS)
        } else if !self.channel_blend_default {
            Some(REFUSE_CHANNEL_BLEND)
        } else {
            None
        }
    }

    /// Does the layer change nothing beyond its own appearance?
    pub(crate) fn is_neutral(&self) -> bool {
        self.blend_if.is_empty() && self.knockout == Knockout::None
    }

    /// Envelope operators this layer uses. `coverage_mask`: the layer carries an enabled coverage
    /// mask (density and feather act on it only).
    pub(crate) fn operators(&self, coverage_mask: bool) -> Vec<ExtOperator> {
        let mut ops = Vec::new();
        if let Some(m) = self.mask.filter(|_| coverage_mask) {
            if m.density < 1.0 {
                ops.push(ExtOperator::MaskDensity);
            }
            if m.feather_px > 0.0 {
                ops.push(ExtOperator::MaskFeather);
            }
        }
        if !self.blend_if.is_empty() {
            ops.push(ExtOperator::BlendIf);
        }
        if self.knockout != Knockout::None {
            ops.push(ExtOperator::Knockout(self.knockout));
        }
        ops
    }
}
