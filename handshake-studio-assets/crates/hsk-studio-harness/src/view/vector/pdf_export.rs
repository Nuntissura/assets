//! PDF export of the open vector document (lane SMB-ILLUSTRATOR-INTEROP). Composite's
//! `expand_vectors` gives every vector layer as plain filled shapes the way the viewer draws it:
//! swatches resolved (their stand-in paints), brushes, width profiles, networks and envelopes
//! expanded; Press `PdfExport` writes them as vector paths on one page the size of the first
//! artboard (1 document px = 0.75 pt, y flipped). Solid paints are DeviceRGB (the harness working
//! space, untagged), gradients are PDF axial / radial shadings, row opacity and blend mode go
//! through the draw state, a layer's own opacity or blend mode wraps it in a transparency group;
//! hidden layers are not written. Everything else is a typed loss in the returned list:
//! `refused:<code>` layers, `vector_clip_not_exported` / `vector_mask_not_exported` (vector clips
//! and masks), `paint_not_exported` (pattern and image rows), `blend_not_exported` (modes PDF has
//! no name for), `raster_layer_not_exported`, `text_layer_not_exported`.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::effective_design;
use super::EntryAssets;
use hsk_studio_composite::FillPaint;
use hsk_studio_composite::vector::{ExpandedLayer, VectorResolution, expand_vectors};
use hsk_studio_folio::{self as folio, Parent, Payload};
use hsk_studio_nib::{OwnedPath, PathElement};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_press::{
    BlendMode, Colour, DrawState, ExportOptions, FillRule, Gradient, GradientGeometry, GradientSpread, GradientStop, GroupStyle, Limits, PageSpec, PathCommand,
    PdfExport,
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

/// A layer and all its ancestors are visible.
fn shown(doc: &folio::StudioDocument, layer_id: &str) -> bool {
    let mut at = Some(layer_id.to_string());
    let mut guard = 0_usize;
    while let Some(id) = at {
        let Some(l) = doc.layers.iter().find(|l| l.layer_id == id) else { return false };
        if !l.visible || guard > doc.layers.len() {
            return false;
        }
        guard = guard.saturating_add(1);
        at = match &l.parent {
            Parent::Node(n) => Some(n.node_id.clone()),
            _ => None,
        };
    }
    true
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
        for a in &records.attachments {
            if a.clip_id.is_some() {
                losses.push(format!("{}: vector_clip_not_exported", a.layer_id));
            }
            if a.mask_id.is_some() {
                losses.push(format!("{}: vector_mask_not_exported", a.layer_id));
            }
        }
        for l in &doc.layers {
            match l.payload {
                Payload::Raster { .. } if shown(doc, &l.layer_id) => losses.push(format!("{}: raster_layer_not_exported", l.layer_id)),
                Payload::Text { .. } if shown(doc, &l.layer_id) => losses.push(format!("{}: text_layer_not_exported", l.layer_id)),
                _ => {}
            }
        }
        let board = doc.artboards.first().ok_or("pdf_export: the document has no artboard")?;
        let page = Page { o: [board.bounds.x.value, board.bounds.y.value], h: board.bounds.height.value };
        let media = [0.0, 0.0, board.bounds.width.value * PT, board.bounds.height.value * PT];
        let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).map_err(|e| format!("pdf_export: {}", e.code()))?;
        let handle = ex.add_page(PageSpec { media_box_pt: media, trim_box_pt: None, bleed_box_pt: None }).map_err(|e| format!("pdf_export: {}", e.code()))?;
        let err = |e: hsk_studio_press::ExportError| format!("pdf_export: {}", e.code());
        for ExpandedLayer { layer_id, rows } in &layers {
            if !shown(doc, layer_id) {
                continue;
            }
            let appearance = doc.layers.iter().find(|l| &l.layer_id == layer_id).and_then(|l| l.appearance.clone());
            let grouped = appearance.as_ref().is_some_and(|a| a.opacity < 100.0 || a.blend_mode != folio::BLEND_NORMAL);
            if grouped {
                ex.begin_group(handle).map_err(err)?;
            }
            for row in rows {
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
                    let cmds = commands(&part, page);
                    let rule = if even_odd { FillRule::EvenOdd } else { FillRule::NonZero };
                    match &row.paint {
                        FillPaint::Solid(c) => {
                            let alpha = f64::from(c[3]) * f64::from(row.opacity);
                            ex.set_draw_state(handle, DrawState { fill_alpha: alpha.clamp(0.0, 1.0), stroke_alpha: 1.0, blend: mode }).map_err(err)?;
                            ex.fill_path(handle, &cmds, rule, Colour::DeviceRgb([f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])).map_err(err)?;
                        }
                        FillPaint::Gradient(g) => {
                            ex.set_draw_state(handle, DrawState { fill_alpha: f64::from(row.opacity).clamp(0.0, 1.0), stroke_alpha: 1.0, blend: mode }).map_err(err)?;
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
                            let shading = Gradient { geometry, stops: &stops, extend: [true, true], spread, matrix: compose(page.affine(), g.transform), profile: None };
                            ex.fill_path_gradient(handle, &cmds, rule, &shading).map_err(err)?;
                        }
                        _ => losses.push(format!("{layer_id}: paint_not_exported")),
                    }
                }
            }
            if let (true, Some(a)) = (grouped, &appearance) {
                let mode = StudioBlendMode::try_from(a.blend_mode).ok().and_then(blend).unwrap_or(BlendMode::Normal);
                ex.end_group(handle, GroupStyle { opacity: (a.opacity / 100.0).clamp(0.0, 1.0), blend: mode, isolated: true, knockout: false, soft_mask: None }).map_err(err)?;
            }
        }
        let (bytes, _) = ex.finish(self.cancel).map_err(err)?;
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
