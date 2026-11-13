//! First widgets, from components.json records: toolbar with icon, toggle and text buttons; tab row (group header and
//! panel tabs); slider (the wide presentation of ScrubValue); layer row. Each widget builds nodes; sizes and colours are
//! tokens. Callers pass full author_ids (STU-MDL-100); sub-elements derive `<id>.<part>`.
use crate::display::{IconId, ImageId};
use crate::tokens::{ColorToken, State, StyleKind, TextToken, radius, size, spacing, state_style};
use crate::text::MAX_LABEL_BYTES;
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
    weighted(text, token, 0)
}
fn weighted(text: &str, token: TextToken, weight: u16) -> Node {
    Node::deco().paint(Paint::Label { text: text.into(), token, weight, color: None, align: Align::Start, ellipsis: false }).size(Size::Hug, Size::Hug)
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
        // The active tab's weight (state table tab.active, 600) is resolved here, so layout measures what emit draws.
        let weight = if i == active { state_style(StyleKind::Tab, State::Active).map_or(0, |s| s.weight) } else { 0 };
        b.leaf(weighted(text, TextToken::Tab, weight).size(Size::Hug, Size::Fill));
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
    b.leaf(label(text, TextToken::Body).paint(Paint::Label { text: text.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
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

/// Passive scrollbar (components.json `scrollbar`: lane 10, thumb 6, min_thumb 24, rest fill surface.control_hover).
/// The lane is always reserved; the thumb and the identified node exist only when the content overflows
/// (`total > shown`: "absent when content fits"). `first`, `shown` and `total` are in the caller's unit (rows or px);
/// `lane_h` is the lane height in logical px. Passive: no actions (dragging and the Page keys belong to the owning
/// view). The thumb is square: radius 3 needs a rounded flat fill, which [`Paint`] has no variant for yet.
pub fn scrollbar(b: &mut Builder<'_>, id: &str, label_text: &str, lane_h: f32, first: usize, shown: usize, total: usize) {
    let lane = LayoutSpec { axis: Axis::Column, w: Size::Fixed(size::DOCK_SCROLL_LANE_W), h: Size::Fill, cross: Align::Center, ..LayoutSpec::default() };
    if total <= shown || !lane_h.is_finite() || lane_h <= 0.0 {
        b.leaf(Node::deco().layout(lane));
        return;
    }
    let range = total - shown;
    let first = first.min(range);
    let thumb = (lane_h * shown as f32 / total as f32).max(size::SCROLLBAR_MIN_THUMB).min(lane_h);
    let top = ((lane_h - thumb) * first as f32 / range as f32).round();
    let mut node = Node::new(id, Role::Slider).label(label_text).value(format!("{} to {} of {}", first + 1, first + shown, total)).layout(lane);
    node.numeric = Some(Numeric { value: first as f64, min: 0.0, max: range as f64, step: 1.0 });
    b.begin(node);
    b.leaf(Node::deco().size(Size::Fixed(size::SCROLLBAR_THUMB_W), Size::Fixed(top)));
    b.leaf(Node::deco().paint(Paint::Fill(ColorToken::SurfaceControlHover)).size(Size::Fixed(size::SCROLLBAR_THUMB_W), Size::Fixed(thumb)));
    b.end();
}

/// Layer row (components.json `layer_row`: h 32, tag edge 3, eye 22, indent 16 per depth, thumb 24, name, badges 12).
#[allow(clippy::too_many_arguments)]
pub fn layer_row(b: &mut Builder<'_>, id: &str, name: &str, tag: ColorToken, visible: bool, selected: bool, depth: u8, thumb: Option<ImageId>, eye: IconId, badges: &[IconId]) {
    layer_row_tinted(b, id, name, tag, visible, selected, depth, thumb, eye, badges, None);
}

/// [`layer_row`] with the visibility icon in a fixed token when `eye_tint` is set (the mock's text.tertiary eye, D-F10);
/// None draws exactly what [`layer_row`] draws.
#[allow(clippy::too_many_arguments)]
pub fn layer_row_tinted(b: &mut Builder<'_>, id: &str, name: &str, tag: ColorToken, visible: bool, selected: bool, depth: u8, thumb: Option<ImageId>, eye: IconId, badges: &[IconId], eye_tint: Option<ColorToken>) {
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
    let eye_paint = match eye_tint {
        Some(color) => Paint::TintedIcon { icon: eye, size: cp::BUTTON_ICON, color },
        None => Paint::Icon { icon: eye, size: cp::BUTTON_ICON },
    };
    b.leaf(Node::deco().paint(eye_paint).size(Size::Fixed(cp::BUTTON_ICON), Size::Fixed(cp::BUTTON_ICON)));
    b.end();
    if depth > 0 {
        // The spacer brings one more row gap, so it is that much narrower: the thumbnail moves 16 px per level.
        b.leaf(Node::deco().size(Size::Fixed((size::ROW_TREE_INDENT * f32::from(depth) - spacing::S2).max(0.0)), Size::Fixed(0.0)));
    }
    b.leaf(Node::new(format!("{id}.thumbnail"), Role::Image).label(name).paint(Paint::Thumbnail(thumb)).size(Size::Fixed(size::ROW_LAYER_THUMB), Size::Fixed(size::ROW_LAYER_THUMB)));
    let name_color = if visible { None } else { Some(ColorToken::TextDisabled) };
    b.leaf(Node::deco().paint(Paint::Label { text: name.into(), token: TextToken::Body, weight: 0, color: name_color, align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    for badge in badges {
        // Badges are decorative: their meaning is in the row's description, and an index is never an id (STU-MDL-101).
        b.leaf(Node::deco().paint(Paint::Icon { icon: *badge, size: cp::LAYER_BADGE }).size(Size::Fixed(cp::LAYER_BADGE), Size::Fixed(cp::LAYER_BADGE)));
    }
    b.end();
}

// ---- Frame widgets (SMB-GUI-FRAME-1, build-order B5 static faces). Ids, labels, states and reasons come from the
// caller; no widget names a panel, tool or menu title. `reason` is availability as data (Node::reason): the control
// keeps its normal look and answers a click; None = built.

/// Frame component sizes components.json records (logical px).
pub mod frame {
    /// components.json `menu_bar.anatomy`: module switch button 32 wide.
    pub const MODULE_SWITCH_W: f32 = 32.0;
    /// `menu_bar.size.title_pad_x_tight`.
    pub const MENU_TITLE_PAD_TIGHT: f32 = 5.0;
    /// `presence_chip.size.d`.
    pub const PRESENCE_D: f32 = 18.0;
    /// `key_chip.size`.
    pub const KEY_CHIP_H: f32 = 16.0;
    pub const KEY_CHIP_PAD: f32 = 4.0;
    /// `search_field.size.w`.
    pub const SEARCH_W: f32 = 240.0;
    /// `document_tab.size.close` and the 6 px unsaved dot.
    pub const DOC_TAB_CLOSE: f32 = 20.0;
    pub const DOC_TAB_DOT: f32 = 6.0;
    /// `tool_slot.size`.
    pub const TOOL_SLOT_ICON: f32 = 20.0;
    pub const TOOL_MARKER: f32 = 4.0;
    /// `group_header.size`.
    pub const GROUP_DISCLOSURE: f32 = 20.0;
    pub const GROUP_MENU: f32 = 20.0;
    /// `icon_rail.size`.
    pub const RAIL_BUTTON: f32 = 32.0;
    pub const RAIL_ICON: f32 = 16.0;
    /// The bottom rail's labelled buttons (h 24, icon 14) and small chrome icons (chevrons, close: 12).
    pub const RAIL_CAPTION_H: f32 = 24.0;
    pub const ICON_SMALL: f32 = 14.0;
    pub const ICON_MICRO: f32 = 12.0;
    /// `status_bar.size.gap` and its 1 x 14 separators.
    pub const STATUS_GAP: f32 = 12.0;
    pub const STATUS_SEPARATOR_H: f32 = 14.0;
    /// The rail's 20 x 1 divider under the expand button.
    pub const RAIL_DIVIDER_W: f32 = 20.0;
    /// The identity chip's tool icon.
    pub const CONTEXT_ICON: f32 = 20.0;
}

/// One menu title: `shown` false keeps it in the tree with no box (STU-SHL-012: every title stays in AccessKit).
#[derive(Clone, Copy, Debug)]
pub struct MenuTitle<'a> {
    pub id: &'a str,
    /// The text drawn (full or abbreviated).
    pub text: &'a str,
    /// The full title, the accessible name.
    pub full: &'a str,
    pub shown: bool,
}

/// A dock group header (components.json `group_header`, h 26 on surface.strip, bottom seam): disclosure 20, the shown
/// tabs (`panel_tab`, pad 8), the +N chip when tabs are hidden, flexible space, the panel menu 20.
pub struct GroupHeader<'a> {
    pub id: &'a str,
    /// (author_id, label) of every tab.
    pub tabs: &'a [(&'a str, &'a str)],
    /// Indices of the tabs shown, in order (the rest are behind +N).
    pub shown: &'a [usize],
    pub active: usize,
    /// The active tab of the focused group has the accent edge; others edge.control.
    pub focused: bool,
    pub collapsed: bool,
    /// (id, icon) of the disclosure, of the +N chip (with its value), of the panel menu.
    pub disclosure: (&'a str, IconId),
    pub more: Option<(&'a str, &'a str, IconId)>,
    pub menu: (&'a str, IconId),
    /// The reason the disclosure and the panel menu answer with (+N carries its own value: the hidden tab ids).
    pub reason: Option<&'a str>,
}

/// `s` cut at [`MAX_LABEL_BYTES`] on a char boundary (a label never shapes more).
fn cut_label(s: &str) -> &str {
    if s.len() <= MAX_LABEL_BYTES {
        return s;
    }
    let mut end = MAX_LABEL_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn icon_leaf(icon: IconId, px: f32) -> Node {
    Node::deco().paint(Paint::Icon { icon, size: px }).size(Size::Fixed(px), Size::Fixed(px))
}

fn text_leaf(text: &str, token: TextToken, weight: u16, color: Option<ColorToken>, w: Size) -> Node {
    Node::deco().paint(Paint::Label { text: cut_label(text).into(), token, weight, color, align: Align::Start, ellipsis: false }).size(w, Size::Fill)
}

const CLICK: &[Action] = &[Action::Click, Action::Focus];

/// A ghost button `w` x `h` holding one icon `px` (chevrons, close, panel menu, expand, new document). `reason` is
/// B6's: Some disables it and states the reason as its value.
#[allow(clippy::too_many_arguments)]
pub fn ghost_icon_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, w: f32, h: f32, px: f32, reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::Button)
            .label(cut_label(name))
            .actions(CLICK)
            .reason(reason)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(w), h: Size::Fixed(h), main: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R1_CONTROL }),
    );
    b.leaf(icon_leaf(icon, px));
    b.end();
}

/// Menu titles (components.json `menu_bar`), into the caller's MenuBar: MenuItem titles in `menu_title` style with
/// `pad` per side, never ellipsized (AC-TEXT-01); hidden titles keep a zero box; `chevron` (id, value, icon) follows
/// when some are hidden.
pub fn menu_title_row(b: &mut Builder<'_>, titles: &[MenuTitle<'_>], pad: f32, chevron: Option<(&str, &str, IconId)>, reason: Option<&str>) {
    for t in titles {
        let n = Node::new(t.id, Role::MenuItem).label(cut_label(t.full)).actions(CLICK).reason(reason);
        if t.shown {
            b.begin(
                n.layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, pad: [pad, 0.0, pad, 0.0], ..LayoutSpec::default() })
                    .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R0_DOCKED }),
            );
            b.leaf(text_leaf(t.text, TextToken::MenuTitle, 0, Some(ColorToken::TextPrimary), Size::Hug));
            b.end();
        } else {
            // Behind the chevron: in the tree (STU-SHL-012), no box, nothing drawn.
            b.leaf(n.size(Size::Fixed(0.0), Size::Fixed(0.0)));
        }
    }
    if let Some((id, value, icon)) = chevron {
        b.begin(
            Node::new(id, Role::Button)
                .label("More menus")
                .value(value)
                .actions(CLICK)
                .reason(reason)
                .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(frame::GROUP_MENU), h: Size::Fill, main: Align::Center, ..LayoutSpec::default() })
                .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R0_DOCKED }),
        );
        b.leaf(icon_leaf(icon, frame::ICON_MICRO));
        b.end();
    }
}

