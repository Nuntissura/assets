//! Vector content reading (lane SMB-ILLUSTRATOR-INTEROP; STU-IO-186 consumers: the Illustrator
//! `.ai` PDF-compatible stream). Every page of a PDF is interpreted by hayro-interpret 0.7.0 (the
//! reader `flatten.rs` renders with) into a recording device instead of a rasteriser, so a caller
//! receives what the page draws, in PDF user space (points, y up), in paint order:
//!
//! - [`VectorItem::Draw`]: one path filled (non-zero or even-odd) or stroked (width, cap, join,
//!   miter limit, dash), with its paint: an RGBA colour as the interpreter resolved it (device
//!   colour spaces converted by hayro; the source space is not kept: [`VectorNote::ColourConverted`]
//!   for non-RGB sources is NOT recorded because hayro does not expose it, declared gap), or an
//!   axial / radial shading sampled at [`GRADIENT_SAMPLES`] stops with its pattern matrix. Text is
//!   drawn as glyph outlines; each outline glyph also carries its font (PostScript name and
//!   TrueType / OpenType bytes when embedded, [`VectorFont`]), glyph id, Unicode text (ToUnicode,
//!   glyph names) and glyph matrix ([`VectorGlyph`]) so a caller can keep text as text. Type 3
//!   glyphs are interpreted ([`VectorNote::TextAsOutlines`]).
//! - [`read_vector_document`] adds what the interpreter does not expose, from Press's own walk of
//!   the same content (`vector_source.rs`): the colour each solid path was written in
//!   ([`VectorColour`]: gray, RGB, CMYK, Lab, ICC-based with the profile bytes, spot inks), the
//!   optional-content groups ([`VectorLayer`], Illustrator layers) as [`VectorItem::BeginLayer`] /
//!   [`VectorItem::EndLayer`] around the content of the outermost `/OC` marked content, hidden
//!   layers read too (from a copy of the file with every group on), and the `/OutputIntents`
//!   profile. Any disagreement between the walk and the interpreter drops that page's source
//!   colours ([`VectorNote::SourceColourUnmatched`]) or layers ([`VectorNote::LayersUnmatched`]).
//! - [`VectorItem::PushClip`] / [`VectorItem::PopClip`]: clip paths (they arrive in user space).
//! - [`VectorItem::PushGroup`] / [`VectorItem::PopGroup`]: transparency groups (opacity, blend).
//! - Everything else is a typed note and is not drawn: images, soft masks, shadings that are not
//!   axial or radial, tiling patterns, shading transfer functions.
//!
//! The interpreter runs on its own thread with the flatten stack ([`crate::RENDER_STACK_BYTES`]);
//! a panic there is a typed refusal. Budgets ([`MAX_VECTOR_ITEMS`], [`MAX_VECTOR_ELEMENTS`]) stop the
//! recording with [`VectorNote::Budget`]; cancellation is polled every 64 items.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{PressError, Result};
use crate::vector_source::{DocSource, PageSource, SrcPaint, read_sources};
pub use crate::vector_source::{VectorColour, VectorIcc, VectorLayer, VectorSpace};
use hayro_interpret::font::Glyph;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask, StrokeProps,
    interpret_page,
};
use hsk_studio_accord::CancellationToken;
use kurbo::{Affine, BezPath, PathEl};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Most recorded items over all pages.
pub const MAX_VECTOR_ITEMS: usize = 200_000;
/// Most path elements over all recorded items.
pub const MAX_VECTOR_ELEMENTS: usize = 4_000_000;
/// Stops a shading is sampled at.
pub const GRADIENT_SAMPLES: usize = 16;
/// Most pages read.
pub const MAX_VECTOR_PAGES: usize = 1000;
/// Most embedded font bytes kept over a document.
pub const MAX_VECTOR_FONT_BYTES: usize = 64 << 20;

/// A path element in user space (points, y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VectorEl {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    CubicTo([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// How a path is painted.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorMode {
    Fill { even_odd: bool },
    Stroke(VectorStroke),
}

