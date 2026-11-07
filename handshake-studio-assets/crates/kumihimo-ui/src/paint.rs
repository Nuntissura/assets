//! CPU painter: draws a [`DisplayList`] into an RGBA8 buffer, so builders and models see real pixels with no window
//! and no GPU. Blending is in sRGB-encoded space (scale_rules.colour_pipeline), so a token's value is the pixel value of
//! a flat fill. Every rect goes through [`Scale::snap`].
use crate::display::{DisplayList, IconId, ImageId, Prim};
use crate::geom::{IRect, Point, Rect, Rgba8, Scale};
use crate::text::{GlyphKey, Text};

/// An RGBA8 image, row-major, stride = 4 * width, straight alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Largest canvas side the painter accepts.
pub const MAX_CANVAS_SIDE: u32 = 16_384;

impl Canvas {
    /// None when a side is 0 or over [`MAX_CANVAS_SIDE`].
    pub fn new(width: u32, height: u32, clear: Rgba8) -> Option<Canvas> {
        if width == 0 || height == 0 || width > MAX_CANVAS_SIDE || height > MAX_CANVAS_SIDE {
            return None;
        }
        let n = (width as usize).checked_mul(height as usize)?;
        let mut rgba = Vec::with_capacity(n.checked_mul(4)?);
        for _ in 0..n {
            rgba.extend_from_slice(&[clear.r, clear.g, clear.b, clear.a]);
        }
        Some(Canvas { width, height, rgba })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
    pub fn pixel(&self, x: u32, y: u32) -> Rgba8 {
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        Rgba8::new(self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3])
    }
    fn bounds(&self) -> IRect {
        IRect { x0: 0, y0: 0, x1: self.width as i32, y1: self.height as i32 }
    }
    /// Blends `c` with coverage `cov` (0..=255) at (x, y), encoded space; no-op outside the canvas.
    fn blend(&mut self, x: i32, y: i32, c: Rgba8, cov: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let a = u32::from(c.a) * u32::from(cov);
        if a == 0 {
            return;
        }
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        if a == 255 * 255 {
            self.rgba[i..i + 4].copy_from_slice(&[c.r, c.g, c.b, 255]);
            return;
        }
        let inv = 255 * 255 - a;
        let mix = |s: u8, d: u8| ((u32::from(s) * a + u32::from(d) * inv + 32512) / 65025) as u8;
        self.rgba[i] = mix(c.r, self.rgba[i]);
        self.rgba[i + 1] = mix(c.g, self.rgba[i + 1]);
        self.rgba[i + 2] = mix(c.b, self.rgba[i + 2]);
        let da = u32::from(self.rgba[i + 3]);
        // Colour mixing assumes an opaque destination (chrome frames are cleared opaque); alpha is plain "over".
        self.rgba[i + 3] = ((a * 255 + da * inv + 32512) / 65025).min(255) as u8;
    }
    /// FNV-1a 64 over width, height and the RGBA bytes: the golden pixel hash.
    pub fn fnv64(&self) -> u64 {
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        let mut eat = |b: u8| {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        };
        for b in self.width.to_le_bytes().into_iter().chain(self.height.to_le_bytes()) {
            eat(b);
        }
        for &b in &self.rgba {
            eat(b);
        }
        h
    }
}

/// An icon mask: two coverage channels per pixel, `[form, hot]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconMask {
    pub width: u32,
    pub height: u32,
    pub rg: Vec<[u8; 2]>,
}
/// Borrowed RGBA8 image.
pub struct ImageRef<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

/// Icons and images the host supplies. Missing ones draw nothing and are counted.
pub trait Resources {
    /// Mask of `icon` rasterised at `px` physical px square.
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask>;
    fn image(&self, image: ImageId) -> Option<ImageRef<'_>>;
}
/// No icons, no images.
pub struct NoResources;
impl Resources for NoResources {
    fn icon(&mut self, _: IconId, _: u32) -> Option<IconMask> {
        None
    }
    fn image(&self, _: ImageId) -> Option<ImageRef<'_>> {
        None
    }
}

/// What one paint call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintStats {
    pub prims: u64,
    pub glyphs: u64,
    /// Glyph rasters the port refused (each is drawn as nothing).
    pub glyph_refusals: u64,
    pub missing_resources: u64,
    /// Unbalanced PopScissor items ignored.
    pub scissor_underflow: u64,
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}
fn cov(v: f32) -> u8 {
    (clamp01(v) * 255.0).round() as u8
}

