//! Native document to PDF (lane SMB-LAYOUT-PDF-EXPORT slice 2, decision L1b): lowers a validated
//! Folio snapshot, the layout placements of its threaded stories and the caller's shaped glyph runs
//! onto press's [`PdfExport`] builder. Press owns PDF serialisation; this module owns the mapping.
//!
//! Mapping:
//! - Pages: every page of every page spread, spreads in order, pages left to right, becomes one PDF
//!   page. Media box = trim size plus the page bleed (`inside_or_left` is the left edge of a
//!   right-hand or single-sided page and the right edge of a left-hand page); TrimBox = the page;
//!   BleedBox = the media box when the page declares a bleed. Spread coordinates (pages side by side,
//!   y down, the convention of hsk-studio-layout's `folio_input`) map to PDF user space (y up).
//! - Layers draw in layer-graph order, bottom first (`StudioLayerGraph` outputs in order, each
//!   composite operation's inputs by `input_ordinal`, 0 = bottom: composite's convention, UNVERIFIED
//!   against InDesign/Illustrator stacking). A document without graph outputs (IDML imports build
//!   none) draws in `StudioDocument::layers` order and reports `composition_graph_absent`; with a
//!   graph, layers it does not reach are reported, not drawn. A layer is drawn on every page of its
//!   spread its geometry overlaps.
//! - Text layers: the frame's lines from the story's `LayoutProposal`; each glyph is the caller's
//!   glyph id (Type's `Glyph::glyph_id`, never re-shaped here, STU-TYP-127) placed at the line start
//!   plus the advances layout used, with the line's justification (`word_scale` on whitespace
//!   glyphs, `letter_gap_pt` between glyphs) and the glyph's own offsets. A hyphenated line ends with
//!   the font's hyphen glyph. Fonts are embedded once each, `/W` from the advances and `/ToUnicode`
//!   from the story text of each glyph cluster.
//! - Vector layers: `SVPT` paths from [`PdfAssets::path`] (points, spread coordinates); the solid
//!   fill and stroke rows of the layer's `fill_style_id` / `stroke_style_id` paint styles. Colours
//!   bound to a profile are written ICCBased with the caller's profile bytes; unassigned colours are
//!   written DeviceRGB and reported.
//! - Raster layers: pigment `ResolvedTile`s from [`PdfAssets::raster`] (validated shape and hashes);
//!   linear-light `rgb_f32le` samples are quantised to 16 bits and written ICCBased with the tile's
//!   own profile (no colour conversion: the profile states what the samples mean); the straight
//!   alpha plane becomes the image's `/SMask`.
//! - Everything not drawn is a typed loss entry (`LossEntry`), never a silent drop: appearance
//!   (opacity, blend, masks), gradients, non-centre stroke alignment, dashes, unresolved paths,
//!   tiles or profiles, fonts press refuses, layers outside page spreads, unknown layer kinds.
//!
//! Hostile input (V4-01/02): index arithmetic is checked; tile sample bytes are computed with
//! checked arithmetic and compared with `Limits::max_decoded_bytes` before allocation; every PDF
//! bound is press's. Tests: `tests/pdf_export.rs`.
#![deny(clippy::arithmetic_side_effects)]
use crate::idml_decode::LossBook;
use crate::model::{LossEntry, LossKind};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    GeometryUnit, Length, NodeKind, OperationKind, PageSide, Parent, Payload, PortAddress, ProfileBinding, Snapshot,
    StrokeAlign, StrokeCap, StrokeJoin, StudioAppearanceRow, StudioColor, StudioDesignRecords,
    StudioDocument, StudioLayer, StudioPaint,
};
use hsk_studio_layout::{FramePlacement, LayoutProposal, ShapedParagraph};
use hsk_studio_nib::{OwnedPath, PathElement, PathLimits};
use hsk_studio_pigment::{Grid, ResolvedTile};
use hsk_studio_press::{
    Colour, ExportError, ExportOptions, ExportReceipt, FillRule, FontHandle, IccHandle, ImageColour,
    ImageSpec, Limits, LineCap, LineJoin, PageHandle, PageSpec, PathCommand, PdfExport, StrokeStyle,
};
use std::collections::BTreeMap;

/// Deepest parent chain followed from a layer to its page spread.
const MAX_PARENT_DEPTH: usize = 64;
/// Longest `/ToUnicode` text of one cluster (press refuses longer).
const MAX_CLUSTER_CHARS: usize = 16;
/// `StudioBlendMode::Normal` discriminant (hsk-studio-pigment `blend_math`).
const BLEND_NORMAL: u16 = 2;

/// A font the glyph runs use: the same bytes the shaper used.
#[derive(Clone, Copy, Debug)]
pub struct PdfFont<'a> {
    pub bytes: &'a [u8],
    pub face_index: u32,
    /// Glyph id and advance (1/1000 em) of the hyphen drawn at the end of a hyphenated line.
    pub hyphen: Option<(u16, u16)>,
}