/// A document tab (components.json `document_tab`): Tab surface `w` wide (96 to 220), pad 12/6, the name (already fit
/// by the caller, full name in the label), the 6 px unsaved dot, close 20, right seam.
#[allow(clippy::too_many_arguments)]
pub fn document_tab(b: &mut Builder<'_>, id: &str, full: &str, shown: &str, w: f32, active: bool, unsaved: bool, close: (&str, IconId), reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::Tab)
            .label(cut_label(full))
            .actions(CLICK)
            .reason(reason)
            .flags(|f| f.selected = active)
            .seam(crate::display::Side::Right)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(w), h: Size::Fill, gap: spacing::S3, pad: [cp::DOC_TAB_PAD_L, 0.0, cp::DOC_TAB_PAD_R, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Tab, radius: radius::R0_DOCKED }),
    );
    let weight = if active { state_style(StyleKind::Tab, State::Active).map_or(0, |s| s.weight) } else { 0 };
    // The caller fit the name (FIT-T1 middle ellipsis): never cut again here, so AC-TEXT-01 sees any misfit.
    b.leaf(Node::deco().paint(Paint::Label { text: cut_label(shown).into(), token: TextToken::Tab, weight, color: None, align: Align::Start, ellipsis: false }).size(Size::Fill, Size::Fill));
    if unsaved {
        b.leaf(Node::deco().paint(Paint::Round { fill: Some(ColorToken::TextSecondary), radius: frame::DOC_TAB_DOT / 2.0, border: None }).size(Size::Fixed(frame::DOC_TAB_DOT), Size::Fixed(frame::DOC_TAB_DOT)));
    }
    ghost_icon_button(b, close.0, "Close document", close.1, frame::DOC_TAB_CLOSE, frame::DOC_TAB_CLOSE, frame::ICON_MICRO, reason);
    b.end();
}

