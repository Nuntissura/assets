//! Illustrator `.ai` import through its PDF-compatible stream (lane SMB-ILLUSTRATOR-INTEROP;
//! STU-IO-186). PDF decoding is Press's: a caller-supplied [`PdfDrawingProvider`] returns what
//! every page draws (a host adapts Press `read_vector_pages`); this module turns that drawing into
//! a Folio proposal, loss-aware:
//!
//! - every page is an artboard (laid out left to right with a 20 pt gap, Studio rule), its content
//!   a group layer; PDF user space (pt, y up) becomes document points (y down) from the crop box;
//! - every draw is a vector layer of nib anchor paths with a PAINT style: a fill row (non-zero or
//!   even-odd winding on the paths), a stroke row (weight, cap, join, miter limit, dash in pt); a
//!   stroke that repeats the previous fill's path joins that layer (PDF `B`); solid colours are
//!   the reader's sRGB with the alpha as row opacity, axial / radial shadings become Folio linear /
//!   radial gradients (sampled stops; the shading matrix and the page flip as the gradient
//!   transform);
//! - transparency groups become group layers (opacity, blend); a clip becomes a group with a Folio
//!   vector clip whose source is a hidden vector layer holding the clip path;
//! - text arrives as glyph outlines (loss `ai_text_as_outlines`, text as text is a declared gap);
//! - Illustrator private data and native editability are typed losses (`ai_private_data_not_imported`,
//!   `ai_native_editability_not_imported`): a PDF reading is never native editability; every reader
//!   note is a loss under its own code.
#![deny(clippy::arithmetic_side_effects)]
use crate::ai_adapter::{AiKind, identify_ai};
use crate::error::{PreservedBlob, Result, VectorError};
use crate::ids::IdMint;
use crate::model::*;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{
    ArtboardSchema, BLEND_NORMAL, Bounds, Budget, DocumentSchema, GeometryUnit, GradientInterpolation, GradientSpread, GraphSchema, Isolation,
    LayerAppearance, LayerSchema, Length as FolioLength, NodeKind, NodeRef, Parent, Payload, ProfileBinding, Resolution, Resolver, StrokeAlign,
    StrokeCap, StrokeJoin, StudioAppearanceRow, StudioArtboard, StudioClipRule, StudioColor, StudioDesignRecords, StudioDocument, StudioGradient,
    StudioGradientGeometry, StudioGradientStop, StudioLayer, StudioLayerGraph, StudioMaskAttachment, StudioNodeDesign, StudioPaint,
    StudioPaintStyle, StudioPublishing, StudioStrokeStyle, StudioVectorClip, StudioVectorRecords, StyleKind, StyleRegistrySchema, TileRef,
    Unresolved, validate_design_records, validate_document, validate_vector_records,
};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use std::collections::{BTreeMap, BTreeSet};

/// Gap between artboards laid out left to right (points).
pub const ARTBOARD_GAP_PT: f64 = 20.0;

/// A path element in PDF user space (points, y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PdfEl {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    CubicTo([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// Stroke parameters in user space (`cap` 0 butt / 1 round / 2 square, `join` 0 miter / 1 round /
/// 2 bevel, as PDF `J` / `j`).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfStroke {
    pub width: f64,
    pub cap: u8,
    pub join: u8,
    pub miter_limit: f64,
    pub dash: Vec<f64>,
    pub dash_offset: f64,
}

/// A paint as the reader resolved it (straight sRGB RGBA, 0..=1).
#[derive(Clone, Debug, PartialEq)]
pub enum PdfPaint {
    Solid([f32; 4]),
    Gradient { axial: bool, coords: [f64; 6], matrix: [f64; 6], stops: Vec<(f64, [f32; 4])>, extend: [bool; 2] },
}

/// One drawn item, in paint order. `blend` is the PDF blend index (ISO 32000-2 table 134 order).
#[derive(Clone, Debug, PartialEq)]
pub enum PdfDrawItem {
    Fill { path: Vec<PdfEl>, even_odd: bool, paint: PdfPaint, blend: u8, glyph: bool },
    Stroke { path: Vec<PdfEl>, stroke: PdfStroke, paint: PdfPaint, blend: u8, glyph: bool },
    PushClip { path: Vec<PdfEl>, even_odd: bool },
    PopClip,
    PushGroup { opacity: f32, blend: u8 },
    PopGroup,
}

