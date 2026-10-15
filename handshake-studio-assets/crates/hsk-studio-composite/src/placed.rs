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
//! differs from the record, and a placed layer carrying smart filters (their stacking over the
//! placed content is lane SMB-PS-SMART-FILTER-WIRING's and is not modelled here yet). Recursion is
//! refused by Folio (`uses` cycles, depth `MAX_PLACED_DEPTH`) and bounded again by
//! [`crate::SceneLimits::max_depth`] at render.
#![deny(clippy::arithmetic_side_effects)]
use crate::CompositeError;
use crate::composition::{SceneComposition, SceneContent, SceneLayer};
use hsk_studio_folio::{StudioLinkHealth, StudioPlacedMode, StudioRasterRecords};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::transform_math::Affine2;
use std::collections::BTreeMap;

/// Refusal codes.
pub const PLACED_SOURCE_UNRESOLVED: &str = "placed_source_unresolved";
pub const PLACED_LINK_MISSING: &str = "placed_link_missing";
pub const PLACED_SOURCE_SIZE: &str = "placed_source_size_mismatch";
pub const PLACED_SMART_FILTERS: &str = "placed_smart_filters_not_modelled";

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
    if entry.smart_filters.is_some() {
        return Err(refuse(PLACED_SMART_FILTERS));
    }
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
    Ok(Some(SceneLayer {
        layer_id: layer_id.to_owned(),
        content: SceneContent::Precomp(Box::new(scene.clone())),
        mode,
        opacity,
        fill_opacity,
        matrix: placed_matrix(asset.transform),
        matte: None,
        clipped: false,
    }))
}
