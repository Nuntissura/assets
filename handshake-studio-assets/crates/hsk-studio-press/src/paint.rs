//! The page rasteriser behind redaction flattening (lead decision D27, `flatten.rs`): a CPU coverage
//! rasteriser implementing `hayro_interpret::Device`, so hayro-interpret (Apache-2.0 OR MIT) reads the
//! PDF content and this module paints it into an RGB canvas the crate owns and bounds.
//!
//! What it draws: fills and strokes with solid colours (non-zero and even-odd rules, kurbo stroke
//! expansion, dashes), glyph outlines and Type 3 glyph procedures, raster and stencil images (nearest
//! sample, image alpha), clip paths (nested, as coverage masks over their bounds), the blend modes of
//! ISO 32000-1 11.3.5 (separable 11.3.5.2 and non-separable 11.3.5.3) over the opaque white page, and
//! transparency (lane SMB-PRESS-PAINT-TRANSPARENCY, clean-room from ISO 32000-1 11.4-11.6):
//! transparency groups as offscreen layers (11.4: each element composited with the basic compositing
//! formula of 11.3.3, shape and opacity kept apart; a non-isolated group starts from its backdrop and
//! removes the backdrop's contribution when it ends with the 11.4.8 formula
//! C = Cn + (Cn - C0) x (a0 / agn - a0); a knockout group composites each element against the group's
//! initial backdrop, weighted by the element's shape, 11.4.6.2), and soft masks (11.6.5): the mask
//! group is rendered into its own layer over a backdrop of `/BC` (luminosity) or transparent (alpha),
//! the mask value is the luminosity (0.30 R + 0.59 G + 0.11 B) or the group alpha, passed through the
//! `/TR` transfer function, and multiplies the opacity of what it masks (an ExtGState soft mask for
//! each later draw, a group's mask for the group result). The interpreter (hayro-interpret 0.7.0)
//! reports groups without their `/I` and `/K` flags, so the flags come from [`Painter::group_flags`]
//! (set by the caller from the document; ISO default non-isolated, non-knockout).
//! Hostile groups (V4-02): nesting deeper than [`MAX_GROUP_DEPTH`] and group or soft-mask layers whose
//! bytes exceed the layer memory cap are refused BEFORE allocation (their content is not drawn,
//! [`FlattenNote::WorkBudget`]); a soft mask that cannot be built leaves what it masks undrawn
//! ([`FlattenNote::SoftMask`]). Tests: the unit tests below and `tests/export.rs`.
//! What it does not draw (absent from the raster, recorded as a [`FlattenNote`]): pattern and shading
//! paints, content past the clip nesting cap or a budget. Absent content is not drawn, and an absent occluder uncovers what it
//! covered (R13-F3: such content shows in the raster where a viewer shows the occluder; it is the page's
//! own content). Region pixels are painted after drawing, so this does not reach a region (test
//! `flattened_region_pixels_are_opaque_and_the_rest_is_rendered`).
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
    BlendMode, CacheKey, ClipPath, Device, FillRule, GlyphDrawMode, Image, ImageData, LumaData, MaskType, Paint, PathDrawMode, SoftMask,
    StrokeProps,
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
/// Deepest nesting of transparency-group and soft-mask layers; deeper ones are refused before
/// allocation.
pub(crate) const MAX_GROUP_DEPTH: usize = 32;
/// Bytes one layer pixel holds (colour, alpha, group alpha, initial backdrop colour and alpha).
const LAYER_PIXEL_BYTES: usize = 36;
/// Largest total of layer and mask bytes alive at once, whatever the canvas.
const MAX_LAYER_BYTES: usize = 1 << 29;
/// Soft masks kept for reuse (by mask and bounds).
const MAX_MASK_CACHE: usize = 16;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayerKind {
    Group,
    Mask,
    /// A refused group or mask: nothing inside it is drawn.
    Dead,
}

/// One offscreen layer over `b` (canvas coordinates): straight colour `c`, alpha `a` (backdrop
/// included), group alpha `ag` (backdrop excluded) and the initial backdrop `c0` / `a0` (kept for a
/// non-isolated or knockout group; empty means transparent).
struct Layer {
    kind: LayerKind,
    b: IRect,
    c: Vec<[f32; 3]>,
    a: Vec<f32>,
    ag: Vec<f32>,
    c0: Vec<[f32; 3]>,
    a0: Vec<f32>,
    isolated: bool,
    knockout: bool,
    opacity: f32,
    blend: BlendMode,
    mask: Option<Rc<MaskBuf>>,
    bytes: usize,
}

