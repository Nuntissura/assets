//! Plain TIFF -> native (SMB-CONVERT-TIFF; feature `import-tiff`). `hsk-studio-interop-tiff` decodes
//! the container (bounded pre-scan, typed refusals passed through unchanged), the first page goes
//! through interop-psd's `raster_proposal` as one raster layer (the single owner of the raster ->
//! layer mapping, colour route, tiling and digests), and the shared tail writes tile assets,
//! records, the import receipt and the original bytes.
//!
//! What is not imported is a typed loss entry, never dropped silently (test
//! `tiff_corpus_files_import_with_typed_losses_or_pass_through_refusals`):
//! - pages after the first: `page[n]` `tiff_pages_not_imported` (held in the original entry);
//! - Photoshop layers in tag 37724 (ImageSourceData; SMB-CONVERT-FOLLOWUPS slice 3): the records are
//!   parsed by interop-psd `image_source_document` (its own PSD layer readers) over this page's
//!   canvas and mapped by `to_proposal` like a PSD (groups, masks, blend modes and their typed
//!   losses); the composite page is then `page[0]` `tiff_composite_not_imported` (kept in the
//!   original). When the envelope, the canvas or the records cannot be used (e.g. little-endian
//!   `MIB8` runs), the composite is imported and `tiff_photoshop_layers_not_imported` is recorded,
//!   with the parse error in the receipt (tests `photoshop_tiff_imports_its_layers_through_interop_psd`,
//!   `photoshop_tiff_imports_its_composite_exactly_and_records_the_layers_as_typed_loss`);
//! - tag 34377 (Photoshop image resources): `tiff_image_resources_not_imported`;
//! - orientation other than 1: `tiff_orientation_not_applied` (interop-tiff records, never applies);
//! - extra samples beyond the image's own alpha are Folio channel records (lane SMB-TIFF-CHANNELS-1,
//!   R52 F-R52-2; STU-RAS-022, STU-RAS-043, STU-IO-006 row 3): one record per extra sample at the
//!   page depth (8 `coverage_u8`, 16 `coverage_u16le`, 32 float `coverage_f32le`), samples copied
//!   exactly as the tone into 256-pixel cells stored as package assets (as PSD channels are). TIFF
//!   carries no channel name, kind, display colour or opacity, so each record takes the import
//!   defaults (`Alpha <n>`, masked-area alpha, red, 50 percent, hidden) with the typed loss
//!   `channel[n]` `tiff_channel_display_defaulted`; whether Photoshop writes its alpha channels as
//!   unspecified extra samples is UNVERIFIED (D5). Extra samples past Folio's channel limit, or of a
//!   sample type without a channel format, are `page[0]/extra_sample[k]`
//!   `tiff_extra_samples_not_imported`; channel cells over the decode budget refuse the import
//!   (`tiff_channel_tiles_over_budget`) before any cell is allocated (tests
//!   `tiff_alpha_channel_and_saved_selection_round_trip_as_channel_records`,
//!   `tiff_channel_mapping_is_bounded_and_export_refuses_what_it_cannot_write`);
//! - associated (premultiplied) alpha is un-premultiplied for the straight-alpha layer:
//!   `tiff_associated_alpha_unpremultiplied`;
//! - CIELab (PI 8, signed a/b) is re-encoded to the ICC Lab offset encoding the Lab route reads:
//!   `tiff_cielab_reencoded_offset`.
//!
//! u32 and f64 samples have no raster mapping (depth 8/16 integer and 32 float only): typed refusal
//! `tiff_sample_type_unmapped`.
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::{SourceMint, package_proposal_with};
use super::{ImportError, ImportedDocument, SourceFormat};
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_folio::{self as folio, Resolver};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_tiff as tiff;
use hsk_studio_prism::{hash_hex, profile_hash};
use psd::IdMint;
use serde_json::{Value, json};

const FORMAT: SourceFormat = SourceFormat::Tiff;

fn importer(code: &'static str) -> ImportError {
    ImportError::Importer { format: FORMAT, code }
}

