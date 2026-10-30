//! Brush pixel mix through pigment's `masked_replace` (lane SMB-HARNESS-UI-3 slice 2): the viewer
//! no longer carries its own copy of the formula. A stroke's coverage is applied to one tile body
//! by running the real engine transaction (`hsk_studio_pigment::masked_replace` with a
//! `SerializedOwner` execution port) once per 64 x 64 block that the stroke covers: the engine
//! bounds one request to 4096 pixels, so the tile is presented to it as blocks of a 64-pixel grid
//! (each block a view of the tile's own pixels with the tile's layer and profile), and the
//! engine's `after` planes are copied back. Replacement = `brush::paint_replacement` (the brush
//! colour at alpha 1), mask = the stroke coverage cropped to the block (`coverage_u16le`).
//!
//! Colour: tiles are `rgb_f32le` linear light + straight `f32le` alpha (the importers layout) in
//! the working space every importer converts into (`import::colour`, Prism
//! `WORKING_PROFILE_LINEAR_SRGB`). The engine admits only metadata-free profiles, so the bytes passed
//! are Prism `WORKING_PROFILE_LINEAR_SRGB_ADMITTED` (same matrix, white point and gamma 1) under the
//! tile own profile id; the engine refuses anything that is not linear light. A package whose tiles
//! use another profile under their id is outside this convention (UNVERIFIED for other producers).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::TileRef;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort, TileAddress};
use hsk_studio_pigment::brush::{BrushLimits, crop_to, paint_replacement};
use hsk_studio_pigment::caller::{MemoryAdmission, SerializedOwner};
use hsk_studio_pigment::render_plan::MaskFormat;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::{Grid, Mask, Outcome, Rect, Request, ResolvedTile, TileItem, masked_replace};
use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB_ADMITTED, profile_hash};
use serde_json::json;

/// Edge of one engine request block (64 x 64 = 4096 pixels, `masked_replace`'s rectangle cap).
pub const BLOCK: u32 = 64;
/// Bytes one block request may use (engine accounting).
pub(super) const BYTE_LIMIT: u64 = hsk_studio_pigment::MAX_BYTES;

/// Delivered engine frames are dropped: the harness records its own operation (`paint`).
pub(super) struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

/// Identity the engine request carries.
pub struct MixTarget<'a> {
    pub document: &'a DomainId,
    pub layer: &'a DomainId,
    pub profile: &'a DomainId,
    pub actor: &'a ActorContext,
}

pub(super) fn plane(bytes: &[u8], stride: usize, px: usize, rect: Rect, at: (usize, usize)) -> Option<Vec<u8>> {
    let w = usize::try_from(rect.width).ok()?;
    let h = usize::try_from(rect.height).ok()?;
    let mut out = Vec::with_capacity(w.checked_mul(h)?.checked_mul(px)?);
    for y in 0..h {
        let row = at.1.checked_add(y)?.checked_mul(stride)?;
        let start = row.checked_add(at.0.checked_mul(px)?)?;
        out.extend_from_slice(bytes.get(start..start.checked_add(w.checked_mul(px)?)?)?);
    }
    Some(out)
}

pub(super) fn put(bytes: &mut [u8], stride: usize, px: usize, rect: Rect, at: (usize, usize), src: &[u8]) -> Option<()> {
    let w = usize::try_from(rect.width).ok()?;
    let h = usize::try_from(rect.height).ok()?;
    let row_bytes = w.checked_mul(px)?;
    for y in 0..h {
        let row = at.1.checked_add(y)?.checked_mul(stride)?;
        let start = row.checked_add(at.0.checked_mul(px)?)?;
        let from = y.checked_mul(row_bytes)?;
        bytes.get_mut(start..start.checked_add(row_bytes)?)?.copy_from_slice(src.get(from..from.checked_add(row_bytes)?)?);
    }
    Some(())
}

