//! The page rasteriser behind redaction flattening (lead decision D27, `flatten.rs`): a CPU coverage
//! rasteriser implementing `hayro_interpret::Device`, so hayro-interpret (Apache-2.0 OR MIT) reads the
//! PDF content and this module paints it into an RGB canvas the crate owns and bounds.
//!
//! What it draws: fills and strokes with solid colours (non-zero and even-odd rules, kurbo stroke
//! expansion, dashes), glyph outlines and Type 3 glyph procedures, raster and stencil images (nearest
//! sample, image alpha), clip paths (nested, as coverage masks over their bounds), group and fill
//! opacity, and the separable blend modes of ISO 32000-2 11.3.5.2 over the opaque white page.
//! What it does not draw (absent from the raster, recorded as a [`FlattenNote`]): pattern and shading
//! paints, content under a soft mask, the non-separable blend modes (drawn as Normal), content past the
//! clip nesting cap or a budget. Absent content is not drawn, and an absent occluder uncovers what it
//! covered (R13-F3: such content shows in the raster where a viewer shows the occluder; it is the page's
//! own content). Region pixels are painted after drawing, so this never reaches a region.
//!
//! Bounds (V4-01/V4-02): the canvas size is checked by the caller against `Limits::max_flatten_pixels`
//! before this module allocates it; every clip mask is sized to its bounds and charged to a memory cap
//! (four canvases' worth of bytes); every draw charges the pixels it visits to a work budget (64 canvas
//! passes) and every flattened path's segment count to a segment cap; every image the renderer is asked
//! to decode charges its declared pixel count to a decode budget BEFORE it is decoded (the caller's
//! aggregate, R13-F5; per-image sizes are bounded before rendering by `render_bounds.rs`); past a cap the
//! draw is skipped, drawing stops and [`FlattenNote::WorkBudget`] is recorded. The cancellation token is
//! polled every 64 draws.
#![deny(clippy::arithmetic_side_effects)]
use crate::flatten::FlattenNote;
use hayro_interpret::font::Glyph;
use hayro_interpret::{
    BlendMode, ClipPath, Device, FillRule, GlyphDrawMode, Image, ImageData, LumaData, Paint, PathDrawMode, SoftMask, StrokeProps,
};
use hsk_studio_accord::CancellationToken;
use kurbo::{Affine, BezPath, PathEl, Point};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

/// Sub-scanlines per pixel row (vertical anti-aliasing); horizontal coverage is exact.
const SUB: usize = 4;
/// Curve flattening tolerance in device pixels.
const TOLERANCE: f64 = 0.2;
/// Control points farther than this from the origin (device pixels) are joined by straight lines
/// instead of being subdivided (a hostile curve with huge coordinates cannot explode the segment count).
const FAR: f64 = 1.0e6;
/// Line segments one flattened path may hold.
const MAX_SEGMENTS: usize = 1 << 20;
/// Path elements one draw may hold.
const MAX_ELEMENTS: usize = 1 << 18;
/// Nested clip paths beyond this depth clip everything away.
const MAX_CLIP_DEPTH: usize = 64;
/// Cached glyph outlines.
const MAX_GLYPH_CACHE: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct IRect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl IRect {
    fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
    fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }
    fn area(&self) -> usize {
        self.width().saturating_mul(self.y1.saturating_sub(self.y0))
    }
    fn intersect(&self, o: &IRect) -> IRect {
        IRect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }
}

/// One clip level: its pixel bounds and, unless the coverage is full everywhere inside them, a coverage
/// mask over the bounds (row-major, 0..=255), already multiplied by the levels below.
struct Clip {
    b: IRect,
    mask: Option<Rc<Vec<u8>>>,
}

impl Clip {
    fn at(&self, x: usize, y: usize) -> f32 {
        match &self.mask {
            None => 1.0,
            Some(m) => {
                let i = y.saturating_sub(self.b.y0).saturating_mul(self.b.width()).saturating_add(x.saturating_sub(self.b.x0));
                f32::from(m.get(i).copied().unwrap_or(0)) / 255.0
            }
        }
    }
}