/// Stroke parameters in user space.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorStroke {
    pub width: f64,
    /// 0 butt, 1 round, 2 square (PDF `J`).
    pub cap: u8,
    /// 0 miter, 1 round, 2 bevel (PDF `j`).
    pub join: u8,
    pub miter_limit: f64,
    pub dash: Vec<f64>,
    pub dash_offset: f64,
}

/// A paint.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorPaint {
    /// Straight RGBA, 0..=1.
    Solid([f32; 4]),
    /// An axial (`coords` x0 y0 x1 y1) or radial (`coords` x0 y0 r0 x1 y1 r1) shading in shading
    /// space, `matrix` shading space -> user space, `stops` (offset, RGBA) sampled evenly over the
    /// domain, `extend` before / after.
    Gradient { axial: bool, coords: [f64; 6], matrix: [f64; 6], stops: Vec<(f64, [f32; 4])>, extend: [bool; 2] },
}

/// A glyph a draw came from.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorGlyph {
    /// Index into [`VectorDocument::fonts`].
    pub font: usize,
    pub glyph_id: u32,
    /// What the glyph stands for (ToUnicode, then glyph names), when known.
    pub unicode: Option<String>,
    /// Glyph space (1000 units per em) to user space.
    pub matrix: [f64; 6],
}

/// A font glyphs were drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorFont {
    /// Without a subset tag.
    pub postscript_name: Option<String>,
    /// TrueType / OpenType / CFF bytes when the font program is readable (not Type 1), within
    /// [`MAX_VECTOR_FONT_BYTES`].
    pub bytes: Option<Arc<[u8]>>,
}

/// One recorded item, in paint order.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum VectorItem {
    /// `source`: the colour a solid paint was written in ([`read_vector_document`] only).
    Draw { path: Vec<VectorEl>, mode: VectorMode, paint: VectorPaint, blend: u8, glyph: Option<VectorGlyph>, source: Option<VectorColour> },
    PushClip { path: Vec<VectorEl>, even_odd: bool },
    PopClip,
    /// `blend` is the PDF blend mode index (0 = Normal, in ISO 32000-2 table 134 order).
    PushGroup { opacity: f32, blend: u8 },
    PopGroup,
    /// The content of an optional-content group (index into [`VectorDocument::layers`]) starts.
    BeginLayer { layer: usize },
    EndLayer,
}

/// What reading left out (typed, never silent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VectorNote {
    TextAsOutlines,
    ImageNotRead,
    SoftMaskNotRead,
    PatternNotRead,
    ShadingNotRead,
    Budget,
    ColourConverted,
    SourceColourUnmatched,
    LayersUnmatched,
    HiddenLayersNotRead,
    SpotAlternateNotRead,
    StructureNotRead,
    FontBytesBudget,
}

impl VectorNote {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TextAsOutlines => "pdf_text_as_outlines",
            Self::ImageNotRead => "pdf_image_not_read",
            Self::SoftMaskNotRead => "pdf_soft_mask_not_read",
            Self::PatternNotRead => "pdf_pattern_not_read",
            Self::ShadingNotRead => "pdf_shading_not_read",
            Self::Budget => "pdf_vector_budget",
            Self::ColourConverted => "pdf_colour_converted",
            Self::SourceColourUnmatched => "pdf_source_colour_unmatched",
            Self::LayersUnmatched => "pdf_layers_unmatched",
            Self::HiddenLayersNotRead => "pdf_hidden_layers_not_read",
            Self::SpotAlternateNotRead => "pdf_spot_alternate_not_read",
            Self::StructureNotRead => "pdf_structure_not_read",
            Self::FontBytesBudget => "pdf_font_bytes_budget",
        }
    }
}

/// One page: its media and crop boxes (points) and what it draws.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorPage {
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub items: Vec<VectorItem>,
    pub notes: Vec<VectorNote>,
}