/// Search field (components.json `search_field`, 240 x 24): field face, search icon, placeholder in text.tertiary,
/// the chord chip.
pub fn search_field(b: &mut Builder<'_>, id: &str, label: &str, placeholder: &str, chord: &str, icon: IconId, reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::SearchInput)
            .label(cut_label(label))
            .actions(CLICK)
            .reason(reason)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(frame::SEARCH_W), h: Size::Fixed(size::FIELD_H), gap: spacing::S3, pad: [spacing::S4, 0.0, spacing::S4, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Field, radius: radius::R2_FIELD }),
    );
    b.leaf(icon_leaf(icon, frame::ICON_SMALL));
    b.leaf(Node::deco().paint(Paint::Label { text: cut_label(placeholder).into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextTertiary), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    key_chip(b, chord);
    b.end();
}

/// Key chip (components.json `key_chip`: 16 high, pad 4, radius 3, surface.control, caption in text.secondary).
pub fn key_chip(b: &mut Builder<'_>, text: &str) {
    b.begin(
        Node::deco()
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fixed(frame::KEY_CHIP_H), pad: [frame::KEY_CHIP_PAD, 0.0, frame::KEY_CHIP_PAD, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Round { fill: Some(ColorToken::SurfaceControl), radius: radius::R1_CONTROL, border: None }),
    );
    b.leaf(text_leaf(text, TextToken::Caption, 0, Some(ColorToken::TextSecondary), Size::Hug));
    b.end();
}

/// Presence chip (components.json `presence_chip`, d 18): `color` circle with the initial in text.on_accent (never
/// colour alone: the label names who). An indicator: it never pops anything and takes no action.
pub fn presence_chip(b: &mut Builder<'_>, id: &str, name: &str, initial: &str, color: ColorToken, reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::Button)
            .label(cut_label(name))
            .actions(CLICK)
            .reason(reason)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(frame::PRESENCE_D), h: Size::Fixed(frame::PRESENCE_D), main: Align::Center, cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Round { fill: Some(color), radius: frame::PRESENCE_D / 2.0, border: None }),
    );
    let weight = TextToken::BodyStrong.style().weight;
    b.leaf(text_leaf(initial, TextToken::Caption, weight, Some(ColorToken::TextOnAccent), Size::Hug));
    b.end();
}