/// Signed distance from (px, py) to a rounded box of half extents (hx, hy), centre (cx, cy), radius r.
fn rbox(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let r = r.min(hx).min(hy).max(0.0);
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// Paints `list` over `canvas` at `scale`.
pub fn paint(canvas: &mut Canvas, list: &DisplayList, scale: Scale, text: &mut Text, res: &mut dyn Resources) -> PaintStats {
    let mut st = PaintStats::default();
    let mut clips: Vec<IRect> = vec![canvas.bounds()];
    let s = scale.get();
    for item in list.items.iter().take(0) {
        // RED STUB: nothing drawn.
        st.prims = st.prims.saturating_add(1);
        let clip = *clips.last().unwrap_or(&IRect::default());
        match item {
            Prim::Rect { rect, color } => {
                let r = scale.snap(*rect).intersect(&clip);
                for y in r.y0..r.y1 {
                    for x in r.x0..r.x1 {
                        canvas.blend(x, y, *color, 255);
                    }
                }
            }
            Prim::RRect { rect, radius, fill, border } => {
                let p = scale.snap(*rect);
                if p.is_empty() {
                    continue;
                }
                let (cx, cy) = ((p.x0 + p.x1) as f32 / 2.0, (p.y0 + p.y1) as f32 / 2.0);
                let (hx, hy) = (p.w() as f32 / 2.0, p.h() as f32 / 2.0);
                let rad = (radius * s).round();
                let bw = border.map(|b| if b.width <= 1.0 { scale.hairline() as f32 } else { (b.width * s).round().max(b.width) });
                let r = p.intersect(&clip);
                for y in r.y0..r.y1 {
                    for x in r.x0..r.x1 {
                        let d = rbox(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hx, hy, rad);
                        let outer = clamp01(0.5 - d);
                        if outer <= 0.0 {
                            continue;
                        }
                        match (border, bw) {
                            (Some(b), Some(w)) => {
                                let din = rbox(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hx - w, hy - w, (rad - w).max(0.0));
                                let inner = clamp01(0.5 - din);
                                canvas.blend(x, y, b.color, cov(outer - inner));
                                canvas.blend(x, y, *fill, cov(inner));
                            }
                            _ => canvas.blend(x, y, *fill, cov(outer)),
                        }
                    }
                }
            }
            Prim::Line { a, b, width, color } => line(canvas, &clip, scale, *a, *b, *width, |_| Some(*color)),
            Prim::Dash { a, b, width, dash, phase, on, off } => {
                let period = (dash * 2.0 * s).max(2.0);
                let on_len = (dash * s).max(1.0);
                let ph = phase * s;
                let (on, off) = (*on, *off);
                line(canvas, &clip, scale, *a, *b, *width, move |t| {
                    if (t + ph).rem_euclid(period) < on_len { Some(on) } else { off }
                });
            }
            Prim::Glyphs { origin, token, glyphs, color } => {
                let size = token.style().size;
                let px_size_64 = (size * s * 64.0).round() as u32;
                let base_y = scale.edge(origin.y);
                for g in glyphs {
                    st.glyphs = st.glyphs.saturating_add(1);
                    let fx = (origin.x + g.x) * s;
                    let mut ix = fx.floor() as i32;
                    let mut bin = ((fx - fx.floor()) * 4.0).round() as u8;
                    if bin >= 4 {
                        bin = 0;
                        ix = ix.saturating_add(1);
                    }
                    let gy = base_y.saturating_sub((g.y * s).round() as i32);
                    let key = GlyphKey { face: g.face, glyph_id: g.glyph_id, px_size_64, subpx_x: bin };
                    let lut = *text.lut();
                    let Ok(mask) = text.mask(key) else {
                        st.glyph_refusals = st.glyph_refusals.saturating_add(1);
                        continue;
                    };
                    for my in 0..mask.height as i32 {
                        let y = gy + mask.top + my;
                        if y < clip.y0 || y >= clip.y1 {
                            continue;
                        }
                        for mx in 0..mask.width as i32 {
                            let x = ix + mask.left + mx;
                            if x < clip.x0 || x >= clip.x1 {
                                continue;
                            }
                            let c = mask.data[(my as usize) * (mask.width as usize) + mx as usize];
                            canvas.blend(x, y, *color, lut[c as usize]);
                        }
                    }
                }
            }
            Prim::Icon { rect, icon, form, hot } => {
                let p = scale.snap(*rect);
                let px = p.w().min(p.h()).max(1) as u32;
                let Some(mask) = res.icon(*icon, px) else {
                    st.missing_resources = st.missing_resources.saturating_add(1);
                    continue;
                };
                for my in 0..mask.height as i32 {
                    for mx in 0..mask.width as i32 {
                        let (x, y) = (p.x0 + mx, p.y0 + my);
                        if x < clip.x0 || x >= clip.x1 || y < clip.y0 || y >= clip.y1 {
                            continue;
                        }
                        let [f, h] = mask.rg[(my as usize) * (mask.width as usize) + mx as usize];
                        canvas.blend(x, y, *form, f);
                        canvas.blend(x, y, *hot, h);
                    }
                }
            }
            Prim::Image { rect, image } => {
                let p = scale.snap(*rect);
                let Some(img) = res.image(*image) else {
                    st.missing_resources = st.missing_resources.saturating_add(1);
                    continue;
                };
                let need = (img.width as usize).saturating_mul(img.height as usize).saturating_mul(4);
                if img.width == 0 || img.height == 0 || img.rgba.len() < need || p.is_empty() {
                    st.missing_resources = st.missing_resources.saturating_add(1);
                    continue;
                }
                let r = p.intersect(&clip);
                for y in r.y0..r.y1 {
                    let sy = (((y - p.y0) as i64 * i64::from(img.height)) / i64::from(p.h())) as usize;
                    for x in r.x0..r.x1 {
                        let sx = (((x - p.x0) as i64 * i64::from(img.width)) / i64::from(p.w())) as usize;
                        let i = (sy * img.width as usize + sx) * 4;
                        let c = Rgba8::new(img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]);
                        canvas.blend(x, y, c, 255);
                    }
                }
            }
            Prim::Checker { rect, a, b, cell } => {
                let p = scale.snap(*rect);
                let c = ((cell * s).round() as i32).max(1);
                let r = p.intersect(&clip);
                for y in r.y0..r.y1 {
                    for x in r.x0..r.x1 {
                        let odd = (((x - p.x0) / c) + ((y - p.y0) / c)) % 2 == 1;
                        canvas.blend(x, y, if odd { *b } else { *a }, 255);
                    }
                }
            }
            Prim::Ramp { rect, corners } => {
                let p = scale.snap(*rect);
                let r = p.intersect(&clip);
                let (w, h) = ((p.w() - 1).max(1) as f32, (p.h() - 1).max(1) as f32);
                for y in r.y0..r.y1 {
                    let v = (y - p.y0) as f32 / h;
                    for x in r.x0..r.x1 {
                        let u = (x - p.x0) as f32 / w;
                        let lerp = |k: fn(&Rgba8) -> u8| {
                            let top = f32::from(k(&corners[0])) * (1.0 - u) + f32::from(k(&corners[1])) * u;
                            let bot = f32::from(k(&corners[2])) * (1.0 - u) + f32::from(k(&corners[3])) * u;
                            (top * (1.0 - v) + bot * v).round().clamp(0.0, 255.0) as u8
                        };
                        let c = Rgba8::new(lerp(|c| c.r), lerp(|c| c.g), lerp(|c| c.b), lerp(|c| c.a));
                        canvas.blend(x, y, c, 255);
                    }
                }
            }
            Prim::Shadow { rect, radius, offset_y, blur, color } => {
                let p = scale.snap(rect.translate(0.0, *offset_y));
                let bl = (blur * s).max(1.0);
                let grow = bl.ceil() as i32;
                let area = IRect { x0: p.x0 - grow, y0: p.y0 - grow, x1: p.x1 + grow, y1: p.y1 + grow }.intersect(&clip);
                let (cx, cy) = ((p.x0 + p.x1) as f32 / 2.0, (p.y0 + p.y1) as f32 / 2.0);
                let (hx, hy) = (p.w() as f32 / 2.0, p.h() as f32 / 2.0);
                for y in area.y0..area.y1 {
                    for x in area.x0..area.x1 {
                        let d = rbox(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hx, hy, radius * s);
                        let t = clamp01(0.5 - d / bl);
                        let k = t * t * (3.0 - 2.0 * t);
                        canvas.blend(x, y, *color, cov(k));
                    }
                }
            }
            Prim::PushScissor(r) => {
                let next = scale.snap(*r).intersect(&clip);
                clips.push(next);
            }
            Prim::PopScissor => {
                if clips.len() > 1 {
                    clips.pop();
                } else {
                    st.scissor_underflow = st.scissor_underflow.saturating_add(1);
                }
            }
        }
    }
    st
}

