//! PSD export of an edited Folio snapshot. The original [`PsdDocument`] is the base: every field
//! the importer does not own (unsupported tagged blocks, ICC and other resources, unknown bytes,
//! compression of untouched channels, layer order and structure) stays exactly as read, and only
//! values that differ from what the importer produced are written back.
//!
//! Layer properties written back: name, visibility, opacity, fill opacity (`iOpa`), blend mode
//! (record key and, on groups, the `lsct` key), clipping, group pass-through (as the blend key
//! `pass`) and the mask enabled/linked flags. Layer colour and alpha are re-encoded from edited
//! tiles only when the caller supplies tiles whose digests differ from the imported ones. Edits
//! that PSD cannot take (blend members without a PSD key, added or removed layers or masks, mask
//! pixel edits, pixels after an ICC or CMYK/Lab display conversion) are reported as typed losses
//! and left out; display-converted tiles are a projection, so the original channel bytes of CMYK,
//! Lab and gray-profile layers survive export unchanged (tests
//! `cmyk_display_conversion_is_a_clamped_projection_and_keeps_the_original_channels`,
//! `sixteen_bit_cmyk_and_lab_use_the_scale_and_the_pcs_native_path`,
//! `gray_ktrc_profile_converts_and_other_gray_cases_stay_typed`). Any applied edit other than a name
//! makes the stored merged composite stale: `hasRealMergedData` is cleared and `merged_composite_stale`
//! is reported (no renderer here to regenerate it; test
//! `edits_that_change_the_picture_mark_the_stored_composite_stale_and_name_edits_do_not`). Malformed
//! tile blobs and non-finite tile values are typed errors (tests
//! `export_with_malformed_tile_blobs_is_a_typed_error_never_a_panic`,
//! `apply_edits_is_all_or_nothing_and_export_keeps_untiled_cells`).
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{BTreeMap, BTreeSet};

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Payload, Snapshot};

use crate::blend::{StudioBlendMode, psd_key_for};
use crate::document::PsdDocument;
use crate::error::{PsdError, Result};
use crate::header::{ColorMode, Header};
use crate::layers::{KEY_SECTION_DIVIDER, KEY_SECTION_DIVIDER_NESTED, PsdChannel, PsdLayer, TaggedBlock};
use crate::limits::Limits;
use crate::loss::{LossEntry, LossKind, LossReport};
use crate::pixels::Plane;
use crate::proposal::{ALPHA_BYTES, COLOUR_BYTES, ImportProposal, TILE_EDGE, TileBlob, floor_div, imported_look};
use crate::resources::{ImageResources, RESOURCE_VERSION_INFO};
use crate::samples::{
    SixteenBitScale, linear_to_srgb, read_f32_le, row_major, sample_range, size_add, size_mul, unit_to_sample,
};
use crate::writer::write_psd;

pub struct ExportOptions<'a> {
    pub limits: &'a Limits,
    /// Layer colour and alpha tiles to encode back into channels. `None` leaves pixels untouched.
    pub tiles: Option<&'a [TileBlob]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// `(path, field)` for every value written back, for example `("layer[2]", "opacity")`.
    pub applied: Vec<(String, &'static str)>,
    /// Edits that could not be exported.
    pub loss: LossReport,
}

impl ExportReport {
    fn applied(&mut self, path: &str, field: &'static str) {
        self.applied.push((path.to_owned(), field));
    }

    fn lost(&mut self, path: &str, kind: LossKind, code: &'static str) {
        self.loss.entries.push(LossEntry { path: path.to_owned(), kind, code });
    }

    pub fn was_applied(&self, field: &str) -> bool {
        self.applied.iter().any(|(_, f)| *f == field)
    }
}

fn percent_to_byte(percent: f64) -> u8 {
    (percent * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8
}

/// Offset of the flags byte the importer reads for the layer's mask (the real user mask's when
/// both a user and a vector mask exist).
fn mask_flag_offset(layer: &PsdLayer) -> Option<usize> {
    let mask = layer.mask()?;
    if layer.channels.iter().any(|c| c.id == -3) && mask.real_rect.is_some() {
        mask.real_flags_offset
    } else {
        Some(mask.flags_offset)
    }
}

fn mask_digests(snapshot: &Snapshot, layer_id: &str) -> Vec<String> {
    snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.layer_id == layer_id)
        .and_then(|l| l.appearance.as_ref())
        .and_then(|a| a.mask.as_ref())
        .map(|m| m.tiles.iter().map(|t| format!("{}:{}", t.object_key, t.content_digest.digest)).collect())
        .unwrap_or_default()
}