/// Identity chip of the Context Bar's Tool Zone: tool icon 20 and the tool name at 600.
pub fn context_chip(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::Button)
            .label(cut_label(name))
            .actions(CLICK)
            .reason(reason)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fixed(size::BUTTON_H), gap: spacing::S2, pad: [spacing::S2, 0.0, spacing::S4, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R1_CONTROL }),
    );
    b.leaf(icon_leaf(icon, frame::CONTEXT_ICON));
    b.leaf(text_leaf(name, TextToken::Body, TextToken::BodyStrong.style().weight, Some(ColorToken::TextPrimary), Size::Hug));
    b.end();
}

/// A static dropdown face (ComboBox): `field` gives the field face (Context Bar), otherwise a ghost face (tab strip);
/// the text, then the chevron; `w` fixes the width.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_face(b: &mut Builder<'_>, id: &str, label: &str, text: &str, chevron: IconId, field: bool, w: Option<f32>, reason: Option<&str>) {
    let (kind, r) = if field { (StyleKind::Field, radius::R2_FIELD) } else { (StyleKind::Ghost, radius::R1_CONTROL) };
    b.begin(
        // Disabled (B6): the value is the reason and the label names the face (`Layout preset: Photo`).
        Node::new(id, Role::ComboBox)
            .label(if reason.is_some() { format!("{}: {}", cut_label(label), cut_label(text)) } else { cut_label(label).to_owned() })
            .value(reason.unwrap_or(cut_label(text)))
            .actions(CLICK)
            .layout(LayoutSpec { axis: Axis::Row, w: w.map_or(Size::Hug, Size::Fixed), h: Size::Fixed(size::FIELD_H), gap: spacing::S2, pad: [spacing::S4, 0.0, spacing::S2, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind, radius: r }),
    );
    b.leaf(text_leaf(text, TextToken::Body, 0, Some(ColorToken::TextPrimary), if w.is_some() { Size::Fill } else { Size::Hug }));
    b.leaf(icon_leaf(chevron, frame::ICON_MICRO));
    b.end();
}

