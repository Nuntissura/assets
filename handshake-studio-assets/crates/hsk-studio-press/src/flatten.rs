//! Redaction flattening (Operator decision D27, "Flatten + narrow", START_LOG 2026-10-11T17:59:47Z).
//!
//! Every page that carries a redaction region is REPLACED in the redaction output by a page whose only
//! content is one RGB raster image of that page with every region on it painted opaque black: no text,
//! vector or font content survives on it, its annotations are dropped (not rendered), and none of its
//! own resources are carried (the page dictionary is rebuilt: `/Type /Page`, `/Parent`, `/MediaBox` =
//! `/CropBox` = the page's visible box, `/Rotate`, `/UserUnit`, `/Resources << /XObject << /HskFl0 >> >>`,
//! `/Contents`). Clearing a region is therefore true by construction: whatever any reader would draw
//! there, the raster holds the fill colour over the whole region (rounded outwards to whole pixels), and
//! nothing else of the page is left to draw (tests `flattened_pages_hold_only_the_raster`,
//! `flattened_region_pixels_are_opaque_and_the_rest_is_rendered`).
//!
//! Rendering: the page is first ISOLATED into a one-page document the crate writes itself (fresh
//! catalog and page tree, the page's inherited `/Resources`, `/Contents` and `/Group`, every object they
//! reach, every stream already decoded by this crate's bounded decoders, a raster codec left only as the
//! last filter, a stream the crate cannot decode given EMPTY), so the renderer never sees the source
//! file's page tree, annotations or encoded streams. hayro-interpret 0.7.0 (Apache-2.0 OR MIT, pure
//! Rust, `#![forbid(unsafe_code)]`; research basis in the lane notes `d27_flatten`) interprets that page
//! into the crate's own rasteriser (`paint.rs`). The page drawn is the page AFTER the region selector
//! ran, so every string the selector removed is absent from the raster even outside the region.
//!
//! Fails safe: content the renderer does not draw is absent from the raster and recorded as a typed
//! [`FlattenNote`] (`RedactedOutput::flattened`); it can never add content. Refusals are typed
//! ([`PressError::FlattenRefused`] with a [`FlattenCause`]), never a fallback to the original page: the
//! raster would exceed `Limits::max_flatten_pixels` or `Limits::max_decoded_bytes` (`PixelBudget`), the
//! page has no visible area (`PageGeometry`), an image of the page declares more pixels than
//! `Limits::max_decoded_bytes` (`ImageBudget`), the renderer cannot load the isolated page
//! (`RendererRefused`) or panics (`RendererPanicked`; caught) (tests `flatten_pixel_budget_is_a_typed_refusal`,
//! `renderer_failure_is_a_typed_refusal`).
//!
//! `check_flattened` re-reads the SAVED bytes independently of this module's state: the page dictionary
//! holds exactly the keys above, its content is exactly the canonical `q W 0 0 H X Y cm /HskFl0 Do Q`
//! for its `/MediaBox`, the XObject is an 8-bit `/DeviceRGB` `/FlateDecode` image of the declared size,
//! and every pixel of every region on the page is the fill colour. Each mismatch counts in
//! `RedactedOutput::residual_in_region`.
#![deny(clippy::arithmetic_side_effects)]
use crate::canon::write_ops;
use crate::content::Op;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::paint::Painter;
use crate::pdf_edit::PdfDoc;
use crate::raster::{decode_prefix, raster_tail};
use crate::reader::{Entry, check_cancel};
use crate::redaction::RedactionRegion;
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::writer::write_document;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::{Context, InterpreterCache, InterpreterSettings, InterpreterWarning, interpret_page};
use hsk_studio_accord::CancellationToken;
use kurbo::Affine;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Resource name of the raster on a flattened page.
pub(crate) const FLATTEN_NAME: &[u8] = b"HskFl0";
/// Fill colour of a region on the raster (opaque black).
pub const FLATTEN_FILL: [u8; 3] = [0, 0, 0];
/// Largest raster width or height in pixels.
const MAX_DIM: f64 = 65_535.0;
/// Page dictionary keys a flattened page holds (`check_flattened`).
const PAGE_KEYS: [&str; 8] = ["Type", "Parent", "MediaBox", "CropBox", "Rotate", "UserUnit", "Resources", "Contents"];

