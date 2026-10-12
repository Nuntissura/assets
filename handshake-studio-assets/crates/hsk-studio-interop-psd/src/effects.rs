//! `lfx2` / `lmfx` layer-effect descriptors mapped to a Folio `StudioEffectStack` record
//! (STU-RAS-040, 156-158). RECORDS only: nothing here renders an effect, so the importer keeps the
//! `layer_effects_not_rendered` loss entry. The descriptor shape (object-effects version u32 = 0,
//! descriptor version u32 = 16, then a descriptor) follows the Adobe Photoshop File Formats
//! Specification; the item keys follow ag-psd `descriptor.ts` `parseEffects` / `parseEffectObject`
//! (read-only reference, MIT): `Scl `, `masterFXSwitch`, the single slots `DrSh`, `IrSh`, `OrGl`,
//! `IrGl`, `ebbl`, `SoFi`, `GrFl`, `patternFill`, `ChFX`, `FrFX`, the multi lists `dropShadowMulti`,
//! `innerShadowMulti`, `solidFillMulti`, `gradientFillMulti`, `frameFXMulti`, and per effect `enab`,
//! `Md  `, `Opct`, `Clr `, `uglg`, `lagl`, `Dstn`, `Ckmt`, `blur`, `Nose`, `AntA`, `TrnS`.
//!
//! Single slots and multi lists are concatenated in descriptor order into one list (STU-RAS-156);
//! a file that carries the same effect in both a single slot and a multi list yields both
//! instances (loss entry `effect_single_and_multi_slots_merged`; whether Photoshop duplicates the
//! first multi entry into the single slot is UNVERIFIED, D5). An effect this module cannot map
//! (gradient or pattern content, an unmapped blend mode, an unknown enumeration, a value outside
//! the Folio range) is skipped with a typed loss entry and not defaulted (test
//! `lfx2_stack_merges_single_and_multi_slots_in_source_order`); the block bytes stay in
//! the document. Blend-mode ids are quoted from ag-psd; the enumeration ids `bvlS`, `bvlD`, `bvlT`,
//! `GlwT`, `glwS`, `Styl`, `PntT` and the bool keys `layerConceals`, `Invr`, `overprint`,
//! `showInDialog` are UNVERIFIED spellings: a file that spells them differently leaves the effect
//! unmapped with a typed loss entry (or the documented default for a bool key).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{
    BevelDirection, BevelStyle, BevelTechnique, GlowSource, GlowTechnique, MAX_CURVE_POINTS, MAX_EFFECTS, ProfileBinding,
    StrokeFillType, StrokePosition, StudioColor, StudioContour, StudioContourPoint, StudioEffect, StudioEffectKind,
    StudioEffectStack,
};
pub(crate) use hsk_studio_pigment::effect_math::{MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN};

use crate::adjust::{Mapped, Note, Reason};

// ---- effect limits -------------------------------------------------------------------------------------------
//
// What a render plan counts (the rule this module matches; nothing here invents it):
// - composite `convert_effects` (hsk-studio-composite `lower_extensions.rs`, 569-593) keeps the ENABLED instances
//   of an ENABLED stack (a switched-off stack and a switched-off instance render nothing) and refuses a layer with
//   more than `MAX_EFFECTS_PER_LAYER` of them;
// - pigment `PlanExtensions::validate` (`render_plan/extensions.rs`, 326-339) adds the effects of every layer entry
//   of the plan and refuses the plan over `MAX_EFFECTS_PER_PLAN`;
// - composite `lower_inner` (`lower_render_plan.rs`, 525-563 and 738-753) lowers only layers reachable through
//   VISIBLE inputs and builds no effects for a hidden layer, so a hidden layer, or a layer inside a hidden group,
//   is in no plan.
// A stack therefore COUNTS as `live` instances (enabled stack, enabled instances) when its layer is visible and all
// its groups are visible, and as 0 otherwise. Counted instances are limited per layer and per document by pigment's
// numbers. Every kept instance, counted or not, is still held in memory and charged by Folio's record validation, so
// the kept instances no plan counts are bounded too (`MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT`, this crate's own bound).
// The importer decides by visibility alone, so it OVER-COUNTS against a plan in two cases (the counted total is at least
// the plan's for them): (a) a visible layer with no pixels, for which composite builds no node (`lower_render_plan.rs`,
// the tile nodes of a source: no tile, no node, an empty slot, no effects), and (b) a visible clipped layer whose clip
// base is hidden or empty, which composite drops together with its base (`lower_render_plan.rs`, phase 1: reachability
// through a live base). Both use up counted instances that no plan counts, so a counted stack can be lost whole
// (`effects_over_document_budget`, or `effects_over_budget` for one layer) although the plan's own count fits; both caps
// apply: the document cap takes more than 4096 apparent counted instances, the per-layer cap more than 64 on one such
// layer alone (a layer the plan never renders), which an ordinary document does not have. Test
// `the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases` (every fixture of
// `tests/effects_budget.rs` is a layer without pixels, so all of them count under (a)).
// Each bound cuts only what it bounds (R10): the counted bounds decide the fate of counted instances, the
// uncounted bound decides the fate of uncounted instances, and passing one never costs a stack the other category.
// A stack that does not fit is not mapped (its `lfx2` block stays byte-exact in the document) and is reported as a
// typed loss; the layer and the rest of the document import. The one exception is the switched-off part of a stack
// that has counted instances: past the uncounted bound it is dropped from the record (the stack keeps every counted
// instance) with a typed loss. Tests `tests/effects_budget.rs`.