/// Tool slot (components.json `tool_slot`: 36 x `h` (32, or 28 under FIT-R1), icon 20, radius 3, ToolSlot states; the
/// 4 px variant marker in text.tertiary when the group has more than one tool).
#[allow(clippy::too_many_arguments)]
pub fn tool_slot(b: &mut Builder<'_>, id: &str, name: &str, value: &str, icon: IconId, h: f32, active: bool, marker: Option<IconId>, reason: Option<&str>) {
    // Icon centred in the 36 wide slot at x 8..28; the marker 4 x 4 at 2 px from the bottom right corner.
    let lead = (size::RAIL_TOOL_SLOT_W - frame::TOOL_SLOT_ICON) / 2.0;
    b.begin(
        Node::new(id, Role::Button)
            .label(cut_label(name))
            .value(cut_label(value))
            .actions(CLICK)
            .reason(reason)
            .flags(|f| f.toggled = active)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(size::RAIL_TOOL_SLOT_W), h: Size::Fixed(h), gap: spacing::S1, pad: [lead, 0.0, spacing::S1, 0.0], cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::ToolSlot, radius: radius::R1_CONTROL }),
    );
    b.leaf(Node::deco().paint(Paint::Icon { icon, size: frame::TOOL_SLOT_ICON }).size(Size::Fixed(frame::TOOL_SLOT_ICON), Size::Fill));
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(frame::TOOL_MARKER), h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
    if let Some(m) = marker {
        b.leaf(Node::deco().size(Size::Fixed(frame::TOOL_MARKER), Size::Fill));
        b.leaf(Node::deco().paint(Paint::TintedIcon { icon: m, size: frame::TOOL_MARKER, color: ColorToken::TextTertiary }).size(Size::Fixed(frame::TOOL_MARKER), Size::Fixed(frame::TOOL_MARKER)));
        b.leaf(Node::deco().size(Size::Fixed(frame::TOOL_MARKER), Size::Fixed(spacing::S1)));
    }
    b.end();
    b.end();
}

/// Icon rail button (components.json `icon_rail`: 32 x 32, icon 16).
pub fn icon_rail_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    ghost_icon_button(b, id, name, icon, frame::RAIL_BUTTON, frame::RAIL_BUTTON, frame::RAIL_ICON, reason);
}

/// A labelled button of the bottom icon rail (h 24, icon 14, caption).
pub fn rail_caption_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    b.begin(
        Node::new(id, Role::Button)
            .label(cut_label(name))
            .actions(CLICK)
            .reason(reason)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fixed(frame::RAIL_CAPTION_H), gap: spacing::S2, pad: [spacing::S3, 0.0, spacing::S3, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R1_CONTROL }),
    );
    b.leaf(icon_leaf(icon, frame::ICON_SMALL));
    b.leaf(text_leaf(name, TextToken::Caption, 0, None, Size::Hug));
    b.end();
}

/// Builds a [`GroupHeader`]; hidden tabs stay in the tree with no box.
pub fn group_header(b: &mut Builder<'_>, h: &GroupHeader<'_>) {
    b.begin(
        Node::new(h.id, Role::TabList)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(size::DOCK_GROUP_HEADER_H), cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfaceStrip))
            .seam(crate::display::Side::Bottom),
    );
    ghost_icon_button(b, h.disclosure.0, if h.collapsed { "Expand group" } else { "Collapse group" }, h.disclosure.1, frame::GROUP_DISCLOSURE, size::DOCK_GROUP_HEADER_H, frame::ICON_MICRO, h.reason);
    let active_weight = state_style(StyleKind::Tab, State::Active).map_or(0, |s| s.weight);
    for (i, (tid, text)) in h.tabs.iter().enumerate() {
        let on = i == h.active;
        let n = Node::new(*tid, Role::Tab).label(cut_label(text)).actions(CLICK).flags(|f| {
            f.selected = on;
            f.unfocused_group = !h.focused;
        });
        if h.shown.contains(&i) {
            b.begin(
                n.layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, pad: [size::TAB_PAD_X, 0.0, size::TAB_PAD_X, 0.0], ..LayoutSpec::default() })
                    .paint(Paint::Surface { kind: StyleKind::Tab, radius: radius::R0_DOCKED }),
            );
            b.leaf(text_leaf(text, TextToken::Tab, if on { active_weight } else { 0 }, None, Size::Hug));
            b.end();
        } else {
            // Behind +N: in the tree, no box.
            b.leaf(n.size(Size::Fixed(0.0), Size::Fixed(0.0)));
        }
    }
    if let Some((id, value, icon)) = h.more {
        let count = h.tabs.len().saturating_sub(h.shown.len());
        b.begin(
            Node::new(id, Role::Button)
                .label("More tabs")
                .value(value)
                .actions(CLICK)
                .layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, gap: 1.0, pad: [spacing::S2, 0.0, spacing::S2, 0.0], cross: Align::Center, ..LayoutSpec::default() })
                .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R0_DOCKED }),
        );
        b.leaf(text_leaf(&format!("+{count}"), TextToken::Caption, 0, Some(ColorToken::TextTertiary), Size::Hug));
        b.leaf(icon_leaf(icon, 10.0));
        b.end();
    }
    spacer(b);
    ghost_icon_button(b, h.menu.0, "Panel menu", h.menu.1, frame::GROUP_MENU, size::DOCK_GROUP_HEADER_H, frame::ICON_MICRO, h.reason);
    b.end();
}

