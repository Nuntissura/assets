//! Illustrator `.ai` import through its PDF-compatible stream (lanes SMB-ILLUSTRATOR-INTEROP and
//! SMB-ILLUSTRATOR-INTEROP-2; STU-IO-186). PDF decoding is Press's: a caller-supplied
//! [`PdfDrawingProvider`] returns what every page draws (a host adapts Press
//! `read_vector_document`); this module turns that drawing into a Folio proposal, loss-aware:
//!
//! - every page is an artboard (laid out left to right with a 20 pt gap, Studio rule), its content
//!   a group layer; PDF user space (pt, y up) becomes document points (y down) from the crop box;
//! - every optional-content group the page content brackets at page level (an Illustrator layer)
//!   is a named group layer under the artboard group, hidden when the PDF's default configuration
//!   hides it; a layer bracket inside other content is flattened (`ai_layer_nested_flattened`);
//! - every draw is a vector layer of nib anchor paths with a PAINT style: a fill row (non-zero or
//!   even-odd winding on the paths), a stroke row (weight, cap, join, miter limit, dash in pt); a
//!   stroke that repeats the previous fill's path joins that layer (PDF `B`); solid colours are
//!   the reader's sRGB with the alpha as row opacity, axial / radial shadings become Folio linear /
//!   radial gradients (sampled stops; the shading matrix and the page flip as the gradient
//!   transform);
//! - a solid colour the PDF wrote in a document space keeps it: CMYK, gray, CIE Lab, ICC-based
//!   CMYK / gray (the profile as a Folio `StudioIccProfile`) and spot inks (with their alternate)
//!   become Folio swatches (`StudioSwatch`) bound to the row (`StudioSwatchPaint`, the spot tint as
//!   the per-use tint); the row's RGB is the stand-in. The `/OutputIntents` CMYK profile is the
//!   document's CMYK working space (`working_space`). ICC-based RGB stays RGB;
//! - transparency groups become group layers (opacity, blend); a clip becomes a group with a Folio
//!   vector clip whose source is a hidden vector layer holding the clip path;
//! - filled glyphs of one font, size, colour and baseline in drawing order form a run; the run is
//!   text (a Folio `text` layer, `StudioTextStory` and paragraph / character `StudioTypeStyle`
//!   records, the font in [`AiImport::fonts`]) when every glyph has Unicode, the run is upright and
//!   unscaled, and the provider confirms it draws the text exactly as placed
//!   ([`PdfDrawingProvider::text_as_text`]); otherwise the run is one vector layer of glyph
//!   outlines (`ai_text_as_outlines`);
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
    ArtboardSchema, BLEND_NORMAL, Bounds, Budget, ContentDigest, DocumentSchema, GeometryUnit, GradientInterpolation, GradientSpread, GraphSchema, Isolation,
    LayerAppearance, LayerSchema, Length as FolioLength, NodeKind, NodeRef, Parent, Payload, ProfileBinding, Resolution, Resolver, StoryContent, StoryDirection,
    StoryParagraph, StoryRange, StoryTextKind, StrokeAlign, StrokeCap, StrokeJoin, StudioAppearanceRow, StudioArtboard, StudioClipRule, StudioColor, StudioColorModel,
    StudioDesignRecords, StudioDocument, StudioGradient, StudioGradientGeometry, StudioGradientStop, StudioIccProfile, StudioLayer, StudioLayerGraph,
    StudioMaskAttachment, StudioNodeDesign, StudioPaint, StudioPaintStyle, StudioPublishing, StudioRgba, StudioSpotInk, StudioStrokeStyle, StudioSwatch,
    StudioSwatchKind, StudioSwatchPaint, StudioTextAlign, StudioTextStory, StudioTypeStyle, StudioTypeStyleRecords, StudioVectorClip, StudioVectorRecords,
    StyleKind, StyleRegistrySchema, SwatchSchema, TextStorySchema, TileRef, TypeStyleSchema, TypeStyleScope, Unresolved, validate_design_records,
    validate_document, validate_text_story, validate_type_style_records, validate_vector_records,
};
use hsk_studio_nib::{Anchor, Mirroring, Point, Winding};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Gap between artboards laid out left to right (points).
pub const ARTBOARD_GAP_PT: f64 = 20.0;
/// Longest text run kept as text (bytes); a longer run is outlines.
pub const MAX_TEXT_RUN_BYTES: usize = 4096;
/// Most text layers one import creates (the rest are outlines).
pub const MAX_TEXT_LAYERS: usize = 256;

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