/// Content of a flattened page the renderer did not draw (absent from the raster) or dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlattenNote {
    /// The page had annotations; they were dropped, not rendered.
    Annotations,
    /// A fill, stroke or glyph painted with a pattern or shading.
    PatternPaint,
    /// Content drawn under a soft mask (ExtGState `/SMask` or a masked group).
    SoftMask,
    /// A non-separable blend mode (Hue, Saturation, Color, Luminosity) drawn as Normal.
    BlendModeApproximated,
    /// Content inside more than 64 nested clip paths.
    ClipDepth,
    /// Drawing stopped at the work or memory budget of the rasteriser.
    WorkBudget,
    /// The renderer reported a font it cannot draw.
    UnsupportedFont,
    /// The renderer reported an image it could not decode.
    ImageDecodeFailure,
    /// A stream this crate cannot decode was given to the renderer empty.
    UndecodableStream,
}

impl FlattenNote {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Annotations => "annotations",
            Self::PatternPaint => "pattern_paint",
            Self::SoftMask => "soft_mask",
            Self::BlendModeApproximated => "blend_mode_approximated",
            Self::ClipDepth => "clip_depth",
            Self::WorkBudget => "work_budget",
            Self::UnsupportedFont => "unsupported_font",
            Self::ImageDecodeFailure => "image_decode_failure",
            Self::UndecodableStream => "undecodable_stream",
        }
    }
}

/// Why a page carrying a region could not be flattened (the redaction refuses).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlattenCause {
    /// The raster at `Limits::flatten_dpi` would exceed `Limits::max_flatten_pixels` pixels, 65535
    /// pixels per side, or `Limits::max_decoded_bytes` bytes (3 per pixel); or the DPI is 0.
    PixelBudget,
    /// The page's visible box (`/CropBox` within `/MediaBox`) is missing, not finite or under 1 unit.
    PageGeometry,
    /// An image of the page declares more pixels than `Limits::max_decoded_bytes`.
    ImageBudget,
    /// The renderer could not load the isolated page.
    RendererRefused,
    /// The renderer panicked (caught).
    RendererPanicked,
}

impl FlattenCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PixelBudget => "pixel_budget",
            Self::PageGeometry => "page_geometry",
            Self::ImageBudget => "image_budget",
            Self::RendererRefused => "renderer_refused",
            Self::RendererPanicked => "renderer_panicked",
        }
    }
}

/// One flattened page of a redaction output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlattenedPage {
    pub page_index: u32,
    /// Object number of the page dictionary (unchanged; the dictionary is replaced).
    pub object: u32,
    pub width_px: u32,
    pub height_px: u32,
    /// What the renderer did not draw (sorted, unique).
    pub notes: Vec<FlattenNote>,
}

fn refuse(c: FlattenCause) -> PressError {
    PressError::FlattenRefused(c)
}