/// Status readouts (components.json `status_bar`: gap 12, 1 x 14 separators in edge.subtle); (author_id, text) each,
/// the full text in the label. `last_w` fixes the last readout's width, which then ellipsizes.
pub fn status_readouts(b: &mut Builder<'_>, items: &[(&str, &str)], last_w: Option<f32>) {
    for (i, (id, text)) in items.iter().enumerate() {
        if i > 0 {
            b.leaf(Node::deco().paint(Paint::Fill(ColorToken::EdgeSubtle)).size(Size::Fixed(1.0), Size::Fixed(frame::STATUS_SEPARATOR_H)));
        }
        let last = i + 1 == items.len();
        let fixed = last_w.filter(|_| last);
        b.leaf(
            Node::new(*id, Role::Label)
                .label(cut_label(text))
                .paint(Paint::Label { text: cut_label(text).into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: fixed.is_some() })
                .size(fixed.map_or(Size::Hug, |w| Size::Fixed(w.max(0.0))), Size::Fixed(TextToken::Body.style().line)),
        );
    }
}

/// A ruler (components.json `ruler`, thickness 20) along `axis` (Row = horizontal) on surface.strip, document 0 at the
/// logical position `origin`, step 20, major every 5; `labels` false for the vertical ruler (no rotated text).
pub fn ruler(b: &mut Builder<'_>, id: &str, label: &str, axis: Axis, origin: f32, labels: bool) {
    let t = size::RULER_THICKNESS;
    let (w, h, seam) = match axis {
        Axis::Row => (Size::Fill, Size::Fixed(t), crate::display::Side::Bottom),
        Axis::Column => (Size::Fixed(t), Size::Fill, crate::display::Side::Right),
    };
    b.leaf(Node::new(id, Role::Group).label(cut_label(label)).paint(Paint::Ruler { axis, origin, step: RULER_STEP, major: RULER_MAJOR_EVERY, labels }).seam(seam).size(w, h));
}

/// components.json `ruler`: a tick every 20 logical px, a major tick every 5 (minor 5 long, major 10).
pub const RULER_STEP: f32 = 20.0;
pub const RULER_MAJOR_EVERY: u32 = 5;

// ---- B6 widgets (SMB-GUI-MENU-1): menu popup and menu item (components.json `menu_popup`, `menu_item`,
// `context_menu`) and the L0 and L1 tooltips (`tooltip_l0`, `tooltip_l1`). Ids, labels, chords and availability come
// from the caller's descriptors; no widget names a menu, command or reason.

/// components.json `menu_popup` and tooltip sizes design-system.json `size` does not record (logical px).
pub mod popup {
    /// `menu_popup.size.pad_y`, the reason line under a greyed item (caption line), the label's right pad.
    pub const PAD_Y: f32 = 4.0;
    pub const REASON_LINE: f32 = 14.0;
    pub const PAD_R: f32 = 8.0;
    /// Room between a label and its right-aligned chord, and the submenu chevron.
    pub const CHORD_GAP: f32 = 16.0;
    pub const CHEVRON: f32 = 12.0;
    /// `tooltip_l0.size`: h 24, pad_x 8; `tooltip_l1.size.pad`: 8 10.
    pub const TIP_L0_H: f32 = 24.0;
    pub const TIP_PAD_X: f32 = 8.0;
    pub const TIP_L1_PAD_Y: f32 = 8.0;
    pub const TIP_L1_PAD_X: f32 = 10.0;
}

/// The height of a menu row: 24, plus the reason line when greyed.
pub fn menu_row_h(greyed: bool) -> f32 {
    size::MENU_ITEM_H + if greyed { popup::REASON_LINE } else { 0.0 }
}

/// One row of a menu popup.
#[derive(Clone, Debug)]
pub struct MenuRow<'a> {
    /// `menu.<command id>` (MENU-INV-5) or the remedy sibling's id.
    pub id: &'a str,
    pub label: &'a str,
    /// The chord resolved at render (STU-SHL-242), right-aligned in text.tertiary.
    pub chord: Option<&'a str>,
    /// The reason line under a greyed item (STU-SHL-018: greyed, reason shown, never hidden); None: enabled.
    pub reason: Option<&'a str>,
    /// The node's availability as node properties (STU-SHL-019).
    pub availability: Option<crate::tree::NodeAvailability>,
    /// The node value: the reason line of a greyed node (the AccessKit description) or None.
    pub value: Option<&'a str>,
    /// A submenu: the chevron, and `open` while its cascade shows.
    pub submenu: bool,
    pub open: bool,
    /// Hover or keyboard highlight (surface.row_selected with the accent edge).
    pub hot: bool,
    /// A toggle that is on (MenuItemCheckBox).
    pub checked: bool,
    /// False: scrolled out of the popup; in the tree with no box.
    pub shown: bool,
    /// The remedy sibling: an enabled row whose label is in accent.text.
    pub remedy: bool,
}