/// A colour space as the PDF wrote it.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfSpace {
    Gray,
    Rgb,
    Cmyk,
    /// CIE Lab with its white point (XYZ).
    Lab { white: [f64; 3] },
    /// ICC-based: index into [`PdfDrawing::profiles`].
    Icc(usize),
}

/// A colour as the PDF wrote it (gray, RGB, CMYK, ICC components 0..=1; Lab L 0..=100, a, b).
#[derive(Clone, Debug, PartialEq)]
pub enum PdfColour {
    Process { space: PdfSpace, components: Vec<f64> },
    /// A spot ink: tint 0..=1 and, when known, the alternate colour at full tint.
    Spot { name: String, tint: f64, alternate: Option<(PdfSpace, Vec<f64>)> },
}

/// The glyph a draw came from.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfGlyph {
    /// Index into [`PdfDrawing::fonts`].
    pub font: usize,
    pub glyph_id: u32,
    pub unicode: Option<String>,
    /// Glyph space (1000 units per em) to user space.
    pub matrix: [f64; 6],
}

/// A font glyphs were drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfFont {
    pub postscript_name: Option<String>,
    pub bytes: Option<Arc<[u8]>>,
}

/// An optional-content group (Illustrator layer).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfLayer {
    pub name: String,
    pub visible: bool,
}

/// An ICC profile a source colour names.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfIcc {
    pub components: u8,
    pub bytes: Vec<u8>,
    /// Lowercase hex sha256 of `bytes`.
    pub sha256: String,
}

/// One drawn item, in paint order. `blend` is the PDF blend index (ISO 32000-2 table 134 order);
/// `source` is the colour a solid paint was written in.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfDrawItem {
    Fill { path: Vec<PdfEl>, even_odd: bool, paint: PdfPaint, blend: u8, glyph: Option<PdfGlyph>, source: Option<PdfColour> },
    Stroke { path: Vec<PdfEl>, stroke: PdfStroke, paint: PdfPaint, blend: u8, glyph: Option<PdfGlyph>, source: Option<PdfColour> },
    PushClip { path: Vec<PdfEl>, even_odd: bool },
    PopClip,
    PushGroup { opacity: f32, blend: u8 },
    PopGroup,
    /// Content of [`PdfDrawing::layers`]`[layer]` starts.
    BeginLayer { layer: usize },
    EndLayer,
}

/// One page's drawing: boxes `[x0, y0, x1, y1]` in points and the reader's note codes.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfDrawingPage {
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub items: Vec<PdfDrawItem>,
    pub notes: Vec<&'static str>,
}

/// A whole drawing: pages and what their items index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfDrawing {
    pub pages: Vec<PdfDrawingPage>,
    pub fonts: Vec<PdfFont>,
    pub layers: Vec<PdfLayer>,
    pub profiles: Vec<PdfIcc>,
    /// `/OutputIntents` profile (index into `profiles`).
    pub output_intent: Option<usize>,
}

/// A run of glyphs the importer would keep as text: its text, size (pt) and each glyph's id and
/// pen offset from the run origin along the baseline (pt).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfTextRun<'a> {
    pub font: &'a PdfFont,
    pub text: &'a str,
    pub size_pt: f64,
    pub glyphs: &'a [(u32, f64)],
}

/// The PDF reading a host supplies (Press `read_vector_document`, adapted).
pub trait PdfDrawingProvider {
    fn drawing(&self, pdf: &[u8], cancel: &CancellationToken) -> Result<PdfDrawing>;
    /// Whether the host lays `run` out exactly as the PDF placed it (the same glyph ids at the same
    /// offsets) with `run.font`, so it can stay editable text. Default: no, outlines.
    fn text_as_text(&self, _run: &PdfTextRun<'_>) -> bool {
        false
    }
}

/// A text layer the import created: where its first baseline starts (document points) and its
/// records; `font` indexes [`AiImport::fonts`].
#[derive(Clone, Debug, PartialEq)]
pub struct AiTextLayer {
    pub layer_id: String,
    pub story_id: String,
    pub origin_pt: [f64; 2],
    pub paragraph_style: String,
    pub character_style: String,
    pub font: usize,
}

/// A font program text layers name by family.
#[derive(Clone, Debug, PartialEq)]
pub struct AiFontFace {
    pub family: String,
    pub bytes: Arc<[u8]>,
}

