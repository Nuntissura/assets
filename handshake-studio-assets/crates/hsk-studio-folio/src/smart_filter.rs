//! Smart (live) filter stack record of a raster layer (STU-RAS-037 `StudioLiveFilter`: "a
//! re-editable, maskable filter"; STU-FX-011a: a filter on any layer is the non-destructive stack;
//! STU-FX-024: liquify "stored as a re-editable mesh on the stack entry"). Lane
//! SMB-PS-SELECT-LIQUIFY-2.
//!
//! The stack is an optional member of [`super::StudioRasterLayerRecord`]
//! (`smart_filters`, `serde(default, skip_serializing_if)`): record sets without it serialize
//! byte-identically to before (test `tests/smart_filter_records.rs`). Rendering is pigment's
//! (`smart_filter::SmartFilterStack`, the render-plan envelope entry `smart_filters`); Folio
//! validates structure, identity, finiteness and counts, not the filter maths ranges (those are
//! pigment's and refused at lowering).
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (V4-01) and every
//! count is checked before use.
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, StudioLayer, StudioMask, err, key, values::{Ctx, percent}};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Entries one stack may hold (pigment's per-layer filter cap).
pub const MAX_SMART_FILTERS: usize = 16;
/// Liquify mesh vertices one entry may store.
pub const MAX_LIQUIFY_VERTICES: u64 = 1 << 22;

/// Noise distribution of `add_noise`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmartNoiseDistribution {
    Uniform,
    Gaussian,
}

/// Resampling of a liquify entry (STU-RAS-141 members 2..=4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmartResample {
    NearestNeighbour,
    Bilinear,
    Bicubic,
}

/// A re-editable liquify mesh (pigment `liquify::LiquifyMesh` in record form): vertex `(i, j)` at
/// pixel centre `(origin_x + 0.5 + i spacing, origin_y + 0.5 + j spacing)`; `displacement` and
/// `freeze` row-major per vertex.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLiquifyMesh {
    pub origin_x: i64,
    pub origin_y: i64,
    pub width: u32,
    pub height: u32,
    pub spacing: u32,
    pub cols: u32,
    pub rows: u32,
    pub displacement: Vec<[f64; 2]>,
    pub freeze: Vec<f32>,
}

/// Kind block of one smart filter, tag `filter_kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "filter_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioSmartFilterKind {
    GaussianBlur { radius_px: f32 },
    UnsharpMask { amount_pct: f32, radius_px: f32, threshold_levels: u8 },
    HighPass { radius_px: f32 },
    Median { radius_px: u32 },
    AddNoise { amount_pct: f32, distribution: SmartNoiseDistribution, monochromatic: bool, seed: u64 },
    Liquify { mesh: StudioLiquifyMesh, resample: SmartResample },
}

/// One stack entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSmartFilter {
    /// Unique within the stack (key grammar).
    pub instance_id: String,
    pub enabled: bool,
    /// Percent `0..=100`.
    pub opacity: f64,
    /// Per-filter coverage mask (`coverage_u8` / `coverage_u16le` tiles of this layer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<StudioMask>,
    pub params: StudioSmartFilterKind,
}

/// The layer's smart-filter stack: entries in render order (first applied first), master enable
/// and the stack-wide filter mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSmartFilterStack {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_mask: Option<StudioMask>,
    pub filters: Vec<StudioSmartFilter>,
}

