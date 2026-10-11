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
//! `exposure_use_legacy`, `channel_blend_unsupported`, `layer_effects_not_rendered`,
//! `blend_clipped_layers_as_group_off_not_modelled` (a clip base with Blend Clipped Layers as Group
//! off). Blend-If on a clip base or a clipped layer is carried to pigment, whose admission refuses
//! it for every backend (`blend_if_clip_base`, `blend_if_clipped_layer`); the ledger says so.
use crate::lower_plan::StepAppearance;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, GlowSource as FolioGlowSource, GlowTechnique as FolioGlowTechnique,
    Parent, Payload, ProfileBinding, StrokeFillType, StrokePosition as FolioStrokePosition,
    StudioAdjustment,
    StudioAdjustmentChannel, StudioAdvancedBlending, StudioBlendChannel, StudioColor,
    StudioContour, StudioCurve, StudioEffect, StudioEffectKind, StudioEffectStack,
    StudioGlobalLight, StudioKnockout, StudioLayer, StudioLevelsRecord, StudioRasterRecords,
};
use hsk_studio_pigment::adjust_math::{
    BrightnessContrast, Curves, Exposure, HueSaturation, Levels, LevelsRecord,
};
use hsk_studio_pigment::blend_math::{BlendSpace, Exactness, StudioBlendMode};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry};
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectError, EffectFeature, EffectOp, GlowSource,
    GlowTechnique, InnerGlowOp, InnerShadowOp, MAX_EFFECTS_PER_LAYER, OuterGlowOp, StrokeOp,
    StrokePosition as PigmentStrokePosition,
};
use hsk_studio_pigment::render_plan::{ClipRole, ExtOperator, Knockout, PlanError};
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
/// Refusal code: an enabled effect instance of a kind pigment does not render (effects wave 1 is
/// drop shadow, colour overlay and solid stroke).
pub const REFUSE_EFFECTS: &str = "layer_effects_not_rendered";
/// Refusal code: effect-stack scale other than 100 percent (pigment has no effect scaling).
pub const REFUSE_EFFECT_SCALE: &str = "effect_stack_scale";
/// Refusal code: a contour other than the linear one (two points, `(0,0)` and `(255,255)`).
pub const REFUSE_EFFECT_CONTOUR: &str = "effect_contour";
/// Refusal code: effect noise above zero.
pub const REFUSE_EFFECT_NOISE: &str = "effect_noise";
/// Refusal code: a gradient or pattern stroke fill.
pub const REFUSE_EFFECT_FILL: &str = "effect_stroke_fill";
/// Refusal code: stroke overprint.
pub const REFUSE_EFFECT_OVERPRINT: &str = "effect_stroke_overprint";
/// Refusal code: a drop shadow that follows Global Light while the records state none.
pub const REFUSE_EFFECT_GLOBAL_LIGHT: &str = "effect_global_light_unresolved";
/// Refusal code: an effect without a colour.
pub const REFUSE_EFFECT_COLOUR: &str = "effect_colour_missing";
/// Refusal code: an effect colour in a profile other than the document's blending profile.
pub const REFUSE_EFFECT_COLOUR_PROFILE: &str = "effect_colour_profile";
/// Refusal code: a mid-tone effect colour in a linear-light plan (its profile transfer would have
/// to be decoded; black and white are the same in every encoding).
pub const REFUSE_EFFECT_COLOUR_LINEAR: &str = "effect_colour_linear_light";
/// Refusal code: an effect blend mode that is not a known layer mode.
pub const REFUSE_EFFECT_BLEND_MODE: &str = "effect_blend_mode";
/// Refusal code: an effect parameter outside pigment's declared range.
pub const REFUSE_EFFECT_PARAMETER: &str = "effect_parameter";
/// Refusal code: an effect size, distance or count above pigment's limits.
pub const REFUSE_EFFECT_LIMIT: &str = "effect_limit";
/// Refusal codes: effects on an adjustment, pass-through or clipped layer, or on the base of a
/// clipping run (pigment refuses all four).
pub const REFUSE_EFFECT_ON_ADJUSTMENT: &str = "effect_on_adjustment_layer";
pub const REFUSE_EFFECT_ON_PASS_THROUGH: &str = "effect_on_pass_through";
pub const REFUSE_EFFECT_ON_CLIPPED: &str = "effect_on_clipped_layer";
pub const REFUSE_EFFECT_ON_CLIP_BASE: &str = "effect_on_clip_base";
/// Refusal code: an adjustment layer whose record is absent.
pub const REFUSE_ADJUSTMENT_RECORD: &str = "adjustment_record_missing";
/// Refusal code: Blend Clipped Layers as Group switched off (PSD `clbl = 0`, Folio
/// `StudioAdvancedBlending::blend_clipped_effects == false`) on the base of a clipping run that
/// renders at least one clipped layer. Pigment's clip-to-base rule composites the run unit with the
/// base's mode (the switched-on reading); the switched-off reading (each clipped layer keeps its own
/// blend mode against the backdrop) is not modelled and must not be dropped silently. The code is
/// the one interop-psd reports for the same file setting.
pub const REFUSE_CLIP_GROUP_BLEND_OFF: &str = "blend_clipped_layers_as_group_off_not_modelled";
/// Refusal code and evaluation-ledger key: the base of a clipping run is a rendered pass-through
/// group. Pigment's `RenderPlan::validate` refuses it as `PlanError::ClipBasePassThrough` and has no
/// operator key for it, so composite reports that error's own code instead of a local spelling
/// (plausible register PLR-006; the ledger entries for an adjustment or effects clip base already do
/// the same through `PlanError::AdjustmentClipBase` and `PlanError::EffectClipRun`).
pub const REFUSE_CLIP_BASE_PASS_THROUGH: &str = PlanError::ClipBasePassThrough { node: 0 }.code();