/// A whole document ([`read_vector_document`]).
#[derive(Clone, Debug, PartialEq)]
pub struct VectorDocument {
    pub pages: Vec<VectorPage>,
    pub fonts: Vec<VectorFont>,
    pub layers: Vec<VectorLayer>,
    pub profiles: Vec<VectorIcc>,
    /// `/OutputIntents` destination profile (index into `profiles`).
    pub output_intent: Option<usize>,
}

/// Fonts seen so far (by hayro's font cache key).
#[derive(Default)]
struct FontTable {
    list: Vec<VectorFont>,
    by_key: BTreeMap<u128, usize>,
    bytes: usize,
    over: bool,
}

fn blend_index(b: BlendMode) -> u8 {
    match b {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::ColorDodge => 6,
        BlendMode::ColorBurn => 7,
        BlendMode::HardLight => 8,
        BlendMode::SoftLight => 9,
        BlendMode::Difference => 10,
        BlendMode::Exclusion => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
    }
}

struct Recorder<'c> {
    items: Vec<VectorItem>,
    notes: BTreeSet<VectorNote>,
    elements: usize,
    blend: u8,
    full: bool,
    canceled: bool,
    cancel: &'c CancellationToken,
    fonts: &'c mut FontTable,
    /// The walk of this page, when there is one.
    src: Option<&'c PageSource>,
    path_at: usize,
    mark_at: usize,
    src_ok: bool,
    marks_ok: bool,
    /// Open marked content: whether it opened a layer.
    mark_stack: Vec<bool>,
    /// Inside a Type 3 glyph procedure (its drawing consumes no walk events).
    type3: u32,
}

