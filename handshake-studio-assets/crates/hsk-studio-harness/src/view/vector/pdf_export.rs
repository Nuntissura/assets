//! PDF export of the open vector document (lane SMB-ILLUSTRATOR-INTEROP). Composite's
//! `expand_vectors` gives every vector layer as plain filled shapes the way the viewer draws it:
//! swatches resolved (their stand-in paints), brushes, width profiles, networks and envelopes
//! expanded; Press `PdfExport` writes them as vector paths on one page the size of the first
//! artboard (1 document px = 0.75 pt, y flipped). Solid paints are DeviceRGB (the harness working
//! space, untagged), gradients are PDF axial / radial shadings, row opacity and blend mode go
//! through the draw state. The layer tree is walked bottom-most first (lane
//! SMB-ILLUSTRATOR-INTEROP-2): a group or vector layer with its own opacity or blend mode wraps
//! its content in a transparency group; a Folio vector clip is a PDF clip path of the clip
//! source's geometry (its fill rule); a VECTOR mask is a clip of its source geometry (inverted:
//! even-odd inside the page rectangle); an ALPHA or LUMINANCE mask is the group's soft mask, an
//! image of the mask plane at document resolution (`masking`); clip and mask sources are drawn
//! only through what they clip or mask; hidden layers are not written. Everything else is a
//! typed loss in the returned list: `refused:<code>` layers, `vector_mask_rasterized_at_document_resolution`,
//! `mask_clip_not_exported`, `mask_parameters_not_exported`, `paint_not_exported` (pattern and
//! image rows), `blend_not_exported` (modes PDF has no name for), `raster_layer_not_exported`,
//! `text_layer_not_exported`.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::effective_design;
use super::EntryAssets;
use hsk_studio_composite::FillPaint;
use hsk_studio_composite::vector::{ExpandedLayer, VectorResolution, expand_vectors};
use super::masking;
use hsk_studio_folio::{self as folio, Payload, StudioMaskMode};
use hsk_studio_nib::raster::PixelFrame;
use std::collections::{BTreeMap, BTreeSet};
use hsk_studio_nib::{OwnedPath, PathElement};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_press::{
    BlendMode, Colour, DrawState, ExportOptions, FillRule, Gradient, GradientGeometry, GradientSpread, GradientStop, GroupStyle, ImageColour, ImageSpec, Limits, MaskImage,
    PageHandle, PageSpec, PathCommand, PdfExport, SoftMask,
};
use std::path::Path;

/// Points per document pixel.
const PT: f64 = 0.75;

fn blend(mode: StudioBlendMode) -> Option<BlendMode> {
    Some(match mode {
        StudioBlendMode::Normal | StudioBlendMode::PassThrough => BlendMode::Normal,
        StudioBlendMode::Multiply => BlendMode::Multiply,
        StudioBlendMode::Screen => BlendMode::Screen,
        StudioBlendMode::Overlay => BlendMode::Overlay,
        StudioBlendMode::Darken => BlendMode::Darken,
        StudioBlendMode::Lighten => BlendMode::Lighten,
        StudioBlendMode::ColourDodge => BlendMode::ColorDodge,
        StudioBlendMode::ColourBurn => BlendMode::ColorBurn,
        StudioBlendMode::HardLight => BlendMode::HardLight,
        StudioBlendMode::SoftLight => BlendMode::SoftLight,
        StudioBlendMode::Difference => BlendMode::Difference,
        StudioBlendMode::Exclusion => BlendMode::Exclusion,
        StudioBlendMode::Hue => BlendMode::Hue,
        StudioBlendMode::Saturation => BlendMode::Saturation,
        StudioBlendMode::Colour => BlendMode::Color,
        StudioBlendMode::Luminosity => BlendMode::Luminosity,
        _ => return None,
    })
}

/// Document px -> PDF points on the page of artboard origin `o` and height `h` (px).
#[derive(Clone, Copy)]
struct Page {
    o: [f64; 2],
    h: f64,
    w: f64,
}
impl Page {
    fn p(self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.o[0]) * PT, (self.o[1] + self.h - y) * PT)
    }
    /// The same map as an affine `[a b c d e f]`.
    fn affine(self) -> [f64; 6] {
        [PT, 0.0, 0.0, -PT, -self.o[0] * PT, (self.o[1] + self.h) * PT]
    }
}