/// After any edit that changes what the file displays, the stored merged composite no longer
/// matches the layers. This crate has no renderer to regenerate it, so resource 1057
/// `hasRealMergedData` is cleared (readers then refuse it as an oracle) and the staleness is a typed
/// loss. The composite bytes themselves stay as read. Name-only edits do not change the picture and
/// do not call this (test `edits_that_change_the_picture_mark_the_stored_composite_stale_and_name_edits_do_not`).
fn mark_merged_stale(resources: &mut ImageResources, report: &mut ExportReport) {
    // Resource 1057 data: u32 version, then the u8 `hasRealMergedData`.
    if let Some(block) = resources.blocks.iter_mut().find(|b| b.id == RESOURCE_VERSION_INFO)
        && block.data.len() > 4
        && block.data[4] != 0
    {
        block.data[4] = 0;
        report.applied("resource[1057]", "has_real_merged_data_cleared");
    }
    report.lost("merged_image", LossKind::Transformed, "merged_composite_stale");
}

/// Applies the edits in `edited` onto `doc` (which must be the document `proposal` was made from).
/// All-or-nothing: the edits land on a working copy that replaces `doc` only when every edit
/// succeeded, so an `Err` leaves `doc` exactly as it was (test
/// `apply_edits_is_all_or_nothing_and_export_keeps_untiled_cells`).
pub fn apply_edits(
    doc: &mut PsdDocument,
    proposal: &ImportProposal,
    edited: &Snapshot,
    opts: &ExportOptions<'_>,
) -> Result<ExportReport> {
    let mut working = doc.clone();
    let report = apply_edits_in_place(&mut working, proposal, edited, opts)?;
    *doc = working;
    Ok(report)
}