/// One page's drawing: boxes `[x0, y0, x1, y1]` in points and the reader's note codes.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfDrawingPage {
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub items: Vec<PdfDrawItem>,
    pub notes: Vec<&'static str>,
}

/// The PDF reading a host supplies (Press `read_vector_pages`, adapted).
pub trait PdfDrawingProvider {
    fn pages(&self, pdf: &[u8], cancel: &CancellationToken) -> Result<Vec<PdfDrawingPage>>;
}

/// Folio blend discriminant of a PDF blend index.
fn blend_of(pdf: u8) -> u16 {
    match pdf {
        1 => 5,
        2 => 9,
        3 => 12,
        4 => 4,
        5 => 8,
        6 => 10,
        7 => 6,
        8 => 14,
        9 => 13,
        10 => 18,
        11 => 19,
        12 => 20,
        13 => 21,
        14 => 22,
        15 => 23,
        _ => BLEND_NORMAL,
    }
}

fn pt(v: f64) -> FolioLength {
    FolioLength { value: v, unit: GeometryUnit::Pt }
}

struct Paths<'a> {
    known: &'a BTreeSet<&'a str>,
}
impl Resolver for Paths<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        if schema_id == hsk_studio_accord::SCHEMA_STUDIO_VECTOR_PATH && self.known.contains(object_id) { Resolution::Available } else { Resolution::Absent }
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// Page user space -> document points (y down), per page.
#[derive(Clone, Copy)]
struct Flip {
    dx: f64,
    top: f64,
    x0: f64,
}
impl Flip {
    fn p(self, q: [f64; 2]) -> [f64; 2] {
        [q[0] - self.x0 + self.dx, self.top - q[1]]
    }
    /// As an affine `[a b c d e f]`.
    fn affine(self) -> [f64; 6] {
        [1.0, 0.0, 0.0, -1.0, self.dx - self.x0, self.top]
    }
}