fn compose(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

fn commands(paths: &[OwnedPath], page: Page) -> Vec<PathCommand> {
    let mut out = Vec::new();
    for path in paths {
        let mut last = (0.0, 0.0);
        for e in &path.elements {
            match *e {
                PathElement::MoveTo(q) => {
                    last = (q.x, q.y);
                    let (x, y) = page.p(q.x, q.y);
                    out.push(PathCommand::MoveTo(x, y));
                }
                PathElement::LineTo(q) => {
                    last = (q.x, q.y);
                    let (x, y) = page.p(q.x, q.y);
                    out.push(PathCommand::LineTo(x, y));
                }
                PathElement::QuadTo(c, q) => {
                    let c1 = page.p(last.0 + (c.x - last.0) * 2.0 / 3.0, last.1 + (c.y - last.1) * 2.0 / 3.0);
                    let c2 = page.p(q.x + (c.x - q.x) * 2.0 / 3.0, q.y + (c.y - q.y) * 2.0 / 3.0);
                    last = (q.x, q.y);
                    let e = page.p(q.x, q.y);
                    out.push(PathCommand::CubicTo(c1.0, c1.1, c2.0, c2.1, e.0, e.1));
                }
                PathElement::CubicTo(a, b, q) => {
                    last = (q.x, q.y);
                    let (a, b, e) = (page.p(a.x, a.y), page.p(b.x, b.y), page.p(q.x, q.y));
                    out.push(PathCommand::CubicTo(a.0, a.1, b.0, b.1, e.0, e.1));
                }
                PathElement::Close => out.push(PathCommand::Close),
            }
        }
    }
    out
}

/// Everything one export walk reads.
struct Walk<'a> {
    doc: &'a folio::StudioDocument,
    entry: &'a super::VectorEntry,
    records: &'a folio::StudioVectorRecords,
    expanded: BTreeMap<&'a str, &'a ExpandedLayer>,
    /// Layers that are clip or mask sources (drawn only through what they clip or mask).
    sources: BTreeSet<&'a str>,
    page: Page,
    /// The artboard in document pixels (the mask planes' frame).
    frame: PixelFrame,
    handle: PageHandle,
}