/// Lowering-report key: one curve point whose corner flag pigment cannot carry.
pub const NOTE_CURVE_CORNER_DROPPED: &str = "curves_corner_flag_dropped";
/// Lowering-report key: exposure linearisation exponent chosen from the plan blend space.
pub const NOTE_EXPOSURE_LINEARIZE: &str = "exposure_linearize_gamma";
/// Lowering-report key: a spread or choke read as a percent of the size (STU-RAS-157 states pixels
/// for it, but the observed drop-shadow spread range 0..100 exceeds the observed size range 0..65,
/// and Photoshop's spread and choke are percentages; spec unit conflict D19, UNVERIFIED).
pub const NOTE_SPREAD_CHOKE_PERCENT: &str = "effect_spread_choke_percent";
/// Lowering-report key: a glow jitter above zero on a solid-colour glow, passed to pigment as 0
/// (Adobe-side evidence says jitter only acts on gradients; D5 case 16 decides, UNVERIFIED).
pub const NOTE_GLOW_JITTER_IGNORED: &str = "glow_jitter_ignored_solid_colour";

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

/// What effect conversion needs besides the stack: the document's Global Light, its blending
/// profile and the plan's blend space.
#[derive(Clone, Copy)]
pub(crate) struct EffectContext<'a> {
    pub global_light: Option<&'a StudioGlobalLight>,
    pub blending: &'a ProfileBinding,
    pub space: BlendSpace,
}

type Notes = Vec<(&'static str, Exactness, String)>;

fn linear_contour(c: &StudioContour) -> bool {
    matches!(c.points.as_slice(), [a, b]
        if a.input == 0.0 && a.output == 0.0 && b.input == 255.0 && b.output == 255.0)
}

/// The effect colour in plan units. Folio keeps the colour in its own profile's encoding; pigment
/// takes numbers already in the plan's blend space (STU-COL-163), and composite converts nothing:
/// a colour in another profile is refused, and in a linear-light plan only black and white (the
/// same in every encoding) are accepted.
fn effect_colour(
    e: &StudioEffect,
    cx: &EffectContext<'_>,
) -> Result<[f32; 3], &'static str> {
    let c = e.colour.as_ref().ok_or(REFUSE_EFFECT_COLOUR)?;
    if let ProfileBinding::Assigned(p) = &c.colour_profile_id
        && !matches!(cx.blending, ProfileBinding::Assigned(b) if b == p)
    {
        return Err(REFUSE_EFFECT_COLOUR_PROFILE);
    }
    let rgb = [c.r as f32, c.g as f32, c.b as f32];
    if cx.space == BlendSpace::LinearLight && rgb.iter().any(|v| *v != 0.0 && *v != 1.0) {
        return Err(REFUSE_EFFECT_COLOUR_LINEAR);
    }
    Ok(rgb)
}