/// One shaped glyph, parallel to `ShapedParagraph::glyphs` (same index).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfGlyph {
    pub glyph_id: u16,
    /// Index into `PdfExportInput::fonts`.
    pub font: usize,
    pub size_pt: f64,
    /// Type's `x_offset_pt` / `y_offset_pt` (y up).
    pub x_offset_pt: f64,
    pub y_offset_pt: f64,
}

/// One story with its layout placement and the glyph runs it was composed from.
#[derive(Clone, Copy, Debug)]
pub struct PdfStory<'a> {
    /// Folio `STXT` id.
    pub story_id: &'a str,
    /// The whole story text (cluster byte ranges index into it).
    pub text: &'a str,
    pub placement: &'a LayoutProposal,
    /// The paragraphs `placement` was composed from.
    pub paragraphs: &'a [ShapedParagraph],
    /// Per paragraph, one entry per shaped glyph.
    pub glyphs: &'a [Vec<PdfGlyph>],
    /// Text colour (Folio type styles carry no lowered fill here; the caller resolves it).
    pub fill: Colour,
}

/// A raster layer's tiles and where pixel (0, 0) of its grid sits.
pub struct PlacedRaster<'a> {
    pub grid: Grid,
    pub tiles: Vec<ResolvedTile<'a>>,
    /// Spread coordinates (points, y down) of grid pixel (0, 0).
    pub origin_pt: [f64; 2],
    pub pt_per_px: f64,
}

/// Caller-owned resolution of what Folio references by id.
pub trait PdfAssets {
    /// `SVPT` geometry in points, spread coordinates, y down.
    fn path(&self, path_id: &str) -> Option<OwnedPath>;
    /// The decoded tiles of a raster layer.
    fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>>;
    /// ICC bytes of an `SCPF` profile id (colours bound to it are written ICCBased).
    fn profile(&self, profile_id: &str) -> Option<&[u8]>;
}

pub struct PdfExportInput<'a> {
    pub snapshot: &'a Snapshot,
    pub design: Option<&'a StudioDesignRecords>,
    pub stories: &'a [PdfStory<'a>],
    pub fonts: &'a [PdfFont<'a>],
    pub assets: &'a dyn PdfAssets,
    pub options: ExportOptions<'a>,
    pub limits: Limits,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfExportOutput {
    pub bytes: Vec<u8>,
    pub receipt: ExportReceipt,
    /// Everything not drawn or drawn approximately (sorted, counted).
    pub loss: Vec<LossEntry>,
    /// Folio `page_id` of each PDF page, in order.
    pub pages: Vec<String>,
    pub glyphs_drawn: u64,
    pub paths_drawn: u64,
    pub tiles_drawn: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfExportError {
    Canceled,
    /// A placement was composed against another document revision.
    StaleRevision { expected: u64, current: u64 },
    /// The input does not line up (`what` names the check).
    InvalidInput(&'static str),
    /// Press refused (budgets, limits, serialisation).
    Press(ExportError),
}
impl PdfExportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidInput(_) => "invalid_input",
            Self::Press(e) => e.code(),
        }
    }
}
impl std::fmt::Display for PdfExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(w) => write!(f, "invalid_input: {w}"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for PdfExportError {}
impl From<ExportError> for PdfExportError {
    fn from(e: ExportError) -> Self {
        match e {
            ExportError::Canceled => Self::Canceled,
            other => Self::Press(other),
        }
    }
}

type Result<T> = std::result::Result<T, PdfExportError>;

struct PageInfo {
    spread: usize,
    handle: PageHandle,
    /// Spread x of the page's left trim edge.
    page_x: f64,
    w: f64,
    h: f64,
    /// Bleed left, bottom, right, top.
    bleed: [f64; 4],
}
impl PageInfo {
    /// Spread point (y down) to PDF page space (y up).
    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        (x - self.page_x + self.bleed[0], self.bleed[1] + self.h - y)
    }
    /// Whether the spread x range [x0, x1] overlaps the page including its bleed.
    fn overlaps_x(&self, x0: f64, x1: f64) -> bool {
        x1 >= self.page_x - self.bleed[0] && x0 <= self.page_x + self.w + self.bleed[2]
    }
}

fn pt(l: &Length) -> Result<f64> {
    if l.unit == GeometryUnit::Pt && l.value.is_finite() {
        Ok(l.value)
    } else {
        Err(PdfExportError::InvalidInput("length_not_pt"))
    }
}

fn canceled(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| PdfExportError::Canceled)
}

struct Cx<'a> {
    input: &'a PdfExportInput<'a>,
    doc: &'a StudioDocument,
    ex: PdfExport,
    pages: Vec<PageInfo>,
    fonts: Vec<Option<FontHandle>>,
    profiles: BTreeMap<[u8; 32], Option<IccHandle>>,
    loss: LossBook,
    glyphs: u64,
    paths: u64,
    tiles: u64,
}