impl Walk<'_> {
    fn err(e: hsk_studio_press::ExportError) -> String {
        format!("pdf_export: {}", e.code())
    }

    /// The expanded rows of one vector layer (module docs).
    fn rows(&self, ex: &mut PdfExport, layer_id: &str, losses: &mut Vec<String>) -> Result<(), String> {
        let Some(layer) = self.expanded.get(layer_id) else { return Ok(()) };
        for row in &layer.rows {
            if row.paths.iter().all(|p| p.elements.is_empty()) {
                continue;
            }
            let mode = blend(row.mode).unwrap_or_else(|| {
                losses.push(format!("{layer_id}: blend_not_exported"));
                BlendMode::Normal
            });
            // One fill per fill rule (paths of one rule share one PDF path).
            for even_odd in [false, true] {
                let part: Vec<OwnedPath> = row.paths.iter().filter(|p| (p.fill_rule == hsk_studio_nib::FillRule::EvenOdd) == even_odd).cloned().collect();
                if part.is_empty() {
                    continue;
                }
                let cmds = commands(&part, self.page);
                let rule = if even_odd { FillRule::EvenOdd } else { FillRule::NonZero };
                match &row.paint {
                    FillPaint::Solid(c) => {
                        let alpha = f64::from(c[3]) * f64::from(row.opacity);
                        ex.set_draw_state(self.handle, DrawState { fill_alpha: alpha.clamp(0.0, 1.0), stroke_alpha: 1.0, blend: mode }).map_err(Self::err)?;
                        ex.fill_path(self.handle, &cmds, rule, Colour::DeviceRgb([f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])).map_err(Self::err)?;
                    }
                    FillPaint::Gradient(g) => {
                        ex.set_draw_state(self.handle, DrawState { fill_alpha: f64::from(row.opacity).clamp(0.0, 1.0), stroke_alpha: 1.0, blend: mode }).map_err(Self::err)?;
                        let stops: Vec<GradientStop> = g.stops.iter().map(|s| GradientStop { offset: s.offset, colour: [f64::from(s.colour[0]), f64::from(s.colour[1]), f64::from(s.colour[2])], opacity: f64::from(s.colour[3]), midpoint: s.midpoint }).collect();
                        let geometry = match g.geometry {
                            hsk_studio_nib::raster::GradientGeometry::Linear { x1, y1, x2, y2 } => GradientGeometry::Axial { from: [x1, y1], to: [x2, y2] },
                            hsk_studio_nib::raster::GradientGeometry::Radial { cx, cy, r, fx, fy, fr } => GradientGeometry::Radial { from: [fx, fy], from_radius: fr, to: [cx, cy], to_radius: r },
                        };
                        let spread = match g.spread {
                            hsk_studio_nib::raster::Spread::Pad => GradientSpread::Pad,
                            hsk_studio_nib::raster::Spread::Repeat => GradientSpread::Repeat,
                            hsk_studio_nib::raster::Spread::Reflect => GradientSpread::Reflect,
                        };
                        let shading = Gradient { geometry, stops: &stops, extend: [true, true], spread, matrix: compose(self.page.affine(), g.transform), profile: None };
                        ex.fill_path_gradient(self.handle, &cmds, rule, &shading).map_err(Self::err)?;
                    }
                    _ => losses.push(format!("{layer_id}: paint_not_exported")),
                }
            }
        }
        Ok(())
    }

    /// Pushes the clip geometry of `source`; false when it has none (nothing pushed).
    fn push_clip(&self, ex: &mut PdfExport, source: &str, rule: FillRule, invert: bool) -> Result<bool, String> {
        let mut cmds = commands(&masking::source_geometry(self.doc, self.entry, source)?, self.page);
        if cmds.is_empty() && !invert {
            return Ok(false);
        }
        let rule = if invert {
            let (w, h) = (self.page.w * PT, self.page.h * PT);
            cmds.extend([PathCommand::MoveTo(0.0, 0.0), PathCommand::LineTo(w, 0.0), PathCommand::LineTo(w, h), PathCommand::LineTo(0.0, h), PathCommand::Close]);
            FillRule::EvenOdd
        } else {
            rule
        };
        ex.push_clip(self.handle, &cmds, rule).map_err(Self::err)?;
        Ok(true)
    }

    /// Writes the children of `parent` bottom-most first.
    fn walk(&self, ex: &mut PdfExport, parent: Option<&str>, losses: &mut Vec<String>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("pdf_export: layer nesting deeper than 64".into());
        }
        for id in masking::children(self.doc, parent) {
            let Some(layer) = self.doc.layers.iter().find(|l| l.layer_id == id) else { continue };
            if !layer.visible || self.sources.contains(id.as_str()) {
                continue;
            }
            let (clip, mask) = masking::attachment(self.records, &id);
            // ALPHA / LUMINANCE masks become a soft mask (an image of the mask plane).
            let soft = match mask {
                Some(m) if m.mode != StudioMaskMode::Vector => {
                    losses.push(format!("{id}: vector_mask_rasterized_at_document_resolution"));
                    if m.mask_clip {
                        losses.push(format!("{id}: mask_clip_not_exported"));
                    }
                    if self.records.mask_parameters(&m.mask_id).is_some() {
                        losses.push(format!("{id}: mask_parameters_not_exported"));
                    }
                    Some((masking::mask_plane(self.doc, &self.expanded, m, self.frame)?, m.mode == StudioMaskMode::Alpha))
                }
                _ => None,
            };
            let appearance = layer.appearance.clone();
            let grouped = soft.is_some() || appearance.as_ref().is_some_and(|a| a.opacity < 100.0 || a.blend_mode != folio::BLEND_NORMAL);
            if grouped {
                ex.begin_group(self.handle).map_err(Self::err)?;
            }
            let mut pushed = 0_u32;
            if let Some(c) = clip
                && self.push_clip(ex, &c.source_layer_id, if c.clip_rule == folio::StudioClipRule::EvenOdd { FillRule::EvenOdd } else { FillRule::NonZero }, false)?
            {
                pushed = pushed.saturating_add(1);
            }
            if let Some(m) = mask.filter(|m| m.mode == StudioMaskMode::Vector)
                && self.push_clip(ex, &m.source_layer_id, FillRule::NonZero, m.invert_mask)?
            {
                pushed = pushed.saturating_add(1);
            }
            match layer.payload {
                Payload::Group => self.walk(ex, Some(&id), losses, depth.saturating_add(1))?,
                Payload::Vector { .. } => self.rows(ex, &id, losses)?,
                Payload::Raster { .. } => losses.push(format!("{id}: raster_layer_not_exported")),
                Payload::Text { .. } => losses.push(format!("{id}: text_layer_not_exported")),
                _ => {}
            }
            for _ in 0..pushed {
                ex.pop_clip(self.handle).map_err(Self::err)?;
            }
            if grouped {
                let (opacity, mode) = appearance.as_ref().map_or((1.0, BlendMode::Normal), |a| ((a.opacity / 100.0).clamp(0.0, 1.0), StudioBlendMode::try_from(a.blend_mode).ok().and_then(blend).unwrap_or(BlendMode::Normal)));
                let (w, h) = (self.frame.width, self.frame.height);
                let zeros;
                let images;
                let soft_mask = match &soft {
                    Some((plane, alpha)) => {
                        zeros = vec![0_u8; plane.len()];
                        let spec = if *alpha {
                            ImageSpec { width: w, height: h, bits: 8, colour: ImageColour::DeviceGray, samples: &zeros, alpha: Some(plane) }
                        } else {
                            ImageSpec { width: w, height: h, bits: 8, colour: ImageColour::DeviceGray, samples: plane, alpha: None }
                        };
                        // The plane covers the artboard: image space (unit square, y up) to page points.
                        images = [MaskImage { image: spec, placement: [self.page.w * PT, 0.0, 0.0, self.page.h * PT, 0.0, 0.0] }];
                        Some(if *alpha { SoftMask::alpha(&images) } else { SoftMask::luminosity(&images) })
                    }
                    None => None,
                };
                ex.end_group(self.handle, GroupStyle { opacity, blend: mode, isolated: true, knockout: false, soft_mask }).map_err(Self::err)?;
            }
        }
        Ok(())
    }
}