/// Antialiased segment; `color_at(t)` gives the colour at physical distance `t` along the segment (None = gap).
fn line(canvas: &mut Canvas, clip: &IRect, scale: Scale, a: Point, b: Point, width: f32, color_at: impl Fn(f32) -> Option<Rgba8>) {
    let s = scale.get();
    let (ax, ay, bx, by) = (a.x * s, a.y * s, b.x * s, b.y * s);
    let hw = if width <= 1.0 { scale.hairline() as f32 / 2.0 } else { width * s / 2.0 };
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let len = len2.sqrt();
    let pad = hw + 1.0;
    let area = IRect {
        x0: (ax.min(bx) - pad).floor() as i32,
        y0: (ay.min(by) - pad).floor() as i32,
        x1: (ax.max(bx) + pad).ceil() as i32,
        y1: (ay.max(by) + pad).ceil() as i32,
    }
    .intersect(clip);
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
            let d = (qx * qx + qy * qy).sqrt();
            let c = clamp01(hw + 0.5 - d);
            if c <= 0.0 {
                continue;
            }
            if let Some(col) = color_at(t * len) {
                canvas.blend(x, y, col, cov(c));
            }
        }
    }
}

/// The rect a logical rect covers on the canvas (diagnostics).
pub fn physical(scale: Scale, r: Rect) -> IRect {
    scale.snap(r)
}
