//! Clips and masks for the vector exports (lane SMB-ILLUSTRATOR-INTEROP-2; PDF in `pdf_export`,
//! SVG in `interop`). Folio keeps a clip or mask as a record whose source is a layer subtree and
//! an attachment naming the clipped / masked layer:
//!
//! - clip geometry ([`source_geometry`]) is the anchor paths of every vector layer under the
//!   source (visible or not: a clip source is usually hidden), in document pixels, with the
//!   clip's own fill rule;
//! - a VECTOR mask is the same geometry used as a clip (its shape is the mask); an inverted one
//!   is that geometry inside a page-covering rectangle under the even-odd rule;
//! - an ALPHA or LUMINANCE mask is rasterised ([`mask_plane`]): the source subtree's expanded
//!   rows (composite `expand_vectors`, the shapes the viewer draws) painted with nib over the
//!   artboard at one sample per document pixel, composited source-over; the plane holds the
//!   mask's alpha (ALPHA) or its luminance times alpha (LUMINANCE, Rec. 709 weights on the
//!   encoded colours), inverted when the mask is, and zero outside the mask region.
#![deny(clippy::arithmetic_side_effects)]
use super::super::layers::stack;
use super::VectorEntry;
use hsk_studio_composite::FillPaint;
use hsk_studio_composite::vector::ExpandedLayer;
use hsk_studio_folio::{self as folio, Parent, Payload, StudioDocument, StudioMaskMode, StudioVectorMask};
use hsk_studio_nib::OwnedPath;
use hsk_studio_nib::raster::{Paint, PixelFrame, RasterOptions, paint_coverage, rasterize};
use std::collections::BTreeMap;

/// Largest mask plane (pixels).
pub const MAX_MASK_PIXELS: u64 = 1 << 24;

/// Children of `parent` (`None`: the roots) bottom-most first: the stacked ones in stack order,
/// then the others in document order.
pub fn children(doc: &StudioDocument, parent: Option<&str>) -> Vec<String> {
    let mut out = stack(doc, parent);
    for l in &doc.layers {
        let under = match (&l.parent, parent) {
            (Parent::Root(()), None) => true,
            (Parent::Node(n), Some(p)) => n.node_id == p,
            _ => false,
        };
        if under && !out.contains(&l.layer_id) {
            out.push(l.layer_id.clone());
        }
    }
    out
}

/// `root` and every layer under it, in paint order (parents before their children).
pub fn subtree(doc: &StudioDocument, root: &str) -> Vec<String> {
    let mut out = vec![root.to_string()];
    let mut at = 0_usize;
    while let Some(id) = out.get(at).cloned() {
        at = at.saturating_add(1);
        if out.len() > doc.layers.len() {
            break;
        }
        for c in children(doc, Some(&id)) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// The anchor paths (document pixels) of every vector layer under `source`.
pub fn source_geometry(doc: &StudioDocument, entry: &VectorEntry, source: &str) -> Result<Vec<OwnedPath>, String> {
    let mut out = Vec::new();
    for id in subtree(doc, source) {
        let Some(l) = doc.layers.iter().find(|l| l.layer_id == id) else { continue };
        if let Payload::Vector { path_ids } = &l.payload {
            for p in path_ids {
                let Some(rec) = entry.path(p) else { continue };
                out.push(rec.path.to_owned_path().map_err(|e| format!("clip geometry {p}: {}", e.code()))?);
            }
        }
    }
    Ok(out)
}

/// 8-bit mask plane over `frame` (row-major, one byte per pixel) of `mask` (module docs).
pub fn mask_plane(doc: &StudioDocument, expanded: &BTreeMap<&str, &ExpandedLayer>, mask: &StudioVectorMask, frame: PixelFrame) -> Result<Vec<u8>, String> {
    let n = u64::from(frame.width).checked_mul(u64::from(frame.height)).filter(|n| *n > 0 && *n <= MAX_MASK_PIXELS).ok_or("mask plane over budget")?;
    let n = usize::try_from(n).map_err(|_| "mask plane over budget")?;
    // Premultiplied RGBA accumulation, source-over in paint order.
    let mut acc = vec![[0.0_f32; 4]; n];
    for id in subtree(doc, &mask.source_layer_id) {
        let Some(layer) = expanded.get(id.as_str()) else { continue };
        for row in &layer.rows {
            let paint = match &row.paint {
                FillPaint::Solid(c) => Paint::Solid(*c),
                FillPaint::Gradient(g) => Paint::Gradient(g.clone()),
                FillPaint::Pattern(p) => Paint::Pattern(p.clone()),
                _ => continue,
            };
            for path in &row.paths {
                let (coverage, _) = rasterize(path, frame, &RasterOptions { max_pixels: MAX_MASK_PIXELS, ..RasterOptions::default() }).map_err(|e| format!("mask raster: {e:?}"))?;
                let painted = paint_coverage(&coverage, &paint).map_err(|e| format!("mask raster: {e:?}"))?;
                for (dst, src) in acc.iter_mut().zip(&painted.data) {
                    let s = src.map(|v| v * row.opacity);
                    let keep = 1.0 - s[3];
                    *dst = [s[0] + dst[0] * keep, s[1] + dst[1] * keep, s[2] + dst[2] * keep, s[3] + dst[3] * keep];
                }
            }
        }
    }
    let region = mask.region.as_ref().map(|r| [r.x.value, r.y.value, r.x.value + r.width.value, r.y.value + r.height.value]);
    let width = usize::try_from(frame.width).map_err(|_| "mask plane over budget")?;
    Ok(acc
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (col, row) = (i.checked_rem(width).unwrap_or(0), i.checked_div(width).unwrap_or(0));
            let (x, y) = (f64::from(frame.x) + col as f64 + 0.5, f64::from(frame.y) + row as f64 + 0.5);
            if region.is_some_and(|r| x < r[0] || y < r[1] || x > r[2] || y > r[3]) {
                return 0;
            }
            // Premultiplied: luminance of the colour over black is luminance times alpha.
            let v = match mask.mode {
                StudioMaskMode::Luminance => 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2],
                _ => p[3],
            };
            let v = if mask.invert_mask { 1.0 - v } else { v };
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect())
}

/// The Folio clip and mask of `layer_id` (its attachment), when it has one.
pub fn attachment<'a>(records: &'a folio::StudioVectorRecords, layer_id: &str) -> (Option<&'a folio::StudioVectorClip>, Option<&'a StudioVectorMask>) {
    let Some(a) = records.attachments.iter().find(|a| a.layer_id == layer_id) else { return (None, None) };
    let clip = a.clip_id.as_ref().and_then(|c| records.clips.iter().find(|x| &x.clip_id == c));
    let mask = a.mask_id.as_ref().and_then(|m| records.masks.iter().find(|x| &x.mask_id == m));
    (clip, mask)
}