impl Viewer {
    /// The open vector document as PDF bytes and the typed losses (module docs).
    pub fn vector_pdf_bytes(&self) -> Result<(Vec<u8>, Vec<String>), String> {
        let snap = self.run.snapshot().ok_or("pdf_export: nothing open")?;
        let doc = snap.document();
        let entry = self.vector_entry()?;
        let design = effective_design(doc, &entry)?;
        let records = entry.folio_records(&doc.document_id);
        let (layers, refused) = expand_vectors(snap, &design, &records, &EntryAssets(&entry), hsk_studio_composite::BlendSpace::Encoded, &VectorResolution::default(), self.cancel)
            .map_err(|e| format!("pdf_export: {e:?}"))?;
        let mut losses: Vec<String> = refused.iter().map(|(l, c)| format!("{l}: refused:{c}")).collect();
        let board = doc.artboards.first().ok_or("pdf_export: the document has no artboard")?;
        let page = Page { o: [board.bounds.x.value, board.bounds.y.value], h: board.bounds.height.value, w: board.bounds.width.value };
        let media = [0.0, 0.0, board.bounds.width.value * PT, board.bounds.height.value * PT];
        let whole = |v: f64| if v.is_finite() && v.abs() < 1e7 { Some(v.round() as i64) } else { None };
        let frame = match (whole(page.o[0]), whole(page.o[1]), whole(page.w.ceil()), whole(page.h.ceil())) {
            (Some(x), Some(y), Some(w), Some(h)) => PixelFrame {
                x: i32::try_from(x).map_err(|_| "pdf_export: artboard")?,
                y: i32::try_from(y).map_err(|_| "pdf_export: artboard")?,
                width: u32::try_from(w.max(1)).map_err(|_| "pdf_export: artboard")?,
                height: u32::try_from(h.max(1)).map_err(|_| "pdf_export: artboard")?,
            },
            _ => return Err("pdf_export: artboard".into()),
        };
        let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).map_err(|e| format!("pdf_export: {}", e.code()))?;
        let handle = ex.add_page(PageSpec { media_box_pt: media, trim_box_pt: None, bleed_box_pt: None }).map_err(|e| format!("pdf_export: {}", e.code()))?;
        let sources: BTreeSet<&str> = records.clips.iter().map(|c| c.source_layer_id.as_str()).chain(records.masks.iter().map(|m| m.source_layer_id.as_str())).collect();
        let walk = Walk { doc, entry: &entry, records: &records, expanded: layers.iter().map(|l| (l.layer_id.as_str(), l)).collect(), sources, page, frame, handle };
        walk.walk(&mut ex, None, &mut losses, 0)?;
        drop(walk);
        let (bytes, _) = ex.finish(self.cancel).map_err(Walk::err)?;
        Ok((bytes, losses))
    }

    /// Writes [`Viewer::vector_pdf_bytes`] to `path` (atomically; never over an existing file
    /// without `overwrite`). Returns the losses.
    pub fn export_vector_pdf(&mut self, path: &Path, overwrite: bool) -> Result<Vec<String>, String> {
        let r = (|| {
            if path.exists() && !overwrite {
                return Err(format!("pdf_export: {} exists", path.display()));
            }
            let (bytes, losses) = self.vector_pdf_bytes()?;
            super::super::export::write_atomic(path, &bytes).map_err(|e| format!("pdf_export: {e}"))?;
            Ok(losses)
        })();
        self.report(r)
    }
}
