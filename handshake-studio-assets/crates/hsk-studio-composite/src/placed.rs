//! Placed assets (smart objects, STU-RAS-012..014; lane SMB-PS-ADJUST-STYLES) through the
//! composition path. A Folio `StudioPlacedAsset` names a SHARED `StudioPlacedSource` and an affine
//! transform; this module turns an instance into a [`SceneLayer`] whose content is the source's
//! evaluated scene ([`SceneContent::Precomp`]), so the existing scene lowering renders the source
//! first into a surface of the source size and then places it with the instance transform
//! (STU-CMP-010: source resolution first, transform after). Every instance of one source reads the
//! same source scene, so editing the source changes every instance (STU-RAS-013) by construction.
//!
//! The caller evaluates each source document (embedded: the stored Studio document; linked: the
//! file at the relative URI) into a [`SceneComposition`] keyed by `source_id`; composite resolves
//! nothing on disk. Refusals are typed codes (`CompositeError::Unavailable`), never a silent fall
//! back: an unresolved source, a linked source whose health is `missing`, a source scene whose size
//! differs from the record. Recursion is refused by Folio (`uses` cycles, depth `MAX_PLACED_DEPTH`)
//! and bounded again by [`crate::SceneLimits::max_depth`] at render.
//!
//! Smart filters over a placed instance (lane SMB-PS-ADJUST-STYLES-2; STU-RAS-037 live filters,
//! STU-FX-011a): the layer's `smart_filters` record converts through pigment's
//! `SmartFilterOp::from_record` (the single owner of the mapping, as on the raster lowering path)
//! and wraps the source scene in [`SceneContent::Filtered`]: the stack runs on the rendered source
//! surface, then the instance transform places the filtered surface (STU-CMP-010: effects before
//! the transform). Photoshop evaluates smart filters on the transformed smart object
//! (document-space radii): that order is UNVERIFIED (D5) and is the documented difference.
//! Enabled per-filter and stack masks (lane SMB-PS-ADJUST-STYLES-3; STU-FX-004): the mask tiles
//! are in document space and the stack runs in source space, so each enabled mask becomes a
//! [`FilteredMask`] (its scene mask id, `layer_id/object_key` of its tiles joined by `|` as on the
//! static path, and the instance transform) that the composition resamples into the source surface
//! before the stack runs ([`crate::composition::FilteredContent`]); an enabled mask without tiles is
//! `placed_smart_filter_mask_empty` (as the raster path's `smart_filter_mask_empty`). A stack
//! pigment would refuse is `smart_filter_parameter`, a disabled stack or one without enabled
//! entries is ignored, a disabled mask applies everywhere and is dropped.
#![deny(clippy::arithmetic_side_effects)]
use crate::CompositeError;
use crate::composition::{FilteredContent, FilteredMask, SceneComposition, SceneContent, SceneLayer};
use hsk_studio_folio::StudioSmartFilterStack;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{MaskFormat, MaskInput, SourceKey};
use hsk_studio_pigment::smart_filter::SmartFilterOp;
use hsk_studio_folio::{StudioLinkHealth, StudioPlacedMode, StudioRasterRecords};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::transform_math::Affine2;
use std::collections::BTreeMap;

/// Refusal codes.
pub const PLACED_SOURCE_UNRESOLVED: &str = "placed_source_unresolved";
pub const PLACED_LINK_MISSING: &str = "placed_link_missing";
pub const PLACED_SOURCE_SIZE: &str = "placed_source_size_mismatch";
/// Kept for stored receipts: no longer produced (smart filters over a placed instance render since
/// lane SMB-PS-ADJUST-STYLES-2).
pub const PLACED_SMART_FILTERS: &str = "placed_smart_filters_not_modelled";
/// Kept for stored receipts: no longer produced (enabled masks over a placed instance render since
/// lane SMB-PS-ADJUST-STYLES-3).
pub const PLACED_SMART_FILTER_MASK: &str = "placed_smart_filter_mask";
pub const PLACED_SMART_FILTER_MASK_EMPTY: &str = "placed_smart_filter_mask_empty";
pub const PLACED_SMART_FILTER_PARAMETER: &str = "smart_filter_parameter";