/// Global Light is a document-level angle shared by the effects that subscribe to it; an effect
/// that follows it needs the record.
fn shadow_angle(e: &StudioEffect, cx: &EffectContext<'_>) -> Result<f32, &'static str> {
    let angle = if e.use_global_light {
        cx.global_light.ok_or(REFUSE_EFFECT_GLOBAL_LIGHT)?.angle
    } else {
        e.angle
    };
    Ok(angle as f32)
}

/// Spread and choke are percents of the size (lead decision, D19): fraction = percent / 100.
fn grown(notes: &mut Notes, e: &StudioEffect, field: &str, percent: f64) -> f32 {
    if percent > 0.0 {
        notes.push((
            NOTE_SPREAD_CHOKE_PERCENT,
            Exactness::Approximate,
            format!("{}: {} {field} {percent}", e.instance_id, e.params.key()),
        ));
    }
    (percent / 100.0) as f32
}

fn glow_technique(t: FolioGlowTechnique) -> GlowTechnique {
    match t {
        FolioGlowTechnique::Softer => GlowTechnique::Softer,
        FolioGlowTechnique::Precise => GlowTechnique::Precise,
    }
}

/// A solid-colour glow passes jitter 0 to pigment (lead decision; Adobe-side evidence says jitter
/// acts on gradients only) and reports the loss.
fn glow_jitter(notes: &mut Notes, e: &StudioEffect, jitter: f64) {
    if jitter > 0.0 {
        notes.push((
            NOTE_GLOW_JITTER_IGNORED,
            Exactness::Approximate,
            format!("{}: jitter {jitter}", e.instance_id),
        ));
    }
}

fn convert_effect(
    e: &StudioEffect,
    cx: &EffectContext<'_>,
    notes: &mut Notes,
) -> Result<EffectOp, &'static str> {
    if e.noise > 0.0 {
        return Err(REFUSE_EFFECT_NOISE);
    }
    if e.contour.as_ref().is_some_and(|c| !linear_contour(c)) {
        return Err(REFUSE_EFFECT_CONTOUR);
    }
    let mode = StudioBlendMode::try_from(e.blend_mode).map_err(|_| REFUSE_EFFECT_BLEND_MODE)?;
    let opacity = (e.opacity / 100.0) as f32;
    let op = match &e.params {
        StudioEffectKind::DropShadow {
            distance,
            spread,
            size,
            layer_knocks_out,
        } => {
            let angle = shadow_angle(e, cx)?;
            EffectOp::DropShadow(
                DropShadowOp::new(effect_colour(e, cx)?, mode, opacity)
                    .with_offset(angle, *distance as f32)
                    .with_spread(grown(notes, e, "spread", *spread))
                    .with_size(*size as f32)
                    .with_layer_knocks_out(*layer_knocks_out),
            )
        }
        StudioEffectKind::InnerShadow {
            distance,
            choke,
            size,
        } => {
            let angle = shadow_angle(e, cx)?;
            EffectOp::InnerShadow(
                InnerShadowOp::new(effect_colour(e, cx)?, mode, opacity)
                    .with_offset(angle, *distance as f32)
                    .with_choke(grown(notes, e, "choke", *choke))
                    .with_size(*size as f32),
            )
        }
        StudioEffectKind::OuterGlow {
            technique,
            spread,
            size,
            range,
            jitter,
            gradient,
        } => {
            if gradient.is_some() {
                return Err(EffectFeature::GlowGradient.key());
            }
            glow_jitter(notes, e, *jitter);
            EffectOp::OuterGlow(
                OuterGlowOp::new(effect_colour(e, cx)?, mode, opacity)
                    .with_spread(grown(notes, e, "spread", *spread))
                    .with_size(*size as f32)
                    .with_technique(glow_technique(*technique))
                    .with_range((*range / 100.0) as f32),
            )
        }
        StudioEffectKind::InnerGlow {
            technique,
            source,
            choke,
            size,
            range,
            jitter,
            gradient,
        } => {
            if gradient.is_some() {
                return Err(EffectFeature::GlowGradient.key());
            }
            glow_jitter(notes, e, *jitter);
            let source = match source {
                FolioGlowSource::Edge => GlowSource::Edge,
                FolioGlowSource::Centre => GlowSource::Centre,
            };
            EffectOp::InnerGlow(
                InnerGlowOp::new(effect_colour(e, cx)?, mode, opacity)
                    .with_choke(grown(notes, e, "choke", *choke))
                    .with_size(*size as f32)
                    .with_source(source)
                    .with_technique(glow_technique(*technique))
                    .with_range((*range / 100.0) as f32),
            )
        }
        StudioEffectKind::ColourOverlay => {
            EffectOp::ColourOverlay(ColourOverlayOp::new(effect_colour(e, cx)?, mode, opacity))
        }
        StudioEffectKind::Stroke {
            size,
            position,
            fill_type,
            overprint,
            ..
        } => {
            if *fill_type != StrokeFillType::Solid {
                return Err(REFUSE_EFFECT_FILL);
            }
            if *overprint {
                return Err(REFUSE_EFFECT_OVERPRINT);
            }
            let position = match position {
                FolioStrokePosition::Inside => PigmentStrokePosition::Inside,
                FolioStrokePosition::Outside => PigmentStrokePosition::Outside,
                FolioStrokePosition::Centre => PigmentStrokePosition::Centre,
            };
            EffectOp::Stroke(StrokeOp::new(
                effect_colour(e, cx)?,
                mode,
                opacity,
                *size as f32,
                position,
            ))
        }
        // Bevel and emboss, satin, gradient and pattern overlay.
        _ => return Err(REFUSE_EFFECTS),
    };
    // Pigment's own parameter and limit judgement, mapped to the code the lowering reports.
    op.validate().map_err(|err| match err {
        EffectError::SizeOutOfRange | EffectError::DistanceOutOfRange | EffectError::OverBudget => {
            REFUSE_EFFECT_LIMIT
        }
        EffectError::ModeNotApplicable(_) => REFUSE_EFFECT_BLEND_MODE,
        // A feature pigment recognises and refuses (glow range, Precise technique, ...): its key.
        EffectError::Unsupported(feature) => feature.key(),
        _ => REFUSE_EFFECT_PARAMETER,
    })?;
    Ok(op)
}