/// Loss code of a stack whose layer has more counted instances than [`MAX_EFFECTS_PER_LAYER`], or more mapped
/// instances than Folio's own per-stack ceiling (`MAX_EFFECTS`, which is above pigment's).
pub(crate) const EFFECTS_OVER_BUDGET: Reason = "effects_over_budget";
/// Loss code of a stack that would take the counted instances of the document past [`MAX_EFFECTS_PER_PLAN`], and of
/// every later stack that has counted instances (the cut is a prefix of the import order, see [`EffectTally`]).
pub(crate) const EFFECTS_OVER_DOCUMENT_BUDGET: Reason = "effects_over_document_budget";
/// Loss code of a stack that has NO counted instances (hidden layer or group, switched-off stack, all instances
/// switched off) and would take the kept uncounted instances past [`MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT`], and of
/// every later such stack. A stack with counted instances is never refused for this bound
/// ([`EFFECTS_UNRENDERED_DROPPED_OVER_DOCUMENT_BUDGET`]).
pub(crate) const EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET: Reason = "effects_unrendered_over_document_budget";
/// Loss code (kind `Transformed`, the stack IS mapped) of a stack with counted instances whose switched-off
/// instances do not fit [`MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT`]: the record keeps its counted instances and none of
/// its switched-off ones.
pub(crate) const EFFECTS_UNRENDERED_DROPPED_OVER_DOCUMENT_BUDGET: Reason =
    "effects_unrendered_dropped_over_document_budget";
/// Kept effect instances no render plan counts (hidden layer or group, switched-off stack, switched-off instance)
/// that one document may hold. This bound is the crate's, not pigment's; it is the size of pigment's own plan limit
/// so that the memory and the record-validation work of the instances nobody renders stay as bounded as those that
/// are rendered: kept instances <= `MAX_EFFECTS_PER_PLAN` + this.
pub(crate) const MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT: usize = MAX_EFFECTS_PER_PLAN;
const _: () = assert!(MAX_EFFECTS_PER_LAYER <= MAX_EFFECTS, "pigment's per-layer limit must be inside Folio's");

/// Whether `reason` is one of the three effect-budget codes above (a typed loss naming the layer, not a mapping
/// problem of the descriptor).
pub(crate) fn is_budget_reason(reason: Reason) -> bool {
    matches!(reason, EFFECTS_OVER_BUDGET | EFFECTS_OVER_DOCUMENT_BUDGET | EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET)
}

/// What one mapped stack adds to the document: `counted` instances (those a plan counts, by the importer's visibility
/// rule, which over-counts two kinds of layer: see the top of this file) and `uncounted` kept instances (together the
/// instances Folio charges one node each).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StackShape {
    counted: usize,
    uncounted: usize,
}

impl StackShape {
    /// `enabled` is the stack's master switch, `visible` is the layer's own visibility and that of all its groups.
    pub(crate) fn of(enabled: bool, effects: &[StudioEffect], visible: bool) -> Self {
        let live = if enabled { effects.iter().filter(|e| e.enabled).count() } else { 0 };
        let counted = if visible { live } else { 0 };
        Self { counted, uncounted: effects.len().saturating_sub(counted) }
    }
}