/// `[x0, y0, x1, y1]` with x0 <= x1, y0 <= y1.
fn normal(r: [f64; 4]) -> [f64; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

/// The visible box of a page: `/CropBox` (inherited, default `/MediaBox`) within `/MediaBox`.
fn visible_box(doc: &PdfDoc, page: u32) -> Result<[f64; 4]> {
    let media = doc.rect_of(doc.inherited(page, "MediaBox")).map(normal).ok_or(refuse(FlattenCause::PageGeometry))?;
    let crop = doc.rect_of(doc.inherited(page, "CropBox")).map(normal).unwrap_or(media);
    let b = [crop[0].max(media[0]), crop[1].max(media[1]), crop[2].min(media[2]), crop[3].min(media[3])];
    if !b.iter().all(|v| v.is_finite()) || b[2] - b[0] < 1.0 || b[3] - b[1] < 1.0 {
        return Err(refuse(FlattenCause::PageGeometry));
    }
    Ok(b)
}

/// Raster size for `vb` at `limits.flatten_dpi`: `(width, height)` in pixels, checked against the budgets
/// BEFORE anything is allocated (V4-01/V4-02; test `flatten_pixel_budget_is_a_typed_refusal`).
pub(crate) fn raster_size(vb: [f64; 4], limits: &Limits) -> Result<(u32, u32)> {
    if limits.flatten_dpi == 0 {
        return Err(refuse(FlattenCause::PixelBudget));
    }
    let k = f64::from(limits.flatten_dpi) / 72.0;
    // A whole number of pixels computed as 1650.0000000000002 is 1650, not 1651.
    let wf = ((vb[2] - vb[0]) * k - 1e-6).ceil().max(1.0);
    let hf = ((vb[3] - vb[1]) * k - 1e-6).ceil().max(1.0);
    if !(wf.is_finite() && hf.is_finite() && wf <= MAX_DIM && hf <= MAX_DIM) {
        return Err(refuse(FlattenCause::PixelBudget));
    }
    let (w, h) = (wf as u32, hf as u32);
    let px = u64::from(w).checked_mul(u64::from(h)).ok_or(refuse(FlattenCause::PixelBudget))?;
    let bytes = px.checked_mul(3).ok_or(refuse(FlattenCause::PixelBudget))?;
    if px > limits.max_flatten_pixels || bytes > u64::try_from(limits.max_decoded_bytes).unwrap_or(u64::MAX) {
        return Err(refuse(FlattenCause::PixelBudget));
    }
    Ok((w, h))
}

/// Pixel rectangle `[x0, y0, x1)` x `[y0, y1)` of a region on a raster of `w` x `h` pixels spanning `vb`,
/// rounded OUTWARDS and clamped to the raster.
pub(crate) fn region_pixels(region: [f64; 4], vb: [f64; 4], w: u32, h: u32) -> Option<[u32; 4]> {
    let r = normal(region);
    if !r.iter().all(|v| v.is_finite()) {
        return None;
    }
    let sx = f64::from(w) / (vb[2] - vb[0]);
    let sy = f64::from(h) / (vb[3] - vb[1]);
    let clamp = |v: f64, hi: u32| -> u32 {
        if v <= 0.0 {
            0
        } else if v >= f64::from(hi) {
            hi
        } else {
            v as u32
        }
    };
    let x0 = clamp(((r[0] - vb[0]) * sx).floor(), w);
    let x1 = clamp(((r[2] - vb[0]) * sx).ceil(), w);
    let y0 = clamp(((vb[3] - r[3]) * sy).floor(), h);
    let y1 = clamp(((vb[3] - r[1]) * sy).ceil(), h);
    (x0 < x1 && y0 < y1).then_some([x0, y0, x1, y1])
}

/// References inside `o` (not following them).
fn refs_in(o: &Object, out: &mut Vec<u32>) {
    let mut stack = vec![o];
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => out.push(r.num),
            Object::Array(a) => stack.extend(a.iter()),
            Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

/// A stream as the renderer receives it: decoded by this crate, or with only its raster codec left,
/// or empty when this crate cannot decode it.
fn renderer_stream(s: &Stream, limits: &Limits, budget: &mut usize, notes: &mut BTreeSet<FlattenNote>) -> Result<Stream> {
    let mut dict = s.dict.clone();
    if filters::is_decodable(&s.dict) {
        match filters::decode_chain(s, limits, budget) {
            Ok(d) => {
                dict.remove("Filter");
                dict.remove("DecodeParms");
                dict.remove("DL");
                return Ok(Stream { dict, data: d.data });
            }
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => {}
        }
    } else if raster_tail(&s.dict).is_some()
        && let Ok(d) = decode_prefix(s, limits, budget)
    {
        let last = |o: Option<&Object>| -> Option<Object> {
            match o? {
                Object::Array(a) => a.last().cloned(),
                other => Some(other.clone()),
            }
        };
        let filter = last(s.dict.get("Filter"));
        let parms = last(s.dict.get("DecodeParms"));
        dict.remove("Filter");
        dict.remove("DecodeParms");
        dict.remove("DL");
        if let Some(f) = filter {
            dict.set("Filter", f);
        }
        if let Some(p) = parms.filter(|p| !matches!(p, Object::Null)) {
            dict.set("DecodeParms", p);
        }
        return Ok(Stream { dict, data: d.data });
    }
    notes.insert(FlattenNote::UndecodableStream);
    dict.remove("Filter");
    dict.remove("DecodeParms");
    dict.remove("DL");
    Ok(Stream { dict, data: Vec::new() })
}

/// The one-page document the renderer reads (module docs): written by this crate, every stream
/// pre-decoded, a fresh page tree, no annotations.
fn isolate(doc: &PdfDoc, page: u32, vb: [f64; 4], notes: &mut BTreeSet<FlattenNote>, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let pd = doc.dict_of(page).ok_or(PressError::Malformed("page_node"))?;
    let limits = &doc.limits;
    let resources = doc.inherited(page, "Resources").cloned().filter(|o| matches!(o, Object::Dict(_)));
    let contents = pd.get("Contents").cloned();
    let group = pd.get("Group").cloned();
    let mut work: Vec<u32> = Vec::new();
    for o in [&resources, &contents, &group].into_iter().flatten() {
        refs_in(o, &mut work);
    }
    let mut objects: BTreeMap<u32, Entry> = BTreeMap::new();
    let mut budget = limits.max_decoded_total;
    let image_cap = u64::try_from(limits.max_decoded_bytes).unwrap_or(u64::MAX);
    while let Some(num) = work.pop() {
        if objects.contains_key(&num) {
            continue;
        }
        let Some(e) = doc.objects.get(&num) else { continue };
        if objects.len() & 255 == 0 {
            check_cancel(cancel)?;
        }
        refs_in(&e.obj, &mut work);
        let obj = match &e.obj {
            Object::Stream(s) => {
                if s.dict.get("Subtype").map(|t| doc.resolve(t)).is_some_and(|t| t.is_name("Image")) {
                    let dim = |k: &str| s.dict.get(k).map(|v| doc.resolve(v)).and_then(Object::as_int).unwrap_or(0);
                    let px = u64::try_from(dim("Width")).unwrap_or(0).saturating_mul(u64::try_from(dim("Height")).unwrap_or(0));
                    if px > image_cap {
                        return Err(refuse(FlattenCause::ImageBudget));
                    }
                }
                Object::Stream(Box::new(renderer_stream(s, limits, &mut budget, notes)?))
            }
            other => other.clone(),
        };
        objects.insert(num, Entry { generation: e.generation, obj, span: None, data_at: None });
    }
    let top = doc.objects.keys().next_back().copied().unwrap_or(0);
    let n = |k: u32| top.checked_add(k).filter(|v| *v <= limits.max_object_number).ok_or(PressError::InputLimit("object_number"));
    let (cat, pages, leaf) = (n(1)?, n(2)?, n(3)?);
    let r = |num: u32| Object::Ref(ObjRef { num, generation: 0 });
    let mut c = Dict::default();
    c.set("Type", Object::Name(b"Catalog".to_vec()));
    c.set("Pages", r(pages));
    let mut t = Dict::default();
    t.set("Type", Object::Name(b"Pages".to_vec()));
    t.set("Kids", Object::Array(vec![r(leaf)]));
    t.set("Count", Object::Int(1));
    let mut p = Dict::default();
    p.set("Type", Object::Name(b"Page".to_vec()));
    p.set("Parent", r(pages));
    p.set("MediaBox", Object::Array(vb.iter().map(|v| Object::Real(*v)).collect()));
    if let Some(res) = resources {
        p.set("Resources", res);
    }
    if let Some(cn) = contents {
        p.set("Contents", cn);
    }
    if let Some(g) = group {
        p.set("Group", g);
    }
    for (num, d) in [(cat, c), (pages, t), (leaf, p)] {
        objects.insert(num, Entry { generation: 0, obj: Object::Dict(d), span: None, data_at: None });
    }
    let mut trailer = Dict::default();
    trailer.set("Root", r(cat));
    write_document("1.7", &objects, &[], &trailer, limits, cancel)
}

/// Renders the isolated one-page document into an RGB raster of `w` x `h` pixels spanning `vb`.
fn render(bytes: Vec<u8>, vb: [f64; 4], w: u32, h: u32, notes: &mut BTreeSet<FlattenNote>, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let fonts = Arc::new(AtomicU32::new(0));
    let images = Arc::new(AtomicU32::new(0));
    let (wu, hu) = (usize::try_from(w).map_err(|_| refuse(FlattenCause::PixelBudget))?, usize::try_from(h).map_err(|_| refuse(FlattenCause::PixelBudget))?);
    let (sx, sy) = (f64::from(w) / (vb[2] - vb[0]), f64::from(h) / (vb[3] - vb[1]));
    let device = Affine::new([sx, 0.0, 0.0, -sy, -vb[0] * sx, vb[3] * sy]);
    let (f2, i2) = (fonts.clone(), images.clone());
    let outcome = catch_unwind(AssertUnwindSafe(|| -> std::result::Result<Painter<'_>, FlattenCause> {
        let pdf = Pdf::new(bytes).map_err(|_| FlattenCause::RendererRefused)?;
        let pages = pdf.pages();
        let [page] = &pages[..] else { return Err(FlattenCause::RendererRefused) };
        let settings = InterpreterSettings {
            render_annotations: false,
            warning_sink: Arc::new(move |warning| {
                let counter = match warning {
                    InterpreterWarning::UnsupportedFont => &f2,
                    InterpreterWarning::ImageDecodeFailure => &i2,
                };
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            ..InterpreterSettings::default()
        };
        let cache = InterpreterCache::new();
        let mut ctx = Context::new(device, kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h)), &cache, pdf.xref(), settings);
        let mut painter = Painter::new(wu, hu, cancel).ok_or(FlattenCause::PixelBudget)?;
        interpret_page(page, &mut ctx, &mut painter);
        Ok(painter)
    }));
    let painter = match outcome {
        Ok(Ok(p)) => p,
        Ok(Err(cause)) => return Err(refuse(cause)),
        Err(_) => return Err(refuse(FlattenCause::RendererPanicked)),
    };
    if painter.canceled {
        return Err(PressError::Canceled);
    }
    notes.extend(painter.notes.iter().copied());
    if fonts.load(Ordering::Relaxed) > 0 {
        notes.insert(FlattenNote::UnsupportedFont);
    }
    if images.load(Ordering::Relaxed) > 0 {
        notes.insert(FlattenNote::ImageDecodeFailure);
    }
    Ok(painter.px)
}

/// Paints every region opaque on the raster.
fn paint_regions(px: &mut [u8], w: u32, h: u32, vb: [f64; 4], rects: &[[f64; 4]]) {
    let wu = usize::try_from(w).unwrap_or(0);
    for r in rects {
        let Some([x0, y0, x1, y1]) = region_pixels(*r, vb, w, h) else { continue };
        for y in y0..y1 {
            let row = usize::try_from(y).unwrap_or(0).saturating_mul(wu);
            let a = row.saturating_add(usize::try_from(x0).unwrap_or(0)).saturating_mul(3);
            let b = row.saturating_add(usize::try_from(x1).unwrap_or(0)).saturating_mul(3);
            if let Some(span) = px.get_mut(a..b) {
                for c in span.chunks_exact_mut(3) {
                    c.copy_from_slice(&FLATTEN_FILL);
                }
            }
        }
    }
}

/// Canonical content of a flattened page whose visible box is `vb` (`canon::write_ops`).
pub(crate) fn raster_content(vb: [f64; 4]) -> Result<Vec<u8>> {
    let op = |name: &[u8], operands: Vec<Object>| Op { name: name.to_vec(), operands, start: 0, end: 0, inline_data: None };
    let ops = [
        op(b"q", vec![]),
        op(
            b"cm",
            vec![Object::Real(vb[2] - vb[0]), Object::Real(0.0), Object::Real(0.0), Object::Real(vb[3] - vb[1]), Object::Real(vb[0]), Object::Real(vb[1])],
        ),
        op(b"Do", vec![Object::Name(FLATTEN_NAME.to_vec())]),
        op(b"Q", vec![]),
    ];
    write_ops(&ops, &[])
}

fn next_object(doc: &PdfDoc) -> Result<u32> {
    doc.objects
        .keys()
        .next_back()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|v| *v <= doc.limits.max_object_number)
        .ok_or(PressError::InputLimit("object_number"))
}