/// The edit pass itself; it may leave `doc` half-edited when it fails, so callers hand it a copy.
fn apply_edits_in_place(
    doc: &mut PsdDocument,
    proposal: &ImportProposal,
    edited: &Snapshot,
    opts: &ExportOptions<'_>,
) -> Result<ExportReport> {
    let mut report = ExportReport::default();
    let header = doc.header;
    let edited_doc = edited.document();
    let Some(info) = doc.layer_section.layer_info.as_mut() else {
        return Ok(report);
    };
    for l in &edited_doc.layers {
        if !proposal.layer_map.iter().any(|e| e.layer_id == l.layer_id) {
            report.lost("layers", LossKind::Unsupported, "layer_added_in_edit_not_exported");
        }
    }
    for entry in &proposal.layer_map {
        let index = entry.psd_index;
        let path = format!("layer[{index}]");
        let Some(folio) = edited_doc.layers.iter().find(|l| l.layer_id == entry.layer_id) else {
            report.lost(&path, LossKind::Unsupported, "layer_removed_in_edit_not_exported");
            continue;
        };
        let Some(layer) = info.layers.get_mut(index) else {
            continue;
        };
        if layer.name != folio.name {
            layer.name.clone_from(&folio.name);
            report.applied(&path, "name");
            // The writer stores at most `max_name_units` UTF-16 units of the name.
            if folio.name.encode_utf16().count() > opts.limits.max_name_units {
                report.lost(&format!("{path}/name"), LossKind::Transformed, "layer_name_truncated");
            }
        }
        let hidden = !folio.visible;
        if layer.hidden() != hidden {
            layer.flags = if hidden { layer.flags | 0x02 } else { layer.flags & !0x02 };
            report.applied(&path, "visible");
        }
        if matches!(folio.payload, Payload::Unsupported { .. }) {
            continue;
        }
        let is_group = folio.kind == "group" && matches!(folio.payload, Payload::Group);
        let mut before = imported_look(layer, is_group);
        // The importer unclips a layer clipped to an adjustment layer (`clip_to_adjustment_not_supported`)
        // while the file keeps its clipping byte: compare against the value it wrote to Folio, so an
        // unedited export leaves that byte alone (test
        // `clipped_to_adjustment_layer_is_unclipped_with_a_typed_loss_and_exports_unchanged`).
        if let Some(imported) = proposal.snapshot.document().layers.iter().find(|l| l.layer_id == entry.layer_id) {
            before.clipped = imported.effective_appearance().clipped;
        }
        let look = folio.effective_appearance();
        if look.opacity != before.opacity {
            layer.opacity = percent_to_byte(look.opacity);
            report.applied(&path, "opacity");
        }
        if look.fill_opacity != before.fill_opacity {
            let byte = percent_to_byte(look.fill_opacity);
            match layer.blocks.iter_mut().find(|b| &b.key == b"iOpa") {
                Some(block) if !block.data.is_empty() => block.data[0] = byte,
                Some(block) => block.data = vec![byte, 0, 0, 0],
                None => layer.blocks.push(TaggedBlock {
                    signature: *b"8BIM",
                    key: *b"iOpa",
                    data: vec![byte, 0, 0, 0],
                    pad: Vec::new(),
                }),
            }
            report.applied(&path, "fill_opacity");
        }
        if look.blend_mode != before.blend {
            match StudioBlendMode::try_from(look.blend_mode).ok().and_then(psd_key_for) {
                Some(key) => {
                    layer.blend_key = key;
                    if is_group {
                        for block in &mut layer.blocks {
                            let divider = block.key == KEY_SECTION_DIVIDER || block.key == KEY_SECTION_DIVIDER_NESTED;
                            if divider && block.data.len() >= 12 && &block.data[4..8] == b"8BIM" {
                                block.data[8..12].copy_from_slice(&key);
                            }
                        }
                    }
                    report.applied(&path, "blend_mode");
                }
                None => report.lost(&format!("{path}/blend"), LossKind::Unsupported, "blend_mode_not_representable_in_psd"),
            }
        }
        if look.clipped != before.clipped {
            layer.clipping = u8::from(look.clipped);
            report.applied(&path, "clipped");
        }
        match (mask_flag_offset(layer), &look.mask) {
            (Some(at), Some(mask)) => {
                let old = layer.mask_raw[at];
                let new = (old & !0x03) | if mask.enabled { 0 } else { 0x02 } | u8::from(mask.linked);
                if new != old {
                    layer.mask_raw[at] = new;
                    report.applied(&path, "mask_flags");
                }
                if mask_digests(edited, &entry.layer_id) != mask_digests(&proposal.snapshot, &entry.layer_id) {
                    report.lost(&format!("{path}/mask"), LossKind::Unsupported, "mask_pixel_edit_not_exported");
                }
            }
            (Some(_), None) => report.lost(&format!("{path}/mask"), LossKind::Unsupported, "mask_removed_in_edit_not_exported"),
            (None, Some(_)) => report.lost(&format!("{path}/mask"), LossKind::Unsupported, "mask_added_in_edit_not_exported"),
            (None, None) => {}
        }
        if let Some(tiles) = opts.tiles
            && matches!(folio.payload, Payload::Raster { .. })
        {
            let edited_tiles: Vec<&TileBlob> = tiles.iter().filter(|t| t.layer_id == entry.layer_id).collect();
            // Compare the pixel bytes themselves: a caller editing a blob does not refresh its digests.
            let bytes_of = |set: &[&TileBlob]| -> BTreeMap<(i64, i64), (Vec<u8>, Vec<u8>)> {
                set.iter().map(|t| ((t.column, t.row), (t.colour.clone(), t.alpha.clone()))).collect()
            };
            let original: Vec<&TileBlob> = proposal.tiles.iter().filter(|t| t.layer_id == entry.layer_id).collect();
            if bytes_of(&edited_tiles) != bytes_of(&original) {
                if proposal.provenance.icc_converted {
                    report.lost(&format!("{path}/pixels"), LossKind::Unsupported, "pixel_export_after_icc_conversion_unsupported");
                } else if proposal.provenance.display.is_some() {
                    // CMYK/Lab tiles are a display projection: the original channels stay as read.
                    report.lost(&format!("{path}/pixels"), LossKind::Unsupported, "pixel_export_after_display_conversion_unsupported");
                } else {
                    let imported: BTreeSet<(i64, i64)> = original.iter().map(|t| (t.column, t.row)).collect();
                    let pixels = PixelExport {
                        header: &header,
                        imported: &imported,
                        scale: proposal.provenance.sixteen_bit_scale,
                        limits: opts.limits,
                    };
                    encode_layer_pixels(layer, &edited_tiles, &pixels, &path, &mut report)?;
                    report.applied(&path, "pixels");
                }
            }
        }
    }
    if report.applied.iter().any(|(_, field)| *field != "name") {
        mark_merged_stale(&mut doc.resources, &mut report);
    }
    Ok(report)
}