struct Group {
    alpha: f32,
    masked: bool,
    blend: BlendMode,
}

#[derive(Clone, Copy)]
struct Seg {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    dir: i32,
}

/// The RGB canvas and the device state. `px` is `w * h * 3` bytes, opaque white at start.
pub(crate) struct Painter<'c> {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
    /// The whole canvas (never popped); `clips` holds the pushed levels.
    base: Clip,
    clips: Vec<Clip>,
    groups: Vec<Group>,
    mask_on: bool,
    blend: BlendMode,
    pub notes: BTreeSet<FlattenNote>,
    work_left: u64,
    /// Image pixels the renderer may still decode (aggregate over the page).
    decode_left: u64,
    mem_left: usize,
    cancel: &'c CancellationToken,
    pub canceled: bool,
    out_of_budget: bool,
    draws: u32,
    glyphs: HashMap<u128, Rc<BezPath>>,
}

impl<'c> Painter<'c> {
    /// `w * h` must already be within the caller's pixel budget (checked arithmetic there);
    /// `decode_budget` is the number of image pixels the renderer may decode for this page.
    pub(crate) fn new(w: usize, h: usize, decode_budget: u64, cancel: &'c CancellationToken) -> Option<Self> {
        let px_len = w.checked_mul(h)?.checked_mul(3)?;
        let canvas = w.checked_mul(h)?;
        Some(Self {
            w,
            h,
            px: vec![255u8; px_len],
            base: Clip { b: IRect { x0: 0, y0: 0, x1: w, y1: h }, mask: None },
            clips: Vec::new(),
            groups: Vec::new(),
            mask_on: false,
            blend: BlendMode::Normal,
            notes: BTreeSet::new(),
            work_left: u64::try_from(canvas).ok()?.checked_mul(64)?,
            decode_left: decode_budget,
            mem_left: canvas.checked_mul(4)?,
            cancel,
            canceled: false,
            out_of_budget: false,
            draws: 0,
            glyphs: HashMap::new(),
        })
    }

    /// Whether drawing goes on (not canceled, budget left).
    fn live(&mut self) -> bool {
        if self.canceled || self.out_of_budget {
            return false;
        }
        self.draws = self.draws.wrapping_add(1);
        if self.draws & 63 == 0 && self.cancel.check().is_err() {
            self.canceled = true;
            return false;
        }
        true
    }

    fn charge(&mut self, pixels: usize) -> bool {
        let p = u64::try_from(pixels).unwrap_or(u64::MAX);
        match self.work_left.checked_sub(p) {
            Some(left) => {
                self.work_left = left;
                true
            }
            None => {
                self.out_of_budget = true;
                self.notes.insert(FlattenNote::WorkBudget);
                false
            }
        }
    }

    /// The drawing state for one draw: `None` when the draw must be skipped (soft mask in force).
    fn state(&mut self) -> Option<(f32, BlendMode)> {
        if self.mask_on || self.groups.iter().any(|g| g.masked) {
            self.notes.insert(FlattenNote::SoftMask);
            return None;
        }
        let alpha = self.groups.iter().fold(1.0f32, |a, g| a * g.alpha);
        let mut blend = self.blend;
        if blend == BlendMode::Normal
            && let Some(g) = self.groups.iter().rev().find(|g| g.blend != BlendMode::Normal)
        {
            blend = g.blend;
        }
        if matches!(blend, BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity) {
            self.notes.insert(FlattenNote::BlendModeApproximated);
            blend = BlendMode::Normal;
        }
        Some((alpha, blend))
    }

    fn clip(&self) -> &Clip {
        self.clips.last().unwrap_or(&self.base)
    }