fn insert(doc: &mut PdfDoc, num: u32, obj: Object) {
    doc.objects.insert(num, Entry { generation: 0, obj, span: None, data_at: None });
}

/// Flattens every page that carries a region (module docs). Called by `redact_and_save_with` after the
/// region selector and the text-field pass, before the canonical rewrite and the save.
pub(crate) fn flatten_region_pages(doc: &mut PdfDoc, regions: &[RedactionRegion], cancel: &CancellationToken) -> Result<Vec<FlattenedPage>> {
    let mut by_page: BTreeMap<u32, Vec<[f64; 4]>> = BTreeMap::new();
    for r in regions {
        by_page.entry(r.page_index).or_default().push(r.rect);
    }
    let mut out = Vec::with_capacity(by_page.len());
    for (index, rects) in by_page {
        check_cancel(cancel)?;
        let page = *doc.pages.get(usize::try_from(index).unwrap_or(usize::MAX)).ok_or(PressError::NoSuchPage)?;
        let vb = visible_box(doc, page)?;
        let (w, h) = raster_size(vb, &doc.limits)?;
        let mut notes = BTreeSet::new();
        let old = doc.dict_of(page).cloned().ok_or(PressError::Malformed("page_node"))?;
        if old.get("Annots").map(|a| doc.resolve(a)).and_then(Object::as_array).is_some_and(|a| !a.is_empty()) {
            notes.insert(FlattenNote::Annotations);
        }
        let bytes = isolate(doc, page, vb, &mut notes, cancel)?;
        let mut px = render(bytes, vb, w, h, &mut notes, cancel)?;
        paint_regions(&mut px, w, h, vb, &rects);
        check_cancel(cancel)?;
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&px, 6);
        drop(px);
        let mut img = Dict::default();
        img.set("Type", Object::Name(b"XObject".to_vec()));
        img.set("Subtype", Object::Name(b"Image".to_vec()));
        img.set("Width", Object::Int(i64::from(w)));
        img.set("Height", Object::Int(i64::from(h)));
        img.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        img.set("BitsPerComponent", Object::Int(8));
        img.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        let img_num = next_object(doc)?;
        insert(doc, img_num, Object::Stream(Box::new(Stream { dict: img, data })));
        let content_num = next_object(doc)?;
        insert(doc, content_num, Object::Stream(Box::new(Stream { dict: Dict::default(), data: raster_content(vb)? })));
        let rotate = doc
            .inherited(page, "Rotate")
            .and_then(Object::as_int)
            .and_then(|r| r.checked_rem_euclid(360))
            .filter(|r| r.checked_rem(90) == Some(0))
            .unwrap_or(0);
        let user_unit = old.get("UserUnit").map(|u| doc.resolve(u)).and_then(Object::as_f64).filter(|u| *u > 0.0);
        let mut p = Dict::default();
        p.set("Type", Object::Name(b"Page".to_vec()));
        if let Some(parent) = old.get("Parent").cloned() {
            p.set("Parent", parent);
        }
        let boxed = Object::Array(vb.iter().map(|v| Object::Real(*v)).collect());
        p.set("MediaBox", boxed.clone());
        p.set("CropBox", boxed);
        p.set("Rotate", Object::Int(rotate));
        if let Some(u) = user_unit {
            p.set("UserUnit", Object::Real(u));
        }
        let mut xo = Dict::default();
        xo.set_key(FLATTEN_NAME, Object::Ref(ObjRef { num: img_num, generation: 0 }));
        let mut res = Dict::default();
        res.set("XObject", Object::Dict(xo));
        p.set("Resources", Object::Dict(res));
        p.set("Contents", Object::Ref(ObjRef { num: content_num, generation: 0 }));
        let generation = doc.objects.get(&page).map(|e| e.generation).unwrap_or(0);
        doc.objects.insert(page, Entry { generation, obj: Object::Dict(p), span: None, data_at: None });
        out.push(FlattenedPage { page_index: index, object: page, width_px: w, height_px: h, notes: notes.into_iter().collect() });
    }
    Ok(out)
}