/// Lowers the snapshot onto a fresh PDF (module docs). Only cancellation, revision mismatches,
/// misaligned inputs and press refusals fail the call; everything else is a loss entry.
pub fn export_pdf(input: &PdfExportInput<'_>, cancel: &CancellationToken) -> Result<PdfExportOutput> {
    canceled(cancel)?;
    let doc = input.snapshot.document();
    check_stories(input, doc)?;
    let mut cx = Cx {
        input,
        doc,
        ex: PdfExport::new(input.options, &input.limits)?,
        pages: Vec::new(),
        fonts: Vec::new(),
        profiles: BTreeMap::new(),
        loss: LossBook::default(),
        glyphs: 0,
        paths: 0,
        tiles: 0,
    };
    let page_ids = cx.add_pages()?;
    cx.embed_fonts()?;
    let by_id: BTreeMap<&str, &StudioLayer> = doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let order = paint_order(input.snapshot, &mut cx.loss);
    cx.draw_nodes(&order, &by_id, cancel)?;
    let Cx { ex, loss, glyphs, paths, tiles, .. } = cx;
    let (bytes, receipt) = ex.finish(cancel)?;
    Ok(PdfExportOutput {
        bytes,
        receipt,
        loss: loss.finish(),
        pages: page_ids,
        glyphs_drawn: glyphs,
        paths_drawn: paths,
        tiles_drawn: tiles,
    })
}

fn check_stories(input: &PdfExportInput<'_>, doc: &StudioDocument) -> Result<()> {
    for s in input.stories {
        if s.placement.base_revision != doc.revision {
            return Err(PdfExportError::StaleRevision { expected: s.placement.base_revision, current: doc.revision });
        }
        if s.glyphs.len() != s.paragraphs.len() {
            return Err(PdfExportError::InvalidInput("glyphs_misaligned"));
        }
        for (gs, p) in s.glyphs.iter().zip(s.paragraphs) {
            if gs.len() != p.glyphs.len() {
                return Err(PdfExportError::InvalidInput("glyphs_misaligned"));
            }
            if gs.iter().any(|g| g.font >= input.fonts.len()) {
                return Err(PdfExportError::InvalidInput("font_index"));
            }
            if gs.iter().any(|g| !(g.size_pt.is_finite() && g.size_pt > 0.0 && g.x_offset_pt.is_finite() && g.y_offset_pt.is_finite())) {
                return Err(PdfExportError::InvalidInput("glyph_metrics"));
            }
        }
        for f in &s.placement.frames {
            for l in &f.lines {
                let ok = s.glyphs.get(l.paragraph).is_some_and(|g| l.glyph_range.start <= l.glyph_range.end && l.glyph_range.end <= g.len());
                if !ok {
                    return Err(PdfExportError::InvalidInput("line_glyph_range"));
                }
            }
        }
    }
    Ok(())
}

/// One entry of the paint order: a layer, or a composite (group) layer with its ordered inputs.
enum Node {
    Layer(usize),
    Group(usize, Vec<Node>),
}

/// Bottom-first paint order from the layer graph (Folio `StudioLayerGraph`, composite convention:
/// `outputs[0]` bottom-most, a composite operation's inputs by `input_ordinal`, 0 = bottom). A
/// document whose graph has no output is drawn in `StudioDocument::layers` order and reported
/// (`composition_graph_absent`, as IDML imports build no graph); with a graph, a layer it never
/// reaches is not drawn and is reported (`layer_not_in_composition_graph`), like composite.
fn paint_order(snap: &Snapshot, loss: &mut LossBook) -> Vec<Node> {
    let doc = snap.document();
    if doc.graph.outputs.is_empty() {
        if !doc.layers.is_empty() {
            loss.note("document", "graph", LossKind::Approximated, "composition_graph_absent");
        }
        return (0..doc.layers.len()).map(Node::Layer).collect();
    }
    let index: BTreeMap<&str, usize> = doc.layers.iter().enumerate().map(|(i, l)| (l.layer_id.as_str(), i)).collect();
    let mut seen = vec![false; doc.layers.len()];
    let mut out = Vec::new();
    for addr in &doc.graph.outputs {
        if let Some(n) = visit(snap, &index, &mut seen, addr, 0) {
            out.push(n);
        }
    }
    for (i, l) in doc.layers.iter().enumerate() {
        if !seen.get(i).copied().unwrap_or(true) && !matches!(l.payload, Payload::Group) {
            loss.note(&l.layer_id, &l.kind, LossKind::Unsupported, "layer_not_in_composition_graph");
        }
    }
    out
}

