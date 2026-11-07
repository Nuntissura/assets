//! Reference scenes: the four widgets the golden tests pin (toolbar, tab row, slider, layer row) and a type specimen,
//! built through the public widget API so goldens, the text sample and later `gui.shot` checks render the same thing.
//! [`GalleryIcons`] is a procedural stand-in icon source (no icon set exists yet; the designer owns it).
use crate::display::{IconId, ImageId};
use crate::geom::{Rect, Scale};
use crate::paint::{Canvas, IconMask, ImageRef, PaintStats, Resources, paint};
use crate::text::Text;
use crate::tokens::{ColorToken, TextToken, Theme, size, spacing};
use crate::tree::{Align, Axis, Builder, Interaction, LayoutSpec, Node, Paint, Panel, Role, Size};
use crate::widgets;

pub const ICON_MOVE: IconId = IconId(1);
pub const ICON_BRUSH: IconId = IconId(2);
pub const ICON_EYE: IconId = IconId(3);
pub const ICON_FX: IconId = IconId(4);
pub const ICON_LOCK: IconId = IconId(5);
pub const THUMB_IMAGE: ImageId = ImageId(1);

/// Procedural two-tint icons: an outlined square (move), a disc with a hot point (brush), an eye ring, a dot and a
/// bar. Deterministic for a given pixel size.
pub struct GalleryIcons {
    thumb: Vec<u8>,
}
impl Default for GalleryIcons {
    fn default() -> Self {
        // 8 x 8 thumbnail: a diagonal two-tone ramp in grey, values only (not chrome colours).
        let mut thumb = Vec::with_capacity(8 * 8 * 4);
        for y in 0..8u8 {
            for x in 0..8u8 {
                let v = 60u8.saturating_add(x.saturating_add(y).saturating_mul(12));
                thumb.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Self { thumb }
    }
}
impl Resources for GalleryIcons {
    fn icon(&mut self, icon: IconId, px: u32) -> Option<IconMask> {
        let n = px.clamp(1, 512);
        let f = n as f32;
        let stroke = (f / 9.0).max(1.0);
        let mut rg = Vec::with_capacity((n * n) as usize);
        for y in 0..n {
            for x in 0..n {
                let (cx, cy) = (x as f32 + 0.5 - f / 2.0, y as f32 + 0.5 - f / 2.0);
                let r = (cx * cx + cy * cy).sqrt();
                let ring = |rad: f32| (stroke / 2.0 + 0.5 - (r - rad).abs()).clamp(0.0, 1.0);
                let (form, hot) = match icon.0 {
                    1 => {
                        let e = (f / 2.0 - 1.0) - cx.abs().max(cy.abs());
                        ((stroke / 2.0 + 0.5 - (e - stroke / 2.0).abs()).clamp(0.0, 1.0), 0.0)
                    }
                    2 => ((f * 0.32 + 0.5 - r).clamp(0.0, 1.0) * if r > f * 0.12 { 1.0 } else { 0.0 }, (f * 0.12 + 0.5 - r).clamp(0.0, 1.0)),
                    3 => (ring(f * 0.36).max((f * 0.12 + 0.5 - r).clamp(0.0, 1.0)), 0.0),
                    4 => ((f * 0.4 + 0.5 - r).clamp(0.0, 1.0), 0.0),
                    _ => ((f * 0.3 + 0.5 - cx.abs()).clamp(0.0, 1.0).min((f * 0.15 + 0.5 - cy.abs()).clamp(0.0, 1.0)), 0.0),
                };
                rg.push([(form * 255.0).round() as u8, (hot * 255.0).round() as u8]);
            }
        }
        Some(IconMask { width: n, height: n, rg })
    }
    fn image(&self, image: ImageId) -> Option<ImageRef<'_>> {
        (image == THUMB_IMAGE).then(|| ImageRef { width: 8, height: 8, rgba: &self.thumb })
    }
}

/// The scenes the golden tests pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Toolbar,
    TabRow,
    Slider,
    LayerRow,
}
impl Scene {
    pub const ALL: [Scene; 4] = [Scene::Toolbar, Scene::TabRow, Scene::Slider, Scene::LayerRow];
    pub fn name(self) -> &'static str {
        match self {
            Scene::Toolbar => "toolbar",
            Scene::TabRow => "tab_row",
            Scene::Slider => "slider",
            Scene::LayerRow => "layer_row",
        }
    }
    /// Logical size of the scene: Inspect Dock width (300) by the widget's height.
    pub fn size(self) -> (f32, f32) {
        let h = match self {
            Scene::Toolbar => size::FRAME_CONTEXT_BAR_H,
            Scene::TabRow => size::DOCK_GROUP_HEADER_H,
            Scene::Slider => widgets::component::SLIDER_ROW_PITCH,
            Scene::LayerRow => size::ROW_LAYER_H * 2.0,
        };
        (size::DOCK_INSPECT_W, h)
    }
    pub fn build(self, b: &mut Builder<'_>) {
        match self {
            Scene::Toolbar => {
                widgets::toolbar_begin(b, "studio.options.gallery", "Gallery options");
                widgets::icon_button(b, "studio.options.gallery.move", "Move", ICON_MOVE, true);
                widgets::icon_button(b, "studio.options.gallery.brush", "Brush", ICON_BRUSH, true);
                widgets::divider(b);
                widgets::toggle_button(b, "studio.options.gallery.snap", "Snap", true);
                widgets::spacer(b);
                widgets::button(b, "studio.options.gallery.reset", "Reset", true);
                b.end();
            }
            Scene::TabRow => widgets::tab_row(
                b,
                "studio.group.layers",
                &[("studio.tab.layers", "Layers"), ("studio.tab.channels", "Channels"), ("studio.tab.paths", "Paths")],
                0,
            ),
            Scene::Slider => {
                b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }).paint(Paint::Fill(ColorToken::SurfacePanel)));
                widgets::slider(b, "studio.panel.layers.opacity", "Opacity", 72.0, 0.0, 100.0, (0.0, 100.0), "72", Some("%"));
                b.end();
            }
            Scene::LayerRow => {
                b.begin(Node::new("studio.panel.layers.list", Role::Tree).layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }).paint(Paint::Fill(ColorToken::SurfacePanel)));
                widgets::layer_row(b, "studio.panel.layers.row.lyr-1", "Background", ColorToken::TagNone, true, false, 0, Some(THUMB_IMAGE), ICON_EYE, &[ICON_LOCK]);
                widgets::layer_row(b, "studio.panel.layers.row.lyr-2", "Portrait retouch", ColorToken::TagBlue, true, true, 1, None, ICON_EYE, &[ICON_FX]);
                b.end();
            }
        }
    }
}