/// How much of a stack the tally keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fit {
    /// The whole stack.
    Whole,
    /// Its counted instances only: it has counted instances and its switched-off ones do not fit the uncounted bound.
    CountedOnly,
}

/// The running effect totals of one import, in import order (panel order, top-most first, a group before its
/// children). Two independent bounds, each cutting only what it bounds:
/// - COUNTED instances (per layer [`MAX_EFFECTS_PER_LAYER`], per document [`MAX_EFFECTS_PER_PLAN`]): the first stack
///   that would take the counted total past the plan limit closes the counted side, and every later stack with
///   counted instances is refused ([`EFFECTS_OVER_DOCUMENT_BUDGET`]). A stack with none is not affected;
/// - UNCOUNTED instances ([`MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT`]): the first stack whose uncounted instances do not
///   fit closes the uncounted side. Later stacks WITHOUT counted instances are refused
///   ([`EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET`]); a stack WITH counted instances is never refused for this bound,
///   it keeps its counted instances and drops its switched-off ones ([`Fit::CountedOnly`]).
///
/// Each side keeps a prefix of the import order, so what is kept does not depend on how the later stacks happen to
/// be sized. A refusal for the per-layer limit closes neither side, and a later stack over the per-layer limit keeps
/// that code. Kept counted <= [`MAX_EFFECTS_PER_PLAN`] and kept uncounted <= [`MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT`]
/// always, so [`Self::kept`] <= their sum. Tests
/// `effects_over_the_document_cap_become_typed_losses_from_the_crossing_stack_on`,
/// `instances_no_plan_counts_are_bounded_too_so_the_kept_total_and_the_record_validation_stay_bounded`,
/// `the_unrendered_bound_never_costs_a_stack_with_counted_instances_its_rendered_effects` and
/// `the_counted_bound_never_costs_a_stack_without_counted_instances_its_record`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EffectTally {
    counted: usize,
    uncounted: usize,
    counted_closed: bool,
    uncounted_closed: bool,
}

impl EffectTally {
    /// Whether, and how much of, a stack of this shape fits. Does not charge it.
    pub(crate) fn admit(&self, shape: StackShape) -> Result<Fit, Reason> {
        if shape.counted == 0 && shape.uncounted == 0 {
            return Ok(Fit::Whole);
        }
        if shape.counted > MAX_EFFECTS_PER_LAYER {
            return Err(EFFECTS_OVER_BUDGET);
        }
        let uncounted_fits = !self.uncounted_closed
            && self.uncounted.checked_add(shape.uncounted).is_some_and(|total| total <= MAX_UNCOUNTED_EFFECTS_PER_DOCUMENT);
        if shape.counted == 0 {
            // No counted instances: only the uncounted bound applies, the counted side is not consulted.
            return if uncounted_fits { Ok(Fit::Whole) } else { Err(EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET) };
        }
        // Counted instances: the counted bounds decide whether the stack is kept; the uncounted bound only decides
        // whether its switched-off instances go with it, never whether the stack is.
        if self.counted_closed || self.counted.checked_add(shape.counted).is_none_or(|total| total > MAX_EFFECTS_PER_PLAN) {
            return Err(EFFECTS_OVER_DOCUMENT_BUDGET);
        }
        Ok(if shape.uncounted == 0 || uncounted_fits { Fit::Whole } else { Fit::CountedOnly })
    }

    /// Charges a stack that was kept: `shape` is the shape of what the record holds, `fit` what [`Self::admit`] said.
    pub(crate) fn charge(&mut self, shape: StackShape, fit: Fit) {
        self.counted = self.counted.saturating_add(shape.counted);
        self.uncounted = self.uncounted.saturating_add(shape.uncounted);
        if fit == Fit::CountedOnly {
            self.uncounted_closed = true;
        }
    }

    /// Records why a stack was refused: a document-total refusal closes its side.
    pub(crate) fn refused(&mut self, reason: Reason) {
        if reason == EFFECTS_OVER_DOCUMENT_BUDGET {
            self.counted_closed = true;
        } else if reason == EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET {
            self.uncounted_closed = true;
        }
    }

    /// Every kept instance: the nodes Folio's record validation charges for effects.
    pub(crate) fn kept(&self) -> usize {
        self.counted.saturating_add(self.uncounted)
    }
}