impl Recorder<'_> {
    fn live(&mut self) -> bool {
        if self.full || self.canceled {
            return false;
        }
        if self.items.len().is_multiple_of(64) && self.cancel.check().is_err() {
            self.canceled = true;
            return false;
        }
        true
    }

    fn convert(&mut self, path: &BezPath, t: Affine) -> Option<Vec<VectorEl>> {
        let n = path.elements().len();
        match self.elements.checked_add(n) {
            Some(total) if total <= MAX_VECTOR_ELEMENTS && self.items.len() < MAX_VECTOR_ITEMS => self.elements = total,
            _ => {
                self.full = true;
                self.notes.insert(VectorNote::Budget);
                return None;
            }
        }
        let m = t.as_coeffs();
        let p = |q: kurbo::Point| [m[0] * q.x + m[2] * q.y + m[4], m[1] * q.x + m[3] * q.y + m[5]];
        let mut out = Vec::with_capacity(n);
        let mut last = kurbo::Point::ZERO;
        for el in path.elements() {
            out.push(match *el {
                PathEl::MoveTo(a) => {
                    last = a;
                    VectorEl::MoveTo(p(a))
                }
                PathEl::LineTo(a) => {
                    last = a;
                    VectorEl::LineTo(p(a))
                }
                PathEl::QuadTo(c, a) => {
                    let c1 = kurbo::Point::new(last.x + (c.x - last.x) * (2.0 / 3.0), last.y + (c.y - last.y) * (2.0 / 3.0));
                    let c2 = kurbo::Point::new(a.x + (c.x - a.x) * (2.0 / 3.0), a.y + (c.y - a.y) * (2.0 / 3.0));
                    last = a;
                    VectorEl::CubicTo(p(c1), p(c2), p(a))
                }
                PathEl::CurveTo(c1, c2, a) => {
                    last = a;
                    VectorEl::CubicTo(p(c1), p(c2), p(a))
                }
                PathEl::ClosePath => VectorEl::Close,
            });
        }
        if out.iter().flat_map(|e| match e {
            VectorEl::MoveTo(a) | VectorEl::LineTo(a) => vec![*a],
            VectorEl::CubicTo(a, b, c) => vec![*a, *b, *c],
            VectorEl::Close => vec![],
        }).any(|q| !(q[0].is_finite() && q[1].is_finite())) {
            return None;
        }
        Some(out)
    }

    fn paint(&mut self, paint: &Paint<'_>) -> Option<VectorPaint> {
        match paint {
            Paint::Color(c) => Some(VectorPaint::Solid(c.to_rgba().components())),
            Paint::Pattern(pattern) => match &**pattern {
                hayro_interpret::pattern::Pattern::Shading(sp) => {
                    let hayro_interpret::shading::ShadingType::RadialAxial { coords, domain, function, extend, axial } = &*sp.shading.shading_type else {
                        self.notes.insert(VectorNote::ShadingNotRead);
                        return None;
                    };
                    if sp.transfer_function.is_some() {
                        self.notes.insert(VectorNote::ShadingNotRead);
                        return None;
                    }
                    let mut stops = Vec::with_capacity(GRADIENT_SAMPLES);
                    for k in 0..GRADIENT_SAMPLES {
                        let s = k as f32 / (GRADIENT_SAMPLES - 1) as f32;
                        let t = domain[0] + s * (domain[1] - domain[0]);
                        let mut input = hayro_interpret::color::ColorComponents::new();
                        input.push(t);
                        let comps = function.eval(&input)?;
                        stops.push((f64::from(s), sp.shading.color_space.to_rgba(&comps, sp.opacity, false).components()));
                    }
                    let m = sp.matrix.as_coeffs();
                    Some(VectorPaint::Gradient { axial: *axial, coords: coords.map(f64::from), matrix: m, stops, extend: *extend })
                }
                _ => {
                    self.notes.insert(VectorNote::PatternNotRead);
                    None
                }
            },
        }
    }

    fn stroke(props: &StrokeProps, t: Affine) -> VectorStroke {
        let scale = t.determinant().abs().sqrt();
        let k = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        VectorStroke {
            width: f64::from(props.line_width).abs() * k,
            cap: match props.line_cap {
                kurbo::Cap::Butt => 0,
                kurbo::Cap::Round => 1,
                kurbo::Cap::Square => 2,
            },
            join: match props.line_join {
                kurbo::Join::Miter => 0,
                kurbo::Join::Round => 1,
                kurbo::Join::Bevel => 2,
            },
            miter_limit: f64::from(props.miter_limit).max(1.0),
            dash: props.dash_array.iter().map(|d| f64::from(d.abs()) * k).collect(),
            dash_offset: f64::from(props.dash_offset) * k,
        }
    }

    /// The next walk event for a path the interpreter paints, checked against its paint.
    fn next_source(&mut self, paint: &Paint<'_>) -> Option<VectorColour> {
        let src = self.src?;
        if !self.src_ok || self.type3 > 0 {
            return None;
        }
        let event = src.paths.get(self.path_at);
        self.path_at = self.path_at.saturating_add(1);
        match (event, paint) {
            (Some(SrcPaint::Pattern), Paint::Pattern(_)) => None,
            (Some(SrcPaint::Colour(c)), Paint::Color(drawn)) => {
                let rgba = drawn.to_rgba().components();
                let close = |want: &[f64]| want.iter().zip(rgba).all(|(w, d)| (w - f64::from(d)).abs() <= 0.02);
                let agrees = match c {
                    Some(VectorColour::Process { space: VectorSpace::Gray, components }) => components.first().is_none_or(|g| close(&[*g, *g, *g])),
                    Some(VectorColour::Process { space: VectorSpace::Rgb, components }) => components.len() != 3 || close(components),
                    _ => true,
                };
                if !agrees {
                    self.src_ok = false;
                    return None;
                }
                c.clone()
            }
            _ => {
                self.src_ok = false;
                None
            }
        }
    }

    fn font_index(&mut self, g: &hayro_interpret::font::OutlineGlyph) -> usize {
        let key = g.font_cache_key();
        if let Some(i) = self.fonts.by_key.get(&key) {
            return *i;
        }
        let data = g.font_data();
        let postscript_name = data.as_ref().and_then(|d| d.postscript_name.clone()).map(|n| match n.split_once('+') {
            Some((tag, rest)) if tag.len() == 6 => rest.to_owned(),
            _ => n,
        });
        let bytes = data.and_then(|d| {
            let b: &[u8] = (*d.data).as_ref();
            match self.fonts.bytes.checked_add(b.len()) {
                Some(total) if total <= MAX_VECTOR_FONT_BYTES => {
                    self.fonts.bytes = total;
                    Some(Arc::<[u8]>::from(b))
                }
                _ => {
                    self.fonts.over = true;
                    None
                }
            }
        });
        let i = self.fonts.list.len();
        self.fonts.list.push(VectorFont { postscript_name, bytes });
        self.fonts.by_key.insert(key, i);
        i
    }

    fn draw(&mut self, path: &BezPath, t: Affine, paint: &Paint<'_>, mode: VectorModeIn<'_>, glyph: Option<VectorGlyph>) {
        // A painted path consumes its walk event whether or not it is recorded.
        let source = if glyph.is_none() { self.next_source(paint) } else { None };
        if !self.live() {
            return;
        }
        let Some(paint) = self.paint(paint) else { return };
        let mode = match mode {
            VectorModeIn::Fill(rule) => VectorMode::Fill { even_odd: rule == FillRule::EvenOdd },
            VectorModeIn::Stroke(props) => VectorMode::Stroke(Self::stroke(props, t)),
        };
        let Some(path) = self.convert(path, t) else { return };
        let blend = self.blend;
        let source = if matches!(paint, VectorPaint::Solid(_)) { source } else { None };
        self.items.push(VectorItem::Draw { path, mode, paint, blend, glyph, source });
    }
}