/// Builds, lays out and paints `scene` at `scale` on `surface.panel`; returns the canvas, its panel and the stats.
pub fn render(scene: Scene, scale: Scale, theme: Theme, ui: &Interaction, text: &mut Text) -> (Canvas, Panel, PaintStats) {
    let (w, h) = scene.size();
    let mut panel = Panel::default();
    let viewport = Rect::xywh(0.0, 0.0, w, h);
    let built = panel.update(1, ui, viewport, scale, theme, text, |b| scene.build(b));
    debug_assert!(built.is_ok());
    let (pw, ph) = (scale.edge(w).max(1) as u32, scale.edge(h).max(1) as u32);
    let mut canvas = Canvas::new(pw, ph, ColorToken::SurfacePanel.resolve(theme)).unwrap_or_else(|| Canvas::new(1, 1, ColorToken::SurfacePanel.resolve(theme)).expect("1x1 canvas"));
    let stats = paint(&mut canvas, panel.display_list(), scale, text, &mut GalleryIcons::default());
    (canvas, panel, stats)
}

/// A type specimen: one row per text style of design-system.json, each a sample in that style, on surface.panel.
pub fn specimen(b: &mut Builder<'_>) {
    b.begin(
        Node::deco()
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, gap: spacing::S2, pad: [spacing::S5, spacing::S4, spacing::S5, spacing::S4], cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfacePanel)),
    );
    let lines: [(TextToken, &str, ColorToken); 12] = [
        (TextToken::MenuTitle, "FILE  EDIT  IMAGE  LAYER  TYPE  SELECT  FILTER  VIEW  WINDOW", ColorToken::TextSecondary),
        (TextToken::Body, "Layers  Channels  Paths  Properties  Adjustments  Swatches", ColorToken::TextPrimary),
        (TextToken::BodyStrong, "Transform  Appearance  Stroke  Fill  Effects", ColorToken::TextPrimary),
        (TextToken::Body, "The quick brown fox jumps over the lazy dog. Hamburgefonstiv 0123456789", ColorToken::TextPrimary),
        (TextToken::Body, "Opacity  Fill  Blend Mode  Lock  Feather  Tolerance  Anti-alias", ColorToken::TextSecondary),
        (TextToken::Value, "100   72.5   1,024   0.25   -12   360   4096 x 2160", ColorToken::TextPrimary),
        (TextToken::Caption, "px  pt  %  deg  W 1920  H 1080  Doc: 24.9M/96.3M  RGB/8", ColorToken::TextTertiary),
        (TextToken::Tab, "Background  Layer 1 copy  Portrait retouch  Dodge & Burn", ColorToken::TextPrimary),
        (TextToken::Title, "Export As  Image Size  New Document", ColorToken::TextPrimary),
        (TextToken::Heading, "Export As", ColorToken::TextPrimary),
        (TextToken::Timecode, "00:01:23:12  00:00:59:29  01:00:00:00", ColorToken::TextPrimary),
        (TextToken::Body, "illegible 1Il| rn m vv w cl d .,;:  ([{ }])  @#&*", ColorToken::TextPrimary),
    ];
    for (token, s, color) in lines {
        b.leaf(Node::deco().paint(Paint::Label { text: s.into(), token, color: Some(color), align: Align::Start, ellipsis: false }).size(Size::Hug, Size::Fixed(token.style().line)));
    }
    b.end();
}

/// Paints the specimen at `scale` into a canvas `w` x `h` logical px.
pub fn render_specimen(scale: Scale, theme: Theme, text: &mut Text, w: f32, h: f32) -> (Canvas, Panel, PaintStats) {
    let mut panel = Panel::default();
    let ui = Interaction::default();
    let built = panel.update(1, &ui, Rect::xywh(0.0, 0.0, w, h), scale, theme, text, specimen);
    debug_assert!(built.is_ok());
    let mut canvas = Canvas::new(scale.edge(w).max(1) as u32, scale.edge(h).max(1) as u32, ColorToken::SurfacePanel.resolve(theme)).unwrap_or_else(|| Canvas::new(1, 1, ColorToken::SurfacePanel.resolve(theme)).expect("1x1 canvas"));
    let stats = paint(&mut canvas, panel.display_list(), scale, text, &mut GalleryIcons::default());
    (canvas, panel, stats)
}