use crate::blend::studio_blend_for_key;
use crate::descriptor::{self, Object, Value};
use crate::layers::PsdLayer;
use crate::limits::Limits;
use crate::loss::LossKind;

/// Blend-mode ids (`BlnM`) quoted from ag-psd `descriptor.ts`, with the PSD layer key each one is.
const BLEND_IDS: [(&str, [u8; 4]); 16] = [
    ("Nrml", *b"norm"),
    ("Mltp", *b"mul "),
    ("Scrn", *b"scrn"),
    ("Ovrl", *b"over"),
    ("SftL", *b"sLit"),
    ("HrdL", *b"hLit"),
    ("CDdg", *b"div "),
    ("CBrn", *b"idiv"),
    ("Drkn", *b"dark"),
    ("Lghn", *b"lite"),
    ("Dfrn", *b"diff"),
    ("Xclu", *b"smud"),
    ("H   ", *b"hue "),
    ("Strt", *b"sat "),
    ("Clr ", *b"colr"),
    ("Lmns", *b"lum "),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    Bevel,
    ColourOverlay,
    GradientOverlay,
    PatternOverlay,
    Satin,
    Stroke,
}

fn slot_of(key: &str) -> Option<(Slot, bool)> {
    Some(match key {
        "DrSh" => (Slot::DropShadow, false),
        "dropShadowMulti" => (Slot::DropShadow, true),
        "IrSh" => (Slot::InnerShadow, false),
        "innerShadowMulti" => (Slot::InnerShadow, true),
        "OrGl" => (Slot::OuterGlow, false),
        "IrGl" => (Slot::InnerGlow, false),
        "ebbl" => (Slot::Bevel, false),
        "SoFi" => (Slot::ColourOverlay, false),
        "solidFillMulti" => (Slot::ColourOverlay, true),
        "GrFl" => (Slot::GradientOverlay, false),
        "gradientFillMulti" => (Slot::GradientOverlay, true),
        "patternFill" => (Slot::PatternOverlay, false),
        "ChFX" => (Slot::Satin, false),
        "FrFX" => (Slot::Stroke, false),
        "frameFXMulti" => (Slot::Stroke, true),
        _ => return None,
    })
}

fn has_percent_unit(value: &Value) -> bool {
    value.unit() == Some(*b"#Prc")
}

/// A percent value in `0..=100` (the Folio range of opacity, noise, spread, choke, range, jitter).
fn percent(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if has_percent_unit(v) => {
            v.finite_number().filter(|n| (0.0..=100.0).contains(n)).ok_or("effect_value_invalid")
        }
        Some(_) => Err("effect_unit_unexpected"),
    }
}

/// A percent value with no upper bound (bevel depth).
fn percent_unbounded(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if has_percent_unit(v) => v.finite_number().ok_or("effect_value_invalid"),
        Some(_) => Err("effect_unit_unexpected"),
    }
}

fn pixels(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if v.unit() == Some(*b"#Pxl") => v.finite_number().filter(|n| *n >= 0.0).ok_or("effect_value_invalid"),
        Some(_) => Err("effect_unit_unexpected"),
    }
}

/// An angle in degrees within `lo..=hi` (`-180..=180` for the light angle, `0..=90` for altitude).
fn angle(o: &Object, key: &str, default: f64, lo: f64, hi: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if v.unit() == Some(*b"#Ang") => {
            v.finite_number().filter(|n| (lo..=hi).contains(n)).ok_or("effect_value_invalid")
        }
        Some(_) => Err("effect_unit_unexpected"),
    }
}

/// A bool item: absent means `default`; present with another type is a refusal (not a default).
fn flag(o: &Object, key: &str, default: bool) -> Result<bool, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) => v.boolean().ok_or("effect_value_invalid"),
    }
}

fn blend(o: &Object, key: &str) -> Result<u16, Reason> {
    let id = o.get(key).and_then(Value::enum_value).ok_or("effect_blend_mode_missing")?;
    BLEND_IDS
        .iter()
        .find(|(name, _)| *name == id)
        .and_then(|(_, psd_key)| studio_blend_for_key(*psd_key))
        .map(|mode| mode.discriminant())
        .ok_or("effect_blend_mode_unmapped")
}