/// Opens a popup (components.json `menu_popup`: role Menu, surface.raised, radius 6, 1 px edge.subtle, pad_y 4,
/// clipped) of `w` x `h` logical px. Close with `b.end()`.
pub fn menu_popup_begin(b: &mut Builder<'_>, id: &str, label: &str, w: f32, h: f32) {
    b.begin(
        Node::new(id, Role::Menu)
            .label(cut_label(label))
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(w), h: Size::Fixed(h), pad: [0.0, popup::PAD_Y, 0.0, popup::PAD_Y], cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Round { fill: Some(ColorToken::SurfaceRaised), radius: radius::R3_POPUP, border: Some(ColorToken::EdgeSubtle) })
            .flags(|f| f.clip = true),
    );
}

/// One menu row (components.json `menu_item`, h 24): icon gutter 24 (a check mark when on), the label, the chord,
/// the submenu chevron; a greyed row is disabled and carries its reason line under the label in caption style.
pub fn menu_item(b: &mut Builder<'_>, r: &MenuRow<'_>, check: IconId, chevron: IconId) {
    let greyed = r.reason.is_some();
    let mut n = Node::new(r.id, Role::MenuItem).label(cut_label(r.label)).actions(CLICK).availability(r.availability.clone()).flags(|f| {
        f.disabled = greyed;
        f.selected = r.hot || r.open;
        f.toggled = r.checked;
    });
    if let Some(v) = r.value {
        n = n.value(cut_label(v));
    }
    if !r.shown {
        // Scrolled out: in the tree (STU-SHL-018 never hides), no box.
        b.leaf(n.size(Size::Fixed(0.0), Size::Fixed(0.0)));
        return;
    }
    b.begin(n.layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(menu_row_h(greyed)), cross: Align::Start, ..LayoutSpec::default() }).paint(Paint::Surface { kind: StyleKind::Row, radius: radius::R0_DOCKED }));
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(size::MENU_GUTTER_ICON), h: Size::Fixed(size::MENU_ITEM_H), main: Align::Center, cross: Align::Center, ..LayoutSpec::default() }));
    if r.checked {
        b.leaf(icon_leaf(check, frame::ICON_MICRO));
    }
    b.end();
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(size::MENU_ITEM_H), gap: popup::CHORD_GAP, pad: [0.0, 0.0, popup::PAD_R, 0.0], cross: Align::Center, ..LayoutSpec::default() }));
    let color = if r.remedy { Some(ColorToken::AccentText) } else { None };
    b.leaf(Node::deco().paint(Paint::Label { text: cut_label(r.label).into(), token: TextToken::Body, weight: 0, color, align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    if let Some(c) = r.chord {
        b.leaf(text_leaf(c, TextToken::Body, 0, Some(ColorToken::TextTertiary), Size::Hug));
    }
    if r.submenu {
        b.leaf(icon_leaf(chevron, popup::CHEVRON));
    }
    b.end();
    if let Some(reason) = r.reason {
        b.leaf(
            Node::deco()
                .paint(Paint::Label { text: cut_label(reason).into(), token: TextToken::Caption, weight: 0, color: Some(ColorToken::TextTertiary), align: Align::Start, ellipsis: true })
                .size(Size::Fill, Size::Fixed(popup::REASON_LINE)),
        );
    }
    b.end();
    b.end();
}

/// A tooltip (components.json `tooltip_l0` and `tooltip_l1`, role Tooltip, author id `studio.tooltip`).
#[derive(Clone, Debug)]
pub struct Tip<'a> {
    pub id: &'a str,
    /// 0: L0 (one line: name and chord); 1: L1.
    pub level: u8,
    pub name: &'a str,
    pub chord: Option<&'a str>,
    /// L1: the one-sentence summary, absent when null (STU-SHL-232 item 2).
    pub summary: Option<&'a str>,
    /// L1 on a dimmed element: the reason line (item 4).
    pub reason: Option<&'a str>,
    /// L1 on an INAPPLICABLE_HERE element: (id, label) of the one-click remedy button.
    pub remedy: Option<(&'a str, &'a str)>,
    /// The node value: the same reason line the element carries (AC-REACH-05 compares them).
    pub value: Option<&'a str>,
}

/// The lines a tip shows, in [`tooltip`]'s order (name line, then on L1 the summary, reason and remedy).
pub fn tip_lines(t: &Tip<'_>) -> usize {
    if t.level == 0 {
        return 1;
    }
    1 + usize::from(t.summary.is_some()) + usize::from(t.reason.is_some()) + usize::from(t.remedy.is_some())
}

/// Builds a tooltip box `w` x `h`: surface.raised, radius 6, edge.subtle; L0 one line (name, chord in caption
/// text.tertiary); L1 the name line, the summary, the reason in text.secondary, the remedy as a button in accent.text.
pub fn tooltip(b: &mut Builder<'_>, t: &Tip<'_>, w: f32, h: f32) {
    let (pad_x, pad_y) = if t.level == 0 { (popup::TIP_PAD_X, 0.0) } else { (popup::TIP_L1_PAD_X, popup::TIP_L1_PAD_Y) };
    let mut n = Node::new(t.id, Role::Tooltip)
        .label(cut_label(t.name))
        .layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(w), h: Size::Fixed(h), pad: [pad_x, pad_y, pad_x, pad_y], cross: Align::Start, main: Align::Center, ..LayoutSpec::default() })
        .paint(Paint::Round { fill: Some(ColorToken::SurfaceRaised), radius: radius::R3_POPUP, border: Some(ColorToken::EdgeSubtle) })
        .flags(|f| f.clip = true);
    if let Some(v) = t.value {
        n = n.value(cut_label(v));
    }
    b.begin(n);
    let line = TextToken::Body.style().line;
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(line), gap: spacing::S4, cross: Align::Center, ..LayoutSpec::default() }));
    b.leaf(Node::deco().paint(Paint::Label { text: cut_label(t.name).into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextPrimary), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    if let Some(c) = t.chord {
        b.leaf(text_leaf(c, TextToken::Caption, 0, Some(ColorToken::TextTertiary), Size::Hug));
    }
    b.end();
    if t.level > 0 {
        let body = |b: &mut Builder<'_>, s: &str, color: ColorToken| {
            b.leaf(Node::deco().paint(Paint::Label { text: cut_label(s).into(), token: TextToken::Body, weight: 0, color: Some(color), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fixed(line)));
        };
        if let Some(s) = t.summary {
            body(b, s, ColorToken::TextSecondary);
        }
        if let Some(r) = t.reason {
            body(b, r, ColorToken::TextSecondary);
        }
        if let Some((id, label)) = t.remedy {
            b.begin(Node::new(id, Role::Button).label(cut_label(label)).actions(CLICK).layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(line), cross: Align::Center, ..LayoutSpec::default() }));
            b.leaf(Node::deco().paint(Paint::Label { text: cut_label(label).into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::AccentText), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
            b.end();
        }
    }
    b.end();
}

/// The B6 state of one menu title: its availability as node properties, greyed with its reason as the value
/// (STU-SHL-052: every level, titles included, greyed, never hidden), and open while its popup shows.
#[derive(Clone, Debug, Default)]
pub struct TitleState {
    pub availability: Option<crate::tree::NodeAvailability>,
    pub value: Option<String>,
    pub greyed: bool,
    pub open: bool,
}

/// [`menu_title_row`] for B6: each title carries its [`TitleState`] (`states[i]` for `titles[i]`; a missing state is
/// the default) instead of one reason for all, and the chevron opens the list of hidden titles (its value names them).
pub fn menu_title_row_states(b: &mut Builder<'_>, titles: &[MenuTitle<'_>], states: &[TitleState], pad: f32, chevron: Option<(&str, &str, IconId)>) {
    let none = TitleState::default();
    for (i, t) in titles.iter().enumerate() {
        let st = states.get(i).unwrap_or(&none);
        let mut n = Node::new(t.id, Role::MenuItem).label(cut_label(t.full)).actions(CLICK).availability(st.availability.clone()).flags(|f| {
            f.disabled = st.greyed;
            f.selected = st.open;
        });
        if let Some(v) = &st.value {
            n = n.value(cut_label(v));
        }
        if t.shown {
            b.begin(
                n.layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, pad: [pad, 0.0, pad, 0.0], ..LayoutSpec::default() })
                    .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R0_DOCKED }),
            );
            b.leaf(text_leaf(t.text, TextToken::MenuTitle, 0, Some(ColorToken::TextPrimary), Size::Hug));
            b.end();
        } else {
            b.leaf(n.size(Size::Fixed(0.0), Size::Fixed(0.0)));
        }
    }
    if let Some((id, value, icon)) = chevron {
        b.begin(
            Node::new(id, Role::Button)
                .label("More menus")
                .value(value)
                .actions(CLICK)
                .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(frame::GROUP_MENU), h: Size::Fill, main: Align::Center, ..LayoutSpec::default() })
                .paint(Paint::Surface { kind: StyleKind::Ghost, radius: radius::R0_DOCKED }),
        );
        b.leaf(icon_leaf(icon, frame::ICON_MICRO));
        b.end();
    }
}
