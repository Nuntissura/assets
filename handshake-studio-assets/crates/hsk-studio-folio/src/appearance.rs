//! Per-layer appearance of `StudioLayer`: layer opacity and fill opacity (STU-RAS-006), blend mode
//! as the stable `StudioBlendMode` discriminant (STU-RAS-154 members 1..=30, STU-RAS-039 members
//! 31..=35), clip-to-base, group isolation and an optional `StudioMask` coverage reference.
//!
//! Folio stores and validates only. The blend maths, the mode facts and the discriminant to enum
//! mapping belong to the Pigment crate (`blend_math::StudioBlendMode`, which accepts exactly the
//! discriminants accepted here); Folio depends on Accord alone, so it holds the closed number set.
//!
//! Absent `appearance` is the kind's identity default and is not encoded (like `sequences`): a
//! group is `pass_through` (the default group mode, STU-RAS-154), anything else is `normal`; both
//! at 100 percent opacity and fill opacity, not clipped, no mask. Old documents therefore stay
//! valid and re-serialize byte-identically.
//!
//! Not stored here (spec parameters that stay pending): mask density and feather
//! (`StudioMask.density`, `StudioMask.feather`), blend-if ranges, knockout, effect stacks.
use super::{
    Budget, Code, Payload, Resolution, ResolutionIssue, Resolver, Result, StudioLayer, TileRef,
    canceled, err, finite_length, id, key,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// `StudioBlendMode::PassThrough`: group layers only, the default group mode.
pub const BLEND_PASS_THROUGH: u16 = 1;
/// `StudioBlendMode::Normal`: the default leaf mode.
pub const BLEND_NORMAL: u16 = 2;
/// Highest assigned `StudioBlendMode` discriminant; the known set is `1..=BLEND_LAST`.
pub const BLEND_LAST: u16 = 35;
/// `behind` and `clear` are tool-only (STU-RAS-155): not settable on a layer.
pub const BLEND_TOOL_ONLY: [u16; 2] = [24, 25];
/// Sample formats a mask coverage tile may carry (the Pigment `Mask` contract).
pub const MASK_FORMATS: [&str; 2] = ["coverage_u8", "coverage_u16le"];

/// Group compositing: `pass_through` blends children straight into the backdrop (non-isolated),
/// `isolated` composites the group on its own first. Meaningful for groups only; a leaf layer is
/// always `isolated`. For a group it is redundant with `blend_mode == 1` and validated against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    PassThrough,
    Isolated,
}

/// Mask coverage reference in the same style as raster tile references.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMask {
    /// Coverage tiles (`coverage_u8` or `coverage_u16le`); empty means an empty mask.
    pub tiles: Vec<TileRef>,
    pub enabled: bool,
    /// Mask moves with the layer (linked) or stays fixed in the document.
    pub linked: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerAppearance {
    /// Percent, `0.0..=100.0`: fades the whole layer including effects.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub opacity: f64,
    /// Percent, `0.0..=100.0`: fades pixel/fill content but not effects.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub fill_opacity: f64,
    /// Stable `StudioBlendMode` discriminant, `1..=35`.
    #[schemars(range(min = 1, max = 35))]
    pub blend_mode: u16,
    /// Clipped to the base layer below (clipping mask).
    pub clipped: bool,
    pub isolation: Isolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<StudioMask>,
}

impl LayerAppearance {
    /// The identity appearance of a group (`pass_through`) or of any other layer (`normal`).
    pub fn identity(group: bool) -> Self {
        Self {
            opacity: 100.0,
            fill_opacity: 100.0,
            blend_mode: if group { BLEND_PASS_THROUGH } else { BLEND_NORMAL },
            clipped: false,
            isolation: if group { Isolation::PassThrough } else { Isolation::Isolated },
            mask: None,
        }
    }

    /// `opacity` as a fraction in `0.0..=1.0`.
    pub fn opacity_fraction(&self) -> f64 {
        self.opacity / 100.0
    }

