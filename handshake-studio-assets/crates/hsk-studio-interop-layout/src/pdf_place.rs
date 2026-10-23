//! PDF place: a page of a PDF placed as a graphic (lane SMB-ID-PREPRESS; STU-LAY-028 placed
//! formats, STU-LAY-130 placed-PDF crop enumeration). The page is read by the existing press
//! readers: its boxes from the press object-model projection (`PdfDoc::proposal`, `PageView`
//! media/crop/bleed/trim/art boxes) and its content from `read_vector_document` (paint order, PDF
//! user space, source colours, optional-content layers).
//!
//! - [`place_pdf_page`] resolves the STU-LAY-130 crop to a rectangle in the page's user space:
//!   art / crop / trim / bleed / media boxes (ISO 32000-1 14.11.2: the crop box defaults to the media
//!   box, bleed / trim / art default to the crop box, every box is intersected with the media box),
//!   `content_visible_layers` / `content_all_layers` = the bounds of what the page draws (path
//!   control points, strokes widened by half their width; with visible layers only, content inside a
//!   hidden optional-content group is left out). The placed content size is the crop size, which is
//!   what `hsk_studio_layout::fit_content` takes as `content_w x content_h`.
//! - [`draw_placed_pdf`] draws the placed page into a press `PdfExport` page through an affine
//!   `[a b c d e f]` (placed page user space -> export page user space), clipped to the crop
//!   rectangle and the caller's frame path: fills and strokes (DeviceCMYK / DeviceGray source
//!   colours kept, other colours as the RGB the interpreter resolved), glyphs as outlines, clips,
//!   transparency groups (opacity, blend mode); hidden layers are not drawn.
//!
//! Typed losses (never silent): shadings, images, soft masks, dashes, rotated pages (`/Rotate`
//! other than 0 is placed unrotated), paint alpha below 1 outside a group, and every reader note of
//! the page. Embedding the page as a form XObject (exact, incl. images and fonts) is a declared gap.
//! InDesign place behaviour UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{
    BlendMode, Colour, ExportError, FillRule, GroupStyle, LineCap, LineJoin, PageHandle, PathCommand, PdfDoc, PdfExport,
    StrokeStyle, VectorColour, VectorDocument, VectorEl, VectorItem, VectorMode, VectorPaint, VectorSpace, read_vector_document,
};

/// STU-LAY-130 crop enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfCrop {
    Art,
    Crop,
    Trim,
    Bleed,
    Media,
    ContentVisibleLayers,
    ContentAllLayers,
}
impl PdfCrop {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Art => "pdf_crop.art",
            Self::Crop => "pdf_crop.crop",
            Self::Trim => "pdf_crop.trim",
            Self::Bleed => "pdf_crop.bleed",
            Self::Media => "pdf_crop.media",
            Self::ContentVisibleLayers => "pdf_crop.content_visible_layers",
            Self::ContentAllLayers => "pdf_crop.content_all_layers",
        }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// The press reader refused the file (its error code).
    Read(&'static str),
    NoSuchPage(usize),
    /// The page has no media box, or the crop is empty.
    EmptyCrop,
    /// `content_*` crops of a page that draws nothing.
    NoContent,
    /// The export refused a draw (its error code).
    Export(&'static str),
    InvalidTransform,
    Canceled,
}
impl PlaceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Read(c) | Self::Export(c) => c,
            Self::NoSuchPage(_) => "pdf_page_missing",
            Self::EmptyCrop => "pdf_crop_empty",
            Self::NoContent => "pdf_page_draws_nothing",
            Self::InvalidTransform => "invalid_transform",
            Self::Canceled => "canceled",
        }
    }
}
impl From<ExportError> for PlaceError {
    fn from(e: ExportError) -> Self {
        Self::Export(e.code())
    }
}

/// What placing or drawing left out.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlaceLoss {
    ShadingNotDrawn,
    DashNotDrawn,
    PaintAlphaIgnored,
    RotatedPagePlacedUnrotated(i64),
    /// A reader note of the page (`VectorNote::code`).
    Reader(&'static str),
}