/// RGB colour object (`RGBC`: `Rd  `, `Grn `, `Bl  ` doubles `0..=255`); other colour models are not
/// mapped. The profile stays unassigned: Photoshop stores effect colours in the document's encoded
/// space, which Folio cannot name without inventing a binding.
fn colour(o: &Object, key: &str) -> Result<Option<StudioColor>, Reason> {
    let Some(value) = o.get(key) else {
        return Ok(None);
    };
    let c = value.object().ok_or("effect_colour_model_unmapped")?;
    let channel = |name: &str| -> Result<f64, Reason> {
        let item = c.get(name).filter(|v| v.is_number()).ok_or("effect_colour_model_unmapped")?;
        match item.finite_number() {
            Some(v) if (0.0..=255.0).contains(&v) => Ok(v / 255.0),
            _ => Err("effect_colour_range"),
        }
    };
    Ok(Some(StudioColor { r: channel("Rd  ")?, g: channel("Grn ")?, b: channel("Bl  ")?, colour_profile_id: ProfileBinding::Unassigned(()) }))
}

fn contour(o: &Object, key: &str) -> Result<Option<StudioContour>, Reason> {
    let Some(value) = o.get(key) else {
        return Ok(None);
    };
    let c = value.object().ok_or("effect_contour_unmapped")?;
    let name = match c.get("Nm  ") {
        Some(Value::Text(t)) if !t.is_empty() && t.len() <= 128 && !t.chars().any(char::is_control) => t.clone(),
        _ => return Err("effect_contour_unmapped"),
    };
    let list = c.get("Crv ").and_then(Value::list).ok_or("effect_contour_unmapped")?;
    if !(2..=MAX_CURVE_POINTS).contains(&list.len()) {
        return Err("effect_contour_unmapped");
    }
    let mut points: Vec<StudioContourPoint> = Vec::with_capacity(list.len());
    for item in list {
        let p = item.object().ok_or("effect_contour_unmapped")?;
        let (input, output) = (p.get("Hrzn").and_then(Value::finite_number), p.get("Vrtc").and_then(Value::finite_number));
        let (Some(input), Some(output)) = (input, output) else {
            return Err("effect_contour_unmapped");
        };
        let in_range = |v: f64| (0.0..=255.0).contains(&v);
        if !in_range(input) || !in_range(output) || points.last().is_some_and(|last| last.input >= input) {
            return Err("effect_contour_unmapped");
        }
        points.push(StudioContourPoint { input, output, corner: flag(p, "Cnty", false)? });
    }
    Ok(Some(StudioContour { name, points }))
}

fn pick<T: Copy>(o: &Object, key: &str, table: &[(&str, T)]) -> Result<T, Reason> {
    let id = o.get(key).and_then(Value::enum_value).ok_or("effect_enumeration_missing")?;
    table.iter().find(|(name, _)| *name == id).map(|(_, v)| *v).ok_or("effect_enumeration_unmapped")
}

const GLOW_TECHNIQUE: [(&str, GlowTechnique); 2] = [("SfBL", GlowTechnique::Softer), ("PrBL", GlowTechnique::Precise)];

