//! First widgets, from components.json records: toolbar with icon, toggle and text buttons; tab row (group header and
//! panel tabs); slider (the wide presentation of ScrubValue); layer row. Each widget builds nodes; sizes and colours are
//! tokens. Callers pass full author_ids (STU-MDL-100); sub-elements derive `<id>.<part>`.
use crate::display::{IconId, ImageId};
use crate::tokens::{ColorToken, StyleKind, TextToken, radius, size, spacing};
use crate::tree::{Action, Align, Axis, Builder, LayoutSpec, Node, Numeric, Paint, Role, Size};

/// Component sizes that components.json records but design-system.json `size` does not (logical px).
pub mod component {
    /// components.json `button.size.icon`, `icon_button.size.icon`.
    pub const BUTTON_ICON: f32 = 14.0;
    /// components.json `slider.size`.
    pub const SLIDER_RAIL_H: f32 = 4.0;
    pub const SLIDER_THUMB: f32 = 12.0;
    pub const SLIDER_ROW_PITCH: f32 = 44.0;
    pub const SLIDER_FIELD_W: f32 = 72.0;
    /// components.json `layer_row.size`.
    pub const LAYER_TAG_EDGE: f32 = 3.0;
    pub const LAYER_EYE: f32 = 22.0;
    pub const LAYER_BADGE: f32 = 12.0;
    /// components.json `context_bar.size.gap`.
    pub const CONTEXT_BAR_GAP: f32 = 8.0;
    /// components.json `document_tab.size`.
    pub const DOC_TAB_PAD_L: f32 = 12.0;
    pub const DOC_TAB_PAD_R: f32 = 6.0;
}
use component as cp;

fn label(text: &str, token: TextToken) -> Node {
    Node::deco().paint(Paint::Label { text: text.into(), token, color: None, align: Align::Start, ellipsis: false }).size(Size::Hug, Size::Hug)
}

/// A toolbar row (components.json `context_bar` frame: height 34, gap 8) on `surface.strip`. Close with `b.end()`.
pub fn toolbar_begin(b: &mut Builder<'_>, id: &str, label_text: &str) {
    b.begin(
        Node::new(id, Role::Toolbar)
            .label(label_text)
            .layout(LayoutSpec {
                axis: Axis::Row,
                w: Size::Fill,
                h: Size::Fixed(size::FRAME_CONTEXT_BAR_H),
                gap: cp::CONTEXT_BAR_GAP,
                pad: [spacing::S4, 0.0, spacing::S4, 0.0],
                cross: Align::Center,
                ..LayoutSpec::default()
            })
            .paint(Paint::Fill(ColorToken::SurfaceStrip)),
    );
}

/// Text button (components.json `button`: h 24, min_w 56, pad_x 8, radius 3).
pub fn button(b: &mut Builder<'_>, id: &str, text: &str, enabled: bool) {
    b.begin(
        Node::new(id, Role::Button)
            .label(text)
            .actions(&[Action::Click, Action::Focus])
            .flags(|f| f.disabled = !enabled)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fixed(size::BUTTON_H), pad: [size::TAB_PAD_X, 0.0, size::TAB_PAD_X, 0.0], main: Align::Center, min_w: size::BUTTON_MIN_W, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Control, radius: radius::R1_CONTROL }),
    );
    b.leaf(label(text, TextToken::Body).size(Size::Hug, Size::Fill));
    b.end();
}

/// Icon button (components.json `icon_button`: box 24, icon 14, ghost style). `name` is the accessible name.
pub fn icon_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, enabled: bool) {
    b.begin(
        Node::new(id, Role::Button)
            .label(name)
            .actions(&[Action::Click, Action::Focus])
            .flags(|f| f.disabled = !enabled)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(size::BUTTON_ICON), h: Size::Fixed(size::BUTTON_ICON), main: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R1_CONTROL }),
    );
    b.leaf(Node::deco().paint(Paint::Icon { icon, size: cp::BUTTON_ICON }).size(Size::Fixed(cp::BUTTON_ICON), Size::Fixed(cp::BUTTON_ICON)));
    b.end();
}