    /// Flattens `path` (already in device space) into edges; `None` when it is not drawable or too big.
    fn segments(&mut self, path: &BezPath) -> Option<Vec<Seg>> {
        if path.elements().len() > MAX_ELEMENTS {
            self.notes.insert(FlattenNote::WorkBudget);
            return None;
        }
        let cb = path.control_box();
        if !(cb.x0.is_finite() && cb.y0.is_finite() && cb.x1.is_finite() && cb.y1.is_finite()) {
            return None;
        }
        let far = cb.x0.abs().max(cb.x1.abs()).max(cb.y0.abs()).max(cb.y1.abs()) > FAR;
        let mut segs: Vec<Seg> = Vec::new();
        let mut start = Point::ZERO;
        let mut last = Point::ZERO;
        let mut open = false;
        let mut overflow = false;
        let mut push = |segs: &mut Vec<Seg>, a: Point, b: Point| {
            if segs.len() >= MAX_SEGMENTS {
                overflow = true;
                return;
            }
            if a.y == b.y {
                return;
            }
            segs.push(if a.y < b.y {
                Seg { x0: a.x, y0: a.y, x1: b.x, y1: b.y, dir: 1 }
            } else {
                Seg { x0: b.x, y0: b.y, x1: a.x, y1: a.y, dir: -1 }
            });
        };
        let mut handle = |el: PathEl, segs: &mut Vec<Seg>| match el {
            PathEl::MoveTo(p) => {
                if open {
                    push(segs, last, start);
                }
                start = p;
                last = p;
                open = true;
            }
            PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => {
                push(segs, last, p);
                last = p;
            }
            PathEl::ClosePath => {
                push(segs, last, start);
                last = start;
            }
        };
        if far {
            for el in path.elements() {
                handle(*el, &mut segs);
            }
        } else {
            kurbo::flatten(path.elements().iter().copied(), TOLERANCE, |el| handle(el, &mut segs));
        }
        if open {
            push(&mut segs, last, start);
        }
        if overflow {
            self.notes.insert(FlattenNote::WorkBudget);
            return None;
        }
        Some(segs)
    }