/// A block of tile planes as the engine reads it (linear light, straight alpha, working profile).
pub(super) fn tile_view<'a>(view: &'a TileRef, block: Rect, colour: &'a [u8], alpha: &'a [u8], colour_stride: u64, alpha_stride: u64, profile: &'a DomainId) -> ResolvedTile<'a> {
    ResolvedTile {
        tile_ref: view,
        bounds: block,
        sample_format: "rgb_f32le",
        colour_bytes: colour,
        colour_stride_bytes: colour_stride,
        colour_sha256: profile_hash(colour),
        alpha_bytes: alpha,
        alpha_stride_bytes: alpha_stride,
        alpha_sha256: profile_hash(alpha),
        transfer: "linear_light",
        alpha_association: "straight",
        profile: ProfileInput { profile_id: profile, bytes: WORKING_PROFILE_LINEAR_SRGB_ADMITTED, expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB_ADMITTED) },
    }
}

/// One block through `masked_replace`: `before` colour/alpha planes of `block` (tight strides),
/// the brush colour and the coverage mask; returns the engine's `after` colour and alpha.
pub fn replace_block(target: &MixTarget<'_>, tile: &TileRef, block: Rect, colour: &[u8], alpha: &[u8], rgb: [f32; 3], mask: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let limits = BrushLimits::default();
    // The block is the single cell (0, 0) of a grid anchored at it, so any block up to the
    // engine cap (a tile edge that is not a multiple of 64 gives narrower edge blocks) is valid.
    let (column, row) = (0i64, 0i64);
    let key = format!("{}-bx{}y{}", tile.object_key, block.x, block.y);
    let view: TileRef = serde_json::from_value(json!({
        "object_key": key, "layer_id": tile.layer_id, "column": column, "row": row,
        "width": {"value": block.width, "unit": "px"}, "height": {"value": block.height, "unit": "px"},
        "format": "rgb_f32le", "colour_profile_id": target.profile.as_str(), "schema_id": "hsk.studio.raster_tile@1",
        "content_digest": tile.content_digest, "artifact_manifest_id": tile.artifact_manifest_id,
    }))
    .map_err(|e| format!("block reference: {e}"))?;
    let (rc, ra) = paint_replacement(block, rgb, &limits).map_err(|e| e.code().to_string())?;
    let colour_stride = u64::from(block.width).checked_mul(12).ok_or("stride")?;
    let alpha_stride = u64::from(block.width).checked_mul(4).ok_or("stride")?;
    let items = [TileItem {
        before: tile_view(&view, block, colour, alpha, colour_stride, alpha_stride, target.profile),
        replacement: tile_view(&view, block, &rc, &ra, colour_stride, alpha_stride, target.profile),
        expected_property_revision: 0,
    }];
    let request = Request {
        transport_version: 1,
        operation: "masked_replace",
        document_id: target.document,
        layer_id: target.layer,
        command_id: "harness-brush",
        correlation_id: 1,
        actor: target.actor,
        base_revision: 0,
        cancel_epoch: 1,
        grid: Grid { origin_x: block.x, origin_y: block.y, tile_width: block.width, tile_height: block.height },
        rectangle: block,
        mask: Mask { bounds: block, sample_format: "coverage_u16le", bytes: mask, stride_bytes: u64::from(block.width).checked_mul(2).ok_or("stride")?, sha256: profile_hash(mask) },
        tiles: &items,
        byte_limit: BYTE_LIMIT,
    };
    let address = TileAddress::new(target.layer.clone(), &key, column, row).map_err(|_| "block address".to_string())?;
    let admission = MemoryAdmission::new(BYTE_LIMIT).map_err(|e| format!("admission {e:?}"))?;
    let mut owner = SerializedOwner::new(target.document.clone(), target.layer.clone(), target.actor.clone(), 1, vec![(address, 0)], admission, Quiet).map_err(|e| format!("owner {e:?}"))?;
    match masked_replace(&request, &mut owner, &CancellationToken::default()) {
        Outcome::Accepted(patch) => match patch.updates.first() {
            Some(update) => Ok((update.after.colour.to_vec(), update.after.alpha.to_vec())),
            None => Ok((colour.to_vec(), alpha.to_vec())),
        },
        Outcome::Rejected(failure) => Err(format!("masked_replace refused: {:?}", failure.error)),
        Outcome::ReconciliationRequired { .. } => Err("masked_replace: reconciliation required".into()),
    }
}