enum VectorModeIn<'p> {
    Fill(FillRule),
    Stroke(&'p StrokeProps),
}

impl<'a> Device<'a> for Recorder<'_> {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        if mask.is_some() {
            self.notes.insert(VectorNote::SoftMaskNotRead);
        }
    }

    fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.blend = blend_index(blend_mode);
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        let mode = match draw_mode {
            PathDrawMode::Fill(rule) => VectorModeIn::Fill(*rule),
            PathDrawMode::Stroke(props) => VectorModeIn::Stroke(props),
        };
        self.draw(path, transform, paint, mode, None);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        if !self.live() {
            // Keep the push/pop pairing: an empty clip stands in.
            self.items.push(VectorItem::PushClip { path: Vec::new(), even_odd: false });
            return;
        }
        let path = self.convert(&clip_path.path, Affine::IDENTITY).unwrap_or_default();
        self.items.push(VectorItem::PushClip { path, even_odd: clip_path.fill == FillRule::EvenOdd });
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode) {
        if mask.is_some() {
            self.notes.insert(VectorNote::SoftMaskNotRead);
        }
        let opacity = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 0.0 };
        self.items.push(VectorItem::PushGroup { opacity, blend: blend_index(blend_mode) });
    }

    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine, paint: &Paint<'a>, draw_mode: &GlyphDrawMode) {
        match glyph {
            Glyph::Outline(g) => {
                if matches!(draw_mode, GlyphDrawMode::Invisible) {
                    return;
                }
                let outline = g.outline();
                let t = std::ops::Mul::mul(transform, glyph_transform);
                let font = self.font_index(g);
                let unicode = g.as_unicode().map(|u| match u {
                    hayro_interpret::hayro_cmap::BfString::Char(c) => c.to_string(),
                    hayro_interpret::hayro_cmap::BfString::String(s) => s,
                });
                let info = VectorGlyph { font, glyph_id: g.glyph_id().to_u32(), unicode, matrix: t.as_coeffs() };
                match draw_mode {
                    GlyphDrawMode::Fill => self.draw(&outline, t, paint, VectorModeIn::Fill(FillRule::NonZero), Some(info)),
                    GlyphDrawMode::Stroke(props) => self.draw(&outline, t, paint, VectorModeIn::Stroke(props), Some(info)),
                    GlyphDrawMode::Invisible => {}
                }
            }
            Glyph::Type3(t3) => {
                if !matches!(draw_mode, GlyphDrawMode::Invisible) {
                    self.notes.insert(VectorNote::TextAsOutlines);
                    self.type3 = self.type3.saturating_add(1);
                    t3.interpret(self, transform, glyph_transform, paint);
                    self.type3 = self.type3.saturating_sub(1);
                }
            }
        }
    }

    fn draw_image(&mut self, _image: Image<'a, '_>, _transform: Affine) {
        self.notes.insert(VectorNote::ImageNotRead);
    }

    fn pop_clip_path(&mut self) {
        self.items.push(VectorItem::PopClip);
    }

    fn pop_transparency_group(&mut self) {
        self.items.push(VectorItem::PopGroup);
    }

    fn begin_marked_content(&mut self, tag: &[u8], _mcid: Option<i32>) {
        if self.type3 > 0 {
            return;
        }
        let Some(src) = self.src else { return };
        let mark = src.marks.get(self.mark_at);
        self.mark_at = self.mark_at.saturating_add(1);
        let opens = match mark {
            Some(m) if self.marks_ok && m.tag == tag => match m.layer {
                // Only the outermost layer is kept (Illustrator layers are page level).
                Some(layer) if !self.mark_stack.contains(&true) => {
                    self.items.push(VectorItem::BeginLayer { layer });
                    true
                }
                _ => false,
            },
            _ => {
                self.marks_ok = false;
                false
            }
        };
        self.mark_stack.push(opens);
    }

    fn end_marked_content(&mut self) {
        if self.type3 > 0 || self.src.is_none() {
            return;
        }
        if self.mark_stack.pop() == Some(true) {
            self.items.push(VectorItem::EndLayer);
        }
    }
}