/// Exports `doc` with the edits in `edited`: a clone of the original with only changed values
/// written, serialized by [`write_psd`].
pub fn export_psd(
    doc: &PsdDocument,
    proposal: &ImportProposal,
    edited: &Snapshot,
    opts: &ExportOptions<'_>,
) -> Result<(Vec<u8>, ExportReport)> {
    let mut working = doc.clone();
    let report = apply_edits_in_place(&mut working, proposal, edited, opts)?;
    Ok((write_psd(&working, opts.limits)?, report))
}

fn put_sample(plane: &mut [u8], index: usize, raw: u32, depth: u16) -> Result<()> {
    let dst = plane.get_mut(sample_range(index, usize::from(depth / 8))?).ok_or(PsdError::Overflow)?;
    match depth {
        8 => dst.copy_from_slice(&[raw as u8]),
        16 => dst.copy_from_slice(&(raw as u16).to_be_bytes()),
        _ => dst.copy_from_slice(&raw.to_be_bytes()),
    }
    Ok(())
}

/// What [`encode_layer_pixels`] needs besides the layer and the edited tiles.
struct PixelExport<'a> {
    header: &'a Header,
    /// Cells that had a tile at import. A cell missing from the edited tiles that is listed here
    /// was erased by the caller; a cell listed in neither never had a tile (alpha 0 throughout).
    imported: &'a BTreeSet<(i64, i64)>,
    scale: SixteenBitScale,
    limits: &'a Limits,
}

fn copy_sample(dst: &mut [u8], src: Option<&Plane>, index: usize, per: usize) -> Result<()> {
    let range = sample_range(index, per)?;
    if let Some(from) = src.and_then(|p| p.bytes.get(range.clone())) {
        dst.get_mut(range).ok_or(PsdError::Overflow)?.copy_from_slice(from);
    }
    Ok(())
}