/// Paints `rgb` (linear) through `coverage` onto one tile body (`rgb_f32le` cell + straight
/// `f32le` alpha plane, tight strides of the tile width) at document rectangle `rect`, block by
/// block through the engine. `Ok(None)` when the stroke covers nothing in this tile.
pub fn paint_tile(target: &MixTarget<'_>, tile: &TileRef, before: &[u8], rect: Rect, coverage: &CoveragePlane, rgb: [f32; 3]) -> Result<Option<Vec<u8>>, String> {
    let (w, h) = (usize::try_from(rect.width).map_err(|_| "width")?, usize::try_from(rect.height).map_err(|_| "height")?);
    let colour_len = w.checked_mul(h).and_then(|n| n.checked_mul(12)).ok_or("tile size")?;
    let alpha_len = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or("tile size")?;
    if before.len() != colour_len.checked_add(alpha_len).ok_or("tile size")? {
        return Err(format!("tile body is {} bytes, expected {}", before.len(), colour_len.saturating_add(alpha_len)));
    }
    let Some(touched) = rect.intersect(coverage.bounds).map_err(|e| format!("{e:?}"))? else {
        return Ok(None);
    };
    let limits = BrushLimits::default();
    let (colour_stride, alpha_stride) = (w.checked_mul(12).ok_or("stride")?, w.checked_mul(4).ok_or("stride")?);
    let mut out = before.to_vec();
    let mut changed = false;
    let block = i64::from(BLOCK);
    let (ex, ey) = touched.end().map_err(|e| format!("{e:?}"))?;
    let first = |v: i64| v.checked_div_euclid(block).and_then(|b| b.checked_mul(block));
    let mut y = first(touched.y).ok_or("block row")?;
    while y < ey {
        let mut x = first(touched.x).ok_or("block column")?;
        while x < ex {
            let b = Rect { x, y, width: BLOCK, height: BLOCK };
            if let Some(b) = b.intersect(rect).map_err(|e| format!("{e:?}"))? {
                let mask_plane = crop_to(coverage, b, &limits).map_err(|e| e.code().to_string())?;
                if mask_plane.data.iter().any(|v| *v != 0) {
                    let mask = mask_plane.to_mask_bytes(MaskFormat::CoverageU16Le).map_err(|e| format!("{e:?}"))?;
                    let at = (usize::try_from(b.x.checked_sub(rect.x).ok_or("x")?).map_err(|_| "x")?, usize::try_from(b.y.checked_sub(rect.y).ok_or("y")?).map_err(|_| "y")?);
                    let colour = plane(&out[..colour_len], colour_stride, 12, b, at).ok_or("block colour")?;
                    let alpha = plane(&out[colour_len..], alpha_stride, 4, b, at).ok_or("block alpha")?;
                    let (after_c, after_a) = replace_block(target, tile, b, &colour, &alpha, rgb, &mask)?;
                    if after_c != colour || after_a != alpha {
                        let (c_part, a_part) = out.split_at_mut(colour_len);
                        put(c_part, colour_stride, 12, b, at, &after_c).ok_or("block write")?;
                        put(a_part, alpha_stride, 4, b, at, &after_a).ok_or("block write")?;
                        changed = true;
                    }
                }
            }
            x = x.checked_add(block).ok_or("block column")?;
        }
        y = y.checked_add(block).ok_or("block row")?;
    }
    Ok((changed && out != before).then_some(out))
}