/// The pigment effect ops of a stack, in list order, or the refusal. A disabled stack and disabled
/// instances render nothing (an unsupported kind that is disabled is not refused).
pub(crate) fn convert_effects(
    stack: &StudioEffectStack,
    cx: &EffectContext<'_>,
) -> Result<(Vec<EffectOp>, Notes), &'static str> {
    let live: Vec<&StudioEffect> = if stack.enabled {
        stack.effects.iter().filter(|e| e.enabled).collect()
    } else {
        Vec::new()
    };
    if live.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if stack.scale != 100.0 {
        return Err(REFUSE_EFFECT_SCALE);
    }
    if live.len() > MAX_EFFECTS_PER_LAYER {
        return Err(REFUSE_EFFECT_LIMIT);
    }
    let mut ops = Vec::with_capacity(live.len());
    let mut notes = Vec::new();
    for e in live {
        ops.push(convert_effect(e, cx, &mut notes)?);
    }
    Ok((ops, notes))
}

/// The Blend-If operator pigment reports for a layer with a non-identity range in the clip role it
/// has in its group (`PlanExtensions::validate`: `BlendIf` on an unclipped layer,
/// `BlendIfClipBase` on the base of a clipping run, `BlendIfClippedLayer` on a clipped layer; the
/// two clip operators are intrinsically Unsupported because no public source defines them). An
/// unclipped layer that also carries effects (`has_effects`) is reported as `EffectsWithBlendIf`
/// instead (intrinsically Unsupported: no public source defines how the Blend-If weight composes
/// with the effect mattes; lane SMB-PIGMENT-FX-OPS), exactly like pigment's validate. The role is a
/// property of the parent group, so [`LayerFeatures::operators`] leaves Blend-If out and the
/// evaluation records this operator per role.
pub(crate) fn blend_if_operator(role: ClipRole, has_effects: bool) -> ExtOperator {
    match role {
        ClipRole::Unclipped if has_effects => ExtOperator::EffectsWithBlendIf,
        ClipRole::Unclipped => ExtOperator::BlendIf,
        ClipRole::Base => ExtOperator::BlendIfClipBase,
        ClipRole::Clipped => ExtOperator::BlendIfClippedLayer,
    }
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
    /// The converted effect ops in list order, or the refusal code of the stack.
    pub effects: Result<Vec<EffectOp>, &'static str>,
    /// Approximations made while converting (effects); adjustments carry their own.
    pub notes: Notes,
    /// Blend Clipped Layers as Group is switched off on this layer (`clbl = 0`). It only matters
    /// when the layer is the base of a clipping run that renders a clipped layer: the callers that
    /// know the run refuse it there (`REFUSE_CLIP_GROUP_BLEND_OFF`).
    pub clipped_group_blend_off: bool,
    channel_blend_default: bool,
}