fn invalid(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

fn mask_tiles(m: &StudioMask, layer: &StudioLayer, target: &str, keys: &mut BTreeSet<String>) -> Result<()> {
    for t in &m.tiles {
        if t.layer_id != layer.layer_id || !key(&t.object_key) || !keys.insert(t.object_key.clone()) {
            return Err(invalid(target, "mask/tiles"));
        }
        let px = |v: f64| v.is_finite() && v > 0.0 && v.fract() == 0.0;
        if !px(t.width.value)
            || !px(t.height.value)
            || t.width.unit != super::GeometryUnit::Px
            || t.height.unit != super::GeometryUnit::Px
            || !super::MASK_FORMATS.contains(&t.format.as_str())
            || t.artifact_manifest_id.is_empty()
            || t.content_digest.digest.is_empty()
        {
            return Err(invalid(target, "mask/tiles"));
        }
    }
    Ok(())
}

fn mesh(m: &StudioLiquifyMesh, target: &str) -> Result<()> {
    let grid = |len: u32| len.saturating_sub(1).checked_div(m.spacing).map(|q| {
        // ceil((len - 1) / spacing) + 1
        let r = len.saturating_sub(1).checked_rem(m.spacing).unwrap_or(0);
        q.saturating_add(u32::from(r != 0)).saturating_add(1)
    });
    if m.spacing == 0 || m.width == 0 || m.height == 0 || grid(m.width) != Some(m.cols) || grid(m.height) != Some(m.rows) {
        return Err(invalid(target, "mesh/grid"));
    }
    let n = u64::from(m.cols).checked_mul(u64::from(m.rows)).ok_or_else(|| invalid(target, "mesh/grid"))?;
    if n > MAX_LIQUIFY_VERTICES {
        return Err(err(Code::BudgetExceeded, target));
    }
    let n = usize::try_from(n).map_err(|_| invalid(target, "mesh/grid"))?;
    if m.displacement.len() != n || m.freeze.len() != n {
        return Err(invalid(target, "mesh/length"));
    }
    if m.origin_x.checked_add(i64::from(m.width)).is_none() || m.origin_y.checked_add(i64::from(m.height)).is_none() {
        return Err(invalid(target, "mesh/origin"));
    }
    if m.displacement.iter().any(|d| !d[0].is_finite() || !d[1].is_finite())
        || m.freeze.iter().any(|f| !f.is_finite() || !(0.0..=1.0).contains(f))
    {
        return Err(invalid(target, "mesh/values"));
    }
    Ok(())
}

/// Structural validation of a layer's stack (called by `validate_raster_records`): count cap,
/// unique key-grammar instance ids, opacity percent, finite non-negative parameters, mask tiles of
/// this layer with unique keys (also against the layer's raster tiles), mesh shape. Entries and
/// mesh vertices count against `Budget::nodes`.
pub(crate) fn validate_smart_filters(s: &StudioSmartFilterStack, layer: &StudioLayer, cx: &mut Ctx) -> Result<()> {
    let target = format!("{}/smart_filters", layer.layer_id);
    if s.filters.len() > MAX_SMART_FILTERS {
        return Err(invalid(&target, "filters"));
    }
    cx.nodes(s.filters.len(), &target)?;
    let mut keys: BTreeSet<String> = BTreeSet::new();
    if let super::Payload::Raster { tiles } = &layer.payload {
        keys.extend(tiles.iter().map(|t| t.object_key.clone()));
    }
    if let Some(m) = &layer.appearance.as_ref().and_then(|a| a.mask.as_ref()) {
        keys.extend(m.tiles.iter().map(|t| t.object_key.clone()));
    }
    if let Some(m) = &s.stack_mask {
        mask_tiles(m, layer, &target, &mut keys)?;
    }
    let mut ids = BTreeSet::new();
    for f in &s.filters {
        cx.tick()?;
        let t = format!("{target}/{}", f.instance_id);
        if !key(&f.instance_id) || !ids.insert(f.instance_id.as_str()) {
            return Err(invalid(&target, "instance_id"));
        }
        if !percent(f.opacity) {
            return Err(invalid(&t, "opacity"));
        }
        if let Some(m) = &f.mask {
            mask_tiles(m, layer, &t, &mut keys)?;
        }
        let ok = |v: f32| v.is_finite() && v >= 0.0;
        match &f.params {
            StudioSmartFilterKind::GaussianBlur { radius_px } | StudioSmartFilterKind::HighPass { radius_px } => {
                if !ok(*radius_px) {
                    return Err(invalid(&t, "radius_px"));
                }
            }
            StudioSmartFilterKind::UnsharpMask { amount_pct, radius_px, .. } => {
                if !ok(*amount_pct) || !ok(*radius_px) {
                    return Err(invalid(&t, "params"));
                }
            }
            StudioSmartFilterKind::Median { .. } => {}
            StudioSmartFilterKind::AddNoise { amount_pct, .. } => {
                if !ok(*amount_pct) {
                    return Err(invalid(&t, "amount_pct"));
                }
            }
            StudioSmartFilterKind::Liquify { mesh: m, .. } => {
                mesh(m, &t)?;
                cx.nodes(m.displacement.len(), &t)?;
            }
        }
    }
    Ok(())
}
