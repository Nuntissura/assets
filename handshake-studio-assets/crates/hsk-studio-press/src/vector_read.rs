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
//!   drawn as glyph outlines ([`VectorNote::TextAsOutlines`]); Type 3 glyphs are interpreted.
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
use hayro_interpret::font::Glyph;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask, StrokeProps,
    interpret_page,
};
use hsk_studio_accord::CancellationToken;
use kurbo::{Affine, BezPath, PathEl};
use std::collections::BTreeSet;

/// Most recorded items over all pages.
pub const MAX_VECTOR_ITEMS: usize = 200_000;
/// Most path elements over all recorded items.
pub const MAX_VECTOR_ELEMENTS: usize = 4_000_000;
/// Stops a shading is sampled at.
pub const GRADIENT_SAMPLES: usize = 16;
/// Most pages read.
pub const MAX_VECTOR_PAGES: usize = 1000;

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

/// One recorded item, in paint order.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorItem {
    Draw { path: Vec<VectorEl>, mode: VectorMode, paint: VectorPaint, blend: u8, glyph: bool },
    PushClip { path: Vec<VectorEl>, even_odd: bool },
    PopClip,
    /// `blend` is the PDF blend mode index (0 = Normal, in ISO 32000-2 table 134 order).
    PushGroup { opacity: f32, blend: u8 },
    PopGroup,
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

    fn draw(&mut self, path: &BezPath, t: Affine, paint: &Paint<'_>, mode: VectorModeIn<'_>, glyph: bool) {
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
        self.items.push(VectorItem::Draw { path, mode, paint, blend, glyph });
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
        self.draw(path, transform, paint, mode, false);
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
                self.notes.insert(VectorNote::TextAsOutlines);
                let outline = g.outline();
                let t = std::ops::Mul::mul(transform, glyph_transform);
                match draw_mode {
                    GlyphDrawMode::Fill => self.draw(&outline, t, paint, VectorModeIn::Fill(FillRule::NonZero), true),
                    GlyphDrawMode::Stroke(props) => self.draw(&outline, t, paint, VectorModeIn::Stroke(props), true),
                    GlyphDrawMode::Invisible => {}
                }
            }
            Glyph::Type3(t3) => {
                if !matches!(draw_mode, GlyphDrawMode::Invisible) {
                    self.notes.insert(VectorNote::TextAsOutlines);
                    t3.interpret(self, transform, glyph_transform, paint);
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
}

fn rect(r: hayro_interpret::hayro_syntax::object::Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

/// Every page of `pdf` as recorded vector content (module docs).
pub fn read_vector_pages(pdf: &[u8], max_bytes: usize, cancel: &CancellationToken) -> Result<Vec<VectorPage>> {
    if pdf.len() > max_bytes {
        return Err(PressError::InputLimit("vector_read"));
    }
    cancel.check().map_err(|_| PressError::Canceled)?;
    let bytes = pdf.to_vec();
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new().name("hsk-press-vector-read".into()).stack_size(crate::RENDER_STACK_BYTES).spawn_scoped(scope, move || -> Result<Vec<VectorPage>> {
            let pdf = Pdf::new(bytes).map_err(|_| PressError::Malformed("pdf_unreadable"))?;
            let pages = pdf.pages();
            if pages.len() > MAX_VECTOR_PAGES {
                return Err(PressError::InputLimit("vector_read"));
            }
            let mut out = Vec::with_capacity(pages.len());
            let mut used_items = 0_usize;
            let mut used_elements = 0_usize;
            for page in pages.iter() {
                let settings = InterpreterSettings { render_annotations: false, ..InterpreterSettings::default() };
                let cache = InterpreterCache::new();
                let media = page.media_box();
                let mut ctx = Context::new(Affine::IDENTITY, kurbo::Rect::new(media.x0, media.y0, media.x1, media.y1), &cache, pdf.xref(), settings);
                let mut rec = Recorder { items: Vec::new(), notes: BTreeSet::new(), elements: used_elements, blend: 0, full: used_items >= MAX_VECTOR_ITEMS, canceled: false, cancel };
                interpret_page(page, &mut ctx, &mut rec);
                if rec.canceled {
                    return Err(PressError::Canceled);
                }
                used_items = used_items.saturating_add(rec.items.len());
                used_elements = rec.elements;
                out.push(VectorPage { media_box: rect(media), crop_box: rect(page.intersected_crop_box()), items: rec.items, notes: rec.notes.into_iter().collect() });
            }
            Ok(out)
        });
        match spawned {
            Ok(handle) => handle.join().unwrap_or(Err(PressError::Malformed("vector_reader_panicked"))),
            Err(_) => Err(PressError::Malformed("vector_reader_thread")),
        }
    })
}
