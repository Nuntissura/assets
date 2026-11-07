//! Geometry in logical px and the one snapping function every rect goes through (design-system.json
//! `scale_rules`, build-order B1/B2: convert EDGES, not sizes, so adjacent panes share one physical edge).

/// sRGB-encoded 8-bit colour, straight alpha (design-system.json `color.format`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
impl Rgba8 {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const TRANSPARENT: Rgba8 = Rgba8::new(0, 0, 0, 0);
    pub fn hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }
}

/// A point in logical px.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rect in logical px: `x0 <= x1`, `y0 <= y1` after [`Rect::new`].
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}
impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }
    pub fn xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self::new(x, y, x + w.max(0.0), y + h.max(0.0))
    }
    pub fn w(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn h(&self) -> f32 {
        self.y1 - self.y0
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x < self.x1 && p.y >= self.y0 && p.y < self.y1
    }
    /// True when the interiors overlap (shared edges do not intersect).
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let r = Rect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) };
        if r.x0 >= r.x1 || r.y0 >= r.y1 { Rect { x0: r.x0, y0: r.y0, x1: r.x0, y1: r.y0 } } else { r }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
    pub fn inset(&self, l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect::new(self.x0 + l, self.y0 + t, (self.x1 - r).max(self.x0 + l), (self.y1 - b).max(self.y0 + t))
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }
    /// True when every edge lands on a multiple of `grid` logical px (within 1/1024).
    pub fn on_grid(&self, grid: f32) -> bool {
        [self.x0, self.y0, self.x1, self.y1].iter().all(|v| ((v / grid) - (v / grid).round()).abs() < 1.0 / 1024.0)
    }
}

/// A rect in physical pixels, half-open: `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}
impl IRect {
    pub fn w(&self) -> i32 {
        self.x1.saturating_sub(self.x0).max(0)
    }
    pub fn h(&self) -> i32 {
        self.y1.saturating_sub(self.y0).max(0)
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let r = IRect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) };
        if r.x0 >= r.x1 || r.y0 >= r.y1 { IRect { x0: r.x0, y0: r.y0, x1: r.x0, y1: r.y0 } } else { r }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

/// Display scale `s = os_monitor_scale * ui_scale` (scale_rules.scale). Always finite and in [0.5, 8].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale(f32);
impl Scale {
    pub const ONE: Scale = Scale(1.0);
    /// None when `s` is not finite or outside [0.5, 8].
    pub fn new(s: f32) -> Option<Scale> {
        (s.is_finite() && (0.5..=8.0).contains(&s)).then_some(Scale(s))
    }
    pub fn get(self) -> f32 {
        self.0
    }
    /// The edge rule: one logical coordinate to its physical pixel edge, `round(v * s)`, clamped to i32 range.
    pub fn edge(self, v: f32) -> i32 {
        let p = (v * self.0).round();
        if p.is_nan() { 0 } else { p.clamp(-1_073_741_824.0, 1_073_741_824.0) as i32 }
    }
    /// THE snapping function: every rect of the CPU and Vulkan painters goes through here. Edges convert, not sizes,
    /// so `snap(a).x1 == snap(b).x0` whenever `a.x1 == b.x0` (AC-GEOM-03).
    pub fn snap(self, r: Rect) -> IRect {
        // RED STUB: sizes, not edges.
        let (x0, y0) = (self.edge(r.x0), self.edge(r.y0));
        IRect { x0, y0, x1: x0 + self.edge(r.w()), y1: y0 + self.edge(r.h()) }
    }
    /// A 1 logical px line: `max(1, floor(s))` physical px (scale_rules.hairline).
    pub fn hairline(self) -> i32 {
        (self.0.floor() as i32).max(1)
    }
    /// An `n` logical px edge (n >= 2): `max(n, round(n * s))` physical px; n = 1 is [`Scale::hairline`].
    pub fn edge_px(self, n: u8) -> i32 {
        if n <= 1 {
            return self.hairline();
        }
        let n = i32::from(n);
        n.max((n as f32 * self.0).round() as i32)
    }
    pub fn px(self, v: f32) -> f32 {
        v * self.0
    }
}