impl LayerFeatures {
    pub(crate) fn of(
        layers: &BTreeMap<&str, &StudioLayer>,
        records: &StudioRasterRecords,
        cx: &EffectContext<'_>,
        layer_id: &str,
    ) -> Self {
        let rec = records.layer(layer_id);
        let blending = rec.and_then(|r| r.advanced_blending.as_ref());
        let (effects, notes) = match rec.and_then(|r| r.effects.as_ref()) {
            Some(stack) => match convert_effects(stack, cx) {
                Ok((ops, notes)) => (Ok(ops), notes),
                Err(code) => (Err(code), Vec::new()),
            },
            None => (Ok(Vec::new()), Vec::new()),
        };
        Self {
            mask: rec.and_then(|r| r.mask.as_ref()).map(|m| MaskParams {
                density: (m.density / 100.0) as f32,
                feather_px: m.feather as f32,
                inverted: m.inverted,
            }),
            blend_if: blending.map(blend_if).unwrap_or_default(),
            knockout: resolve_knockout(layers, records, layer_id),
            effects,
            notes,
            clipped_group_blend_off: blending.is_some_and(|b| !b.blend_clipped_effects),
            channel_blend_default: blending
                .is_none_or(|b| b.channel_blend.red && b.channel_blend.green && b.channel_blend.blue),
        }
    }

    /// Does the layer carry rendered effects (or an effect stack that is refused)?
    pub(crate) fn has_effects(&self) -> bool {
        self.effects.as_ref().map_or(true, |ops| !ops.is_empty())
    }

    /// The effect ops (empty when none or refused; see [`LayerFeatures::refusal`]).
    pub(crate) fn effect_ops(&self) -> &[EffectOp] {
        self.effects.as_deref().unwrap_or_default()
    }

    /// The refusal code when the layer uses something the envelope cannot carry. `appearance` is
    /// the layer's own (effects need an isolated, unclipped, non-adjustment layer);
    /// `adjustment`: the layer is an adjustment layer.
    pub(crate) fn refusal(
        &self,
        appearance: &StepAppearance,
        adjustment: bool,
    ) -> Option<&'static str> {
        if !self.channel_blend_default {
            return Some(REFUSE_CHANNEL_BLEND);
        }
        if let Err(code) = &self.effects {
            return Some(code);
        }
        if self.has_effects() {
            if adjustment {
                return Some(REFUSE_EFFECT_ON_ADJUSTMENT);
            }
            if appearance.mode == StudioBlendMode::PassThrough {
                return Some(REFUSE_EFFECT_ON_PASS_THROUGH);
            }
            if appearance.clipped {
                return Some(REFUSE_EFFECT_ON_CLIPPED);
            }
        }
        None
    }

    /// Does the layer change nothing beyond its own appearance?
    pub(crate) fn is_neutral(&self) -> bool {
        self.blend_if.is_empty() && self.knockout == Knockout::None && !self.has_effects()
    }

    /// Envelope operators this layer uses besides Blend-If (see [`blend_if_operator`]).
    /// `coverage_mask`: the layer carries an enabled coverage mask (density and feather act on it
    /// only).
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
        if self.knockout != Knockout::None {
            ops.push(ExtOperator::Knockout(self.knockout));
        }
        for op in self.effect_ops() {
            let e = ExtOperator::Effect(op.kind());
            if !ops.contains(&e) {
                ops.push(e);
            }
        }
        ops
    }

    /// The blend modes the effects composite with (pigment's admission asks the backend for each
    /// mode's v1 class).
    pub(crate) fn effect_modes(&self) -> Vec<StudioBlendMode> {
        let mut modes = Vec::new();
        for op in self.effect_ops() {
            if !modes.contains(&op.mode()) {
                modes.push(op.mode());
            }
        }
        modes
    }
}
