//! Long-document content of the PDF lowering (lane SMB-LAYOUT-LONGDOC): everything hsk-studio-layout
//! composes beside a frame's own story is drawn right after that frame layer's text, so it takes the
//! frame's position in the paint order.
//!
//! - Tables ([`PdfTable`]): per table part in the frame, the cell fills (layout `tint_rgb` of the
//!   Folio colour by its tint), then the resolved stroke segments in the proposal's order (butt caps,
//!   miter joins, centred on the cell boundary), then the cell text (`TableProposal::text`, glyphs
//!   and fills as for any story).
//! - Overlays: stories whose placement frames name a frame layer they do not own (footnote bodies,
//!   generated text), drawn after the host frame's text.
//!
//! Colours bound to a profile are written ICCBased as everywhere in this module; unassigned colours
//! are DeviceRGB and reported (`paint_colour_untagged`).
use super::{Cx, PdfExportError, PdfExportInput, PdfStory, Result, check_story_list};
use crate::model::LossKind;
use hsk_studio_folio::{StudioColor, StudioDocument, StudioLayer};
use hsk_studio_layout::{TableProposal, tint_rgb};
use hsk_studio_press::{FillRule, LineCap, LineJoin, PathCommand, StrokeStyle};

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
}
impl<'a> PdfLongDoc<'a> {
    /// Every story this content draws.
    pub(super) fn stories(&self) -> impl Iterator<Item = &PdfStory<'a>> {
        self.tables.iter().map(|t| &t.cells).chain(self.overlays.iter())
    }
    pub(super) fn check(&self, input: &PdfExportInput<'_>, doc: &StudioDocument) -> Result<()> {
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
}