/// Re-encodes the layer's colour (and alpha) channels from linear-light tiles over the layer
/// rectangle: sRGB encoding for 8/16-bit, floats as stored for 32-bit, raw (uncompressed) channel
/// data. A cell the caller removed from the tile set is transparent black; a cell that never had a
/// tile keeps its original samples (colour under alpha 0 survives). A non-finite tile value is a
/// typed error, and an alpha outside `0..=1` is clamped with a typed loss.
fn encode_layer_pixels(
    layer: &mut PsdLayer,
    tiles: &[&TileBlob],
    ctx: &PixelExport<'_>,
    path: &str,
    report: &mut ExportReport,
) -> Result<()> {
    let (header, scale) = (ctx.header, ctx.scale);
    let depth = header.depth;
    if !matches!(depth, 8 | 16 | 32) || !matches!(header.color_mode, ColorMode::Rgb | ColorMode::Grayscale) {
        return Err(PsdError::UnsupportedMode("pixel_export_mode"));
    }
    // Tile blobs are caller data (public fields): a blob that is not exactly one cell is a typed
    // error here, never an out-of-bounds index below.
    if tiles.iter().any(|t| t.colour.len() != COLOUR_BYTES || t.alpha.len() != ALPHA_BYTES) {
        return Err(PsdError::Encode("tile_blob_size"));
    }
    let rect = layer.rect;
    let (width, height) = (rect.width() as usize, rect.height() as usize);
    let per = usize::from(depth / 8);
    let plane_len = size_mul(size_mul(width, height)?, per)?;
    let colour_ids: &[i16] = if header.color_mode == ColorMode::Rgb { &[0, 1, 2] } else { &[0] };
    let by_cell: BTreeMap<(i64, i64), &TileBlob> = tiles.iter().map(|t| ((t.column, t.row), *t)).collect();
    let edge = i64::from(TILE_EDGE);
    // Cells that never had a tile keep the file's own samples; decode them once, only if one exists.
    let mut needs_original = false;
    if width > 0 && height > 0 {
        let (left, top) = (i64::from(rect.left), i64::from(rect.top));
        let (right, bottom) = (i64::from(rect.right), i64::from(rect.bottom));
        'scan: for row in floor_div(top, edge)..=floor_div(bottom.saturating_sub(1), edge) {
            for column in floor_div(left, edge)..=floor_div(right.saturating_sub(1), edge) {
                if !by_cell.contains_key(&(column, row)) && !ctx.imported.contains(&(column, row)) {
                    needs_original = true;
                    break 'scan;
                }
            }
        }
    }
    let originals: Vec<Option<Plane>> = if needs_original {
        let cancel = CancellationToken::default();
        let mut planes = Vec::with_capacity(size_add(colour_ids.len(), 1)?);
        for &id in colour_ids.iter().chain(&[-1i16]) {
            planes.push(layer.decode_channel(id, header, ctx.limits, &cancel)?);
        }
        planes
    } else {
        Vec::new()
    };
    let mut planes = vec![vec![0u8; plane_len]; colour_ids.len()];
    let mut alpha_plane = vec![0u8; plane_len];
    let mut translucent = false;
    let mut alpha_clamped = false;
    for y in 0..height {
        for x in 0..width {
            let px = i64::from(rect.left).saturating_add(x as i64);
            let py = i64::from(rect.top).saturating_add(y as i64);
            let key = (floor_div(px, edge), floor_div(py, edge));
            let index = row_major(y as i64, x as i64, width)?;
            let (rgb, alpha) = match by_cell.get(&key) {
                Some(tile) => {
                    let at = row_major(py.rem_euclid(edge), px.rem_euclid(edge), TILE_EDGE as usize)?;
                    let base = size_mul(at, 3)?;
                    let mut rgb = [0.0f32; 3];
                    for (k, slot) in rgb.iter_mut().enumerate() {
                        *slot = read_f32_le(&tile.colour, size_add(base, k)?)?;
                    }
                    (rgb, read_f32_le(&tile.alpha, at)?)
                }
                None if !ctx.imported.contains(&key) => {
                    for (k, plane) in planes.iter_mut().enumerate() {
                        copy_sample(plane, originals.get(k).and_then(Option::as_ref), index, per)?;
                    }
                    copy_sample(&mut alpha_plane, originals.get(colour_ids.len()).and_then(Option::as_ref), index, per)?;
                    translucent = true;
                    continue;
                }
                None => ([0.0; 3], 0.0),
            };
            if !(rgb.iter().all(|v| v.is_finite()) && alpha.is_finite()) {
                return Err(PsdError::Encode("non_finite_sample"));
            }
            for (k, plane) in planes.iter_mut().enumerate() {
                let raw = if depth == 32 { rgb[k].to_bits() } else { unit_to_sample(linear_to_srgb(rgb[k]), depth, scale) };
                put_sample(plane, index, raw, depth)?;
            }
            translucent |= alpha != 1.0;
            alpha_clamped |= !(0.0..=1.0).contains(&alpha);
            put_sample(&mut alpha_plane, index, unit_to_sample(alpha.clamp(0.0, 1.0), depth, scale), depth)?;
        }
    }
    if alpha_clamped {
        report.lost(&format!("{path}/pixels"), LossKind::Transformed, "alpha_range_clamped");
    }
    for (&id, data) in colour_ids.iter().zip(planes) {
        match layer.channels.iter_mut().find(|c| c.id == id) {
            Some(channel) => {
                channel.compression = 0;
                channel.data = data;
            }
            None => layer.channels.push(PsdChannel { id, compression: 0, data }),
        }
    }
    match layer.channels.iter_mut().find(|c| c.id == -1) {
        Some(channel) => {
            channel.compression = 0;
            channel.data = alpha_plane;
        }
        None if translucent => layer.channels.push(PsdChannel { id: -1, compression: 0, data: alpha_plane }),
        None => {}
    }
    Ok(())
}