/// The pigment stack of a placed layer's smart-filter record and its document-space masks
/// (`None`: nothing to apply), or the refusal code. Each enabled mask of an enabled filter and an
/// enabled stack mask becomes a plan mask source (key `1..`, a placeholder over the source `rect`
/// that the composition fills, see the module doc) and a [`FilteredMask`] mapped by `to_document`;
/// disabled masks apply everywhere and are dropped first.
#[allow(clippy::type_complexity)]
fn smart_filters(
    stack: &StudioSmartFilterStack,
    to_document: Affine2,
    rect: Rect,
) -> Result<Option<(SmartFilterOp, Vec<FilteredMask>)>, &'static str> {
    if !stack.enabled {
        return Ok(None);
    }
    let keep = |m: &Option<hsk_studio_folio::StudioMask>| m.as_ref().filter(|m| m.enabled).cloned();
    let mut stack = stack.clone();
    stack.stack_mask = keep(&stack.stack_mask);
    for f in &mut stack.filters {
        f.mask = if f.enabled { keep(&f.mask) } else { None };
    }
    let mut masks: Vec<FilteredMask> = Vec::new();
    let mut empty = false;
    let op = SmartFilterOp::from_record(&stack, |m| {
        if m.tiles.is_empty() {
            empty = true;
            return None;
        }
        let key = SourceKey(u32::try_from(masks.len()).ok()?.checked_add(1)?);
        let ids: Vec<String> = m.tiles.iter().map(|t| format!("{}/{}", t.layer_id, t.object_key)).collect();
        masks.push(FilteredMask { key, mask_id: ids.join("|"), to_document });
        Some(MaskInput {
            key,
            placement: rect,
            format: MaskFormat::CoverageU16Le,
            sha256: [0; 32],
            inverted: false,
            density: 1.0,
        })
    });
    if empty {
        return Err(PLACED_SMART_FILTER_MASK_EMPTY);
    }
    let op = op.map_err(|_| PLACED_SMART_FILTER_PARAMETER)?;
    if op.entries.iter().all(|e| !e.enabled) {
        return Ok(None);
    }
    if !op.check() {
        return Err(PLACED_SMART_FILTER_PARAMETER);
    }
    Ok(Some((op, masks)))
}

/// Folio's `[a, b, c, d, e, f]` (point = `[a c e; b d f] * [x y 1]`) as pigment's row-major
/// [`Affine2`].
pub fn placed_matrix(t: [f64; 6]) -> Affine2 {
    let [a, b, c, d, e, f] = t;
    Affine2 {
        m: [a, c, e, b, d, f],
    }
}

/// The scene layer of the placed-asset instance on `layer_id`, `Ok(None)` when the layer has no
/// placed-asset record. `mode`, `opacity` and `fill_opacity` are the layer's own (the instance is
/// composited like any layer).
pub fn placed_scene_layer(
    layer_id: &str,
    records: &StudioRasterRecords,
    sources: &BTreeMap<String, SceneComposition>,
    mode: StudioBlendMode,
    opacity: f64,
    fill_opacity: f64,
) -> Result<Option<SceneLayer>, CompositeError> {
    let Some(entry) = records.layer(layer_id) else {
        return Ok(None);
    };
    let Some(asset) = &entry.placed_asset else {
        return Ok(None);
    };
    let refuse = |code: &str| CompositeError::Unavailable(format!("{layer_id}:{code}"));
    let source = records
        .placed_source(&asset.source_id)
        .ok_or_else(|| refuse(PLACED_SOURCE_UNRESOLVED))?;
    if let StudioPlacedMode::Linked {
        health: StudioLinkHealth::Missing,
        ..
    } = source.location
    {
        return Err(refuse(PLACED_LINK_MISSING));
    }
    let scene = sources
        .get(&asset.source_id)
        .ok_or_else(|| refuse(PLACED_SOURCE_UNRESOLVED))?;
    if f64::from(scene.width) != source.width || f64::from(scene.height) != source.height {
        return Err(refuse(PLACED_SOURCE_SIZE));
    }
    let matrix = placed_matrix(asset.transform);
    let filters = match &entry.smart_filters {
        Some(stack) => smart_filters(stack, matrix, scene.rect()).map_err(refuse)?,
        None => None,
    };
    let precomp = SceneContent::Precomp(Box::new(scene.clone()));
    let content = match filters {
        Some((filters, masks)) => SceneContent::Filtered(Box::new(FilteredContent {
            content: precomp,
            rect: scene.rect(),
            filters,
            masks,
        })),
        None => precomp,
    };
    Ok(Some(SceneLayer {
        layer_id: layer_id.to_owned(),
        content,
        mode,
        opacity,
        fill_opacity,
        matrix,
        matte: None,
        clipped: false,
    }))
}