fn compose(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    // a . b (apply b first).
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// Anchors `(position, incoming control, outgoing control)` of one subpath and whether it closes.
type RawSub = (Vec<([f64; 2], [f64; 2], [f64; 2])>, bool);

struct Builder<'m> {
    mint: &'m mut dyn IdMint,
    limits: Limits,
    layers: Vec<StudioLayer>,
    paths: Vec<VectorPath>,
    styles: Vec<StudioPaintStyle>,
    nodes: Vec<StudioNodeDesign>,
    clips: Vec<StudioVectorClip>,
    attachments: Vec<StudioMaskAttachment>,
    loss: Vec<LossEntry>,
    noted: BTreeSet<(String, &'static str)>,
    segments: usize,
    pending_clip: Option<String>,
}

impl Builder<'_> {
    fn id(&mut self, prefix: &'static str) -> Result<String> {
        let id = self.mint.mint(prefix);
        match DomainId::parse(&id) {
            Ok(d) if d.prefix() == prefix => Ok(id),
            _ => Err(VectorError::InvalidPolicy("id_mint")),
        }
    }

    fn note(&mut self, path: &str, kind: LossKind, code: &'static str) {
        if self.noted.insert((path.to_owned(), code)) {
            self.loss.push(LossEntry { path: path.to_owned(), kind, code });
        }
    }

    fn layer(&mut self, name: String, parent: &Parent, kind: &str, payload: Payload, visible: bool, appearance: Option<LayerAppearance>) -> Result<String> {
        if self.layers.len() >= self.limits.max_nodes as usize {
            return Err(VectorError::BudgetExceeded("nodes"));
        }
        let layer_id = self.id("SLYR")?;
        self.layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: layer_id.clone(),
            name,
            parent: parent.clone(),
            kind: kind.into(),
            payload,
            visible,
            appearance,
            text_frame: None,
        });
        Ok(layer_id)
    }

    /// Anchor subpaths of `path` (document points) as path records of `layer_id`.
    fn subpaths(&mut self, path: &[PdfEl], flip: Flip, winding: Winding, layer_id: &str) -> Result<Vec<String>> {
        self.segments = self.segments.checked_add(path.len()).filter(|n| *n <= self.limits.max_segments).ok_or(VectorError::BudgetExceeded("segments"))?;
        let point = |q: [f64; 2]| -> Result<Point> {
            let v = flip.p(q);
            if !(v[0].is_finite() && v[1].is_finite()) || v[0].abs() > 1e9 || v[1].abs() > 1e9 {
                return Err(VectorError::InvalidGeometry);
            }
            Point::pt(v[0], v[1]).map_err(|_| VectorError::InvalidGeometry)
        };
        let rel = |a: [f64; 2], b: [f64; 2]| -> Result<Point> {
            let (u, v) = (flip.p(a), flip.p(b));
            Point::pt(u[0] - v[0], u[1] - v[1]).map_err(|_| VectorError::InvalidGeometry)
        };
        let mut subs: Vec<RawSub> = Vec::new();
        for el in path {
            match *el {
                PdfEl::MoveTo(p) => subs.push((vec![(p, p, p)], false)),
                PdfEl::LineTo(p) => {
                    let s = subs.last_mut().ok_or(VectorError::InvalidGeometry)?;
                    s.0.push((p, p, p));
                }
                PdfEl::CubicTo(c1, c2, p) => {
                    let s = subs.last_mut().ok_or(VectorError::InvalidGeometry)?;
                    let last = s.0.last_mut().ok_or(VectorError::InvalidGeometry)?;
                    last.2 = c1;
                    s.0.push((p, c2, p));
                }
                PdfEl::Close => {
                    if let Some(s) = subs.last_mut() {
                        s.1 = true;
                        // A closing anchor on the start point merges into it.
                        if s.0.len() > 1
                            && let (Some(first), Some(last)) = (s.0.first().copied(), s.0.last().copied())
                            && (first.0[0] - last.0[0]).abs() < 1e-9
                            && (first.0[1] - last.0[1]).abs() < 1e-9
                        {
                            s.0.pop();
                            if let Some(f) = s.0.first_mut() {
                                f.1 = last.1;
                            }
                        }
                    }
                }
            }
        }
        let mut ids = Vec::with_capacity(subs.len());
        for (anchors, closed) in subs {
            if anchors.len() < 2 && !closed {
                continue;
            }
            let mut out = Vec::with_capacity(anchors.len());
            for (pos, cin, cout) in anchors {
                out.push(Anchor { position: point(pos)?, incoming: rel(cin, pos)?, outgoing: rel(cout, pos)?, handle_mirroring: Mirroring::None });
            }
            let path_id = self.id("SVPT")?;
            ids.push(path_id.clone());
            self.paths.push(VectorPath { path_id, layer_id: layer_id.to_owned(), closed, anchors: out, winding });
        }
        Ok(ids)
    }

    fn colour(c: [f32; 4]) -> (StudioColor, f64) {
        let ch = |v: f32| f64::from(v.clamp(0.0, 1.0));
        (StudioColor { r: ch(c[0]), g: ch(c[1]), b: ch(c[2]), colour_profile_id: ProfileBinding::Unassigned(()) }, ch(c[3]) * 100.0)
    }

    /// The Folio paint and row opacity of a reader paint.
    fn paint(&mut self, paint: &PdfPaint, flip: Flip, loc: &str) -> (StudioPaint, f64) {
        match paint {
            PdfPaint::Solid(c) => {
                let (colour, opacity) = Self::colour(*c);
                (StudioPaint::Solid { colour }, opacity)
            }
            PdfPaint::Gradient { axial, coords, matrix, stops, extend } => {
                if *extend != [true, true] {
                    self.note(loc, LossKind::Approximated, "pdf_gradient_extend_padded");
                }
                let geometry = if *axial {
                    StudioGradientGeometry::Linear { x1: coords[0], y1: coords[1], x2: coords[2], y2: coords[3] }
                } else {
                    StudioGradientGeometry::Radial { cx: coords[3], cy: coords[4], r: coords[5], fx: coords[0], fy: coords[1], fr: coords[2] }
                };
                let stops = stops
                    .iter()
                    .map(|(s, c)| {
                        let (colour, opacity) = Self::colour(*c);
                        StudioGradientStop { position: (s * 100.0).clamp(0.0, 100.0), midpoint: None, opacity, colour }
                    })
                    .collect();
                let gradient = StudioGradient { geometry, transform: compose(flip.affine(), *matrix), spread: GradientSpread::Pad, interpolation: GradientInterpolation::Classic, dither: false, stops };
                (StudioPaint::Gradient { gradient }, 100.0)
            }
        }
    }

    fn style(&mut self, rows: Vec<StudioAppearanceRow>, layer_id: &str) -> Result<()> {
        let style_id = self.id("SSTY")?;
        let n = self.styles.len().saturating_add(1);
        self.styles.push(StudioPaintStyle {
            schema_id: StyleRegistrySchema::V1,
            style_id: style_id.clone(),
            kind: StyleKind::Paint,
            name: format!("ai/{n}"),
            publishing: StudioPublishing { description: String::new(), description_markdown: String::new(), documentation_links: Vec::new(), publish_key: format!("ai-import/{n}"), is_remote: false },
            rows,
        });
        self.nodes.push(StudioNodeDesign {
            node: NodeRef { node_kind: NodeKind::Layer, node_id: layer_id.to_owned() },
            auto_layout: None,
            child_layout: None,
            size_clamps: None,
            constraints: None,
            reactions: Vec::new(),
            component_property_references: BTreeMap::new(),
            explicit_variable_modes: BTreeMap::new(),
            style_refs: BTreeMap::from([(LAYER_STYLE_REF.to_owned(), style_id)]),
        });
        Ok(())
    }

    fn stroke_row(&mut self, s: &PdfStroke, paint: &PdfPaint, blend: u8, flip: Flip, loc: &str) -> StudioAppearanceRow {
        let (paint, opacity) = self.paint(paint, flip, loc);
        let mut dash: Vec<FolioLength> = s.dash.iter().map(|d| pt(*d)).collect();
        if !dash.len().is_multiple_of(2) {
            // An odd PDF dash array repeats (ISO 32000-2 8.4.3.6); Folio stores an even count.
            dash.extend(dash.clone());
        }
        if dash.len() > hsk_studio_folio::MAX_DASH_ENTRIES {
            self.note(loc, LossKind::Approximated, "pdf_dash_truncated");
            dash.truncate(hsk_studio_folio::MAX_DASH_ENTRIES & !1);
        }
        StudioAppearanceRow::Stroke {
            paint,
            stroke: StudioStrokeStyle {
                weight: pt(s.width),
                cap: match s.cap {
                    1 => StrokeCap::Round,
                    2 => StrokeCap::Projecting,
                    _ => StrokeCap::Butt,
                },
                join: match s.join {
                    1 => StrokeJoin::Round,
                    2 => StrokeJoin::Bevel,
                    _ => StrokeJoin::Miter,
                },
                align: StrokeAlign::Center,
                miter_limit: s.miter_limit.clamp(1.0, 500.0),
                dash,
                dash_offset: pt(s.dash_offset),
            },
            opacity,
            blend_mode: blend_of(blend),
            visible: true,
        }
    }

    fn page(&mut self, page: &PdfDrawingPage, index: usize, dx: f64, cancel: &CancellationToken) -> Result<StudioArtboard> {
        let b = page.crop_box;
        if !b.iter().all(|v| v.is_finite()) || b[2] <= b[0] || b[3] <= b[1] {
            return Err(VectorError::InvalidGeometry);
        }
        let flip = Flip { dx, top: b[3], x0: b[0] };
        let loc = format!("/page[{}]", index.saturating_add(1));
        for code in &page.notes {
            let kind = if *code == "pdf_text_as_outlines" { LossKind::Approximated } else { LossKind::Unsupported };
            let code = if *code == "pdf_text_as_outlines" { "ai_text_as_outlines" } else { code };
            self.note(&loc, kind, code);
        }
        let page_group = self.layer(format!("Artboard {}", index.saturating_add(1)), &Parent::Root(()), "group", Payload::Group, true, None)?;
        let mut parents = vec![page_group];
        // The previous fill (layer, path) a repeated stroke joins (PDF `B`).
        let mut last_fill: Option<(usize, Vec<PdfEl>)> = None;
        for (n, item) in page.items.iter().enumerate() {
            if n.is_multiple_of(64) {
                cancel.check().map_err(|_| VectorError::Canceled)?;
            }
            let parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: parents.last().cloned().unwrap_or_default() });
            match item {
                PdfDrawItem::Fill { path, even_odd, paint, blend, glyph } => {
                    let name = if *glyph { "text outline".to_owned() } else { "path".to_owned() };
                    let layer_id = self.layer(name, &parent, "vector", Payload::Vector { path_ids: Vec::new() }, true, None)?;
                    let ids = self.subpaths(path, flip, if *even_odd { Winding::EvenOdd } else { Winding::NonZero }, &layer_id)?;
                    let (p, opacity) = self.paint(paint, flip, &loc);
                    self.style(vec![StudioAppearanceRow::Fill { paint: p, opacity, blend_mode: blend_of(*blend), visible: true }], &layer_id)?;
                    if let Some(l) = self.layers.last_mut() {
                        l.payload = Payload::Vector { path_ids: ids };
                    }
                    last_fill = Some((self.layers.len().saturating_sub(1), path.clone()));
                }
                PdfDrawItem::Stroke { path, stroke, paint, blend, glyph } => {
                    let row = self.stroke_row(stroke, paint, *blend, flip, &loc);
                    if let Some((i, p)) = &last_fill
                        && p == path
                        && let Some(layer_id) = self.layers.get(*i).map(|l| l.layer_id.clone())
                        && let Some(style) = self.nodes.iter().find(|x| x.node.node_id == layer_id).and_then(|x| x.style_refs.get(LAYER_STYLE_REF).cloned())
                        && let Some(s) = self.styles.iter_mut().find(|s| s.style_id == style)
                    {
                        s.rows.push(row);
                        last_fill = None;
                        continue;
                    }
                    let name = if *glyph { "text outline".to_owned() } else { "path".to_owned() };
                    let layer_id = self.layer(name, &parent, "vector", Payload::Vector { path_ids: Vec::new() }, true, None)?;
                    let ids = self.subpaths(path, flip, Winding::NonZero, &layer_id)?;
                    self.style(vec![row], &layer_id)?;
                    if let Some(l) = self.layers.last_mut() {
                        l.payload = Payload::Vector { path_ids: ids };
                    }
                    last_fill = None;
                }
                PdfDrawItem::PushGroup { opacity, blend } => {
                    let appearance = LayerAppearance { opacity: f64::from(opacity.clamp(0.0, 1.0)) * 100.0, fill_opacity: 100.0, blend_mode: blend_of(*blend), clipped: false, isolation: Isolation::Isolated, mask: None };
                    let g = self.layer("group".into(), &parent, "group", Payload::Group, true, Some(appearance))?;
                    parents.push(g);
                    last_fill = None;
                }
                PdfDrawItem::PushClip { path, even_odd } => {
                    // The hidden clip source sits beside the clipped group (never inside what it clips).
                    let source = if path.is_empty() { None } else { Some(self.layer("clip path".into(), &parent, "vector", Payload::Vector { path_ids: Vec::new() }, false, None)?) };
                    if let Some(source) = source {
                        let rule = if *even_odd { Winding::EvenOdd } else { Winding::NonZero };
                        let ids = self.subpaths(path, flip, rule, &source)?;
                        if let Some(l) = self.layers.last_mut() {
                            l.payload = Payload::Vector { path_ids: ids };
                        }
                        let clip_id = format!("clip-{}", self.clips.len().saturating_add(1));
                        self.clips.push(StudioVectorClip { clip_id: clip_id.clone(), source_layer_id: source, clip_rule: if *even_odd { StudioClipRule::EvenOdd } else { StudioClipRule::NonZero } });
                        self.pending_clip = Some(clip_id);
                    }
                    let g = self.layer("clip group".into(), &parent, "group", Payload::Group, true, None)?;
                    if let Some(clip_id) = self.pending_clip.take() {
                        self.attachments.push(StudioMaskAttachment { layer_id: g.clone(), clip_id: Some(clip_id), mask_id: None });
                    }
                    parents.push(g);
                    last_fill = None;
                }
                PdfDrawItem::PopClip | PdfDrawItem::PopGroup => {
                    if parents.len() > 1 {
                        parents.pop();
                    }
                    last_fill = None;
                }
            }
        }
        let w = b[2] - b[0];
        Ok(StudioArtboard {
            schema_id: ArtboardSchema::V1,
            artboard_id: self.id("SART")?,
            name: format!("Artboard {}", index.saturating_add(1)),
            parent: Parent::Root(()),
            bounds: Bounds { x: pt(dx), y: pt(0.0), width: pt(w), height: pt(b[3] - b[1]) },
        })
    }
}

