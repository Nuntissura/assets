//! Long-document content of the PDF lowering (lane SMB-LAYOUT-LONGDOC): everything hsk-studio-layout
//! composes beside a frame's own story is drawn right after that frame layer's text, so it takes the
//! frame's position in the paint order.
//!
//! - Tables ([`PdfTable`]): per table part in the frame, the cell fills (layout `tint_rgb` of the
//!   Folio colour by its tint), then the resolved stroke segments in the proposal's order (butt caps,
//!   miter joins, centred on the cell boundary), then the cell text (`TableProposal::text`, glyphs
//!   and fills as for any story).
//! - Overlays: stories whose placement frames name a frame layer they do not own (footnote bodies,
//!   reference markers, generated text), drawn after the host frame's text.
//! - Rules ([`RuleSegment`] from layout `layout_footnotes`): footnote separator rules stroked in
//!   their frame (butt caps), before the overlays.
//! - Placed graphics ([`PlacedGraphic`] from layout `place_graphic`): a raster layer with a graphic
//!   frame is drawn in its own paint-order position through its fitting transform instead of its
//!   `PlacedRaster` origin: pigment tiles (validated as for every raster layer) map pixel -> content
//!   pt by `content_size / pixel extent` per axis, then content pt -> spread by the fitting
//!   transform, clipped to the frame shape (rectangle, inscribed ellipse as four cubic arcs with
//!   k = 0.5522847498, or the `SVPT` path from `PdfAssets::path`; an unresolved path clips to the
//!   rectangle and is reported `frame_shape_unresolved`).
//!
//! Colours bound to a profile are written ICCBased as everywhere in this module; unassigned colours
//! are DeviceRGB and reported (`paint_colour_untagged`).
use super::{Cx, PdfExportError, PdfExportInput, PdfStory, Result, check_story_list};
use crate::model::LossKind;
use hsk_studio_folio::{StudioColor, StudioDocument, StudioFrameShape, StudioLayer};
use hsk_studio_layout::{PlacedGraphic, RuleSegment, TableProposal, tint_rgb};
use hsk_studio_nib::PathLimits;
use hsk_studio_press::{FillRule, ImageColour, ImageSpec, LineCap, LineJoin, PathCommand, StrokeStyle};

/// One laid-out table and the glyph runs of its cell story.
#[derive(Clone, Copy, Debug)]
pub struct PdfTable<'a> {
    pub proposal: &'a TableProposal,
    /// The cell story; `placement` must be `&proposal.text`.
    pub cells: PdfStory<'a>,
}