/// Imports the first page of a plain TIFF. `camera_raw_check` names who decided this is not a
/// camera raw: `develop_unsupported_container` (develop refused and [`camera_raw_marker`] found no
/// marker) or `container_markers_absent` (no `import-raw`; [`camera_raw_marker`] found no marker).
pub(crate) fn import_tiff(
    bytes: &[u8],
    resolver: &dyn Resolver,
    actor: &ActorContext,
    cancel: &CancellationToken,
    camera_raw_check: &'static str,
) -> Result<ImportedDocument, ImportError> {
    let file = tiff::decode(bytes, &tiff::Limits::default(), cancel).map_err(|e| match e {
        tiff::TiffError::Canceled => ImportError::Canceled,
        other => importer(other.code()),
    })?;
    let page = file.pages.first().ok_or_else(|| importer("tiff_no_pages"))?;
    let mut losses: Vec<(String, psd::LossKind, &'static str)> = Vec::new();
    for index in 1..file.pages.len() {
        losses.push((format!("page[{index}]"), psd::LossKind::PreservedOpaque, "tiff_pages_not_imported"));
    }
    if page.image_resources.is_some() {
        losses.push(("page[0]/tag/34377".into(), psd::LossKind::PreservedOpaque, "tiff_image_resources_not_imported"));
    }
    if page.orientation.is_some_and(|o| o != 1) {
        losses.push(("page[0]/tag/274".into(), psd::LossKind::Unsupported, "tiff_orientation_not_applied"));
    }

    let source = psd::SourceIdentity::of(bytes);
    let mut mint = SourceMint::new(&source.sha256);
    let mut frames = super::colour::FrameCount::default();
    let mut converter = super::colour::converter(FORMAT, &mint, &mut frames, actor)?;
    let limits = psd::Limits::default();
    let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), resolver);
    let psd_error = |e: psd::PsdError| if e == psd::PsdError::Canceled { ImportError::Canceled } else { importer(e.code()) };
    let (width, height) = (page.width, page.height);
    // Channel cells are counted before the layer tiles or any channel cell are allocated (V4-02).
    channel_cell_bytes(page, limits.max_decoded_bytes)?;

    // Photoshop layers (tag 37724): parsed by interop-psd's own layer readers and mapped like a PSD.
    // Any parse or mapping failure falls back to the composite with `tiff_photoshop_layers_not_imported`.
    let mut layered = None;
    let mut image_source = None;
    if let Some(block) = page.image_source_data.as_ref() {
        match layered_document(block, page, &limits, cancel)? {
            Ok((doc, mut facts)) => match psd::to_proposal(&doc, &source, &cfg, &mut mint, Some(&mut converter), cancel) {
                Ok(proposal) => {
                    facts["layers"] = json!("imported");
                    image_source = Some(facts);
                    layered = Some(proposal);
                }
                Err(psd::PsdError::Canceled) => return Err(ImportError::Canceled),
                Err(e) => {
                    facts["parse_error"] = json!(e.to_string());
                    image_source = Some(facts);
                }
            },
            Err(facts) => image_source = Some(facts),
        }
    }
    let mut proposal = match layered {
        Some(mut proposal) => {
            // The header-only document has no PSD image-data section; the TIFF page is the composite.
            proposal.loss.entries.retain(|e| !matches!(e.code, "merged_image_oracle_unknown" | "image_data_missing"));
            losses.push(("page[0]".into(), psd::LossKind::PreservedOpaque, "tiff_composite_not_imported"));
            proposal
        }
        None => {
            if image_source.is_some() {
                losses.push(("page[0]/tag/37724".into(), psd::LossKind::PreservedOpaque, "tiff_photoshop_layers_not_imported"));
            }
            let raster = raster_of(page)?;
            losses.splice(0..0, raster.losses);
            let raster_source = psd::RasterSource {
                format: "tiff",
                name: "Background",
                width,
                height,
                depth: raster.depth,
                colour_mode: raster.mode,
                planes: raster.planes,
                alpha: raster.alpha,
                icc: page.icc.clone(),
            };
            psd::raster_proposal(raster_source, &source, &cfg, &mut mint, Some(&mut converter), cancel).map_err(psd_error)?
        }
    };
    drop(converter);
    // Extra samples beyond the image's alpha: channel records in the same tile profile as the layers.
    let profile_id = proposal
        .snapshot
        .document()
        .layers
        .iter()
        .find_map(|l| match &l.payload {
            folio::Payload::Raster { tiles } => tiles.first().map(|t| t.colour_profile_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| mint.linear_working_profile_id());
    let layer_bytes = proposal
        .tiles
        .iter()
        .map(|t| t.colour.len().saturating_add(t.alpha.len()))
        .chain(proposal.mask_tiles.iter().map(|m| m.coverage.len()))
        .fold(0usize, usize::saturating_add);
    let budget = limits.max_decoded_bytes.saturating_sub(layer_bytes);
    let channels = channel_records(page, &profile_id, &mut mint, budget, cancel)?;
    proposal.raster_records.channels.extend(channels.records);
    proposal.loss.entries.extend(channels.loss);
    let colour = super::colour::summary(&proposal, frames.0, "embedded TIFF profile (tag 34675) -> working space");
    for (path, kind, code) in losses {
        proposal.loss.entries.push(psd::LossEntry { path, kind, code });
    }
    let resolution = page.resolution.map(|r| json!({"x": [r.x.0, r.x.1], "y": [r.y.0, r.y.1], "unit": r.unit}));
    let extra = json!({
        "colour": colour,
        "tiff": {
            "byte_order": order(file.byte_order), "big_tiff": file.big_tiff, "pages": file.pages.len(),
            "imported_page": 0, "camera_raw_check": camera_raw_check,
            "page": {
                "width": width, "height": height, "source_photometric": page.source_photometric,
                "bits_per_sample": page.bits_per_sample, "source_bits_per_sample": page.source_bits_per_sample,
                "sample_format": format!("{:?}", page.sample_format), "samples_per_pixel": page.samples_per_pixel,
                "planar_in_file": page.planar_in_file, "alpha": format!("{:?}", page.alpha),
                "extra_samples": page.extra_samples, "compression": page.compression, "predictor": page.predictor,
                "orientation": page.orientation, "resolution": resolution,
                "icc_bytes": page.icc.as_ref().map(Vec::len),
            },
            "image_source_data": image_source,
            "channels": channels.facts,
        },
    });
    package_proposal_with(FORMAT, "hsk-studio-interop-tiff + hsk-studio-interop-psd", bytes, proposal, extra, channels.assets)
}

/// What [`export_tiff_with_channels`] wrote, and the typed losses of the channel record fields TIFF
/// cannot carry.
#[derive(Clone, Debug, PartialEq)]
pub struct TiffChannelExport {
    pub bytes: Vec<u8>,
    pub loss: Vec<psd::LossEntry>,
}

/// Host-callable TIFF export of a document's channel records as extra sample planes (lane
/// SMB-TIFF-CHANNELS-1; R52 F-R52-2, STU-RAS-022, STU-RAS-043, STU-IO-006 row 3). No host path writes a
/// document to TIFF yet (the only TIFF writer in the workspace is develop's raw-develop export, which
/// has no channel records), so the host passes the image it writes as `base` (colour samples and the
/// image's own alpha, interop-tiff's request) and the records it keeps; `tile_bytes` serves a tile's
/// bytes by its content digest (size and digest are checked).
///
/// Each record whose extent and depth match the image (8 `U8`, 16 `U16`, 32 `F32`) becomes one
/// unspecified (ExtraSamples 0) plane after the image's samples, in record order, with its tone
/// samples exactly; the TIFF import reads it back as a channel record with identical tiles (test
/// `tiff_alpha_channel_and_saved_selection_round_trip_as_channel_records`). TIFF has no slot for the
/// rest of a record, so every written channel `channel[i]` records the typed losses
/// `/name` `tiff_channel_name_not_exported`, `/kind` `tiff_channel_kind_not_exported` (Transformed:
/// it re-imports as masked-area alpha), `/colour` `tiff_channel_colour_not_exported`, `/opacity`
/// `tiff_channel_opacity_not_exported` (a spot channel's solidity: `tiff_channel_solidity_not_exported`)
/// and, for a visible channel, `/visible` `tiff_channel_visibility_not_exported`. Records TIFF cannot
/// take are left out with a typed loss: another extent (`tiff_channel_extent_not_exported`), another
/// depth (`tiff_channel_depth_not_exported`), and records past the 64 samples per pixel interop-tiff
/// writes (`tiff_channel_count_not_exported`). Typed errors, nothing written: a tile the host cannot
/// serve (`channel_tile_missing`), of another size (`channel_tile_size`), digest
/// (`channel_tile_digest`) or format (`channel_tile_format`), a tile edge that is not a whole pixel
/// count in 1..=65536 (`channel_tile_edge`), and the encode byte budget over all planes, checked
/// before each plane is allocated (`limit_encode_bytes`); interop-tiff's own refusals pass through
/// (test `tiff_channel_mapping_is_bounded_and_export_refuses_what_it_cannot_write`). The terminal
/// outcome goes through `hsk_studio_interop_tiff::report` over this result (codes only).
pub fn export_tiff_with_channels(
    base: &tiff::EncodeRequest<'_>,
    channels: &[folio::StudioChannelRecord],
    tile_bytes: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    opts: &tiff::EncodeOptions,
) -> tiff::Result<TiffChannelExport> {
    let mut loss = Vec::new();
    let mut lost = |path: String, kind: psd::LossKind, code: &'static str| loss.push(psd::LossEntry { path, kind, code });
    let base_spp = base.color.samples_per_pixel();
    let room = usize::from(tiff::MAX_ENCODE_SAMPLES_PER_PIXEL.saturating_sub(base_spp));
    let depth: Option<u8> = match base.samples {
        tiff::Samples::U8(_) => Some(8),
        tiff::Samples::U16(_) => Some(16),
        tiff::Samples::F32(_) => Some(32),
        tiff::Samples::U32(_) | tiff::Samples::F64(_) => None,
    };
    let pixels = u64::from(base.width).checked_mul(u64::from(base.height)).ok_or(tiff::TiffError::DimsOverflow)?;
    let mut planes: Vec<tiff::ExtraPlane> = Vec::new();
    for (i, c) in channels.iter().enumerate() {
        let path = format!("channel[{i}]");
        if planes.len() >= room {
            lost(path, psd::LossKind::Unsupported, "tiff_channel_count_not_exported");
            continue;
        }
        if (c.width, c.height) != (base.width, base.height) {
            lost(path, psd::LossKind::Unsupported, "tiff_channel_extent_not_exported");
            continue;
        }
        let per = match (depth, folio::channel_sample_bytes(c.depth)) {
            (Some(d), Some(per)) if d == c.depth => per,
            _ => {
                lost(path, psd::LossKind::Unsupported, "tiff_channel_depth_not_exported");
                continue;
            }
        };
        // V4-02: the planes with this one stay inside the encode budget before it is allocated.
        let samples = u64::from(base_spp)
            .checked_add(u64::try_from(planes.len()).map_err(|_| tiff::TiffError::DimsOverflow)?)
            .and_then(|s| s.checked_add(1))
            .and_then(|s| s.checked_mul(pixels))
            .and_then(|s| s.checked_mul(u64::try_from(per).ok()?))
            .ok_or(tiff::TiffError::DimsOverflow)?;
        if samples > tiff::MAX_ENCODE_BYTES {
            return Err(tiff::TiffError::Limit("limit_encode_bytes"));
        }
        planes.push(tiff::ExtraPlane { extra_sample: 0, samples: channel_samples(c, per, tile_bytes)? });
        lost(format!("{path}/name"), psd::LossKind::Unsupported, "tiff_channel_name_not_exported");
        lost(format!("{path}/kind"), psd::LossKind::Transformed, "tiff_channel_kind_not_exported");
        lost(format!("{path}/colour"), psd::LossKind::Unsupported, "tiff_channel_colour_not_exported");
        let opacity = if c.kind == folio::StudioChannelKind::SpotColour { "tiff_channel_solidity_not_exported" } else { "tiff_channel_opacity_not_exported" };
        lost(format!("{path}/opacity"), psd::LossKind::Unsupported, opacity);
        if c.visible {
            lost(format!("{path}/visible"), psd::LossKind::Unsupported, "tiff_channel_visibility_not_exported");
        }
    }
    let bytes = tiff::encode_with_planes(base, &planes, opts)?;
    Ok(TiffChannelExport { bytes, loss })
}

/// Largest channel tile edge the export reads (pixels).
const MAX_CHANNEL_TILE_EDGE: f64 = 65_536.0;

/// The record's tone samples as one `width x height` plane, assembled from its tiles (little-endian
/// tile samples; cells beyond the extent are padding).
fn channel_samples(c: &folio::StudioChannelRecord, per: usize, tile_bytes: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> tiff::Result<tiff::Samples> {
    let usage = tiff::TiffError::Usage;
    let format = folio::channel_tile_format(c.depth).ok_or(usage("channel_tile_format"))?;
    let (width, height) = (usize::try_from(c.width).map_err(|_| tiff::TiffError::DimsOverflow)?, usize::try_from(c.height).map_err(|_| tiff::TiffError::DimsOverflow)?);
    let row_bytes = width.checked_mul(per).ok_or(tiff::TiffError::DimsOverflow)?;
    let mut plane = vec![0u8; row_bytes.checked_mul(height).ok_or(tiff::TiffError::DimsOverflow)?];
    let edge = |v: f64| -> tiff::Result<usize> {
        if v.is_finite() && v.fract() == 0.0 && (1.0..=MAX_CHANNEL_TILE_EDGE).contains(&v) {
            Ok(v as usize)
        } else {
            Err(usage("channel_tile_edge"))
        }
    };
    for t in &c.tiles {
        if t.format != format {
            return Err(usage("channel_tile_format"));
        }
        let (tw, th) = (edge(t.width.value)?, edge(t.height.value)?);
        let bytes = tile_bytes(&t.content_digest.digest).ok_or(usage("channel_tile_missing"))?;
        let tile_row = tw.checked_mul(per).ok_or(tiff::TiffError::DimsOverflow)?;
        if Some(bytes.len()) != tile_row.checked_mul(th) {
            return Err(usage("channel_tile_size"));
        }
        if hash_hex(&profile_hash(&bytes)) != t.content_digest.digest {
            return Err(usage("channel_tile_digest"));
        }
        let column = usize::try_from(t.column).map_err(|_| usage("channel_tile_grid"))?;
        let row = usize::try_from(t.row).map_err(|_| usage("channel_tile_grid"))?;
        let x0 = column.checked_mul(tw).ok_or(tiff::TiffError::DimsOverflow)?;
        let y0 = row.checked_mul(th).ok_or(tiff::TiffError::DimsOverflow)?;
        if x0 >= width || y0 >= height {
            continue;
        }
        let x_end = width.min(x0.saturating_add(tw));
        let span = x_end.saturating_sub(x0).saturating_mul(per);
        for y in y0..height.min(y0.saturating_add(th)) {
            let dst_at = y.saturating_mul(row_bytes).saturating_add(x0.saturating_mul(per));
            let src_at = y.saturating_sub(y0).saturating_mul(tile_row);
            let src = bytes.get(src_at..src_at.saturating_add(span)).ok_or(usage("channel_tile_size"))?;
            plane.get_mut(dst_at..dst_at.saturating_add(span)).ok_or(tiff::TiffError::DimsOverflow)?.copy_from_slice(src);
        }
    }
    Ok(match per {
        1 => tiff::Samples::U8(plane),
        2 => tiff::Samples::U16(plane.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect()),
        _ => tiff::Samples::F32(plane.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()),
    })
}

/// Channel records made from a page's extra samples, with their tile bytes, typed losses and
/// receipt facts.
#[derive(Default)]
struct TiffChannels {
    records: Vec<folio::StudioChannelRecord>,
    assets: Vec<Vec<u8>>,
    loss: Vec<psd::LossEntry>,
    facts: Vec<Value>,
}

/// Channel tile layout of a page's samples: (bytes per sample, tile format, record depth).
fn channel_layout(samples: &tiff::Samples) -> Option<(usize, &'static str, u8)> {
    let depth = match samples {
        tiff::Samples::U8(_) => 8,
        tiff::Samples::U16(_) => 16,
        tiff::Samples::F32(_) => 32,
        tiff::Samples::U32(_) | tiff::Samples::F64(_) => return None,
    };
    Some((folio::channel_sample_bytes(depth)?, folio::channel_tile_format(depth)?, depth))
}

/// Extra samples of `page` beyond its own alpha: (first index among the extra samples, count).
fn channel_extras(page: &tiff::TiffPage) -> (usize, usize) {
    let skip = usize::from(page.alpha != tiff::Alpha::None);
    let extras = usize::from(page.samples_per_pixel.saturating_sub(page.colour_samples_per_pixel()));
    (skip, extras)
}

/// Bytes the channel cells of `page` take (256-pixel cells, Folio's channel limit), refused
/// `tiff_channel_tiles_over_budget` above `budget` before anything is allocated.
fn channel_cell_bytes(page: &tiff::TiffPage, budget: usize) -> Result<usize, ImportError> {
    let (skip, extras) = channel_extras(page);
    let Some((per, _, _)) = channel_layout(page.pixels()) else {
        return Ok(0);
    };
    let count = extras.saturating_sub(skip).min(folio::MAX_CHANNELS);
    let edge = psd::TILE_EDGE;
    let cells = usize::try_from(u64::from(page.width.div_ceil(edge)).saturating_mul(u64::from(page.height.div_ceil(edge)))).unwrap_or(usize::MAX);
    let cell = usize::try_from(edge).unwrap_or(usize::MAX).saturating_pow(2).saturating_mul(per);
    let total = cells.saturating_mul(cell).saturating_mul(count);
    if total > budget {
        return Err(importer("tiff_channel_tiles_over_budget"));
    }
    Ok(total)
}

/// One Folio channel record per extra sample of `page` beyond its own alpha (see the module notes).
fn channel_records(
    page: &tiff::TiffPage,
    profile_id: &str,
    mint: &mut SourceMint,
    budget: usize,
    cancel: &CancellationToken,
) -> Result<TiffChannels, ImportError> {
    let mut out = TiffChannels::default();
    let (skip, extras) = channel_extras(page);
    if extras <= skip {
        return Ok(out);
    }
    let not_imported = |out: &mut TiffChannels, k: usize| {
        out.loss.push(psd::LossEntry { path: format!("page[0]/extra_sample[{k}]"), kind: psd::LossKind::PreservedOpaque, code: "tiff_extra_samples_not_imported" });
    };
    let Some((per, format, depth)) = channel_layout(page.pixels()) else {
        for k in skip..extras {
            not_imported(&mut out, k);
        }
        return Ok(out);
    };
    channel_cell_bytes(page, budget)?;
    let edge = psd::TILE_EDGE;
    let edge_px = usize::try_from(edge).map_err(|_| importer("dims_overflow"))?;
    let (width, height) = (usize::try_from(page.width).map_err(|_| importer("dims_overflow"))?, usize::try_from(page.height).map_err(|_| importer("dims_overflow"))?);
    let row_bytes = width.checked_mul(per).ok_or_else(|| importer("dims_overflow"))?;
    let cell_row = edge_px.saturating_mul(per);
    let colour = usize::from(page.colour_samples_per_pixel());
    for (k, plane) in page.extra_planes().into_iter().enumerate().skip(skip) {
        cancel.check().map_err(|_| ImportError::Canceled)?;
        let n = out.records.len();
        if n >= folio::MAX_CHANNELS {
            not_imported(&mut out, k);
            continue;
        }
        let tone: Vec<u8> = match &plane.samples {
            tiff::Samples::U8(v) => v.clone(),
            tiff::Samples::U16(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            tiff::Samples::F32(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            tiff::Samples::U32(_) | tiff::Samples::F64(_) => return Err(importer("tiff_sample_type_unmapped")),
        };
        let sample = colour.saturating_add(k);
        let channel_id = format!("tiff-channel-{sample}");
        let mut tiles = Vec::new();
        for row in 0..page.height.div_ceil(edge) {
            for column in 0..page.width.div_ceil(edge) {
                let mut cell = vec![0u8; cell_row.saturating_mul(edge_px)];
                let (x0, y0) = (usize::try_from(column.saturating_mul(edge)).unwrap_or(usize::MAX), usize::try_from(row.saturating_mul(edge)).unwrap_or(usize::MAX));
                let span = width.min(x0.saturating_add(edge_px)).saturating_sub(x0).saturating_mul(per);
                for y in y0..height.min(y0.saturating_add(edge_px)) {
                    let src_at = y.saturating_mul(row_bytes).saturating_add(x0.saturating_mul(per));
                    let dst_at = y.saturating_sub(y0).saturating_mul(cell_row);
                    let src = tone.get(src_at..src_at.saturating_add(span)).ok_or_else(|| importer("tiff_sample_count_mismatch"))?;
                    cell.get_mut(dst_at..dst_at.saturating_add(span)).ok_or_else(|| importer("dims_overflow"))?.copy_from_slice(src);
                }
                let object_key = format!("ch_{column}_{row}");
                let digest = hash_hex(&profile_hash(&cell));
                tiles.push(json!({
                    "object_key": object_key, "layer_id": channel_id, "column": column, "row": row,
                    "width": {"value": f64::from(edge), "unit": "px"}, "height": {"value": f64::from(edge), "unit": "px"},
                    "format": format, "colour_profile_id": profile_id, "schema_id": "hsk.studio.raster_tile@1",
                    "content_digest": {"algorithm": "sha256", "digest": digest},
                    "artifact_manifest_id": mint.tile_manifest_id(&channel_id, &object_key),
                }));
                out.assets.push(cell);
            }
        }
        let record: folio::StudioChannelRecord = serde_json::from_value(json!({
            "channel_id": channel_id, "name": format!("Alpha {}", n.saturating_add(1)), "kind": folio::StudioChannelKind::MaskedAreaAlpha,
            "visible": false, "colour": folio::StudioChannelColour::Rgb { r: 1.0, g: 0.0, b: 0.0 }, "opacity": 50.0,
            "depth": depth, "width": page.width, "height": page.height, "tiles": tiles,
        }))
        .map_err(|_| importer("tiff_channel_record"))?;
        out.loss.push(psd::LossEntry { path: format!("channel[{n}]"), kind: psd::LossKind::Transformed, code: "tiff_channel_display_defaulted" });
        out.facts.push(json!({"channel_id": channel_id, "sample": sample, "extra_sample": plane.extra_sample}));
        out.records.push(record);
    }
    Ok(out)
}

/// Why a TIFF container is a camera raw, or `None`: DNGVersion (tag 50706) in any walked IFD, or a
/// CFA (32803) / LinearRaw (34892) PhotometricInterpretation in any IFD of the bounded walk (IFD
/// chain plus SubIFDs, where TIFF/EP raws such as NEF and ARW keep the sensor image behind a
/// reduced-resolution IFD0 with NewSubfileType 1). An unwalkable chain falls back to IFD0 alone; an
/// unparseable header is no marker (the plain decoder then refuses with its own code).
pub(crate) fn camera_raw_marker(bytes: &[u8]) -> Option<&'static str> {
    const DNG_VERSION: u16 = 50706;
    const PHOTOMETRIC: u16 = 262;
    const CFA: u64 = 32803;
    const LINEAR_RAW: u64 = 34892;
    let container = tiff::Container::parse(bytes, &tiff::Limits::default()).ok()?;
    let ifds = match container.walk() {
        Ok(ifds) => ifds,
        Err(_) => vec![container.ifd(container.first_ifd_offset()).ok()?],
    };
    if ifds.iter().any(|ifd| ifd.find(DNG_VERSION).is_some()) {
        return Some("dng_version");
    }
    ifds.iter().find_map(|ifd| {
        let entry = ifd.find(PHOTOMETRIC)?;
        match container.uints(entry).ok()?.first().copied()? {
            CFA => Some("cfa_photometric"),
            LINEAR_RAW => Some("linear_raw_photometric"),
            _ => None,
        }
    })
}

/// The layer records of tag 37724 as a header-only PSD document over this page's canvas, with the
/// receipt facts; `Ok(Err(facts))` when the envelope, the canvas or the records cannot be used (the
/// composite route then records `tiff_photoshop_layers_not_imported`). Only cancellation is an error.
fn layered_document(
    block: &tiff::RawBlock,
    page: &tiff::TiffPage,
    limits: &psd::Limits,
    cancel: &CancellationToken,
) -> Result<Result<(psd::PsdDocument, serde_json::Value), serde_json::Value>, ImportError> {
    let isd = match tiff::image_source_data(block) {
        Ok(isd) => isd,
        Err(e) => return Ok(Err(json!({"envelope": e.code(), "layers": "not_imported"}))),
    };
    let mut facts = json!({
        "envelope": "valid", "byte_order": order(isd.byte_order), "wide_sizes": isd.wide_sizes,
        "block_bytes": isd.blocks.len(), "layers": "not_imported",
    });
    let depth = match page.pixels() {
        tiff::Samples::U8(_) => 8,
        tiff::Samples::U16(_) => 16,
        tiff::Samples::F32(_) => 32,
        tiff::Samples::U32(_) | tiff::Samples::F64(_) => {
            facts["parse_error"] = json!("tiff_sample_type_unmapped");
            return Ok(Err(facts));
        }
    };
    let colour_mode = match page.photometric {
        tiff::Photometric::BlackIsZero => psd::ColorMode::Grayscale,
        tiff::Photometric::Rgb => psd::ColorMode::Rgb,
        tiff::Photometric::Cmyk => psd::ColorMode::Cmyk,
        tiff::Photometric::IccLab | tiff::Photometric::CieLab => psd::ColorMode::Lab,
    };
    // interop-tiff hands out the run after its first signature; the PSD readers expect the signature.
    let signature: &[u8; 4] = match (isd.byte_order, isd.wide_sizes) {
        (tiff::ByteOrder::Big, false) => b"8BIM",
        (tiff::ByteOrder::Big, true) => b"8B64",
        (tiff::ByteOrder::Little, false) => b"MIB8",
        (tiff::ByteOrder::Little, true) => b"46B8",
    };
    let mut tagged = Vec::with_capacity(isd.blocks.len().saturating_add(4));
    tagged.extend_from_slice(signature);
    tagged.extend_from_slice(&isd.blocks);
    drop(isd);
    let canvas = psd::ImageSourceCanvas { width: page.width, height: page.height, depth, colour_mode, icc: page.icc.clone() };
    match psd::image_source_document(&tagged, canvas, limits, cancel) {
        Ok(doc) if doc.layers().is_empty() => {
            facts["parse_error"] = json!("image_source_no_layers");
            Ok(Err(facts))
        }
        Ok(doc) => {
            facts["layer_records"] = json!(doc.layers().len());
            Ok(Ok((doc, facts)))
        }
        Err(psd::PsdError::Canceled) => Err(ImportError::Canceled),
        Err(e) => {
            facts["parse_error"] = json!(e.to_string());
            Ok(Err(facts))
        }
    }
}

fn order(o: tiff::ByteOrder) -> &'static str {
    match o {
        tiff::ByteOrder::Little => "little",
        tiff::ByteOrder::Big => "big",
    }
}

/// Planes of the first page in the PSD sample layout `raster_proposal` reads.
struct Raster {
    mode: psd::ColorMode,
    depth: u16,
    planes: Vec<psd::Plane>,
    alpha: Option<psd::Plane>,
    losses: Vec<(String, psd::LossKind, &'static str)>,
}

/// Sample domain of one page: integer samples with their full-scale value, or 32-bit floats.
enum Domain<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    F32(&'a [f32]),
}

fn raster_of(page: &tiff::TiffPage) -> Result<Raster, ImportError> {
    let (mode, colour) = match page.photometric {
        tiff::Photometric::BlackIsZero => (psd::ColorMode::Grayscale, 1usize),
        tiff::Photometric::Rgb => (psd::ColorMode::Rgb, 3),
        tiff::Photometric::Cmyk => (psd::ColorMode::Cmyk, 4),
        tiff::Photometric::IccLab | tiff::Photometric::CieLab => (psd::ColorMode::Lab, 3),
    };
    let (domain, depth) = match page.pixels() {
        tiff::Samples::U8(v) => (Domain::U8(v), 8u16),
        tiff::Samples::U16(v) => (Domain::U16(v), 16),
        tiff::Samples::F32(v) => (Domain::F32(v), 32),
        tiff::Samples::U32(_) | tiff::Samples::F64(_) => return Err(importer("tiff_sample_type_unmapped")),
    };
    let pixels = usize::try_from(u64::from(page.width).checked_mul(u64::from(page.height)).ok_or_else(|| importer("dims_overflow"))?)
        .map_err(|_| importer("dims_overflow"))?;
    let total = page.pixels().len();
    if total.checked_rem(pixels) != Some(0) {
        return Err(importer("tiff_sample_count_mismatch"));
    }
    let spp = total.checked_div(pixels).unwrap_or(0);
    if spp < colour {
        return Err(importer("tiff_sample_count_mismatch"));
    }
    let alpha_at = (page.alpha != tiff::Alpha::None && spp > colour).then_some(colour);
    let used = if alpha_at.is_some() { colour.saturating_add(1) } else { colour };
    // Extra samples after `used` are channel records (`channel_records`), not layer samples.
    let mut losses = Vec::new();
    let associated = alpha_at.is_some() && page.alpha == tiff::Alpha::Associated;
    if associated {
        losses.push(("page[0]/alpha".into(), psd::LossKind::Transformed, "tiff_associated_alpha_unpremultiplied"));
    }
    let cielab = page.photometric == tiff::Photometric::CieLab;
    if cielab {
        if depth == 32 {
            return Err(importer("tiff_cielab_float_unmapped"));
        }
        losses.push(("page[0]/photometric".into(), psd::LossKind::Transformed, "tiff_cielab_reencoded_offset"));
    }
    let cmyk = mode == psd::ColorMode::Cmyk;
    let width_bytes = usize::from(depth.checked_div(8).unwrap_or(0));
    let plane_len = pixels.checked_mul(width_bytes).ok_or_else(|| importer("dims_overflow"))?;
    let mut out: Vec<Vec<u8>> = (0..used).map(|_| Vec::with_capacity(plane_len)).collect();
    match domain {
        Domain::U8(v) => split_int(v.iter().map(|&s| u32::from(s)), spp, colour, alpha_at, 255, associated, cielab, cmyk, &mut out, |p, s| {
            p.push(u8::try_from(s).unwrap_or(u8::MAX));
        }),
        Domain::U16(v) => split_int(v.iter().map(|&s| u32::from(s)), spp, colour, alpha_at, 65_535, associated, cielab, cmyk, &mut out, |p, s| {
            p.extend_from_slice(&u16::try_from(s).unwrap_or(u16::MAX).to_be_bytes());
        }),
        Domain::F32(v) => split_float(v, spp, colour, alpha_at, associated, cmyk, &mut out),
    }
    let mut planes: Vec<psd::Plane> =
        out.into_iter().map(|bytes| psd::Plane { width: page.width, height: page.height, depth, bytes }).collect();
    let alpha = alpha_at.and_then(|_| planes.pop());
    Ok(Raster { mode, depth, planes, alpha, losses })
}

/// Splits interleaved integer samples into planes. Associated alpha is divided out (rounded,
/// clamped; zero alpha gives zero colour); CIELab a/b flip their sign bit (two's complement -> offset
/// encoding); CMYK is stored inverted as in PSD (`max - ink`).
#[allow(clippy::too_many_arguments)]
fn split_int(
    samples: impl Iterator<Item = u32>,
    spp: usize,
    colour: usize,
    alpha_at: Option<usize>,
    max: u32,
    associated: bool,
    cielab: bool,
    cmyk: bool,
    out: &mut [Vec<u8>],
    push: impl Fn(&mut Vec<u8>, u32),
) {
    let half = max.div_ceil(2);
    let mut px: Vec<u32> = Vec::with_capacity(spp);
    let mut samples = samples.peekable();
    while samples.peek().is_some() {
        px.clear();
        px.extend(samples.by_ref().take(spp));
        let a = alpha_at.and_then(|i| px.get(i).copied());
        for (c, plane) in out.iter_mut().enumerate().take(colour) {
            let mut v = px.get(c).copied().unwrap_or(0);
            if let (true, Some(a)) = (associated, a) {
                v = match a {
                    0 => 0,
                    a => {
                        let rounding = u64::from(a.checked_div(2).unwrap_or(0));
                        let scaled = u64::from(v).saturating_mul(u64::from(max)).saturating_add(rounding).checked_div(u64::from(a));
                        u32::try_from(scaled.unwrap_or(0).min(u64::from(max))).unwrap_or(max)
                    }
                };
            }
            if cielab && c > 0 {
                v ^= half;
            }
            if cmyk {
                v = max.saturating_sub(v);
            }
            push(plane, v);
        }
        if let (Some(a), Some(plane)) = (a, out.get_mut(colour)) {
            push(plane, a);
        }
    }
}

/// Float counterpart of [`split_int`] (big-endian IEEE planes; CMYK ink inverted as `1 - ink`).
fn split_float(samples: &[f32], spp: usize, colour: usize, alpha_at: Option<usize>, associated: bool, cmyk: bool, out: &mut [Vec<u8>]) {
    for px in samples.chunks_exact(spp) {
        let a = alpha_at.and_then(|i| px.get(i).copied());
        for (c, plane) in out.iter_mut().enumerate().take(colour) {
            let mut v = px.get(c).copied().unwrap_or(0.0);
            if let (true, Some(a)) = (associated, a) {
                v = if a > 0.0 { v / a } else { 0.0 };
            }
            if cmyk {
                v = 1.0 - v;
            }
            plane.extend_from_slice(&v.to_be_bytes());
        }
        if let (Some(a), Some(plane)) = (a, out.get_mut(colour)) {
            plane.extend_from_slice(&a.to_be_bytes());
        }
    }
}