/// Imports a PDF-compatible Illustrator file through `provider` (module docs). A PostScript
/// (pre-PDF) file stays [`VectorError::NativeEditabilityBlocked`]; anything else is unsupported.
pub fn import_ai_with(bytes: &[u8], policy: &ImportPolicy, provider: &dyn PdfDrawingProvider, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<VectorProposal> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    policy.validate()?;
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    let has_private_data = match identify_ai(bytes) {
        AiKind::PdfCompatible { has_private_data } => has_private_data,
        AiKind::LegacyPostScript => {
            return Err(VectorError::NativeEditabilityBlocked { version: None, reason: "native_ai_decoder_missing_postscript", preserved: PreservedBlob { kind: "ai_postscript", bytes: bytes.to_vec() } });
        }
        AiKind::NotAi | AiKind::Unknown => return Err(VectorError::Unsupported("not_an_ai_file")),
    };
    let pages = provider.pages(bytes, cancel)?;
    if pages.is_empty() {
        return Err(VectorError::Unsupported("ai_without_pages"));
    }
    let document_id = {
        let id = mint.mint("SDOC");
        match DomainId::parse(&id) {
            Ok(d) if d.prefix() == "SDOC" => id,
            _ => return Err(VectorError::InvalidPolicy("id_mint")),
        }
    };
    let mut b = Builder { mint, limits: policy.limits, layers: Vec::new(), paths: Vec::new(), styles: Vec::new(), nodes: Vec::new(), clips: Vec::new(), attachments: Vec::new(), loss: Vec::new(), noted: BTreeSet::new(), segments: 0, pending_clip: None };
    b.note("/ai", LossKind::Unsupported, "ai_native_editability_not_imported");
    if has_private_data {
        b.note("/ai", LossKind::Unsupported, "ai_private_data_not_imported");
    }
    let mut artboards = Vec::with_capacity(pages.len());
    let mut dx = 0.0;
    let mut extent = [0.0_f64, 0.0_f64];
    for (i, page) in pages.iter().enumerate() {
        let a = b.page(page, i, dx, cancel)?;
        extent = [dx + a.bounds.width.value, extent[1].max(a.bounds.height.value)];
        dx += a.bounds.width.value + ARTBOARD_GAP_PT;
        artboards.push(a);
    }
    let Builder { layers, paths, styles, nodes, clips, attachments, loss, .. } = b;
    let mut design = StudioDesignRecords::new(document_id.clone());
    design.paint_styles = styles;
    design.nodes = nodes;
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: document_id.clone(),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards,
        page_spreads: Vec::new(),
        layers,
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: Vec::new(), edges: Vec::new(), outputs: Vec::new() },
        sequences: Vec::new(),
    };
    let nodes_cap = (policy.limits.max_nodes as usize).max(document.layers.len());
    let budget = Budget {
        input_bytes: nodes_cap.saturating_mul(2048).max(1 << 20),
        nodes: nodes_cap.saturating_add(8),
        edges: 0,
        payload_bytes: nodes_cap.saturating_mul(512).max(1 << 18),
        depth: policy.limits.max_depth.saturating_add(4),
    };
    let known: BTreeSet<&str> = paths.iter().map(|p| p.path_id.as_str()).collect();
    let snapshot = validate_document(document, budget, cancel, &Paths { known: &known }).map_err(|d| VectorError::Folio(d.code))?;
    drop(known);
    let design_budget = Budget { nodes: design.paint_styles.iter().map(|s| s.rows.len().saturating_add(2)).sum::<usize>().saturating_add(design.nodes.len()).saturating_add(8), edges: 0, ..budget };
    let issues = validate_design_records(&design, snapshot.document(), design_budget, cancel, &Unresolved).map_err(|d| VectorError::Folio(d.code))?;
    if !issues.is_empty() {
        return Err(VectorError::Folio(hsk_studio_folio::Code::DanglingReference));
    }
    let mut vector_records = StudioVectorRecords::new(document_id);
    vector_records.clips = clips;
    vector_records.attachments = attachments;
    if !vector_records.clips.is_empty() {
        let record_budget = Budget { nodes: vector_records.clips.len().saturating_mul(2).saturating_add(8), edges: 0, ..budget };
        validate_vector_records(&vector_records, snapshot.document(), Some(&design), record_budget, cancel).map_err(|d| VectorError::Folio(d.code))?;
    }
    Ok(VectorProposal {
        snapshot,
        paths,
        design,
        vector_records,
        gradient_provenance: Vec::new(),
        loss,
        provenance: Provenance {
            source_format: "ai_pdf_compatible",
            source_unit: "pt",
            pixels_per_inch: policy.pixels_per_inch,
            px_to_pt: 72.0 / policy.pixels_per_inch,
            source_len: bytes.len(),
            decoded_len: bytes.len(),
            view_box: None,
            viewport_px: [extent[0] * policy.pixels_per_inch / 72.0, extent[1] * policy.pixels_per_inch / 72.0],
        },
    })
}