/// Independent check of one flattened page on the SAVED file (module docs): number of mismatches.
pub(crate) fn check_flattened(saved: &PdfDoc, flat: &FlattenedPage, rects: &[[f64; 4]], cancel: &CancellationToken) -> Result<u32> {
    check_cancel(cancel)?;
    let Some(page) = saved.pages.get(usize::try_from(flat.page_index).unwrap_or(usize::MAX)).copied() else { return Ok(1) };
    let Some(pd) = saved.dict_of(page) else { return Ok(1) };
    let mut bad = 0u32;
    let mut fail = |c: bool| {
        if !c {
            bad = bad.saturating_add(1);
        }
    };
    fail(pd.entries.iter().all(|(k, _)| PAGE_KEYS.iter().any(|p| p.as_bytes() == k.as_slice())));
    fail(pd.get("Type").is_some_and(|t| t.is_name("Page")));
    let mb = saved.rect_of(pd.get("MediaBox"));
    fail(mb.is_some() && saved.rect_of(pd.get("CropBox")) == mb);
    let Some(vb) = mb.map(normal) else { return Ok(bad.saturating_add(1)) };
    let res = pd.get("Resources").map(|r| saved.resolve(r)).and_then(Object::as_dict);
    let xo = res.filter(|r| r.entries.len() == 1).and_then(|r| r.get("XObject")).map(|x| saved.resolve(x)).and_then(Object::as_dict);
    let img_ref = xo.filter(|x| x.entries.len() == 1).and_then(|x| x.get_key(FLATTEN_NAME)).and_then(Object::as_ref);
    let mut budget = saved.limits.max_decoded_total;
    let content = match pd.get("Contents").map(|c| saved.resolve(c)) {
        Some(Object::Stream(s)) => filters::decode_stream(s, &saved.limits, &mut budget).ok(),
        _ => None,
    };
    fail(content.is_some_and(|c| raster_content(vb).is_ok_and(|want| want == c)));
    let Some(Object::Stream(img)) = img_ref.and_then(|r| saved.objects.get(&r.num)).map(|e| &e.obj) else { return Ok(bad.saturating_add(1)) };
    let d = &img.dict;
    let int = |k: &str| d.get(k).map(|v| saved.resolve(v)).and_then(Object::as_int);
    let (Some(w), Some(h)) = (int("Width").and_then(|v| u32::try_from(v).ok()), int("Height").and_then(|v| u32::try_from(v).ok())) else {
        return Ok(bad.saturating_add(1));
    };
    fail(d.get("Subtype").is_some_and(|t| t.is_name("Image")));
    fail(d.get("ColorSpace").is_some_and(|t| t.is_name("DeviceRGB")));
    fail(int("BitsPerComponent") == Some(8));
    fail(d.get("Filter").is_some_and(|t| t.is_name("FlateDecode")) && d.get("DecodeParms").is_none() && d.get("SMask").is_none() && d.get("Mask").is_none());
    let Ok(px) = filters::decode_stream(img, &saved.limits, &mut budget) else { return Ok(bad.saturating_add(1)) };
    let wu = usize::try_from(w).unwrap_or(0);
    let want_len = wu.checked_mul(usize::try_from(h).unwrap_or(0)).and_then(|v| v.checked_mul(3));
    fail(want_len == Some(px.len()));
    for r in rects {
        let Some([x0, y0, x1, y1]) = region_pixels(*r, vb, w, h) else { continue };
        let mut opaque = true;
        for y in y0..y1 {
            let row = usize::try_from(y).unwrap_or(0).saturating_mul(wu);
            let a = row.saturating_add(usize::try_from(x0).unwrap_or(0)).saturating_mul(3);
            let b = row.saturating_add(usize::try_from(x1).unwrap_or(0)).saturating_mul(3);
            match px.get(a..b) {
                Some(span) => opaque &= span.chunks_exact(3).all(|c| c == FLATTEN_FILL),
                None => opaque = false,
            }
        }
        fail(opaque);
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_pixels_round_outwards_and_clamp() {
        let vb = [0.0, 0.0, 100.0, 50.0];
        // 2 px per unit: region [10.2, 5.1, 20.3, 9.9] -> x 20..41, y (50-9.9)*2=80.2 -> 80 .. (50-5.1)*2=89.8 -> 90
        assert_eq!(region_pixels([10.2, 5.1, 20.3, 9.9], vb, 200, 100), Some([20, 80, 41, 90]));
        assert_eq!(region_pixels([-50.0, -50.0, 500.0, 500.0], vb, 200, 100), Some([0, 0, 200, 100]));
        assert_eq!(region_pixels([200.0, 0.0, 300.0, 10.0], vb, 200, 100), None, "outside the visible box");
        assert_eq!(region_pixels([f64::NAN, 0.0, 1.0, 1.0], vb, 200, 100), None);
    }

    #[test]
    fn raster_size_is_checked_before_allocation() {
        let letter = [0.0, 0.0, 612.0, 792.0];
        assert_eq!(raster_size(letter, &Limits::default()), Ok((1275, 1650)));
        let tight = Limits { max_flatten_pixels: 1275 * 1650 - 1, ..Limits::default() };
        assert_eq!(raster_size(letter, &tight), Err(PressError::FlattenRefused(FlattenCause::PixelBudget)));
        let zero = Limits { flatten_dpi: 0, ..Limits::default() };
        assert_eq!(raster_size(letter, &zero), Err(PressError::FlattenRefused(FlattenCause::PixelBudget)));
        // dims_u32_max: a box whose pixel size overflows every integer type is refused, not wrapped.
        let huge = [0.0, 0.0, 1.0e300, 1.0e300];
        assert_eq!(raster_size(huge, &Limits::default()), Err(PressError::FlattenRefused(FlattenCause::PixelBudget)));
        let side = [0.0, 0.0, 65_536.0 * 72.0 / 150.0 + 1.0, 1.0];
        assert_eq!(raster_size(side, &Limits { max_flatten_pixels: u64::MAX, ..Limits::default() }), Err(PressError::FlattenRefused(FlattenCause::PixelBudget)));
        // The proof must be able to decode the raster: 3 bytes per pixel within max_decoded_bytes.
        let small_decode = Limits { max_decoded_bytes: 1275 * 1650 * 3 - 1, ..Limits::default() };
        assert_eq!(raster_size(letter, &small_decode), Err(PressError::FlattenRefused(FlattenCause::PixelBudget)));
    }

    #[test]
    fn raster_content_is_canonical() {
        let c = raster_content([0.0, 0.0, 612.0, 792.0]).expect("content");
        let mut budget = usize::MAX;
        let parsed = crate::content::parse_strict(&c, &Limits::default(), &crate::content::no_color_spaces, &mut budget).expect("parse");
        assert!(crate::canon::is_canonical(&c, &parsed));
        assert_eq!(parsed.ops.iter().map(|o| o.name.as_slice()).collect::<Vec<_>>(), vec![&b"q"[..], b"cm", b"Do", b"Q"]);
    }
}