    /// Coverage of `segs` with `rule` over `bounds`, row by row: calls `f(x, y, coverage)` for every
    /// pixel with non-zero coverage. Returns false when the work budget ran out.
    fn cover(&mut self, segs: &[Seg], rule: FillRule, bounds: IRect, mut f: impl FnMut(&mut Self, usize, usize, f32)) -> bool {
        if segs.is_empty() || bounds.is_empty() {
            return true;
        }
        let (mut ymin, mut ymax, mut xmin, mut xmax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for s in segs {
            ymin = ymin.min(s.y0);
            ymax = ymax.max(s.y1);
            xmin = xmin.min(s.x0.min(s.x1));
            xmax = xmax.max(s.x0.max(s.x1));
        }
        let b = IRect {
            x0: clamp_floor(xmin, bounds.x0, bounds.x1),
            y0: clamp_floor(ymin, bounds.y0, bounds.y1),
            x1: clamp_ceil(xmax, bounds.x0, bounds.x1),
            y1: clamp_ceil(ymax, bounds.y0, bounds.y1),
        };
        if b.is_empty() {
            return true;
        }
        if !self.charge(b.area().saturating_mul(2)) {
            return false;
        }
        let mut order: Vec<usize> = (0..segs.len()).collect();
        order.sort_by(|a, c| segs[*a].y0.total_cmp(&segs[*c].y0));
        let mut next = 0usize;
        let mut active: Vec<usize> = Vec::new();
        let width = b.width();
        let mut acc = vec![0.0f32; width];
        let mut xs: Vec<(f64, i32)> = Vec::new();
        let step = 1.0 / SUB as f64;
        for y in b.y0..b.y1 {
            let top = y as f64;
            let bottom = top + 1.0;
            while let Some(&i) = order.get(next) {
                if segs[i].y0 >= bottom {
                    break;
                }
                active.push(i);
                next = next.saturating_add(1);
            }
            active.retain(|&i| segs[i].y1 > top);
            if active.is_empty() {
                continue;
            }
            acc.iter_mut().for_each(|a| *a = 0.0);
            for sub in 0..SUB {
                let sy = top + (sub as f64 + 0.5) * step;
                xs.clear();
                for &i in &active {
                    let s = &segs[i];
                    if s.y0 <= sy && sy < s.y1 {
                        let x = s.x0 + (sy - s.y0) * (s.x1 - s.x0) / (s.y1 - s.y0);
                        xs.push((x, s.dir));
                    }
                }
                xs.sort_by(|a, c| a.0.total_cmp(&c.0));
                let mut wind = 0i32;
                for k in 0..xs.len() {
                    let (x, d) = xs[k];
                    wind = wind.saturating_add(d);
                    let inside = match rule {
                        FillRule::NonZero => wind != 0,
                        FillRule::EvenOdd => wind & 1 != 0,
                    };
                    if inside && let Some(&(xn, _)) = xs.get(k.saturating_add(1)) {
                        add_span(&mut acc, b.x0, x, xn, step as f32);
                    }
                }
            }
            for (i, a) in acc.iter().enumerate() {
                if *a > 0.0 {
                    f(self, b.x0.saturating_add(i), y, a.min(1.0));
                }
            }
        }
        true
    }

    fn fill_device(&mut self, path: &BezPath, rule: FillRule, rgba: [f32; 4]) {
        let Some((alpha, blend)) = self.state() else { return };
        let Some(segs) = self.segments(path) else { return };
        let bounds = self.clip().b;
        let a = rgba[3] * alpha;
        if a <= 0.0 {
            return;
        }
        let color = [rgba[0], rgba[1], rgba[2]];
        self.cover(&segs, rule, bounds, |p, x, y, c| {
            let m = p.clip().at(x, y);
            p.composite(x, y, color, c * m * a, blend);
        });
    }

    fn composite(&mut self, x: usize, y: usize, src: [f32; 3], alpha: f32, blend: BlendMode) {
        if alpha <= 0.0 {
            return;
        }
        let Some(i) = y.checked_mul(self.w).and_then(|v| v.checked_add(x)).and_then(|v| v.checked_mul(3)) else { return };
        let Some(px) = self.px.get_mut(i..i.saturating_add(3)) else { return };
        let a = alpha.min(1.0);
        for (k, slot) in px.iter_mut().enumerate() {
            let cb = f32::from(*slot) / 255.0;
            let cs = src[k].clamp(0.0, 1.0);
            let mixed = blend_channel(blend, cb, cs);
            let out = cb * (1.0 - a) + mixed * a;
            *slot = (out.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }

    fn paint_rgba(&mut self, paint: &Paint<'_>) -> Option<[f32; 4]> {
        match paint {
            Paint::Color(c) => Some(c.to_rgba().components()),
            Paint::Pattern(_) => {
                self.notes.insert(FlattenNote::PatternPaint);
                None
            }
        }
    }

    fn stroke_device(&mut self, path: &BezPath, transform: Affine, props: &StrokeProps, rgba: [f32; 4]) {
        let scale = transform.determinant().abs().sqrt();
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let mut width = f64::from(props.line_width).abs();
        if width * scale < 1.0 {
            width = 1.0 / scale;
        }
        let mut stroke = kurbo::Stroke::new(width)
            .with_join(props.line_join)
            .with_miter_limit(f64::from(props.miter_limit).max(1.0))
            .with_caps(props.line_cap);
        let period: f64 = props.dash_array.iter().map(|d| f64::from(d.abs())).sum();
        let span = path.control_box();
        let length = (span.width() + span.height()).abs();
        // Dashes are drawn when each period is at least half a device pixel and the path holds a
        // bounded number of them; otherwise the line is drawn solid.
        if period.is_finite() && length.is_finite() && period * scale >= 0.5 && length / period <= 100_000.0 {
            stroke = stroke.with_dashes(f64::from(props.dash_offset), props.dash_array.iter().map(|d| f64::from(d.abs())));
        }
        let tol = (TOLERANCE / scale).max(1e-6);
        let outline = kurbo::stroke(path.elements().iter().copied(), &stroke, &kurbo::StrokeOpts::default(), tol);
        let device = transformed(&outline, transform);
        self.fill_device(&device, FillRule::NonZero, rgba);
    }

    fn draw_path_impl(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'_>, mode: &PathDrawMode) {
        if !self.live() || !transform.is_finite() {
            return;
        }
        let Some(rgba) = self.paint_rgba(paint) else { return };
        match mode {
            PathDrawMode::Fill(rule) => {
                let device = transformed(path, transform);
                self.fill_device(&device, *rule, rgba);
            }
            PathDrawMode::Stroke(props) => self.stroke_device(path, transform, props, rgba),
        }
    }

    fn glyph_outline(&mut self, g: &hayro_interpret::font::OutlineGlyph) -> Rc<BezPath> {
        let key = hayro_interpret::CacheKey::cache_key(&g.identifier());
        if let Some(p) = self.glyphs.get(&key) {
            return p.clone();
        }
        let p = Rc::new(g.outline());
        if self.glyphs.len() < MAX_GLYPH_CACHE {
            self.glyphs.insert(key, p.clone());
        }
        p
    }

    fn image_impl(&mut self, image: Image<'_, '_>, transform: Affine) {
        if !self.live() || !transform.is_finite() {
            return;
        }
        let Some((alpha, blend)) = self.state() else { return };
        // V4-02 aggregate (R13-F5): the renderer decodes the whole image before it is drawn, so its
        // declared pixels are charged first; past the budget nothing more is decoded or drawn.
        let declared = u64::from(image.width()).saturating_mul(u64::from(image.height()));
        match self.decode_left.checked_sub(declared) {
            Some(left) => self.decode_left = left,
            None => {
                self.out_of_budget = true;
                self.notes.insert(FlattenNote::WorkBudget);
                return;
            }
        }
        let bounds = self.clip().b;
        let bb = transform.transform_rect_bbox(kurbo::Rect::new(0.0, 0.0, f64::from(image.width()), f64::from(image.height())));
        let target = (
            clamp_ceil(bb.width(), 1, self.w.max(1)).max(1),
            clamp_ceil(bb.height(), 1, self.h.max(1)).max(1),
        );
        let hint = Some((u32::try_from(target.0).unwrap_or(u32::MAX), u32::try_from(target.1).unwrap_or(u32::MAX)));
        match image {
            Image::Raster(r) => {
                r.with_rgba(
                    |data, mask| {
                        let (fx, fy) = data.scale_factors();
                        let t = transform.pre_scale_non_uniform(f64::from(fx), f64::from(fy));
                        let (dw, dh) = (data.width(), data.height());
                        let mask_t = mask.as_ref().map(|m| transform.pre_scale_non_uniform(f64::from(m.scale_factors.0), f64::from(m.scale_factors.1)));
                        self.sample(t, dw, dh, bounds, |p, u, v, x, y, mt| {
                            let (r, g, b) = match &data {
                                ImageData::Rgb(d) => {
                                    let i = v.saturating_mul(d.width as usize).saturating_add(u).saturating_mul(3);
                                    match d.data.get(i..i.saturating_add(3)) {
                                        Some(s) => (s[0], s[1], s[2]),
                                        None => return,
                                    }
                                }
                                ImageData::Luma(d) => match d.data.get(v.saturating_mul(d.width as usize).saturating_add(u)) {
                                    Some(l) => (*l, *l, *l),
                                    None => return,
                                },
                            };
                            let ma = match (&mask, mt) {
                                (Some(m), Some(mt)) => luma_at(m, mt, x, y),
                                _ => 1.0,
                            };
                            let c = p.clip().at(x, y);
                            p.composite(x, y, [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0], ma * c * alpha, blend);
                        }, mask_t);
                    },
                    hint,
                );
            }
            Image::Stencil(s) => {
                s.with_stencil(
                    |luma, paint| {
                        let Some(rgba) = self.paint_rgba(paint) else { return };
                        let (fx, fy) = luma.scale_factors;
                        let t = transform.pre_scale_non_uniform(f64::from(fx), f64::from(fy));
                        let (dw, dh) = (luma.width, luma.height);
                        self.sample(t, dw, dh, bounds, |p, u, v, x, y, _| {
                            let Some(l) = luma.data.get(v.saturating_mul(luma.width as usize).saturating_add(u)) else { return };
                            let c = p.clip().at(x, y);
                            p.composite(x, y, [rgba[0], rgba[1], rgba[2]], f32::from(*l) / 255.0 * rgba[3] * c * alpha, blend);
                        }, None);
                    },
                    hint,
                );
            }
        }
    }

    /// Visits every device pixel whose centre maps inside the `dw` x `dh` data grid under `t` (data
    /// pixel space to device): `f(painter, u, v, x, y, mask transform)`.
    #[allow(clippy::too_many_arguments)]
    fn sample(
        &mut self,
        t: Affine,
        dw: u32,
        dh: u32,
        bounds: IRect,
        mut f: impl FnMut(&mut Self, usize, usize, usize, usize, Option<Affine>),
        mask_t: Option<Affine>,
    ) {
        if dw == 0 || dh == 0 || !t.is_finite() || t.determinant().abs() < 1e-12 {
            return;
        }
        let inv = t.inverse();
        let bb = t.transform_rect_bbox(kurbo::Rect::new(0.0, 0.0, f64::from(dw), f64::from(dh)));
        let b = IRect {
            x0: clamp_floor(bb.x0, bounds.x0, bounds.x1),
            y0: clamp_floor(bb.y0, bounds.y0, bounds.y1),
            x1: clamp_ceil(bb.x1, bounds.x0, bounds.x1),
            y1: clamp_ceil(bb.y1, bounds.y0, bounds.y1),
        };
        if b.is_empty() || !self.charge(b.area()) {
            return;
        }
        let (fw, fh) = (f64::from(dw), f64::from(dh));
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                let p = map(inv, x as f64 + 0.5, y as f64 + 0.5);
                if p.x >= 0.0 && p.y >= 0.0 && p.x < fw && p.y < fh {
                    f(self, p.x as usize, p.y as usize, x, y, mask_t);
                }
            }
        }
    }
}

/// `a` after `b` (`b` is applied first), as kurbo's `a * b` (float arithmetic, no operator overload).
fn compose(a: Affine, b: Affine) -> Affine {
    let [a0, a1, a2, a3, a4, a5] = a.as_coeffs();
    let [b0, b1, b2, b3, b4, b5] = b.as_coeffs();
    Affine::new([
        a0 * b0 + a2 * b1,
        a1 * b0 + a3 * b1,
        a0 * b2 + a2 * b3,
        a1 * b2 + a3 * b3,
        a0 * b4 + a2 * b5 + a4,
        a1 * b4 + a3 * b5 + a5,
    ])
}

fn map(a: Affine, x: f64, y: f64) -> Point {
    let [a0, a1, a2, a3, a4, a5] = a.as_coeffs();
    Point::new(a0 * x + a2 * y + a4, a1 * x + a3 * y + a5)
}

fn transformed(path: &BezPath, t: Affine) -> BezPath {
    let mut p = path.clone();
    p.apply_affine(t);
    p
}

fn luma_at(m: &LumaData, mt: Affine, x: usize, y: usize) -> f32 {
    if !mt.is_finite() || mt.determinant().abs() < 1e-12 {
        return 1.0;
    }
    let p = map(mt.inverse(), x as f64 + 0.5, y as f64 + 0.5);
    if p.x < 0.0 || p.y < 0.0 || p.x >= f64::from(m.width) || p.y >= f64::from(m.height) {
        return 0.0;
    }
    let i = (p.y as usize).saturating_mul(m.width as usize).saturating_add(p.x as usize);
    f32::from(m.data.get(i).copied().unwrap_or(0)) / 255.0
}

fn clamp_floor(v: f64, lo: usize, hi: usize) -> usize {
    if v.is_nan() {
        return lo;
    }
    let f = v.floor();
    if f <= lo as f64 {
        lo
    } else if f >= hi as f64 {
        hi
    } else {
        f as usize
    }
}

fn clamp_ceil(v: f64, lo: usize, hi: usize) -> usize {
    if v.is_nan() {
        return lo;
    }
    let c = v.ceil();
    if c <= lo as f64 {
        lo
    } else if c >= hi as f64 {
        hi
    } else {
        c as usize
    }
}

/// Adds coverage `w` over `[xa, xb)` (device x) to `acc`, whose first slot is pixel `x0`.
fn add_span(acc: &mut [f32], x0: usize, xa: f64, xb: f64, w: f32) {
    let lo = x0 as f64;
    let hi = lo + acc.len() as f64;
    let (a, b) = (xa.max(lo).min(hi), xb.max(lo).min(hi));
    if b.partial_cmp(&a) != Some(std::cmp::Ordering::Greater) {
        return;
    }
    let ia = (a.floor() - lo) as usize;
    let ib = (b.floor() - lo) as usize;
    if ia == ib {
        if let Some(s) = acc.get_mut(ia) {
            *s += ((b - a) as f32) * w;
        }
        return;
    }
    if let Some(s) = acc.get_mut(ia) {
        *s += ((a.floor() + 1.0 - a) as f32) * w;
    }
    for s in acc.iter_mut().take(ib).skip(ia.saturating_add(1)) {
        *s += w;
    }
    if let Some(s) = acc.get_mut(ib) {
        *s += ((b - b.floor()) as f32) * w;
    }
}

/// ISO 32000-2 11.3.5.2 separable blend functions B(cb, cs); non-separable modes arrive as Normal.
fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 { ((16.0 * cb - 12.0) * cb + 4.0) * cb } else { cb.sqrt() };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        _ => cs,
    }
}

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 { cb * 2.0 * cs } else { let s = 2.0 * cs - 1.0; cb + s - cb * s }
}