fn map_effect(slot: Slot, o: &Object, ordinal: usize, notes: &mut Vec<Note>) -> Result<StudioEffect, Reason> {
    if matches!(slot, Slot::GradientOverlay | Slot::PatternOverlay) {
        // Gradient and pattern content are references to primitives this importer does not mint.
        return Err("effect_fill_reference_unavailable");
    }
    let opacity = percent(o, "Opct", 100.0)?;
    let mut effect = StudioEffect {
        instance_id: String::new(),
        enabled: flag(o, "enab", true)?,
        visible_in_editor: flag(o, "showInDialog", true)?,
        blend_mode: blend(o, "Md  ")?,
        opacity,
        colour: colour(o, "Clr ")?,
        contour: None,
        anti_alias: flag(o, "AntA", false)?,
        noise: percent(o, "Nose", 0.0)?,
        angle: angle(o, "lagl", 0.0, -180.0, 180.0)?,
        use_global_light: flag(o, "uglg", false)?,
        params: StudioEffectKind::ColourOverlay,
    };
    match contour(o, "TrnS") {
        Ok(c) => effect.contour = c,
        Err(code) => notes.push((LossKind::Unsupported, code)),
    }
    if o.get("Grad").is_some() {
        return Err("effect_fill_reference_unavailable");
    }
    effect.params = match slot {
        Slot::DropShadow => StudioEffectKind::DropShadow {
            distance: pixels(o, "Dstn", 0.0)?,
            spread: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            layer_knocks_out: flag(o, "layerConceals", true)?,
        },
        Slot::InnerShadow => StudioEffectKind::InnerShadow {
            distance: pixels(o, "Dstn", 0.0)?,
            choke: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
        },
        Slot::OuterGlow => StudioEffectKind::OuterGlow {
            technique: pick(o, "GlwT", &GLOW_TECHNIQUE)?,
            spread: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            range: percent(o, "Inpr", 50.0)?,
            jitter: percent(o, "ShdN", 0.0)?,
            gradient: None,
        },
        Slot::InnerGlow => StudioEffectKind::InnerGlow {
            technique: pick(o, "GlwT", &GLOW_TECHNIQUE)?,
            source: pick(o, "glwS", &[("SrcE", GlowSource::Edge), ("SrcC", GlowSource::Centre)])?,
            choke: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            range: percent(o, "Inpr", 50.0)?,
            jitter: percent(o, "ShdN", 0.0)?,
            gradient: None,
        },
        Slot::Satin => StudioEffectKind::Satin {
            distance: pixels(o, "Dstn", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            invert: flag(o, "Invr", false)?,
        },
        Slot::ColourOverlay => StudioEffectKind::ColourOverlay,
        Slot::Stroke => {
            if pick(o, "PntT", &[("SClr", StrokeFillType::Solid), ("GrFl", StrokeFillType::Gradient), ("Ptrn", StrokeFillType::Pattern)])?
                != StrokeFillType::Solid
            {
                return Err("effect_fill_reference_unavailable");
            }
            StudioEffectKind::Stroke {
                size: pixels(o, "Sz  ", 0.0)?,
                position: pick(
                    o,
                    "Styl",
                    &[("InsF", StrokePosition::Inside), ("OutF", StrokePosition::Outside), ("CtrF", StrokePosition::Centre)],
                )?,
                fill_type: StrokeFillType::Solid,
                fill: None,
                overprint: flag(o, "overprint", false)?,
            }
        }
        Slot::Bevel => {
            if flag(o, "useTexture", false)? {
                return Err("effect_fill_reference_unavailable");
            }
            let altitude = angle(o, "Lald", 30.0, 0.0, 90.0)?;
            StudioEffectKind::BevelEmboss {
                style: pick(
                    o,
                    "bvlS",
                    &[
                        ("InrB", BevelStyle::InnerBevel),
                        ("OtrB", BevelStyle::OuterBevel),
                        ("Embs", BevelStyle::Emboss),
                        ("PlEb", BevelStyle::PillowEmboss),
                        ("strokeEmboss", BevelStyle::StrokeEmboss),
                    ],
                )?,
                technique: pick(
                    o,
                    "bvlT",
                    &[("SfBL", BevelTechnique::Smooth), ("PrBL", BevelTechnique::ChiselHard), ("Slmt", BevelTechnique::ChiselSoft)],
                )?,
                direction: pick(o, "bvlD", &[("In  ", BevelDirection::Up), ("Out ", BevelDirection::Down)])?,
                depth: percent_unbounded(o, "srgR", 100.0)?,
                size: pixels(o, "blur", 0.0)?,
                soften: pixels(o, "Sftn", 0.0)?,
                altitude,
                gloss_contour: None,
                highlight_mode: blend(o, "hglM")?,
                highlight_colour: colour(o, "hglC")?.ok_or("effect_colour_model_unmapped")?,
                highlight_opacity: percent(o, "hglO", 100.0)?,
                shadow_mode: blend(o, "sdwM")?,
                shadow_colour: colour(o, "sdwC")?.ok_or("effect_colour_model_unmapped")?,
                shadow_opacity: percent(o, "sdwO", 100.0)?,
                anti_alias_gloss: flag(o, "antialiasGloss", false)?,
                use_contour: flag(o, "useShape", false)?,
                use_texture: false,
                texture: None,
            }
        }
        Slot::GradientOverlay | Slot::PatternOverlay => return Err("effect_fill_reference_unavailable"),
    };
    effect.instance_id = format!("{}-{ordinal}", effect.params.key());
    Ok(effect)
}

/// Maps the effects block of `layer`. `None` when it has no `lfx2` / `lmfx` block; `Err` when the
/// descriptor cannot be read or the stack does not fit: more mapped instances than Folio's per-stack ceiling, more
/// counted instances than [`MAX_EFFECTS_PER_LAYER`] ([`EFFECTS_OVER_BUDGET`]), or past a document total of `tally`
/// ([`EFFECTS_OVER_DOCUMENT_BUDGET`] for a stack with counted instances, [`EFFECTS_UNRENDERED_OVER_DOCUMENT_BUDGET`]
/// for one without). A stack with counted instances whose switched-off instances do not fit the uncounted bound is
/// `Ok` with [`Fit::CountedOnly`]: the stack holds its counted instances only. `visible` is the layer's own
/// visibility and that of all its groups (see the rule at the top of this file). `stack_id` mints the `SEFX`
/// identity and is only called once the descriptor parsed and the stack fits, so a refused stack consumes no id.
pub(crate) fn effect_stack(
    layer: &PsdLayer,
    visible: bool,
    tally: &EffectTally,
    stack_id: impl FnOnce() -> Option<String>,
    limits: &Limits,
) -> Option<Result<(Mapped<StudioEffectStack>, Fit), Reason>> {
    let block = layer.block(b"lfx2").or_else(|| layer.block(b"lmfx"))?;
    Some(map_stack(&block.data, visible, tally, stack_id, limits))
}

fn map_stack(
    data: &[u8],
    visible: bool,
    tally: &EffectTally,
    stack_id: impl FnOnce() -> Option<String>,
    limits: &Limits,
) -> Result<(Mapped<StudioEffectStack>, Fit), Reason> {
    let root = descriptor::parse_versioned(data, &[0, 16], limits)?;
    let mut notes: Vec<Note> = Vec::new();
    let enabled = flag(&root, "masterFXSwitch", true)?;
    let scale = match root.get("Scl ") {
        None => 100.0,
        Some(v) if has_percent_unit(v) => v.finite_number().filter(|n| *n > 0.0).ok_or("effect_scale_invalid")?,
        Some(_) => return Err("effect_scale_invalid"),
    };
    let mut effects: Vec<StudioEffect> = Vec::new();
    let (mut singles, mut multis) = ([false; 10], [false; 10]);
    for (key, value) in &root.items {
        let Some((slot, multi)) = slot_of(key) else {
            continue;
        };
        let index = slot as usize;
        if multi {
            multis[index] = true;
        } else {
            singles[index] = true;
        }
        let objects: Vec<&Object> = match (multi, value) {
            (false, Value::Object(o)) => vec![o],
            (true, Value::List(list)) => {
                let objects: Vec<&Object> = list.iter().filter_map(Value::object).collect();
                if objects.len() != list.len() {
                    notes.push((LossKind::Unsupported, "effect_slot_malformed"));
                }
                objects
            }
            _ => {
                notes.push((LossKind::Unsupported, "effect_slot_malformed"));
                continue;
            }
        };
        for object in objects {
            match map_effect(slot, object, effects.len(), &mut notes) {
                Ok(effect) => {
                    // The instance that would be one past Folio's per-stack ceiling refuses the whole stack,
                    // before any more of the descriptor is mapped (test `a_stack_past_folios_ceiling_...`).
                    // `MAX_EFFECTS_PER_LAYER` counts only the live instances of a visible layer and is judged
                    // below, once the stack is known.
                    if effects.len() >= MAX_EFFECTS {
                        return Err(EFFECTS_OVER_BUDGET);
                    }
                    effects.push(effect);
                }
                Err(code) => notes.push((LossKind::Unsupported, code)),
            }
        }
    }
    if singles.iter().zip(&multis).any(|(single, multi)| *single && *multi) {
        notes.push((LossKind::Transformed, "effect_single_and_multi_slots_merged"));
    }
    let fit = tally.admit(StackShape::of(enabled, &effects, visible))?;
    if fit == Fit::CountedOnly {
        // Only a stack with counted instances gets here: `enabled` and `visible`, so the live instances are exactly
        // the enabled ones, in their stack order. The instances that are switched off are not recorded.
        effects.retain(|effect| effect.enabled);
    }
    notes.push((LossKind::Transformed, "effect_colour_profile_unassigned"));
    let effect_stack_id = stack_id().ok_or("effect_stack_id_unavailable")?;
    Ok((Mapped { value: StudioEffectStack { effect_stack_id, enabled, scale, effects }, notes }, fit))
}