impl Layer {
    fn dead() -> Self {
        Self {
            kind: LayerKind::Dead,
            b: IRect { x0: 0, y0: 0, x1: 0, y1: 0 },
            c: Vec::new(),
            a: Vec::new(),
            ag: Vec::new(),
            c0: Vec::new(),
            a0: Vec::new(),
            isolated: true,
            knockout: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
            mask: None,
            bytes: 0,
        }
    }
    fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.b.x0 || x >= self.b.x1 || y < self.b.y0 || y >= self.b.y1 {
            return None;
        }
        y.checked_sub(self.b.y0)?.checked_mul(self.b.width())?.checked_add(x.checked_sub(self.b.x0)?)
    }
    /// Composites one element pixel: colour `cs`, shape `f`, opacity `q` (ISO 32000-1 11.3.3, 11.4).
    fn put(&mut self, x: usize, y: usize, cs: [f32; 3], f: f32, q: f32, mode: BlendMode) {
        let Some(i) = self.index(x, y) else { return };
        let (Some(c), Some(a), Some(ag)) = (self.c.get(i).copied(), self.a.get(i).copied(), self.ag.get(i).copied()) else { return };
        let (nc, na, nag) = if self.knockout {
            let (c0, a0) = match (self.c0.get(i), self.a0.get(i)) {
                (Some(c0), Some(a0)) => (*c0, *a0),
                _ => ([0.0; 3], 0.0),
            };
            let (r, ar) = blend_into(c0, a0, cs, q, mode);
            let na = (1.0 - f) * a + f * ar;
            let mut nc = [0.0f32; 3];
            if na > 0.0 {
                for k in 0..3 {
                    nc[k] = ((1.0 - f) * a * c[k] + f * ar * r[k]) / na;
                }
            }
            (nc, na, (1.0 - f) * ag + f * q)
        } else {
            let s = f * q;
            let (r, ar) = blend_into(c, a, cs, s, mode);
            (r, ar, ag + s - ag * s)
        };
        if let (Some(c), Some(a), Some(ag)) = (self.c.get_mut(i), self.a.get_mut(i), self.ag.get_mut(i)) {
            *c = nc;
            *a = na.clamp(0.0, 1.0);
            *ag = nag.clamp(0.0, 1.0);
        }
    }
}

/// Soft-mask values over `b` (0 outside).
pub(crate) struct MaskBuf {
    b: IRect,
    v: Vec<f32>,
}

impl MaskBuf {
    fn at(&self, x: usize, y: usize) -> f32 {
        if x < self.b.x0 || x >= self.b.x1 || y < self.b.y0 || y >= self.b.y1 {
            return 0.0;
        }
        let i = y.saturating_sub(self.b.y0).saturating_mul(self.b.width()).saturating_add(x.saturating_sub(self.b.x0));
        self.v.get(i).copied().unwrap_or(0.0)
    }
}