    /// `fill_opacity` as a fraction in `0.0..=1.0`.
    pub fn fill_opacity_fraction(&self) -> f64 {
        self.fill_opacity / 100.0
    }
}

impl StudioLayer {
    /// The stored appearance or, when absent, the kind's identity default.
    pub fn effective_appearance(&self) -> LayerAppearance {
        self.appearance.clone().unwrap_or_else(|| {
            LayerAppearance::identity(self.kind == "group" && matches!(self.payload, Payload::Group))
        })
    }
}

fn percent(v: f64) -> bool {
    v.is_finite() && (0.0..=100.0).contains(&v)
}

pub(crate) fn validate_layer(
    l: &StudioLayer,
    group: bool,
    b: Budget,
    token: &hsk_studio_accord::CancellationToken,
    r: &dyn Resolver,
    payload_size: &mut usize,
    issues: &mut Vec<ResolutionIssue>,
) -> Result<()> {
    let Some(a) = &l.appearance else {
        return Ok(());
    };
    canceled(token)?;
    if !percent(a.opacity) || !percent(a.fill_opacity) {
        return Err(err(Code::InvalidAppearance, &l.layer_id));
    }
    if !(1..=BLEND_LAST).contains(&a.blend_mode) {
        return Err(err(Code::UnknownBlendMode, &l.layer_id));
    }
    if BLEND_TOOL_ONLY.contains(&a.blend_mode) || (a.blend_mode == BLEND_PASS_THROUGH && !group) {
        return Err(err(Code::InapplicableBlendMode, &l.layer_id));
    }
    let expected = if group && a.blend_mode == BLEND_PASS_THROUGH {
        Isolation::PassThrough
    } else {
        Isolation::Isolated
    };
    if a.isolation != expected {
        return Err(err(Code::InvalidAppearance, &l.layer_id));
    }
    let Some(mask) = &a.mask else {
        return Ok(());
    };
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    if let Payload::Raster { tiles } = &l.payload {
        keys.extend(tiles.iter().map(|t| t.object_key.as_str()));
    }
    for t in &mask.tiles {
        canceled(token)?;
        if t.layer_id != l.layer_id || !key(&t.object_key) || !keys.insert(&t.object_key) {
            return Err(err(Code::InvalidAppearance, &l.layer_id));
        }
        finite_length(&t.width, super::GeometryUnit::Px, true, &l.layer_id)?;
        finite_length(&t.height, super::GeometryUnit::Px, true, &l.layer_id)?;
        if t.width.value <= 0.0
            || t.height.value <= 0.0
            || t.width.value.fract() != 0.0
            || t.height.value.fract() != 0.0
        {
            return Err(err(Code::InvalidUnit, &l.layer_id));
        }
        id(&t.colour_profile_id, "SCPF", &l.layer_id)?;
        if t.artifact_manifest_id.is_empty()
            || t.content_digest.algorithm.is_empty()
            || t.content_digest.digest.is_empty()
            || !MASK_FORMATS.contains(&t.format.as_str())
        {
            return Err(err(Code::InvalidField, &l.layer_id));
        }
        *payload_size = payload_size
            .checked_add(
                t.object_key.len()
                    + t.layer_id.len()
                    + t.format.len()
                    + t.colour_profile_id.len()
                    + t.content_digest.algorithm.len()
                    + t.content_digest.digest.len()
                    + t.artifact_manifest_id.len()
                    + 128,
            )
            .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?;
        if *payload_size > b.payload_bytes {
            return Err(err(Code::BudgetExceeded, &l.layer_id));
        }
        let disposition = if r.format_supported(&t.format) { r.tile(t) } else { Resolution::Unsupported };
        if disposition != Resolution::Available {
            issues.push(ResolutionIssue {
                target: format!("{}/mask/{}", l.layer_id, t.object_key),
                disposition,
            });
        }
    }
    Ok(())
}