/// A placed page.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedPdf {
    pub page_index: usize,
    pub crop: PdfCrop,
    /// `[x0 y0 x1 y1]` in the page's user space.
    pub crop_rect: [f64; 4],
    pub content_w: f64,
    pub content_h: f64,
    pub document: VectorDocument,
    pub losses: Vec<PlaceLoss>,
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// Bounds of what the page draws (see module docs); `visible_only` skips hidden layers.
fn content_bounds(doc: &VectorDocument, page: usize, visible_only: bool) -> Option<[f64; 4]> {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut hidden: Vec<bool> = Vec::new();
    for item in &doc.pages.get(page)?.items {
        match item {
            VectorItem::BeginLayer { layer } => hidden.push(visible_only && doc.layers.get(*layer).is_some_and(|l| !l.visible)),
            VectorItem::EndLayer => {
                hidden.pop();
            }
            VectorItem::Draw { path, mode, .. } if !hidden.iter().any(|h| *h) => {
                let pad = match mode {
                    VectorMode::Stroke(s) if s.width.is_finite() => s.width.abs() / 2.0,
                    _ => 0.0,
                };
                for el in path {
                    let pts: &[[f64; 2]] = match el {
                        VectorEl::MoveTo(p) | VectorEl::LineTo(p) => std::slice::from_ref(p),
                        VectorEl::CubicTo(a, c, p) => &[*a, *c, *p][..],
                        VectorEl::Close => &[],
                    };
                    for p in pts {
                        if p[0].is_finite() && p[1].is_finite() {
                            b = [b[0].min(p[0] - pad), b[1].min(p[1] - pad), b[2].max(p[0] + pad), b[3].max(p[1] + pad)];
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (b[0] < b[2] && b[1] < b[3]).then_some(b)
}

/// Places page `page_index` of `pdf` with `crop` (module docs). `max_bytes` bounds the input.
pub fn place_pdf_page(pdf: &[u8], page_index: usize, crop: PdfCrop, max_bytes: usize, cancel: &CancellationToken) -> Result<PlacedPdf, PlaceError> {
    cancel.check().map_err(|_| PlaceError::Canceled)?;
    let limits = hsk_studio_press::Limits { max_bytes, ..hsk_studio_press::Limits::default() };
    let pdoc = PdfDoc::open(pdf, &limits, cancel).map_err(|e| PlaceError::Read(e.code()))?;
    let model = pdoc.proposal(cancel).map_err(|e| PlaceError::Read(e.code()))?;
    let view = model.pages.get(page_index).ok_or(PlaceError::NoSuchPage(page_index))?;
    let document = read_vector_document(pdf, max_bytes, cancel).map_err(|e| PlaceError::Read(e.code()))?;
    let page = document.pages.get(page_index).ok_or(PlaceError::NoSuchPage(page_index))?;
    let media = view.media_box.ok_or(PlaceError::EmptyCrop)?;
    let cropbox = view.crop_box.map_or(media, |c| intersect(c, media));
    let boxed = |b: Option<[f64; 4]>| b.map_or(cropbox, |b| intersect(b, media));
    let rect = match crop {
        PdfCrop::Media => media,
        PdfCrop::Crop => cropbox,
        PdfCrop::Bleed => boxed(view.bleed_box),
        PdfCrop::Trim => boxed(view.trim_box),
        PdfCrop::Art => boxed(view.art_box),
        PdfCrop::ContentVisibleLayers => content_bounds(&document, page_index, true).ok_or(PlaceError::NoContent)?,
        PdfCrop::ContentAllLayers => content_bounds(&document, page_index, false).ok_or(PlaceError::NoContent)?,
    };
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err(PlaceError::EmptyCrop);
    }
    let mut losses: Vec<PlaceLoss> = page.notes.iter().map(|n| PlaceLoss::Reader(n.code())).collect();
    if view.rotate.rem_euclid(360) != 0 {
        losses.push(PlaceLoss::RotatedPagePlacedUnrotated(view.rotate));
    }
    Ok(PlacedPdf { page_index, crop, crop_rect: rect, content_w: w, content_h: h, document, losses })
}

fn map(m: [f64; 6], p: [f64; 2]) -> (f64, f64) {
    (m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5])
}

fn commands(m: [f64; 6], path: &[VectorEl]) -> Vec<PathCommand> {
    path.iter()
        .map(|el| match el {
            VectorEl::MoveTo(p) => {
                let (x, y) = map(m, *p);
                PathCommand::MoveTo(x, y)
            }
            VectorEl::LineTo(p) => {
                let (x, y) = map(m, *p);
                PathCommand::LineTo(x, y)
            }
            VectorEl::CubicTo(a, c, p) => {
                let ((ax, ay), (cx, cy), (x, y)) = (map(m, *a), map(m, *c), map(m, *p));
                PathCommand::CubicTo(ax, ay, cx, cy, x, y)
            }
            VectorEl::Close => PathCommand::Close,
        })
        .collect()
}

fn blend_of(i: u8) -> BlendMode {
    match i {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::ColorDodge,
        7 => BlendMode::ColorBurn,
        8 => BlendMode::HardLight,
        9 => BlendMode::SoftLight,
        10 => BlendMode::Difference,
        11 => BlendMode::Exclusion,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

fn colour_of(paint: &[f32; 4], source: &Option<VectorColour>) -> Colour {
    match source {
        Some(VectorColour::Process { space: VectorSpace::Cmyk, components }) if components.len() == 4 => {
            Colour::DeviceCmyk([components[0], components[1], components[2], components[3]])
        }
        Some(VectorColour::Process { space: VectorSpace::Gray, components }) if components.len() == 1 => Colour::DeviceGray(components[0]),
        _ => Colour::DeviceRgb([f64::from(paint[0]), f64::from(paint[1]), f64::from(paint[2])]),
    }
}

/// Draws a placed page into `page` of `ex` (module docs). `transform` maps the placed page's user
/// space to the export page's user space; `frame` (export space) clips in addition to the crop.
/// Returns the losses of this draw.
pub fn draw_placed_pdf(
    ex: &mut PdfExport,
    page: PageHandle,
    placed: &PlacedPdf,
    transform: [f64; 6],
    frame: Option<&[PathCommand]>,
    cancel: &CancellationToken,
) -> Result<Vec<PlaceLoss>, PlaceError> {
    let det = transform[0] * transform[3] - transform[1] * transform[2];
    if !transform.iter().all(|v| v.is_finite()) || det.abs() < 1e-12 {
        return Err(PlaceError::InvalidTransform);
    }
    let scale = det.abs().sqrt();
    let src = placed.document.pages.get(placed.page_index).ok_or(PlaceError::NoSuchPage(placed.page_index))?;
    let mut losses = Vec::new();
    let add = |l: PlaceLoss, losses: &mut Vec<PlaceLoss>| {
        if !losses.contains(&l) {
            losses.push(l);
        }
    };
    let mut clips = 0usize;
    if let Some(f) = frame {
        ex.push_clip(page, f, FillRule::NonZero)?;
        clips = clips.saturating_add(1);
    }
    let [x0, y0, x1, y1] = placed.crop_rect;
    let crop = [VectorEl::MoveTo([x0, y0]), VectorEl::LineTo([x1, y0]), VectorEl::LineTo([x1, y1]), VectorEl::LineTo([x0, y1]), VectorEl::Close];
    ex.push_clip(page, &commands(transform, &crop), FillRule::NonZero)?;
    clips = clips.saturating_add(1);
    let mut hidden: Vec<bool> = Vec::new();
    let mut groups: Vec<(f32, u8)> = Vec::new();
    for (n, item) in src.items.iter().enumerate() {
        if n.is_multiple_of(64) {
            cancel.check().map_err(|_| PlaceError::Canceled)?;
        }
        let skip = hidden.iter().any(|h| *h);
        match item {
            VectorItem::BeginLayer { layer } => hidden.push(placed.document.layers.get(*layer).is_some_and(|l| !l.visible)),
            VectorItem::EndLayer => {
                hidden.pop();
            }
            _ if skip => {}
            VectorItem::Draw { path, mode, paint, source, .. } => {
                let VectorPaint::Solid(rgba) = paint else {
                    add(PlaceLoss::ShadingNotDrawn, &mut losses);
                    continue;
                };
                if rgba[3] < 1.0 {
                    add(PlaceLoss::PaintAlphaIgnored, &mut losses);
                }
                let cmds = commands(transform, path);
                let colour = colour_of(rgba, source);
                match mode {
                    VectorMode::Fill { even_odd } => ex.fill_path(page, &cmds, if *even_odd { FillRule::EvenOdd } else { FillRule::NonZero }, colour)?,
                    VectorMode::Stroke(s) => {
                        if !s.dash.is_empty() {
                            add(PlaceLoss::DashNotDrawn, &mut losses);
                        }
                        let style = StrokeStyle {
                            width_pt: (s.width * scale).abs(),
                            cap: match s.cap {
                                1 => LineCap::Round,
                                2 => LineCap::Square,
                                _ => LineCap::Butt,
                            },
                            join: match s.join {
                                1 => LineJoin::Round,
                                2 => LineJoin::Bevel,
                                _ => LineJoin::Miter,
                            },
                        };
                        ex.stroke_path(page, &cmds, colour, style)?;
                    }
                }
            }
            VectorItem::PushClip { path, even_odd } => {
                ex.push_clip(page, &commands(transform, path), if *even_odd { FillRule::EvenOdd } else { FillRule::NonZero })?;
                clips = clips.saturating_add(1);
            }
            VectorItem::PopClip => {
                if clips > 0 {
                    ex.pop_clip(page)?;
                    clips = clips.saturating_sub(1);
                }
            }
            VectorItem::PushGroup { opacity, blend } => {
                ex.begin_group(page)?;
                groups.push((*opacity, *blend));
            }
            VectorItem::PopGroup => {
                if let Some((opacity, blend)) = groups.pop() {
                    ex.end_group(page, GroupStyle { opacity: f64::from(opacity), blend: blend_of(blend), isolated: true, knockout: false, soft_mask: None })?;
                }
            }
        }
    }
    while let Some((opacity, blend)) = groups.pop() {
        ex.end_group(page, GroupStyle { opacity: f64::from(opacity), blend: blend_of(blend), isolated: true, knockout: false, soft_mask: None })?;
    }
    for _ in 0..clips {
        ex.pop_clip(page)?;
    }
    for l in &placed.losses {
        add(l.clone(), &mut losses);
    }
    Ok(losses)
}