/// Toggle button (components.json `toggle_button`: h 24, pad_x 8, ghost; on = surface.row_selected).
pub fn toggle_button(b: &mut Builder<'_>, id: &str, text: &str, on: bool) {
    b.begin(
        Node::new(id, Role::Button)
            .label(text)
            .value(if on { "on" } else { "off" })
            .actions(&[Action::Click, Action::Focus])
            .flags(|f| f.toggled = on)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fixed(size::BUTTON_H), pad: [size::TAB_PAD_X, 0.0, size::TAB_PAD_X, 0.0], ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R1_CONTROL }),
    );
    b.leaf(label(text, TextToken::Body).size(Size::Hug, Size::Fill));
    b.end();
}

/// A 1 x 20 zone divider (components.json `context_bar.size.zone_divider`).
pub fn divider(b: &mut Builder<'_>) {
    b.leaf(Node::deco().paint(Paint::Divider).size(Size::Fixed(1.0), Size::Fixed(size::BUTTON_H - spacing::S2)));
}

/// Flexible space in a row.
pub fn spacer(b: &mut Builder<'_>) {
    b.leaf(Node::deco().size(Size::Fill, Size::Fixed(0.0)));
}

/// Group header with panel tabs (components.json `group_header` h 26 on surface.strip, `panel_tab` pad_x 8).
/// `tabs` are (author_id, label); tab `active` is selected.
pub fn tab_row(b: &mut Builder<'_>, id: &str, tabs: &[(&str, &str)], active: usize) {
    b.begin(
        Node::new(id, Role::TabList)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(size::DOCK_GROUP_HEADER_H), cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfaceStrip)),
    );
    for (i, (tid, text)) in tabs.iter().enumerate() {
        b.begin(
            Node::new(*tid, Role::Tab)
                .label(*text)
                .actions(&[Action::Click, Action::Focus])
                .flags(|f| f.selected = i == active)
                .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, pad: [size::TAB_PAD_X, 0.0, size::TAB_PAD_X, 0.0], ..LayoutSpec::default() })
                .paint(Paint::Surface { kind: StyleKind::Tab, radius: radius::R0_DOCKED }),
        );
        b.leaf(label(text, TextToken::Tab).size(Size::Hug, Size::Fill));
        b.end();
    }
    spacer(b);
    b.end();
}