/// The basic compositing formula (ISO 32000-1 11.3.3): source `cs` with alpha `as_` over backdrop
/// `cb` with alpha `ab` under blend mode `mode`; returns the result colour and alpha.
fn blend_into(cb: [f32; 3], ab: f32, cs: [f32; 3], as_: f32, mode: BlendMode) -> ([f32; 3], f32) {
    let as_ = as_.clamp(0.0, 1.0);
    let ar = ab + as_ - ab * as_;
    if ar <= 0.0 {
        return (cb, 0.0);
    }
    let cs = [cs[0].clamp(0.0, 1.0), cs[1].clamp(0.0, 1.0), cs[2].clamp(0.0, 1.0)];
    let mixed = blend(mode, cb, cs);
    let t = as_ / ar;
    let mut out = [0.0f32; 3];
    for k in 0..3 {
        out[k] = ((1.0 - t) * cb[k] + t * ((1.0 - ab) * cs[k] + ab * mixed[k])).clamp(0.0, 1.0);
    }
    (out, ar)
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
    /// Open transparency-group and soft-mask layers, innermost last.
    layers: Vec<Layer>,
    /// The ExtGState soft mask in force for the next draws.
    active_mask: Option<Rc<MaskBuf>>,
    /// The soft mask in force could not be built: the next draws are not drawn.
    mask_dead: bool,
    masks: HashMap<(u128, usize, usize, usize, usize), Rc<MaskBuf>>,
    /// Layer and mask bytes that may still be allocated.
    layer_mem_left: usize,
    /// `/I` (isolated) and `/K` (knockout) of transparency groups (module docs).
    pub group_flags: (bool, bool),
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
            layers: Vec::new(),
            active_mask: None,
            mask_dead: false,
            masks: HashMap::new(),
            layer_mem_left: canvas.checked_mul(LAYER_PIXEL_BYTES)?.checked_mul(4)?.min(MAX_LAYER_BYTES),
            group_flags: (false, false),
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

    /// The blend mode of one draw: `None` when the draw must be skipped (inside a refused group, or
    /// under a soft mask that could not be built).
    fn state(&mut self) -> Option<(f32, BlendMode)> {
        if self.mask_dead {
            self.notes.insert(FlattenNote::SoftMask);
            return None;
        }
        if self.layers.iter().any(|l| l.kind == LayerKind::Dead) {
            return None;
        }
        Some((1.0, self.blend))
    }

    /// Charges `bytes` of layer memory; `false` (and a note) when the cap is reached.
    fn take_layer_mem(&mut self, bytes: usize) -> bool {
        match self.layer_mem_left.checked_sub(bytes) {
            Some(left) => {
                self.layer_mem_left = left;
                true
            }
            None => {
                self.notes.insert(FlattenNote::WorkBudget);
                false
            }
        }
    }

    /// Opens a layer over the current clip bounds; a [`LayerKind::Dead`] layer when the depth or
    /// memory cap refuses it (checked before allocating).
    fn open_layer(&mut self, kind: LayerKind, isolated: bool, knockout: bool, init: Option<([f32; 3], f32)>) -> Layer {
        let b = self.clip().b;
        if self.layers.len() >= MAX_GROUP_DEPTH {
            self.notes.insert(FlattenNote::WorkBudget);
            return Layer::dead();
        }
        let area = b.area();
        let keep_backdrop = !isolated || knockout;
        let Some(bytes) = area.checked_mul(LAYER_PIXEL_BYTES) else {
            self.notes.insert(FlattenNote::WorkBudget);
            return Layer::dead();
        };
        if !self.take_layer_mem(bytes) || !self.charge(area) {
            return Layer::dead();
        }
        let mut l = Layer {
            kind,
            b,
            c: vec![[0.0; 3]; area],
            a: vec![0.0; area],
            ag: vec![0.0; area],
            c0: Vec::new(),
            a0: Vec::new(),
            isolated,
            knockout,
            opacity: 1.0,
            blend: BlendMode::Normal,
            mask: None,
            bytes,
        };
        if let Some((c, a)) = init {
            l.c.iter_mut().for_each(|v| *v = c);
            l.a.iter_mut().for_each(|v| *v = a);
        } else if !isolated {
            // Non-isolated: the group starts from its backdrop (the enclosing layer or the page).
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    let (bc, ba) = self.backdrop_at(x, y);
                    if let Some(i) = l.index(x, y) {
                        l.c[i] = bc;
                        l.a[i] = ba;
                    }
                }
            }
        }
        if keep_backdrop {
            l.c0 = l.c.clone();
            l.a0 = l.a.clone();
        }
        l
    }

    /// Colour and alpha under the innermost layer at (x, y): the enclosing layer, or the opaque page.
    fn backdrop_at(&self, x: usize, y: usize) -> ([f32; 3], f32) {
        if let Some(l) = self.layers.last() {
            return match l.index(x, y) {
                Some(i) => (l.c.get(i).copied().unwrap_or([0.0; 3]), l.a.get(i).copied().unwrap_or(0.0)),
                None => ([0.0; 3], 0.0),
            };
        }
        let i = y.saturating_mul(self.w).saturating_add(x).saturating_mul(3);
        match self.px.get(i..i.saturating_add(3)) {
            Some(p) => ([f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0], 1.0),
            None => ([1.0; 3], 1.0),
        }
    }

    /// Renders `mask` (ISO 32000-1 11.6.5) over the current clip bounds into mask values; `None`
    /// when it is refused (depth or memory cap).
    fn build_mask<'a>(&mut self, mask: &SoftMask<'a>) -> Option<Rc<MaskBuf>>
    where
        Self: Device<'a>,
    {
        let b = self.clip().b;
        let key = (mask.cache_key(), b.x0, b.y0, b.x1, b.y1);
        if let Some(m) = self.masks.get(&key) {
            return Some(m.clone());
        }
        let lum = mask.mask_type() == MaskType::Luminosity;
        let bc = mask.background_color().to_rgba().components();
        let init = if lum { Some(([bc[0], bc[1], bc[2]], 1.0)) } else { None };
        let values_bytes = b.area().checked_mul(4)?;
        if !self.take_layer_mem(values_bytes) {
            return None;
        }
        let layer = self.open_layer(LayerKind::Mask, true, false, init);
        if layer.kind == LayerKind::Dead {
            self.layer_mem_left = self.layer_mem_left.saturating_add(values_bytes);
            return None;
        }
        let depth = self.layers.len();
        self.layers.push(layer);
        let (saved_mask, saved_dead, saved_blend) = (self.active_mask.take(), self.mask_dead, self.blend);
        self.mask_dead = false;
        self.blend = BlendMode::Normal;
        mask.interpret(self);
        self.active_mask = saved_mask;
        self.mask_dead = saved_dead;
        self.blend = saved_blend;
        // Pop anything the mask content left open, then the mask layer itself.
        while self.layers.len() > depth.saturating_add(1) {
            self.pop_layer();
        }
        let l = self.layers.pop()?;
        self.layer_mem_left = self.layer_mem_left.saturating_add(l.bytes);
        let tr = mask.transfer_function();
        let mut v = Vec::with_capacity(l.c.len());
        for (c, ag) in l.c.iter().zip(&l.ag) {
            let raw = if lum { 0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2] } else { *ag };
            let raw = raw.clamp(0.0, 1.0);
            v.push(tr.map_or(raw, |t| t.apply(raw)));
        }
        let m = Rc::new(MaskBuf { b: l.b, v });
        if self.masks.len() < MAX_MASK_CACHE {
            self.masks.insert(key, m.clone());
        } else {
            self.layer_mem_left = self.layer_mem_left.saturating_add(values_bytes);
        }
        Some(m)
    }

    /// Ends the innermost layer: a group result is composited into what encloses it (ISO 32000-1
    /// 11.4.8 for a non-isolated group) with the group's opacity, soft mask, blend mode and the clip.
    fn pop_layer(&mut self) {
        let Some(l) = self.layers.pop() else { return };
        self.layer_mem_left = self.layer_mem_left.saturating_add(l.bytes);
        if l.kind != LayerKind::Group || !self.charge(l.b.area()) {
            return;
        }
        for y in l.b.y0..l.b.y1 {
            for x in l.b.x0..l.b.x1 {
                let Some(i) = l.index(x, y) else { continue };
                let (Some(cn), Some(agn)) = (l.c.get(i).copied(), l.ag.get(i).copied()) else { continue };
                if agn <= 0.0 {
                    continue;
                }
                let mut c = cn;
                if !l.isolated
                    && let (Some(c0), Some(a0)) = (l.c0.get(i), l.a0.get(i))
                {
                    let k = a0 / agn - a0;
                    for j in 0..3 {
                        c[j] = (cn[j] + (cn[j] - c0[j]) * k).clamp(0.0, 1.0);
                    }
                }
                let q = l.opacity * l.mask.as_ref().map_or(1.0, |m| m.at(x, y));
                let f = self.clip().at(x, y);
                self.put_raw(x, y, c, f, agn * q, l.blend);
            }
        }
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
            p.composite(x, y, color, c * m, a, blend);
        });
    }

    /// One element pixel with shape `shape` and opacity `opacity`, under the ExtGState soft mask in
    /// force.
    fn composite(&mut self, x: usize, y: usize, src: [f32; 3], shape: f32, opacity: f32, blend: BlendMode) {
        let q = match &self.active_mask {
            Some(m) => opacity * m.at(x, y),
            None => opacity,
        };
        self.put_raw(x, y, src, shape, q, blend);
    }

    /// Composites into the innermost layer, or into the opaque page (backdrop alpha 1).
    fn put_raw(&mut self, x: usize, y: usize, src: [f32; 3], shape: f32, opacity: f32, blend: BlendMode) {
        let (f, q) = (shape.clamp(0.0, 1.0), opacity.clamp(0.0, 1.0));
        if f <= 0.0 || q <= 0.0 {
            return;
        }
        if let Some(l) = self.layers.last_mut() {
            if l.kind != LayerKind::Dead {
                l.put(x, y, src, f, q, blend);
            }
            return;
        }
        let Some(i) = y.checked_mul(self.w).and_then(|v| v.checked_add(x)).and_then(|v| v.checked_mul(3)) else { return };
        let Some(px) = self.px.get_mut(i..i.saturating_add(3)) else { return };
        let cb = [f32::from(px[0]) / 255.0, f32::from(px[1]) / 255.0, f32::from(px[2]) / 255.0];
        let (out, _) = blend_into(cb, 1.0, src, f * q, blend);
        for (slot, v) in px.iter_mut().zip(out) {
            *slot = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
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
                            p.composite(x, y, [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0], c, ma * alpha, blend);
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
                            p.composite(x, y, [rgba[0], rgba[1], rgba[2]], c, f32::from(*l) / 255.0 * rgba[3] * alpha, blend);
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

/// The blend function B(cb, cs) of ISO 32000-1 11.3.5: separable modes per channel (11.3.5.2), Hue,
/// Saturation, Color and Luminosity through Lum / SetLum / Sat / SetSat (11.3.5.3).
fn blend(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        _ => [blend_channel(mode, cb[0], cs[0]), blend_channel(mode, cb[1], cs[1]), blend_channel(mode, cb[2], cs[2])],
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > 0.0 {
        for v in &mut out {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 && x - l > 0.0 {
        for v in &mut out {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// SetSat: the largest component becomes `s`, the smallest 0, the middle one scaled between.
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| c[*a].total_cmp(&c[*b]));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[mx] > c[mn] {
        out[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        out[mx] = s;
    }
    out
}

/// ISO 32000-1 11.3.5.2 separable blend function B(cb, cs) of one channel.
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
        match mask {
            None => {
                self.active_mask = None;
                self.mask_dead = false;
            }
            Some(m) => match self.build_mask(&m) {
                Some(buf) => {
                    self.active_mask = Some(buf);
                    self.mask_dead = false;
                }
                None => {
                    self.active_mask = None;
                    self.mask_dead = true;
                }
            },
        }
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
        let group_mask = match mask {
            Some(m) => match self.build_mask(&m) {
                Some(b) => Some(b),
                None => {
                    self.notes.insert(FlattenNote::SoftMask);
                    self.layers.push(Layer::dead());
                    return;
                }
            },
            None => None,
        };
        let (isolated, knockout) = self.group_flags;
        let mut l = self.open_layer(LayerKind::Group, isolated, knockout, None);
        l.opacity = alpha;
        l.blend = blend_mode;
        l.mask = group_mask;
        self.layers.push(l);
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
        self.pop_layer();
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

    /// Hostile nesting: groups past MAX_GROUP_DEPTH are refused before allocation (their content
    /// is not drawn) and pops stay balanced; a layer over the memory cap is refused the same way.
    #[test]
    fn deep_and_huge_groups_are_refused_before_allocation() {
        let cancel = CancellationToken::default();
        let mut p = painter(&cancel);
        for _ in 0..(MAX_GROUP_DEPTH + 8) {
            p.push_transparency_group(1.0, None, BlendMode::Normal);
        }
        assert_eq!(p.layers.len(), MAX_GROUP_DEPTH + 8);
        assert!(p.layers.iter().skip(MAX_GROUP_DEPTH).all(|l| l.kind == LayerKind::Dead && l.c.capacity() == 0));
        let mut sq = BezPath::new();
        sq.move_to((0.0, 0.0));
        sq.line_to((20.0, 0.0));
        sq.line_to((20.0, 10.0));
        sq.line_to((0.0, 10.0));
        sq.close_path();
        p.fill_device(&sq, FillRule::NonZero, [0.0, 0.0, 0.0, 1.0]);
        for _ in 0..(MAX_GROUP_DEPTH + 8) {
            p.pop_transparency_group();
        }
        assert!(p.layers.is_empty());
        assert_eq!(p.px[0], 255, "content inside a refused group is not drawn");
        assert!(p.notes.contains(&FlattenNote::WorkBudget));
        // Memory cap: a 2000 x 2000 canvas with only 1000 layer bytes left refuses the group layer.
        let mut big = Painter::new(2000, 2000, u64::MAX, &cancel).expect("canvas");
        big.layer_mem_left = 1000;
        big.push_transparency_group(0.5, None, BlendMode::Normal);
        assert_eq!(big.layers[0].kind, LayerKind::Dead);
        assert_eq!(big.layers[0].c.capacity(), 0, "nothing allocated");
        big.pop_transparency_group();
        assert_eq!(big.layer_mem_left, 1000);
    }

    /// Non-separable blend modes (ISO 32000-1 11.3.5.3): Luminosity takes the backdrop's hue and
    /// saturation with the source's luminosity; Color the reverse.
    #[test]
    fn non_separable_blend_modes() {
        let red = [1.0, 0.0, 0.0];
        let gray = [0.5, 0.5, 0.5];
        let l = blend(BlendMode::Luminosity, red, gray);
        assert!((lum(l) - 0.5).abs() < 1e-5, "{l:?}");
        assert!(l[0] > l[1] && (l[1] - l[2]).abs() < 1e-6, "keeps red's hue {l:?}");
        let c = blend(BlendMode::Color, gray, red);
        assert!((lum(c) - 0.5).abs() < 1e-5 && c[0] > c[1], "{c:?}");
        assert_eq!(blend(BlendMode::Saturation, red, gray), [0.3, 0.3, 0.3].map(|v: f32| v), "zero saturation -> gray at red's lum");
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