impl<'a> Device<'a> for Painter<'_> {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        self.mask_on = mask.is_some();
    }

    fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.blend = blend_mode;
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        self.draw_path_impl(path, transform, paint, draw_mode);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        let parent_b = self.clip().b;
        if self.clips.len() >= MAX_CLIP_DEPTH || self.canceled || self.out_of_budget {
            if self.clips.len() >= MAX_CLIP_DEPTH {
                self.notes.insert(FlattenNote::ClipDepth);
            }
            self.clips.push(Clip { b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 }, mask: None });
            return;
        }
        let segs = if clip_path.path.elements().len() > MAX_ELEMENTS { None } else { self.segments(&clip_path.path) };
        let Some(segs) = segs else {
            self.notes.insert(FlattenNote::WorkBudget);
            self.clips.push(Clip { b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 }, mask: None });
            return;
        };
        let bb = clip_path.path.control_box();
        let b = IRect {
            x0: clamp_floor(bb.x0, parent_b.x0, parent_b.x1),
            y0: clamp_floor(bb.y0, parent_b.y0, parent_b.y1),
            x1: clamp_ceil(bb.x1, parent_b.x0, parent_b.x1),
            y1: clamp_ceil(bb.y1, parent_b.y0, parent_b.y1),
        }
        .intersect(&parent_b);
        let area = b.area();
        match self.mem_left.checked_sub(area) {
            Some(left) if !b.is_empty() => self.mem_left = left,
            Some(_) => {
                self.clips.push(Clip { b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 }, mask: None });
                return;
            }
            None => {
                self.notes.insert(FlattenNote::WorkBudget);
                self.out_of_budget = true;
                self.clips.push(Clip { b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 }, mask: None });
                return;
            }
        }
        let mut mask = vec![0u8; area];
        let width = b.width();
        let ok = self.cover(&segs, clip_path.fill, b, |p, x, y, c| {
            let parent = p.clip().at(x, y);
            let i = y.saturating_sub(b.y0).saturating_mul(width).saturating_add(x.saturating_sub(b.x0));
            if let Some(slot) = mask.get_mut(i) {
                *slot = ((c * parent).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
        if !ok {
            self.clips.push(Clip { b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 }, mask: None });
            return;
        }
        let full = mask.iter().all(|m| *m == 255);
        if full {
            // No mask is kept: the bytes go back to the memory cap (pop gives back only kept masks).
            self.mem_left = self.mem_left.saturating_add(area);
        }
        self.clips.push(Clip { b, mask: if full { None } else { Some(Rc::new(mask)) } });
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode) {
        let alpha = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 0.0 };
        self.groups.push(Group { alpha, masked: mask.is_some(), blend: blend_mode });
    }

    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine, paint: &Paint<'a>, draw_mode: &GlyphDrawMode) {
        if !self.live() {
            return;
        }
        match glyph {
            Glyph::Outline(g) => {
                let t = compose(transform, glyph_transform);
                if !t.is_finite() {
                    return;
                }
                let Some(rgba) = self.paint_rgba(paint) else { return };
                let outline = self.glyph_outline(g);
                match draw_mode {
                    GlyphDrawMode::Fill => {
                        let device = transformed(&outline, t);
                        self.fill_device(&device, FillRule::NonZero, rgba);
                    }
                    GlyphDrawMode::Stroke(props) => self.stroke_device(&outline, t, props, rgba),
                    GlyphDrawMode::Invisible => {}
                }
            }
            Glyph::Type3(t3) => {
                if !matches!(draw_mode, GlyphDrawMode::Invisible) {
                    t3.interpret(self, transform, glyph_transform, paint);
                }
            }
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        self.image_impl(image, transform);
    }

    fn pop_clip_path(&mut self) {
        if let Some(c) = self.clips.pop() {
            self.mem_left = self.mem_left.saturating_add(if c.mask.is_some() { c.b.area() } else { 0 });
        }
    }

    fn pop_transparency_group(&mut self) {
        self.groups.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painter(cancel: &CancellationToken) -> Painter<'_> {
        Painter::new(20, 10, u64::MAX, cancel).expect("canvas")
    }

    #[test]
    fn fills_follow_the_fill_rule_and_clip() {
        let cancel = CancellationToken::default();
        let mut p = painter(&cancel);
        let mut path = BezPath::new();
        // Outer square 2..18 x 1..9 and an inner square 6..14 x 3..7, same direction.
        for (x0, y0, x1, y1) in [(2.0, 1.0, 18.0, 9.0), (6.0, 3.0, 14.0, 7.0)] {
            path.move_to((x0, y0));
            path.line_to((x1, y0));
            path.line_to((x1, y1));
            path.line_to((x0, y1));
            path.close_path();
        }
        p.fill_device(&path, FillRule::EvenOdd, [0.0, 0.0, 0.0, 1.0]);
        let at = |p: &Painter<'_>, x: usize, y: usize| p.px[(y * p.w + x) * 3];
        assert_eq!(at(&p, 3, 2), 0, "between the squares: filled");
        assert_eq!(at(&p, 10, 5), 255, "inside the inner square: even-odd hole");
        assert_eq!(at(&p, 0, 0), 255, "outside: untouched");
        p.fill_device(&path, FillRule::NonZero, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(at(&p, 10, 5), 0, "non-zero fills the inner square");
    }

    #[test]
    fn spans_cover_partial_pixels() {
        let mut acc = vec![0.0f32; 4];
        add_span(&mut acc, 10, 10.5, 12.25, 1.0);
        assert!((acc[0] - 0.5).abs() < 1e-6 && (acc[1] - 1.0).abs() < 1e-6 && (acc[2] - 0.25).abs() < 1e-6 && acc[3] == 0.0);
        let mut acc = vec![0.0f32; 2];
        add_span(&mut acc, 0, -5.0, 1e300, 1.0);
        assert_eq!(acc, vec![1.0, 1.0], "clamped to the row");
    }
}