/// Long-document content for [`super::export_pdf_with`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PdfLongDoc<'a> {
    pub tables: &'a [PdfTable<'a>],
    /// Stories drawn into frames they do not own (their placement's `frame_id`s name the host frame
    /// layers).
    pub overlays: &'a [PdfStory<'a>],
    /// Graphic frames of raster layers (layout `place_graphic`), keyed by `layer_id`.
    pub graphics: &'a [PlacedGraphic],
    /// Footnote separator rules (layout `FootnoteProposal::rules`).
    pub rules: &'a [RuleSegment],
}
impl<'a> PdfLongDoc<'a> {
    /// Every story this content draws.
    pub(super) fn stories(&self) -> impl Iterator<Item = &PdfStory<'a>> {
        self.tables.iter().map(|t| &t.cells).chain(self.overlays.iter())
    }
    pub(super) fn graphic(&self, layer_id: &str) -> Option<&'a PlacedGraphic> {
        self.graphics.iter().find(|g| g.layer_id == layer_id)
    }
    pub(super) fn check(&self, input: &PdfExportInput<'_>, doc: &StudioDocument) -> Result<()> {
        for r in self.rules {
            let finite = r.from.iter().chain(&r.to).chain([r.origin_x_pt, r.origin_y_pt, r.weight_pt].iter()).all(|v| v.is_finite());
            if !finite {
                return Err(PdfExportError::InvalidInput("rule_placement"));
            }
        }
        for g in self.graphics {
            let finite = g.transform.iter().chain(&g.content_size).all(|v| v.is_finite())
                && [g.frame.x0_pt, g.frame.y0_pt, g.frame.x1_pt, g.frame.y1_pt].iter().all(|v| v.is_finite());
            if !finite || !(g.transform[0] > 0.0 && g.transform[3] > 0.0 && g.content_size.iter().all(|v| *v > 0.0)) {
                return Err(PdfExportError::InvalidInput("graphic_placement"));
            }
        }
        for t in self.tables {
            if !std::ptr::eq(t.cells.placement, &t.proposal.text) {
                return Err(PdfExportError::InvalidInput("table_text_placement"));
            }
            if t.proposal.base_revision != doc.revision {
                return Err(PdfExportError::StaleRevision { expected: t.proposal.base_revision, current: doc.revision });
            }
        }
        check_story_list(input, doc, self.stories())
    }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> [(f64, f64); 4] {
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

impl Cx<'_> {
    /// Tables and overlays whose frames are `layer` (module docs).
    pub(super) fn frame_extras(&mut self, layer: &StudioLayer) -> Result<()> {
        let longdoc = self.longdoc;
        for t in longdoc.tables {
            self.table_parts(layer, t)?;
        }
        for r in longdoc.rules.iter().filter(|r| r.frame_id == layer.layer_id) {
            let Some(pi) = self.page_index(r.spread, r.page) else {
                return Err(PdfExportError::InvalidInput("frame_page"));
            };
            if !(r.weight_pt.is_finite() && r.weight_pt > 0.0) {
                continue;
            }
            let Some(colour) = self.table_colour(&layer.layer_id, &r.colour, r.tint)? else { continue };
            let Some(page) = self.pages.get(pi) else { continue };
            let handle = page.handle;
            let (a, b) = (page.map(r.origin_x_pt + r.from[0], r.origin_y_pt + r.from[1]), page.map(r.origin_x_pt + r.to[0], r.origin_y_pt + r.to[1]));
            let cmds = [PathCommand::MoveTo(a.0, a.1), PathCommand::LineTo(b.0, b.1)];
            let style = StrokeStyle { width_pt: r.weight_pt, cap: LineCap::Butt, join: LineJoin::Miter };
            self.ex.stroke_path(handle, &cmds, colour, style)?;
            self.paths = self.paths.saturating_add(1);
        }
        for s in longdoc.overlays {
            for f in s.placement.frames.iter().filter(|f| f.frame_id == layer.layer_id) {
                let Some(pi) = self.page_index(f.spread, f.page) else {
                    return Err(PdfExportError::InvalidInput("frame_page"));
                };
                self.frame_lines(s, f, pi)?;
            }
        }
        Ok(())
    }

    fn table_colour(&mut self, owner: &str, c: &StudioColor, tint: f64) -> Result<Option<hsk_studio_press::Colour>> {
        let rgb = tint_rgb([c.r, c.g, c.b], tint);
        self.colour_of(owner, "table", rgb, &c.colour_profile_id)
    }

    fn table_parts(&mut self, layer: &StudioLayer, t: &PdfTable<'_>) -> Result<()> {
        let p = t.proposal;
        for (k, part) in p.parts.iter().enumerate().filter(|(_, part)| part.frame_id == layer.layer_id) {
            let Some(pi) = self.page_index(part.spread, part.page) else {
                return Err(PdfExportError::InvalidInput("frame_page"));
            };
            let Some(page) = self.pages.get(pi) else { return Err(PdfExportError::InvalidInput("frame_page")) };
            let (handle, ox, oy) = (page.handle, part.origin_x_pt, part.origin_y_pt);
            let map = |page: &super::PageInfo, x: f64, y: f64| page.map(ox + x, oy + y);
            for cell in p.cells.iter().filter(|c| c.part == k) {
                let Some(fill) = &cell.fill else { continue };
                let Some(colour) = self.table_colour(&layer.layer_id, fill, cell.fill_tint)? else { continue };
                let Some(page) = self.pages.get(pi) else { continue };
                let r = &cell.rect;
                let pts = rect(r.x0_pt, r.y0_pt, r.x1_pt, r.y1_pt).map(|(x, y)| map(page, x, y));
                let cmds = [
                    PathCommand::MoveTo(pts[0].0, pts[0].1),
                    PathCommand::LineTo(pts[1].0, pts[1].1),
                    PathCommand::LineTo(pts[2].0, pts[2].1),
                    PathCommand::LineTo(pts[3].0, pts[3].1),
                    PathCommand::Close,
                ];
                self.ex.fill_path(handle, &cmds, FillRule::NonZero, colour)?;
                self.paths = self.paths.saturating_add(1);
            }
            for s in p.strokes.iter().filter(|s| s.part == k) {
                let Some(colour) = self.table_colour(&layer.layer_id, &s.colour, s.tint)? else { continue };
                if !(s.weight_pt.is_finite() && s.weight_pt > 0.0) {
                    self.loss.note(&layer.layer_id, "table", LossKind::Unsupported, "table_stroke_weight");
                    continue;
                }
                let Some(page) = self.pages.get(pi) else { continue };
                let (a, b) = (map(page, s.from[0], s.from[1]), map(page, s.to[0], s.to[1]));
                let cmds = [PathCommand::MoveTo(a.0, a.1), PathCommand::LineTo(b.0, b.1)];
                let style = StrokeStyle { width_pt: s.weight_pt, cap: LineCap::Butt, join: LineJoin::Miter };
                self.ex.stroke_path(handle, &cmds, colour, style)?;
                self.paths = self.paths.saturating_add(1);
            }
        }
        for f in t.cells.placement.frames.iter().filter(|f| f.frame_id == layer.layer_id) {
            let Some(pi) = self.page_index(f.spread, f.page) else {
                return Err(PdfExportError::InvalidInput("frame_page"));
            };
            self.frame_lines(&t.cells, f, pi)?;
        }
        Ok(())
    }

    /// The clip path of a graphic frame in page space (module docs).
    fn frame_clip(&mut self, layer: &StudioLayer, g: &PlacedGraphic, pi: usize) -> Option<Vec<PathCommand>> {
        let f = g.frame;
        let assets = self.input.assets;
        let resolved = match &g.shape {
            StudioFrameShape::Path { path_id } => match assets.path(path_id).filter(|p| p.validate(&PathLimits::default()).is_ok()) {
                Some(p) => Some(p),
                None => {
                    self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "frame_shape_unresolved");
                    None
                }
            },
            _ => None,
        };
        let page = self.pages.get(pi)?;
        let m = |x: f64, y: f64| page.map(x, y);
        let rect_cmds = || {
            let pts = rect(f.x0_pt, f.y0_pt, f.x1_pt, f.y1_pt).map(|(x, y)| m(x, y));
            vec![
                PathCommand::MoveTo(pts[0].0, pts[0].1),
                PathCommand::LineTo(pts[1].0, pts[1].1),
                PathCommand::LineTo(pts[2].0, pts[2].1),
                PathCommand::LineTo(pts[3].0, pts[3].1),
                PathCommand::Close,
            ]
        };
        Some(match (&g.shape, resolved) {
            (StudioFrameShape::Ellipse, _) => {
                const K: f64 = 0.552_284_749_8;
                let (cx, cy) = ((f.x0_pt + f.x1_pt) / 2.0, (f.y0_pt + f.y1_pt) / 2.0);
                let (rx, ry) = ((f.x1_pt - f.x0_pt) / 2.0, (f.y1_pt - f.y0_pt) / 2.0);
                let c = |a: (f64, f64), b: (f64, f64), e: (f64, f64)| PathCommand::CubicTo(a.0, a.1, b.0, b.1, e.0, e.1);
                let start = m(cx + rx, cy);
                vec![
                    PathCommand::MoveTo(start.0, start.1),
                    c(m(cx + rx, cy + K * ry), m(cx + K * rx, cy + ry), m(cx, cy + ry)),
                    c(m(cx - K * rx, cy + ry), m(cx - rx, cy + K * ry), m(cx - rx, cy)),
                    c(m(cx - rx, cy - K * ry), m(cx - K * rx, cy - ry), m(cx, cy - ry)),
                    c(m(cx + K * rx, cy - ry), m(cx + rx, cy - K * ry), m(cx + rx, cy)),
                    PathCommand::Close,
                ]
            }
            (StudioFrameShape::Path { .. }, Some(p)) => super::commands(&p, page),
            _ => rect_cmds(),
        })
    }

    /// A raster layer drawn into its graphic frame (module docs).
    pub(super) fn graphic_layer(&mut self, layer: &StudioLayer, g: &PlacedGraphic) -> Result<()> {
        let assets = self.input.assets;
        let Some(raster) = assets.raster(&layer.layer_id) else {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_unresolved");
            return Ok(());
        };
        let Some(pi) = self.page_index(g.spread, g.page) else {
            return Err(PdfExportError::InvalidInput("graphic_page"));
        };
        let mut valid = Vec::new();
        let (mut px_w, mut px_h) = (0u64, 0u64);
        for tile in &raster.tiles {
            if tile.validate_shape(raster.grid).is_err() || tile.validate_hashes().is_err() {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_tile_invalid");
                continue;
            }
            if hsk_studio_prism::profile_hash(tile.profile.bytes) != tile.profile.expected_sha256 {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_profile_hash");
                continue;
            }
            let b = tile.bounds;
            let (Ok(x), Ok(y)) = (u64::try_from(b.x), u64::try_from(b.y)) else {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_tile_invalid");
                continue;
            };
            px_w = px_w.max(x.saturating_add(u64::from(b.width)));
            px_h = px_h.max(y.saturating_add(u64::from(b.height)));
            valid.push(tile);
        }
        if valid.is_empty() || px_w == 0 || px_h == 0 {
            return Ok(());
        }
        let (kx, ky) = (g.content_size[0] / px_w as f64, g.content_size[1] / px_h as f64);
        let [sx, _, _, sy, tx, ty] = g.transform;
        let Some(clip) = self.frame_clip(layer, g, pi) else { return Err(PdfExportError::InvalidInput("graphic_page")) };
        let Some(handle) = self.pages.get(pi).map(|p| p.handle) else { return Err(PdfExportError::InvalidInput("graphic_page")) };
        self.ex.push_clip(handle, &clip, FillRule::NonZero)?;
        for tile in valid {
            let Some(icc) = self.profile(tile.profile.bytes)? else {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_profile_not_embedded");
                continue;
            };
            let (samples, alpha) = super::quantise(tile, &self.input.limits)?;
            let b = tile.bounds;
            let x0 = sx * (b.x as f64 * kx) + tx;
            let y0 = sy * (b.y as f64 * ky) + ty;
            let (w, h) = (sx * f64::from(b.width) * kx, sy * f64::from(b.height) * ky);
            let spec = ImageSpec { width: b.width, height: b.height, bits: 16, colour: ImageColour::IccBased(icc), samples: &samples, alpha: Some(&alpha) };
            let Some(page) = self.pages.get(pi) else { continue };
            let (px, py) = page.map(x0, y0 + h);
            self.ex.draw_image(handle, spec, [w, 0.0, 0.0, h, px, py])?;
            self.tiles = self.tiles.saturating_add(1);
        }
        self.ex.pop_clip(handle)?;
        Ok(())
    }
}