/// Slider (components.json `slider`): row 1 label left, ScrubValue field (72 x 24) right; row 2 the rail. Pitch 44.
#[allow(clippy::too_many_arguments)]
pub fn slider(b: &mut Builder<'_>, id: &str, text: &str, value: f64, min: f64, max: f64, soft: (f64, f64), shown: &str, unit: Option<&str>) {
    let span = (max - min).abs().max(f64::MIN_POSITIVE);
    let frac = (((value - min) / span) as f32).clamp(0.0, 1.0);
    let sf = ((((soft.0 - min) / span) as f32).clamp(0.0, 1.0), (((soft.1 - min) / span) as f32).clamp(0.0, 1.0));
    let num = Numeric { value, min, max, step: 1.0 };
    b.begin(
        Node::new(id, Role::Group)
            .label(text)
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fixed(cp::SLIDER_ROW_PITCH), pad: [size::DOCK_PANEL_PAD_L, 0.0, size::DOCK_PANEL_PAD_R, 0.0], ..LayoutSpec::default() }),
    );
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(size::FIELD_H), ..LayoutSpec::default() }));
    b.leaf(label(text, TextToken::Body).paint(Paint::Label { text: text.into(), token: TextToken::Body, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    let mut field = Node::new(format!("{id}.field"), Role::SpinButton)
        .label(text)
        .value(shown)
        .actions(&[Action::Focus, Action::Increment, Action::Decrement, Action::SetValue])
        .size(Size::Fixed(cp::SLIDER_FIELD_W), Size::Fixed(size::FIELD_H))
        .paint(Paint::Surface { kind: StyleKind::Field, radius: radius::R2_FIELD });
    field.numeric = Some(num);
    b.begin(field);
    b.leaf(Node::deco().paint(Paint::Value { text: shown.into(), unit: unit.map(str::to_owned) }).size(Size::Fill, Size::Fill));
    b.end();
    b.end();
    let mut rail = Node::new(format!("{id}.rail"), Role::Slider)
        .label(text)
        .value(shown)
        .actions(&[Action::Increment, Action::Decrement, Action::SetValue])
        .size(Size::Fill, Size::Fixed(size::FIELD_H - spacing::S2))
        .paint(Paint::Rail { frac, soft: sf });
    rail.numeric = Some(num);
    // The field and the rail are one tab stop (components.json slider keyboard).
    rail.flags.focusable = false;
    b.leaf(rail);
    b.end();
}

/// Layer row (components.json `layer_row`: h 32, tag edge 3, eye 22, indent 16 per depth, thumb 24, name, badges 12).
#[allow(clippy::too_many_arguments)]
pub fn layer_row(b: &mut Builder<'_>, id: &str, name: &str, tag: ColorToken, visible: bool, selected: bool, depth: u8, thumb: Option<ImageId>, eye: IconId, badges: &[IconId]) {
    b.begin(
        Node::new(id, Role::TreeItem)
            .label(name)
            .value(if visible { "visible" } else { "hidden" })
            .actions(&[Action::Click, Action::Focus])
            .flags(|f| f.selected = selected)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(size::ROW_LAYER_H), gap: spacing::S2, pad: [0.0, 0.0, spacing::S4, 0.0], ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Row, radius: radius::R0_DOCKED }),
    );
    b.leaf(Node::deco().paint(Paint::TagEdge(tag)).size(Size::Fixed(cp::LAYER_TAG_EDGE), Size::Fill));
    b.begin(
        Node::new(format!("{id}.visibility"), Role::CheckBox)
            .label("Visibility")
            .value(if visible { "on" } else { "off" })
            .actions(&[Action::Click])
            .flags(|f| f.toggled = visible)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(cp::LAYER_EYE), h: Size::Fixed(cp::LAYER_EYE), main: Align::Center, ..LayoutSpec::default() }),
    );
    b.leaf(Node::deco().paint(Paint::Icon { icon: eye, size: cp::BUTTON_ICON }).size(Size::Fixed(cp::BUTTON_ICON), Size::Fixed(cp::BUTTON_ICON)));
    b.end();
    if depth > 0 {
        // The spacer brings one more row gap, so it is that much narrower: the thumbnail moves 16 px per level.
        b.leaf(Node::deco().size(Size::Fixed((size::ROW_TREE_INDENT * f32::from(depth) - spacing::S2).max(0.0)), Size::Fixed(0.0)));
    }
    b.leaf(Node::new(format!("{id}.thumbnail"), Role::Image).label(name).paint(Paint::Thumbnail(thumb)).size(Size::Fixed(size::ROW_LAYER_THUMB), Size::Fixed(size::ROW_LAYER_THUMB)));
    let name_color = if visible { None } else { Some(ColorToken::TextDisabled) };
    b.leaf(Node::deco().paint(Paint::Label { text: name.into(), token: TextToken::Body, color: name_color, align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    for badge in badges {
        // Badges are decorative: their meaning is in the row's description, and an index is never an id (STU-MDL-101).
        b.leaf(Node::deco().paint(Paint::Icon { icon: *badge, size: cp::LAYER_BADGE }).size(Size::Fixed(cp::LAYER_BADGE), Size::Fixed(cp::LAYER_BADGE)));
    }
    b.end();
}