/// Everything an `.ai` import produced: the vector proposal (its snapshot holds the text layers),
/// the text records Folio keeps outside the document, the fonts they name and the ICC profile
/// bytes the proposal's `StudioIccProfile` records digest.
#[derive(Clone, Debug)]
pub struct AiImport {
    pub proposal: VectorProposal,
    pub text_layers: Vec<AiTextLayer>,
    pub stories: Vec<StudioTextStory>,
    pub type_styles: StudioTypeStyleRecords,
    pub fonts: Vec<AiFontFace>,
    pub icc: Vec<PdfIcc>,
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

struct Known<'a> {
    paths: &'a BTreeSet<&'a str>,
    stories: &'a BTreeSet<&'a str>,
    styles: &'a BTreeSet<&'a str>,
}
impl Resolver for Known<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        let known = match schema_id {
            hsk_studio_accord::SCHEMA_STUDIO_VECTOR_PATH => self.paths.contains(object_id),
            hsk_studio_accord::SCHEMA_STUDIO_TEXT_STORY => self.stories.contains(object_id),
            hsk_studio_accord::SCHEMA_STUDIO_TYPE_STYLE => self.styles.contains(object_id),
            _ => false,
        };
        if known { Resolution::Available } else { Resolution::Absent }
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

/// A Folio colour model, its components and its profile (`SCPF`).
type Process = (StudioColorModel, Vec<f64>, Option<String>);
/// One glyph of a run: glyph, outline, paint, blend, source colour.
type RunGlyph<'a> = (&'a PdfGlyph, &'a Vec<PdfEl>, &'a PdfPaint, u8, Option<&'a PdfColour>);
/// The PAINT row bound to a swatch, and the per-use tint.
type Bound = (String, Option<f64>);

/// D50 (the Folio Lab reference white).
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// One unit of a page's items: a single item, or a run of filled glyphs (item indices).
enum Unit {
    One(usize),
    Run(Vec<usize>),
}

/// An upright, unscaled glyph's size (pt) and pen origin.
fn upright(m: &[f64; 6]) -> Option<(f64, [f64; 2])> {
    let size = m[3] * 1000.0;
    let ok = m[1].abs() <= 1e-9 && m[2].abs() <= 1e-9 && m[3] > 0.0 && (m[0] - m[3]).abs() <= m[3] * 1e-6 && size.is_finite() && size <= 10_000.0;
    ok.then_some((size, [m[4], m[5]]))
}