fn visit(snap: &Snapshot, index: &BTreeMap<&str, usize>, seen: &mut [bool], addr: &PortAddress, depth: usize) -> Option<Node> {
    if depth > MAX_PARENT_DEPTH {
        return None;
    }
    let i = *index.get(addr.layer_id.as_str())?;
    let slot = seen.get_mut(i)?;
    if *slot {
        return None;
    }
    *slot = true;
    let op = snap
        .document()
        .graph
        .operations
        .iter()
        .find(|o| o.layer_id == addr.layer_id && o.operation_key == addr.operation_key)?;
    if op.operation_kind != OperationKind::Composite {
        return Some(Node::Layer(i));
    }
    let mut children = Vec::new();
    for port in &op.inputs {
        let at = PortAddress { layer_id: op.layer_id.clone(), operation_key: op.operation_key.clone(), port_key: port.port_key.clone() };
        for src in snap.ordered_inputs(&at) {
            if let Some(n) = visit(snap, index, seen, src, depth.saturating_add(1)) {
                children.push(n);
            }
        }
    }
    Some(Node::Group(i, children))
}

impl Cx<'_> {
    fn draw_nodes(&mut self, nodes: &[Node], by_id: &BTreeMap<&str, &StudioLayer>, cancel: &CancellationToken) -> Result<()> {
        let doc = self.doc;
        for node in nodes {
            canceled(cancel)?;
            match node {
                Node::Group(i, children) => {
                    let Some(layer) = doc.layers.get(*i) else { continue };
                    if !layer.visible {
                        continue;
                    }
                    if layer.appearance.is_some() {
                        self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "layer_appearance_ignored");
                    }
                    self.draw_nodes(children, by_id, cancel)?;
                }
                Node::Layer(i) => {
                    let Some(layer) = doc.layers.get(*i) else { continue };
                    self.draw_layer(layer, by_id)?;
                }
            }
        }
        Ok(())
    }

    fn draw_layer(&mut self, layer: &StudioLayer, by_id: &BTreeMap<&str, &StudioLayer>) -> Result<()> {
        if !layer.visible || matches!(layer.payload, Payload::Group) {
            return Ok(());
        }
        let Some(spread) = spread_of(self.doc, by_id, layer) else {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "layer_not_on_page_spread");
            return Ok(());
        };
        if layer.appearance.is_some() {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "layer_appearance_ignored");
        }
        match &layer.payload {
            Payload::Text { story_id } => self.text_layer(layer, story_id),
            Payload::Vector { path_ids } => self.vector_layer(layer, spread, path_ids),
            Payload::Raster { .. } => self.raster_layer(layer, spread),
            _ => {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "layer_kind_not_exported");
                Ok(())
            }
        }
    }
}

/// Index of the page spread a layer belongs to (through group parents).
fn spread_of(doc: &StudioDocument, by_id: &BTreeMap<&str, &StudioLayer>, layer: &StudioLayer) -> Option<usize> {
    let mut parent = &layer.parent;
    for _ in 0..MAX_PARENT_DEPTH {
        match parent {
            Parent::Root(()) => return None,
            Parent::Node(n) => match n.node_kind {
                NodeKind::PageSpread => return doc.page_spreads.iter().position(|s| s.page_spread_id == n.node_id),
                NodeKind::Artboard => return None,
                NodeKind::Layer => parent = &by_id.get(n.node_id.as_str())?.parent,
            },
        }
    }
    None
}

