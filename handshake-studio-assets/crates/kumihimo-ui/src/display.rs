//! The display list: one enum, exactly the designer's primitive list (design-system.json `renderer.primitives`:
//! rect, rrect, line, dash, glyph, icon, image, checker, ramp, shadow, scissor). Coordinates are logical px; painters
//! convert through [`crate::geom::Scale::snap`]. The CPU painter ([`crate::paint`]) and the later Vulkan pass draw
//! the same list.
use crate::geom::{Point, Rect, Rgba8};
use crate::text::ShapedGlyph;
use crate::tokens::TextToken;

/// An icon the host's icon source draws (two-tint RG mask: R form, G hot point).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IconId(pub u32);
/// An RGBA8 image the host's image source holds (canvas, thumbnails).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// Logical px; 1 is a hairline.
    pub width: f32,
    pub color: Rgba8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// Flat colour: all surfaces, edges, seams, ticks.
    Rect { rect: Rect, color: Rgba8 },
    /// Rounded box, 1 physical px antialias, optional border inside the rect.
    RRect { rect: Rect, radius: f32, fill: Rgba8, border: Option<Border> },
    /// Antialiased segment of `width` logical px.
    Line { a: Point, b: Point, width: f32, color: Rgba8 },
    /// Dashed segment: `dash` logical px on, `dash` off, from `phase`; `off` fills the gaps when set (ants).
    Dash { a: Point, b: Point, width: f32, dash: f32, phase: f32, on: Rgba8, off: Option<Rgba8> },
    /// A shaped run; `origin` is the pen start on the baseline, logical px.
    Glyphs { origin: Point, token: TextToken, glyphs: Vec<ShapedGlyph>, color: Rgba8 },
    /// Icon in `rect` (square), form and hot-point tints.
    Icon { rect: Rect, icon: IconId, form: Rgba8, hot: Rgba8 },
    /// Image scaled into `rect` (nearest sample).
    Image { rect: Rect, image: ImageId },
    /// Two-colour transparency grid, `cell` logical px.
    Checker { rect: Rect, a: Rgba8, b: Rgba8, cell: f32 },
    /// Four-corner interpolation in encoded values: top-left, top-right, bottom-left, bottom-right.
    Ramp { rect: Rect, corners: [Rgba8; 4] },
    /// Pre-blurred rounded-rect shadow under `rect` (menus, tooltips, dialogs, overlay panels).
    Shadow { rect: Rect, radius: f32, offset_y: f32, blur: f32, color: Rgba8 },
    /// Scissor: push intersects with the current clip; pop restores it.
    PushScissor(Rect),
    PopScissor,
}

impl Prim {
    /// The designer's primitive id of this item.
    pub fn kind(&self) -> &'static str {
        match self {
            Prim::Rect { .. } => "rect",
            Prim::RRect { .. } => "rrect",
            Prim::Line { .. } => "line",
            Prim::Dash { .. } => "dash",
            Prim::Glyphs { .. } => "glyph",
            Prim::Icon { .. } => "icon",
            Prim::Image { .. } => "image",
            Prim::Checker { .. } => "checker",
            Prim::Ramp { .. } => "ramp",
            Prim::Shadow { .. } => "shadow",
            Prim::PushScissor(_) | Prim::PopScissor => "scissor",
        }
    }
    /// Every primitive id, in the order of design-system.json `renderer.primitives`.
    pub const KINDS: [&'static str; 11] = ["rect", "rrect", "line", "dash", "glyph", "icon", "image", "checker", "ramp", "shadow", "scissor"];
}

/// An ordered display list; later items draw over earlier ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub items: Vec<Prim>,
}
impl DisplayList {
    pub fn push(&mut self, p: Prim) {
        self.items.push(p);
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