/// Groups consecutive filled glyphs of one font, size, colour, blend and baseline whose pen
/// origins advance by at most 1.5 em (a wider gap starts a new run).
fn units(items: &[PdfDrawItem]) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut key: Option<(usize, u64, [u32; 4], u8, u64)> = None;
    let mut last_x = f64::NEG_INFINITY;
    let flush = |run: &mut Vec<usize>, out: &mut Vec<Unit>| {
        if !run.is_empty() {
            out.push(Unit::Run(std::mem::take(run)));
        }
    };
    for (i, item) in items.iter().enumerate() {
        let glyph = match item {
            PdfDrawItem::Fill { glyph: Some(g), paint: PdfPaint::Solid(c), blend, .. } if g.unicode.as_deref().is_some_and(|u| !u.is_empty()) => upright(&g.matrix).map(|(size, o)| ((g.font, size.to_bits(), c.map(f32::to_bits), *blend, o[1].to_bits()), o[0], size)),
            _ => None,
        };
        match glyph {
            Some((k, x, size)) if key == Some(k) && x >= last_x - 1e-6 && x - last_x <= size * 1.5 => {
                run.push(i);
                last_x = x;
            }
            Some((k, x, _)) => {
                flush(&mut run, &mut out);
                run.push(i);
                key = Some(k);
                last_x = x;
            }
            None => {
                flush(&mut run, &mut out);
                key = None;
                out.push(Unit::One(i));
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

struct Builder<'m> {
    mint: &'m mut dyn IdMint,
    limits: Limits,
    layers: Vec<StudioLayer>,
    paths: Vec<VectorPath>,
    styles: Vec<StudioPaintStyle>,
    nodes: Vec<StudioNodeDesign>,
    clips: Vec<StudioVectorClip>,
    attachments: Vec<StudioMaskAttachment>,
    swatches: Vec<StudioSwatch>,
    swatch_paints: Vec<StudioSwatchPaint>,
    swatch_keys: BTreeMap<String, String>,
    icc_profiles: Vec<StudioIccProfile>,
    /// PDF profile index -> SCPF id.
    profile_ids: BTreeMap<usize, String>,
    stories: Vec<StudioTextStory>,
    type_styles: Vec<StudioTypeStyle>,
    text_layers: Vec<AiTextLayer>,
    fonts: Vec<AiFontFace>,
    /// PDF font index -> `fonts` index.
    font_ids: BTreeMap<usize, usize>,
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

    fn style(&mut self, rows: Vec<StudioAppearanceRow>, layer_id: &str) -> Result<String> {
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
            style_refs: BTreeMap::from([(LAYER_STYLE_REF.to_owned(), style_id.clone())]),
        });
        Ok(style_id)
    }

    /// The SCPF id of PDF profile `i` (a Folio `StudioIccProfile`, created once).
    fn profile_id(&mut self, drawing: &PdfDrawing, i: usize) -> Result<Option<(String, StudioColorModel)>> {
        let Some(p) = drawing.profiles.get(i) else { return Ok(None) };
        let model = match p.components {
            1 => StudioColorModel::Gray,
            3 => StudioColorModel::Rgb,
            4 => StudioColorModel::Cmyk,
            _ => return Ok(None),
        };
        if let Some(id) = self.profile_ids.get(&i) {
            return Ok(Some((id.clone(), model)));
        }
        let id = self.id("SCPF")?;
        let intent = drawing.output_intent == Some(i);
        // The output intent is the document working space of its model (one per model).
        let working = intent && !self.icc_profiles.iter().any(|q| q.working_space && q.colour_space == model);
        let name = if intent { "Output intent".to_owned() } else { format!("PDF profile {}", i.saturating_add(1)) };
        self.icc_profiles.push(StudioIccProfile { profile_id: id.clone(), name, colour_space: model, content_digest: ContentDigest { algorithm: "sha256".into(), digest: p.sha256.clone() }, working_space: working });
        self.profile_ids.insert(i, id.clone());
        Ok(Some((id, model)))
    }

    /// A process colour of `space` as a Folio model, components and profile.
    fn process(&mut self, drawing: &PdfDrawing, space: &PdfSpace, components: &[f64], loc: &str) -> Result<Option<Process>> {
        let (model, profile) = match space {
            PdfSpace::Rgb => return Ok(None),
            PdfSpace::Gray => (StudioColorModel::Gray, None),
            PdfSpace::Cmyk => (StudioColorModel::Cmyk, None),
            PdfSpace::Lab { white } => {
                if white.iter().zip(D50).any(|(w, d)| (w - d).abs() > 1e-3) {
                    self.note(loc, LossKind::Approximated, "pdf_lab_white_point_not_d50");
                }
                (StudioColorModel::Lab, None)
            }
            PdfSpace::Icc(i) => match self.profile_id(drawing, *i)? {
                Some((_, StudioColorModel::Rgb)) | None => return Ok(None),
                Some((id, model)) => (model, Some(id)),
            },
        };
        if !model.accepts(components) {
            self.note(loc, LossKind::Approximated, "pdf_colour_out_of_range");
            return Ok(None);
        }
        Ok(Some((model, components.to_vec(), profile)))
    }

    /// The swatch (created once per distinct colour) and per-use tint of a source colour; `None`
    /// keeps the row plain RGB.
    fn swatch(&mut self, drawing: &PdfDrawing, c: &PdfColour, loc: &str) -> Result<Option<Bound>> {
        let (key, swatch, tint) = match c {
            PdfColour::Process { space, components } => {
                let Some((model, comps, profile)) = self.process(drawing, space, components, loc)? else { return Ok(None) };
                let name = match model {
                    StudioColorModel::Cmyk => format!("C={:.0} M={:.0} Y={:.0} K={:.0}", comps[0] * 100.0, comps[1] * 100.0, comps[2] * 100.0, comps[3] * 100.0),
                    StudioColorModel::Lab => format!("L={:.0} a={:.0} b={:.0}", comps[0], comps[1], comps[2]),
                    StudioColorModel::Gray => format!("K={:.0}", (1.0 - comps[0]) * 100.0),
                    StudioColorModel::Rgb => format!("R={:.0} G={:.0} B={:.0}", comps[0] * 255.0, comps[1] * 255.0, comps[2] * 255.0),
                };
                let key = format!("{model:?}{comps:?}{profile:?}");
                let s = StudioSwatch { schema_id: SwatchSchema::V1, swatch_id: String::new(), name, swatch_kind: StudioSwatchKind::Process, color_model: Some(model), components: comps, profile_ref: profile, spot: None, base_swatch_ref: None, tint_value: None, gradient: None, group_ref: None };
                (key, s, None)
            }
            PdfColour::Spot { name, tint, alternate } => {
                let alt = match alternate {
                    Some((space, comps)) => self.process(drawing, space, comps, loc)?,
                    None => None,
                };
                if alt.as_ref().is_some_and(|a| a.2.is_some()) {
                    // Folio's spot alternate names no profile: the alternate is in the working space.
                    self.note(loc, LossKind::Approximated, "pdf_spot_alternate_profile_dropped");
                }
                let ink_name: String = name.chars().filter(|c| !c.is_control()).take(256).collect();
                if ink_name.is_empty() || !tint.is_finite() {
                    return Ok(None);
                }
                let key = format!("spot{ink_name}{alt:?}");
                let ink = StudioSpotInk { ink_name: ink_name.clone(), measured_lab: None, alternate_space: alt.as_ref().map(|a| a.0), alternate_color_value: alt.map(|a| a.1).unwrap_or_default() };
                let s = StudioSwatch { schema_id: SwatchSchema::V1, swatch_id: String::new(), name: ink_name, swatch_kind: StudioSwatchKind::Spot, color_model: None, components: Vec::new(), profile_ref: None, spot: Some(ink), base_swatch_ref: None, tint_value: None, gradient: None, group_ref: None };
                (key, s, Some((tint * 100.0).clamp(0.0, 100.0)))
            }
        };
        if let Some(id) = self.swatch_keys.get(&key) {
            return Ok(Some((id.clone(), tint)));
        }
        let id = self.id("SSWT")?;
        self.swatches.push(StudioSwatch { swatch_id: id.clone(), ..swatch });
        self.swatch_keys.insert(key, id.clone());
        Ok(Some((id, tint)))
    }

    /// Binds row `row` of `style_id` to the swatch of `source` (solid paints only).
    fn bind(&mut self, drawing: &PdfDrawing, style_id: &str, row: usize, paint: &PdfPaint, source: Option<&PdfColour>, loc: &str) -> Result<()> {
        let (Some(c), PdfPaint::Solid(_)) = (source, paint) else { return Ok(()) };
        if let Some((swatch_id, tint)) = self.swatch(drawing, c, loc)? {
            let row = u32::try_from(row).map_err(|_| VectorError::BudgetExceeded("rows"))?;
            self.swatch_paints.push(StudioSwatchPaint { style_id: style_id.to_owned(), row, swatch_id, tint });
        }
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

    /// A run of filled glyphs: one text layer when the provider keeps it as text, else one vector
    /// layer of their outlines.
    fn run(&mut self, drawing: &PdfDrawing, glyphs: &[RunGlyph<'_>], parent: &Parent, flip: Flip, provider: &dyn PdfDrawingProvider, loc: &str) -> Result<()> {
        let Some(&(first, _, paint, blend, source)) = glyphs.first() else { return Ok(()) };
        let text: String = glyphs.iter().filter_map(|g| g.0.unicode.as_deref()).collect();
        let (size_pt, origin) = upright(&first.matrix).unwrap_or((0.0, [first.matrix[4], first.matrix[5]]));
        let placed: Vec<(u32, f64)> = glyphs.iter().map(|g| (g.0.glyph_id, g.0.matrix[4] - origin[0])).collect();
        let font = drawing.fonts.get(first.font);
        let as_text = font.is_some_and(|f| f.bytes.is_some())
            && text.len() <= MAX_TEXT_RUN_BYTES
            && !text.contains(['\n', '\r'])
            && self.text_layers.len() < MAX_TEXT_LAYERS
            && source.is_none_or(|c| matches!(c, PdfColour::Process { space: PdfSpace::Rgb | PdfSpace::Gray, .. }))
            && font.is_some_and(|f| provider.text_as_text(&PdfTextRun { font: f, text: &text, size_pt, glyphs: &placed }));
        let name: String = text.chars().take(32).collect();
        if as_text {
            let (Some(font), PdfPaint::Solid(c)) = (font, paint) else { return Ok(()) };
            let Some(bytes) = font.bytes.clone() else { return Ok(()) };
            let face = match self.font_ids.get(&first.font) {
                Some(f) => *f,
                None => {
                    let n = self.fonts.len();
                    let stem: String = font.postscript_name.as_deref().unwrap_or("Font").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(48).collect();
                    self.fonts.push(AiFontFace { family: format!("{stem}#ai{}", n.saturating_add(1)), bytes });
                    self.font_ids.insert(first.font, n);
                    n
                }
            };
            let family = self.fonts.get(face).map(|f| f.family.clone()).unwrap_or_default();
            let story_id = self.id("STXT")?;
            let (paragraph_style, character_style) = (self.id("STYS")?, self.id("STYS")?);
            let layer_id = self.layer(name, parent, "text", Payload::Text { story_id: story_id.clone() }, true, None)?;
            if blend != 0 {
                self.note(loc, LossKind::Unsupported, "pdf_text_blend_not_imported");
            }
            let len = text.len() as u64;
            self.stories.push(StudioTextStory {
                schema_id: TextStorySchema::V1,
                story_id: story_id.clone(),
                text_kind: StoryTextKind::Point,
                story_direction: StoryDirection::LeftToRight,
                content: StoryContent::Inline { text_utf8: text },
                paragraphs: vec![StoryParagraph { start: 0, end: len, style_ref: Some(paragraph_style.clone()), paragraph_direction: None }],
                character_ranges: vec![StoryRange { start: 0, end: len, style_ref: Some(character_style.clone()), applied_language: None }],
            });
            let ch = |v: f32| f64::from(v.clamp(0.0, 1.0));
            let style = |id: &str, scope: TypeStyleScope, name: &str| StudioTypeStyle {
                schema_id: TypeStyleSchema::V1,
                style_id: id.to_owned(),
                scope,
                name: name.to_owned(),
                based_on: None,
                next_style: None,
                font_family: None,
                font_style: None,
                font_size: None,
                fill: None,
                line_height: None,
                letter_spacing: None,
                align: None,
                grep_styles: Vec::new(),
            };
            self.type_styles.push(StudioTypeStyle { align: Some(StudioTextAlign::Left), ..style(&paragraph_style, TypeStyleScope::Paragraph, "ai/Paragraph") });
            self.type_styles.push(StudioTypeStyle {
                font_family: Some(family),
                font_size: Some(pt(size_pt)),
                fill: Some(StudioRgba { r: ch(c[0]), g: ch(c[1]), b: ch(c[2]), a: ch(c[3]), colour_profile_id: ProfileBinding::Unassigned(()) }),
                ..style(&character_style, TypeStyleScope::Character, "ai/Character")
            });
            self.text_layers.push(AiTextLayer { layer_id, story_id, origin_pt: flip.p(origin), paragraph_style, character_style, font: face });
            return Ok(());
        }
        self.note(loc, LossKind::Approximated, "ai_text_as_outlines");
        let layer_id = self.layer(format!("text outline: {name}"), parent, "vector", Payload::Vector { path_ids: Vec::new() }, true, None)?;
        let mut ids = Vec::new();
        for g in glyphs {
            ids.extend(self.subpaths(g.1, flip, Winding::NonZero, &layer_id)?);
        }
        let (p, opacity) = self.paint(paint, flip, loc);
        let style_id = self.style(vec![StudioAppearanceRow::Fill { paint: p, opacity, blend_mode: blend_of(blend), visible: true }], &layer_id)?;
        self.bind(drawing, &style_id, 0, paint, source, loc)?;
        if let Some(l) = self.layers.last_mut().filter(|l| l.layer_id == layer_id) {
            l.payload = Payload::Vector { path_ids: ids };
        }
        Ok(())
    }

    fn page(&mut self, drawing: &PdfDrawing, page: &PdfDrawingPage, index: usize, dx: f64, provider: &dyn PdfDrawingProvider, cancel: &CancellationToken) -> Result<StudioArtboard> {
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
        // Open layer brackets: the parents depth an AI layer group sits at, or `None` (flattened).
        let mut layer_stack: Vec<Option<usize>> = Vec::new();
        // The previous fill (layer, path) a repeated stroke joins (PDF `B`).
        let mut last_fill: Option<(usize, Vec<PdfEl>)> = None;
        for (n, unit) in units(&page.items).into_iter().enumerate() {
            if n.is_multiple_of(64) {
                cancel.check().map_err(|_| VectorError::Canceled)?;
            }
            let parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: parents.last().cloned().unwrap_or_default() });
            let i = match unit {
                Unit::Run(run) => {
                    let glyphs: Vec<RunGlyph<'_>> = run
                        .iter()
                        .filter_map(|i| match page.items.get(*i) {
                            Some(PdfDrawItem::Fill { path, paint, blend, glyph: Some(g), source, .. }) => Some((g, path, paint, *blend, source.as_ref())),
                            _ => None,
                        })
                        .collect();
                    self.run(drawing, &glyphs, &parent, flip, provider, &loc)?;
                    last_fill = None;
                    continue;
                }
                Unit::One(i) => i,
            };
            let Some(item) = page.items.get(i) else { continue };
            match item {
                PdfDrawItem::Fill { path, even_odd, paint, blend, glyph, source } => {
                    let name = if glyph.is_some() { "text outline".to_owned() } else { "path".to_owned() };
                    if glyph.is_some() {
                        self.note(&loc, LossKind::Approximated, "ai_text_as_outlines");
                    }
                    let layer_id = self.layer(name, &parent, "vector", Payload::Vector { path_ids: Vec::new() }, true, None)?;
                    let ids = self.subpaths(path, flip, if *even_odd { Winding::EvenOdd } else { Winding::NonZero }, &layer_id)?;
                    let (p, opacity) = self.paint(paint, flip, &loc);
                    let style_id = self.style(vec![StudioAppearanceRow::Fill { paint: p, opacity, blend_mode: blend_of(*blend), visible: true }], &layer_id)?;
                    self.bind(drawing, &style_id, 0, paint, source.as_ref(), &loc)?;
                    if let Some(l) = self.layers.iter_mut().rev().find(|l| l.layer_id == layer_id) {
                        l.payload = Payload::Vector { path_ids: ids };
                    }
                    last_fill = Some((self.layers.len().saturating_sub(1), path.clone()));
                }
                PdfDrawItem::Stroke { path, stroke, paint, blend, glyph, source } => {
                    if glyph.is_some() {
                        self.note(&loc, LossKind::Approximated, "ai_text_as_outlines");
                    }
                    let row = self.stroke_row(stroke, paint, *blend, flip, &loc);
                    if let Some((i, p)) = &last_fill
                        && p == path
                        && let Some(layer_id) = self.layers.get(*i).map(|l| l.layer_id.clone())
                        && let Some(style) = self.nodes.iter().find(|x| x.node.node_id == layer_id).and_then(|x| x.style_refs.get(LAYER_STYLE_REF).cloned())
                        && let Some(at) = self.styles.iter().position(|s| s.style_id == style)
                    {
                        let index = self.styles.get(at).map_or(0, |s| s.rows.len());
                        if let Some(s) = self.styles.get_mut(at) {
                            s.rows.push(row);
                        }
                        self.bind(drawing, &style, index, paint, source.as_ref(), &loc)?;
                        last_fill = None;
                        continue;
                    }
                    let name = if glyph.is_some() { "text outline".to_owned() } else { "path".to_owned() };
                    let layer_id = self.layer(name, &parent, "vector", Payload::Vector { path_ids: Vec::new() }, true, None)?;
                    let ids = self.subpaths(path, flip, Winding::NonZero, &layer_id)?;
                    let style_id = self.style(vec![row], &layer_id)?;
                    self.bind(drawing, &style_id, 0, paint, source.as_ref(), &loc)?;
                    if let Some(l) = self.layers.iter_mut().rev().find(|l| l.layer_id == layer_id) {
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
                    // Never below an open layer's group or the artboard group.
                    let floor = layer_stack.iter().rev().find_map(|d| *d).unwrap_or(1);
                    if parents.len() > floor {
                        parents.pop();
                    }
                    last_fill = None;
                }
                PdfDrawItem::BeginLayer { layer } => {
                    last_fill = None;
                    let top_level = parents.len() == 1 && layer_stack.iter().all(Option::is_none);
                    match drawing.layers.get(*layer) {
                        Some(l) if top_level => {
                            let name: String = l.name.chars().filter(|c| !c.is_control()).take(256).collect();
                            let g = self.layer(if name.is_empty() { format!("Layer {}", layer.saturating_add(1)) } else { name }, &parent, "group", Payload::Group, l.visible, None)?;
                            parents.push(g);
                            layer_stack.push(Some(parents.len()));
                        }
                        _ => {
                            self.note(&loc, LossKind::Approximated, "ai_layer_nested_flattened");
                            layer_stack.push(None);
                        }
                    }
                }
                PdfDrawItem::EndLayer => {
                    last_fill = None;
                    if let Some(Some(depth)) = layer_stack.pop() {
                        if parents.len() > depth {
                            self.note(&loc, LossKind::Approximated, "ai_layer_nesting_flattened");
                        }
                        parents.truncate(depth.saturating_sub(1).max(1));
                    }
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
/// The proposal's text layers need [`AiImport`]'s records: callers that keep text use
/// [`import_ai_document`].
pub fn import_ai_with(bytes: &[u8], policy: &ImportPolicy, provider: &dyn PdfDrawingProvider, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<VectorProposal> {
    import_ai_document(bytes, policy, provider, mint, cancel).map(|a| a.proposal)
}

/// [`import_ai_with`] with the text records, fonts and ICC profile bytes (module docs).
pub fn import_ai_document(bytes: &[u8], policy: &ImportPolicy, provider: &dyn PdfDrawingProvider, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<AiImport> {
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
    let drawing = provider.drawing(bytes, cancel)?;
    if drawing.pages.is_empty() {
        return Err(VectorError::Unsupported("ai_without_pages"));
    }
    let document_id = {
        let id = mint.mint("SDOC");
        match DomainId::parse(&id) {
            Ok(d) if d.prefix() == "SDOC" => id,
            _ => return Err(VectorError::InvalidPolicy("id_mint")),
        }
    };
    let mut b = Builder {
        mint,
        limits: policy.limits,
        layers: Vec::new(),
        paths: Vec::new(),
        styles: Vec::new(),
        nodes: Vec::new(),
        clips: Vec::new(),
        attachments: Vec::new(),
        swatches: Vec::new(),
        swatch_paints: Vec::new(),
        swatch_keys: BTreeMap::new(),
        icc_profiles: Vec::new(),
        profile_ids: BTreeMap::new(),
        stories: Vec::new(),
        type_styles: Vec::new(),
        text_layers: Vec::new(),
        fonts: Vec::new(),
        font_ids: BTreeMap::new(),
        loss: Vec::new(),
        noted: BTreeSet::new(),
        segments: 0,
        pending_clip: None,
    };
    b.note("/ai", LossKind::Unsupported, "ai_native_editability_not_imported");
    if has_private_data {
        b.note("/ai", LossKind::Unsupported, "ai_private_data_not_imported");
    }
    // The output intent is the document's working space of its model even when no colour names it.
    if let Some(i) = drawing.output_intent {
        b.profile_id(&drawing, i)?;
    }
    let mut artboards = Vec::with_capacity(drawing.pages.len());
    let mut dx = 0.0;
    let mut extent = [0.0_f64, 0.0_f64];
    for (i, page) in drawing.pages.iter().enumerate() {
        let a = b.page(&drawing, page, i, dx, provider, cancel)?;
        extent = [dx + a.bounds.width.value, extent[1].max(a.bounds.height.value)];
        dx += a.bounds.width.value + ARTBOARD_GAP_PT;
        artboards.push(a);
    }
    let Builder { layers, paths, styles, nodes, clips, attachments, swatches, swatch_paints, icc_profiles, stories, type_styles, text_layers, fonts, loss, .. } = b;
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
    let mut type_records = StudioTypeStyleRecords::new(document_id.clone());
    type_records.styles = type_styles;
    let known_paths: BTreeSet<&str> = paths.iter().map(|p| p.path_id.as_str()).collect();
    let known_stories: BTreeSet<&str> = stories.iter().map(|s| s.story_id.as_str()).collect();
    let known_styles: BTreeSet<&str> = type_records.styles.iter().map(|s| s.style_id.as_str()).collect();
    let known = Known { paths: &known_paths, stories: &known_stories, styles: &known_styles };
    let snapshot = validate_document(document, budget, cancel, &known).map_err(|d| VectorError::Folio(d.code))?;
    // Folio meters design records per member (rows, stops, dash entries): the SVG importer's cap
    // of 16 records per layer node.
    let design_budget = Budget { nodes: nodes_cap.saturating_mul(16).saturating_add(8), edges: 0, ..budget };
    let issues = validate_design_records(&design, snapshot.document(), design_budget, cancel, &Unresolved).map_err(|d| VectorError::Folio(d.code))?;
    if !issues.is_empty() {
        return Err(VectorError::Folio(hsk_studio_folio::Code::DanglingReference));
    }
    if !type_records.styles.is_empty() {
        validate_type_style_records(&type_records, snapshot.document(), budget, cancel, &Unresolved).map_err(|d| VectorError::Folio(d.code))?;
        for s in &stories {
            let issues = validate_text_story(s, budget, cancel, &known).map_err(|d| VectorError::Folio(d.code))?;
            if !issues.is_empty() {
                return Err(VectorError::Folio(hsk_studio_folio::Code::DanglingReference));
            }
        }
    }
    let mut vector_records = StudioVectorRecords::new(document_id);
    vector_records.clips = clips;
    vector_records.attachments = attachments;
    vector_records.swatches = swatches;
    vector_records.swatch_paints = swatch_paints;
    vector_records.icc_profiles = icc_profiles;
    let records = vector_records.clips.len().saturating_add(vector_records.attachments.len()).saturating_add(vector_records.swatches.len()).saturating_add(vector_records.swatch_paints.len()).saturating_add(vector_records.icc_profiles.len());
    if records > 0 {
        let record_budget = Budget { nodes: records.saturating_add(nodes_cap).saturating_add(8), edges: 0, ..budget };
        validate_vector_records(&vector_records, snapshot.document(), Some(&design), record_budget, cancel).map_err(|d| VectorError::Folio(d.code))?;
    }
    let icc: Vec<PdfIcc> = drawing.profiles.iter().filter(|p| vector_records.icc_profiles.iter().any(|r| r.content_digest.digest == p.sha256)).cloned().collect();
    Ok(AiImport {
        proposal: VectorProposal {
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
        },
        text_layers,
        stories,
        type_styles: type_records,
        fonts,
        icc,
    })
}