impl Cx<'_> {
    fn add_pages(&mut self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for (si, spread) in self.doc.page_spreads.iter().enumerate() {
            let mut page_x = 0.0;
            for page in &spread.pages {
                let (w, h) = (pt(&page.width)?, pt(&page.height)?);
                let bleed = match &page.bleed {
                    None => [0.0; 4],
                    Some(b) => {
                        let (inside, outside) = (pt(&b.inside_or_left)?, pt(&b.outside_or_right)?);
                        let (left, right) = if page.side == PageSide::LeftHand { (outside, inside) } else { (inside, outside) };
                        [left, pt(&b.bottom)?, right, pt(&b.top)?]
                    }
                };
                if bleed.iter().any(|v| *v < 0.0) {
                    return Err(PdfExportError::InvalidInput("negative_bleed"));
                }
                let media = [0.0, 0.0, bleed[0] + w + bleed[2], bleed[1] + h + bleed[3]];
                let spec = PageSpec {
                    media_box_pt: media,
                    trim_box_pt: Some([bleed[0], bleed[1], bleed[0] + w, bleed[1] + h]),
                    bleed_box_pt: page.bleed.as_ref().map(|_| media),
                };
                let handle = self.ex.add_page(spec)?;
                self.pages.push(PageInfo { spread: si, handle, page_x, w, h, bleed });
                ids.push(page.page_id.clone());
                page_x += w;
            }
        }
        Ok(ids)
    }

    /// Embeds every font the stories use, with `/W` and `/ToUnicode` gathered from all glyphs. A
    /// font press refuses is a loss; its glyphs are not drawn.
    fn embed_fonts(&mut self) -> Result<()> {
        let fonts = self.input.fonts;
        let mut widths: Vec<BTreeMap<u16, u16>> = vec![BTreeMap::new(); fonts.len()];
        let mut unicode: Vec<BTreeMap<u16, String>> = vec![BTreeMap::new(); fonts.len()];
        let mut used = vec![false; fonts.len()];
        for s in self.input.stories {
            for (gs, p) in s.glyphs.iter().zip(s.paragraphs) {
                let mut last_cluster = None;
                for (g, lg) in gs.iter().zip(&p.glyphs) {
                    let (Some(w), Some(u), Some(flag)) = (widths.get_mut(g.font), unicode.get_mut(g.font), used.get_mut(g.font)) else {
                        continue;
                    };
                    *flag = true;
                    let em = lg.advance_pt / g.size_pt * 1000.0;
                    if em.is_finite() && (0.0..=65535.0).contains(&em) {
                        w.entry(g.glyph_id).or_insert(em.round() as u16);
                    }
                    let cluster = (lg.cluster.start, lg.cluster.end);
                    if last_cluster != Some(cluster) {
                        last_cluster = Some(cluster);
                        let text = usize::try_from(cluster.0)
                            .ok()
                            .zip(usize::try_from(cluster.1).ok())
                            .and_then(|(a, b)| s.text.get(a..b));
                        match text {
                            Some(t) if !t.is_empty() && t.chars().count() <= MAX_CLUSTER_CHARS => {
                                u.entry(g.glyph_id).or_insert_with(|| t.to_owned());
                            }
                            _ => self.loss.note(s.story_id, "text", LossKind::Approximated, "to_unicode_cluster_skipped"),
                        }
                    }
                }
            }
        }
        for (i, f) in fonts.iter().enumerate() {
            if !used.get(i).copied().unwrap_or(false) {
                self.fonts.push(None);
                continue;
            }
            let mut w: Vec<(u16, u16)> = widths.get(i).map(|m| m.iter().map(|(a, b)| (*a, *b)).collect()).unwrap_or_default();
            if let Some(h) = f.hyphen {
                w.push(h);
            }
            let u: Vec<(u16, &str)> = unicode.get(i).map(|m| m.iter().map(|(a, b)| (*a, b.as_str())).collect()).unwrap_or_default();
            match self.ex.embed_truetype(f.bytes, f.face_index, &w, &u) {
                Ok(h) => self.fonts.push(Some(h)),
                Err(
                    e @ (ExportError::FontUnreadable
                    | ExportError::FontCffNotEmbedded
                    | ExportError::FontCollectionNotEmbedded
                    | ExportError::FontEmbeddingRestricted
                    | ExportError::FontGlyphOutOfRange
                    | ExportError::ToUnicodeInvalid),
                ) => {
                    self.loss.note(&format!("font#{i}"), "font", LossKind::Unsupported, e.code());
                    self.fonts.push(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn page_index(&self, spread: usize, page: usize) -> Option<usize> {
        let first = self.pages.iter().position(|p| p.spread == spread)?;
        let i = first.checked_add(page)?;
        self.pages.get(i).filter(|p| p.spread == spread).map(|_| i)
    }

    fn text_layer(&mut self, layer: &StudioLayer, story_id: &str) -> Result<()> {
        let Some(story) = self.input.stories.iter().find(|s| s.story_id == story_id) else {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "story_not_supplied");
            return Ok(());
        };
        let story = *story;
        let Some(frame) = story.placement.frames.iter().find(|f| f.frame_id == layer.layer_id) else {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "frame_not_placed");
            return Ok(());
        };
        let Some(pi) = self.page_index(frame.spread, frame.page) else {
            return Err(PdfExportError::InvalidInput("frame_page"));
        };
        self.frame_lines(&story, frame, pi)
    }

    fn frame_lines(&mut self, story: &PdfStory<'_>, frame: &FramePlacement, pi: usize) -> Result<()> {
        // (font, size bits) -> pending glyph run on this frame.
        let mut run: Vec<(u16, f64, f64)> = Vec::new();
        let mut run_key: Option<(usize, u64)> = None;
        for line in &frame.lines {
            let (Some(glyphs), Some(para)) = (story.glyphs.get(line.paragraph), story.paragraphs.get(line.paragraph)) else {
                continue;
            };
            let (scale, gap) = line.justification.map_or((1.0, 0.0), |j| (j.word_scale, j.letter_gap_pt));
            let mut x = frame.origin_x_pt + line.x_start_pt;
            let y = frame.origin_y_pt + line.baseline_y_pt;
            let mut last: Option<PdfGlyph> = None;
            for idx in line.glyph_range.clone() {
                let (Some(g), Some(lg)) = (glyphs.get(idx), para.glyphs.get(idx)) else { continue };
                let space = usize::try_from(lg.cluster.start)
                    .ok()
                    .zip(usize::try_from(lg.cluster.end).ok())
                    .and_then(|(a, b)| story.text.get(a..b))
                    .is_some_and(|t| !t.is_empty() && t.chars().all(char::is_whitespace));
                self.place(&mut run, &mut run_key, story, pi, *g, x + g.x_offset_pt, y - g.y_offset_pt)?;
                x += lg.advance_pt * if space { scale } else { 1.0 };
                if idx.checked_add(1).is_some_and(|n| n < line.glyph_range.end) {
                    x += gap;
                }
                last = Some(*g);
            }
            if line.hyphenated {
                match last.and_then(|g| self.input.fonts.get(g.font).and_then(|f| f.hyphen).map(|h| (g, h))) {
                    Some((g, (hid, _))) => {
                        let hy = PdfGlyph { glyph_id: hid, x_offset_pt: 0.0, y_offset_pt: 0.0, ..g };
                        self.place(&mut run, &mut run_key, story, pi, hy, x, y)?;
                    }
                    None => self.loss.note(story.story_id, "text", LossKind::Approximated, "hyphen_glyph_missing"),
                }
            }
        }
        self.flush(&mut run, run_key, story, pi)
    }

    /// Adds one glyph (spread coordinates) to the pending run, flushing on a font or size change.
    #[allow(clippy::too_many_arguments)]
    fn place(&mut self, run: &mut Vec<(u16, f64, f64)>, key: &mut Option<(usize, u64)>, story: &PdfStory<'_>, pi: usize, g: PdfGlyph, x: f64, y: f64) -> Result<()> {
        if self.fonts.get(g.font).copied().flatten().is_none() {
            self.loss.note(story.story_id, "text", LossKind::Unsupported, "glyph_font_not_embedded");
            return Ok(());
        }
        let k = (g.font, g.size_pt.to_bits());
        if *key != Some(k) {
            self.flush(run, *key, story, pi)?;
            *key = Some(k);
        }
        let Some(page) = self.pages.get(pi) else { return Err(PdfExportError::InvalidInput("frame_page")) };
        let (px, py) = page.map(x, y);
        run.push((g.glyph_id, px, py));
        Ok(())
    }

    fn flush(&mut self, run: &mut Vec<(u16, f64, f64)>, key: Option<(usize, u64)>, story: &PdfStory<'_>, pi: usize) -> Result<()> {
        if run.is_empty() {
            return Ok(());
        }
        let Some((font, bits)) = key else { return Ok(()) };
        let (Some(Some(handle)), Some(page)) = (self.fonts.get(font).copied(), self.pages.get(pi)) else {
            run.clear();
            return Ok(());
        };
        self.ex.show_glyphs(page.handle, handle, f64::from_bits(bits), [0.0, 0.0], story.fill, run)?;
        self.glyphs = self.glyphs.saturating_add(u64::try_from(run.len()).unwrap_or(u64::MAX));
        run.clear();
        Ok(())
    }

    /// ICCBased handle for profile bytes (embedded once per content hash); `None` when press refuses.
    fn profile(&mut self, bytes: &[u8]) -> Result<Option<IccHandle>> {
        let key = hsk_studio_prism::profile_hash(bytes);
        if let Some(h) = self.profiles.get(&key) {
            return Ok(*h);
        }
        let h = match self.ex.embed_icc(bytes, 3) {
            Ok(h) => Some(h),
            Err(ExportError::IccUnreadable | ExportError::IccComponentsMismatch) => None,
            Err(e) => return Err(e.into()),
        };
        self.profiles.insert(key, h);
        Ok(h)
    }

    fn colour(&mut self, layer: &StudioLayer, c: &StudioColor) -> Result<Option<Colour>> {
        let rgb = [c.r, c.g, c.b];
        if !rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "paint_colour_out_of_range");
            return Ok(None);
        }
        match &c.colour_profile_id {
            ProfileBinding::Unassigned(()) => {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "paint_colour_untagged");
                Ok(Some(Colour::DeviceRgb(rgb)))
            }
            ProfileBinding::Assigned(id) => {
                let assets = self.input.assets;
                let handle = match assets.profile(id) {
                    Some(bytes) => self.profile(bytes)?,
                    None => None,
                };
                match handle {
                    Some(h) => Ok(Some(Colour::IccBased(h, [rgb[0], rgb[1], rgb[2], 0.0]))),
                    None => {
                        self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "paint_profile_unresolved");
                        Ok(Some(Colour::DeviceRgb(rgb)))
                    }
                }
            }
        }
    }

    /// The appearance rows of the layer's fill and stroke paint styles, bottom first.
    fn rows(&mut self, layer: &StudioLayer) -> Vec<StudioAppearanceRow> {
        let Some(design) = self.input.design else { return Vec::new() };
        let Some(node) = design.node_design(&layer.layer_id) else { return Vec::new() };
        let mut out = Vec::new();
        for field in ["fill_style_id", "stroke_style_id"] {
            let Some(id) = node.style_refs.get(field) else { continue };
            match design.paint_styles.iter().find(|s| &s.style_id == id) {
                Some(style) => out.extend(style.rows.iter().cloned()),
                None => self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "paint_style_unresolved"),
            }
        }
        out
    }

    fn vector_layer(&mut self, layer: &StudioLayer, spread: usize, path_ids: &[String]) -> Result<()> {
        let assets = self.input.assets;
        let mut paths = Vec::with_capacity(path_ids.len());
        for id in path_ids {
            match assets.path(id).filter(|p| p.validate(&PathLimits::default()).is_ok()) {
                Some(p) => paths.push(p),
                None => {
                    self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "vector_path_unresolved");
                    return Ok(());
                }
            }
        }
        let Some((x0, x1)) = x_extent(&paths) else { return Ok(()) };
        let rows = self.rows(layer);
        for row in rows {
            let (paint, opacity, blend, visible, stroke) = match &row {
                StudioAppearanceRow::Fill { paint, opacity, blend_mode, visible } => (paint, *opacity, *blend_mode, *visible, None),
                StudioAppearanceRow::Stroke { paint, stroke, opacity, blend_mode, visible } => (paint, *opacity, *blend_mode, *visible, Some(stroke)),
            };
            if !visible {
                continue;
            }
            if opacity != 100.0 || blend != BLEND_NORMAL {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "paint_opacity_or_blend_ignored");
            }
            let StudioPaint::Solid { colour } = paint else {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "paint_gradient");
                continue;
            };
            let Some(colour) = self.colour(layer, colour)? else { continue };
            let style = match stroke {
                None => None,
                Some(s) => {
                    let Ok(width) = pt(&s.weight) else {
                        self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "stroke_weight_unit");
                        continue;
                    };
                    if s.align != StrokeAlign::Center {
                        self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "stroke_align_drawn_centred");
                    }
                    if !s.dash.is_empty() {
                        self.loss.note(&layer.layer_id, &layer.kind, LossKind::Approximated, "stroke_dash_ignored");
                    }
                    Some(StrokeStyle {
                        width_pt: width,
                        cap: match s.cap {
                            StrokeCap::Butt => LineCap::Butt,
                            StrokeCap::Round => LineCap::Round,
                            StrokeCap::Projecting => LineCap::Square,
                        },
                        join: match s.join {
                            StrokeJoin::Miter => LineJoin::Miter,
                            StrokeJoin::Round => LineJoin::Round,
                            StrokeJoin::Bevel => LineJoin::Bevel,
                        },
                    })
                }
            };
            for pi in 0..self.pages.len() {
                let Some(page) = self.pages.get(pi).filter(|p| p.spread == spread && p.overlaps_x(x0, x1)) else { continue };
                let handle = page.handle;
                let mapped: Vec<(Vec<PathCommand>, FillRule)> =
                    paths.iter().map(|p| (commands(p, page), fill_rule(p))).collect();
                for (cmds, rule) in &mapped {
                    match style {
                        None => self.ex.fill_path(handle, cmds, *rule, colour)?,
                        Some(st) => self.ex.stroke_path(handle, cmds, colour, st)?,
                    }
                    self.paths = self.paths.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    fn raster_layer(&mut self, layer: &StudioLayer, spread: usize) -> Result<()> {
        let assets = self.input.assets;
        let Some(raster) = assets.raster(&layer.layer_id) else {
            self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_unresolved");
            return Ok(());
        };
        if !(raster.pt_per_px.is_finite() && raster.pt_per_px > 0.0 && raster.origin_pt.iter().all(|v| v.is_finite())) {
            return Err(PdfExportError::InvalidInput("raster_placement"));
        }
        for tile in &raster.tiles {
            if tile.validate_shape(raster.grid).is_err() || tile.validate_hashes().is_err() {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_tile_invalid");
                continue;
            }
            if hsk_studio_prism::profile_hash(tile.profile.bytes) != tile.profile.expected_sha256 {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_profile_hash");
                continue;
            }
            let Some(icc) = self.profile(tile.profile.bytes)? else {
                self.loss.note(&layer.layer_id, &layer.kind, LossKind::Unsupported, "raster_profile_not_embedded");
                continue;
            };
            let (samples, alpha) = quantise(tile, &self.input.limits)?;
            let b = tile.bounds;
            let k = raster.pt_per_px;
            let x0 = raster.origin_pt[0] + b.x as f64 * k;
            let y0 = raster.origin_pt[1] + b.y as f64 * k;
            let (w, h) = (f64::from(b.width) * k, f64::from(b.height) * k);
            let spec = ImageSpec {
                width: b.width,
                height: b.height,
                bits: 16,
                colour: ImageColour::IccBased(icc),
                samples: &samples,
                alpha: Some(&alpha),
            };
            for pi in 0..self.pages.len() {
                let Some(page) = self.pages.get(pi).filter(|p| p.spread == spread && p.overlaps_x(x0, x0 + w)) else { continue };
                let (px, py) = page.map(x0, y0 + h);
                let handle = page.handle;
                self.ex.draw_image(handle, spec, [w, 0.0, 0.0, h, px, py])?;
                self.tiles = self.tiles.saturating_add(1);
            }
        }
        Ok(())
    }
}

/// Linear `rgb_f32le` colour and `f32le` alpha planes (already validated by pigment) to 16-bit
/// big-endian samples; non-finite samples read as 0, everything is clamped to 0..=1. The output
/// size is checked against `Limits::max_decoded_bytes` before allocation.
fn quantise(tile: &ResolvedTile<'_>, limits: &Limits) -> Result<(Vec<u8>, Vec<u8>)> {
    let too_big = || PdfExportError::Press(ExportError::StreamOverBudget);
    let (w, h) = (usize::try_from(tile.bounds.width).map_err(|_| too_big())?, usize::try_from(tile.bounds.height).map_err(|_| too_big())?);
    let pixels = w.checked_mul(h).ok_or_else(too_big)?;
    let colour_len = pixels.checked_mul(6).ok_or_else(too_big)?;
    if colour_len > limits.max_decoded_bytes {
        return Err(too_big());
    }
    let (cs, as_) = (usize::try_from(tile.colour_stride_bytes).map_err(|_| too_big())?, usize::try_from(tile.alpha_stride_bytes).map_err(|_| too_big())?);
    let q = |b: &[u8]| -> [u8; 2] {
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        ((v * 65535.0).round() as u16).to_be_bytes()
    };
    let mut colour = Vec::with_capacity(colour_len);
    let mut alpha = Vec::with_capacity(pixels.checked_mul(2).ok_or_else(too_big)?);
    for row in 0..h {
        let (cr, ar) = (row.checked_mul(cs).ok_or_else(too_big)?, row.checked_mul(as_).ok_or_else(too_big)?);
        for col in 0..w {
            let c = cr.checked_add(col.checked_mul(12).ok_or_else(too_big)?).ok_or_else(too_big)?;
            let px = tile.colour_bytes.get(c..c.saturating_add(12)).ok_or(PdfExportError::InvalidInput("raster_plane"))?;
            for ch in px.chunks_exact(4) {
                colour.extend_from_slice(&q(ch));
            }
            let a = ar.checked_add(col.checked_mul(4).ok_or_else(too_big)?).ok_or_else(too_big)?;
            let av = tile.alpha_bytes.get(a..a.saturating_add(4)).ok_or(PdfExportError::InvalidInput("raster_plane"))?;
            alpha.extend_from_slice(&q(av));
        }
    }
    Ok((colour, alpha))
}

fn fill_rule(p: &OwnedPath) -> FillRule {
    match p.fill_rule {
        hsk_studio_nib::FillRule::NonZero => FillRule::NonZero,
        hsk_studio_nib::FillRule::EvenOdd => FillRule::EvenOdd,
    }
}

/// Spread x extent of the paths' points (control points included: a conservative bound).
fn x_extent(paths: &[OwnedPath]) -> Option<(f64, f64)> {
    let mut xs = paths.iter().flat_map(|p| p.elements.iter()).flat_map(|e| match *e {
        PathElement::MoveTo(a) | PathElement::LineTo(a) => vec![a.x],
        PathElement::QuadTo(a, b) => vec![a.x, b.x],
        PathElement::CubicTo(a, b, c) => vec![a.x, b.x, c.x],
        PathElement::Close => vec![],
    });
    let first = xs.next()?;
    Some(xs.fold((first, first), |(lo, hi), x| (lo.min(x), hi.max(x))))
}

/// Nib path elements to press commands in page space; a quadratic becomes the exact cubic
/// (control points at 2/3 toward the quadratic control point).
fn commands(p: &OwnedPath, page: &PageInfo) -> Vec<PathCommand> {
    let mut out = Vec::with_capacity(p.elements.len());
    let (mut cur, mut start) = ((0.0, 0.0), (0.0, 0.0));
    for e in &p.elements {
        match *e {
            PathElement::MoveTo(a) => {
                cur = (a.x, a.y);
                start = cur;
                let (x, y) = page.map(a.x, a.y);
                out.push(PathCommand::MoveTo(x, y));
            }
            PathElement::LineTo(a) => {
                cur = (a.x, a.y);
                let (x, y) = page.map(a.x, a.y);
                out.push(PathCommand::LineTo(x, y));
            }
            PathElement::QuadTo(q, a) => {
                let c1 = (cur.0 + 2.0 / 3.0 * (q.x - cur.0), cur.1 + 2.0 / 3.0 * (q.y - cur.1));
                let c2 = (a.x + 2.0 / 3.0 * (q.x - a.x), a.y + 2.0 / 3.0 * (q.y - a.y));
                let (p1, p2, p3) = (page.map(c1.0, c1.1), page.map(c2.0, c2.1), page.map(a.x, a.y));
                out.push(PathCommand::CubicTo(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1));
                cur = (a.x, a.y);
            }
            PathElement::CubicTo(c1, c2, a) => {
                let (p1, p2, p3) = (page.map(c1.x, c1.y), page.map(c2.x, c2.y), page.map(a.x, a.y));
                out.push(PathCommand::CubicTo(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1));
                cur = (a.x, a.y);
            }
            PathElement::Close => {
                out.push(PathCommand::Close);
                cur = start;
            }
        }
    }
    out
}