fn rect(r: hayro_interpret::hayro_syntax::object::Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

/// Every page of `pdf` as recorded vector content (module docs), without source colours or
/// layers (see [`read_vector_document`]).
pub fn read_vector_pages(pdf: &[u8], max_bytes: usize, cancel: &CancellationToken) -> Result<Vec<VectorPage>> {
    read(pdf, max_bytes, None, cancel).map(|d| d.pages)
}

/// [`read_vector_pages`] plus fonts, source colours, layers and profiles (module docs).
pub fn read_vector_document(pdf: &[u8], max_bytes: usize, cancel: &CancellationToken) -> Result<VectorDocument> {
    if pdf.len() > max_bytes {
        return Err(PressError::InputLimit("vector_read"));
    }
    cancel.check().map_err(|_| PressError::Canceled)?;
    let sources = read_sources(pdf, cancel);
    cancel.check().map_err(|_| PressError::Canceled)?;
    let mut doc = read(pdf, max_bytes, Some(sources.as_ref()), cancel)?;
    if let Some(s) = sources {
        doc.layers = s.layers;
        doc.profiles = s.profiles;
        doc.output_intent = s.output_intent;
    }
    Ok(doc)
}

/// Strips what a page's walk could not vouch for.
fn settle(rec: &mut Recorder<'_>) {
    let Some(src) = rec.src else { return };
    if !rec.src_ok || rec.path_at != src.paths.len() {
        for i in &mut rec.items {
            if let VectorItem::Draw { source, .. } = i {
                *source = None;
            }
        }
        rec.notes.insert(VectorNote::SourceColourUnmatched);
    } else {
        if src.spot_without_alternate {
            rec.notes.insert(VectorNote::SpotAlternateNotRead);
        }
        if src.converted {
            rec.notes.insert(VectorNote::ColourConverted);
        }
    }
    if !rec.marks_ok || rec.mark_at != src.marks.len() || !rec.mark_stack.is_empty() {
        if src.marks.iter().any(|m| m.layer.is_some()) {
            rec.notes.insert(VectorNote::LayersUnmatched);
        }
        rec.items.retain(|i| !matches!(i, VectorItem::BeginLayer { .. } | VectorItem::EndLayer));
    }
}

/// `sources`: `None` reads without a walk ([`read_vector_pages`]); `Some(None)` is a walk that
/// failed (every page noted [`VectorNote::StructureNotRead`]).
fn read(pdf: &[u8], max_bytes: usize, sources: Option<Option<&DocSource>>, cancel: &CancellationToken) -> Result<VectorDocument> {
    if pdf.len() > max_bytes {
        return Err(PressError::InputLimit("vector_read"));
    }
    cancel.check().map_err(|_| PressError::Canceled)?;
    let walk = sources.flatten();
    // Hidden layers are read from the copy with every group on.
    let bytes = walk.and_then(|s| s.all_visible.clone()).unwrap_or_else(|| pdf.to_vec());
    let hidden_not_read = walk.is_some_and(|s| s.hidden_not_read);
    let walked_any = sources.is_some();
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new().name("hsk-press-vector-read".into()).stack_size(crate::RENDER_STACK_BYTES).spawn_scoped(scope, move || -> Result<VectorDocument> {
            let pdf = Pdf::new(bytes).map_err(|_| PressError::Malformed("pdf_unreadable"))?;
            let pages = pdf.pages();
            if pages.len() > MAX_VECTOR_PAGES {
                return Err(PressError::InputLimit("vector_read"));
            }
            // A page count the walk disagrees with: no page is matched.
            let walked = walk.filter(|s| s.pages.len() == pages.len());
            let mut fonts = FontTable::default();
            let mut out = Vec::with_capacity(pages.len());
            let mut used_items = 0_usize;
            let mut used_elements = 0_usize;
            for (n, page) in pages.iter().enumerate() {
                let settings = InterpreterSettings { render_annotations: false, ..InterpreterSettings::default() };
                let cache = InterpreterCache::new();
                let media = page.media_box();
                let mut ctx = Context::new(Affine::IDENTITY, kurbo::Rect::new(media.x0, media.y0, media.x1, media.y1), &cache, pdf.xref(), settings);
                let src = walked.and_then(|s| s.pages.get(n)).and_then(Option::as_ref);
                let mut rec = Recorder {
                    items: Vec::new(),
                    notes: BTreeSet::new(),
                    elements: used_elements,
                    blend: 0,
                    full: used_items >= MAX_VECTOR_ITEMS,
                    canceled: false,
                    cancel,
                    fonts: &mut fonts,
                    src,
                    path_at: 0,
                    mark_at: 0,
                    src_ok: true,
                    marks_ok: true,
                    mark_stack: Vec::new(),
                    type3: 0,
                };
                interpret_page(page, &mut ctx, &mut rec);
                if rec.canceled {
                    return Err(PressError::Canceled);
                }
                settle(&mut rec);
                if walked_any && src.is_none() {
                    rec.notes.insert(VectorNote::StructureNotRead);
                }
                if hidden_not_read {
                    rec.notes.insert(VectorNote::HiddenLayersNotRead);
                }
                used_items = used_items.saturating_add(rec.items.len());
                used_elements = rec.elements;
                let (items, mut notes) = (rec.items, rec.notes);
                if fonts.over {
                    notes.insert(VectorNote::FontBytesBudget);
                }
                out.push(VectorPage { media_box: rect(media), crop_box: rect(page.intersected_crop_box()), items, notes: notes.into_iter().collect() });
            }
            Ok(VectorDocument { pages: out, fonts: fonts.list, layers: Vec::new(), profiles: Vec::new(), output_intent: None })
        });
        match spawned {
            Ok(handle) => handle.join().unwrap_or(Err(PressError::Malformed("vector_reader_panicked"))),
            Err(_) => Err(PressError::Malformed("vector_reader_thread")),
        }
    })
}
