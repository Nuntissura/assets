//! First widgets, from components.json records: toolbar with icon, toggle and text buttons; tab row (group header and
//! panel tabs); slider (the wide presentation of ScrubValue); layer row. Each widget builds nodes; sizes and colours are
//! tokens. Callers pass full author_ids (STU-MDL-100); sub-elements derive `<id>.<part>`.
use crate::display::{IconId, ImageId};
use crate::tokens::{ColorToken, State, StyleKind, TextToken, radius, size, spacing, state_style};
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
    b.leaf(Node::deco().paint(Paint::Label { text: name.into(), token: TextToken::Body, weight: 0, color: name_color, align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fill));
    for badge in badges {
        // Badges are decorative: their meaning is in the row's description, and an index is never an id (STU-MDL-101).
        b.leaf(Node::deco().paint(Paint::Icon { icon: *badge, size: cp::LAYER_BADGE }).size(Size::Fixed(cp::LAYER_BADGE), Size::Fixed(cp::LAYER_BADGE)));
    }
    b.end();
}

// ---- Frame widgets (SMB-GUI-FRAME-1, build-order B5 static faces). Ids, labels, states and reasons come from the
// caller; no widget names a panel, tool or menu title. `reason` is B6 "disabled with a reason": None = enabled.

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

/// Menu titles (components.json `menu_bar`), into the caller's MenuBar: MenuItem titles in `menu_title` style with `pad` per side,
/// never ellipsized (AC-TEXT-01); hidden titles keep a zero box; `chevron` (id, value, icon) follows when some are
/// hidden.
pub fn menu_title_row(b: &mut Builder<'_>, titles: &[MenuTitle<'_>], pad: f32, chevron: Option<(&str, &str, IconId)>, reason: Option<&str>) {
    let _ = (b, titles, pad, chevron, reason);
}

/// A document tab (components.json `document_tab`): Tab surface `w` wide (96 to 220), pad 12/6, the name (already fit
/// by the caller, full name in the label), the 6 px unsaved dot, close 20, right seam.
#[allow(clippy::too_many_arguments)]
pub fn document_tab(b: &mut Builder<'_>, id: &str, full: &str, shown: &str, w: f32, active: bool, unsaved: bool, close: (&str, IconId), reason: Option<&str>) {
    let _ = (b, id, full, shown, w, active, unsaved, close, reason);
}

/// Search field (components.json `search_field`, 240 x 24): field face, search icon, placeholder in text.tertiary,
/// the chord chip.
pub fn search_field(b: &mut Builder<'_>, id: &str, label: &str, placeholder: &str, chord: &str, icon: IconId, reason: Option<&str>) {
    let _ = (b, id, label, placeholder, chord, icon, reason);
}

/// Key chip (components.json `key_chip`: 16 high, pad 4, radius 3, surface.control, caption in text.secondary).
pub fn key_chip(b: &mut Builder<'_>, text: &str) {
    let _ = (b, text);
}

/// Presence chip (components.json `presence_chip`, d 18): `color` circle with the initial in text.on_accent (never
/// colour alone: the label names who). An indicator: it never pops anything and takes no action.
pub fn presence_chip(b: &mut Builder<'_>, id: &str, name: &str, initial: &str, color: ColorToken) {
    let _ = (b, id, name, initial, color);
}

/// Identity chip of the Context Bar's Tool Zone: tool icon 20 and the tool name at 600.
pub fn context_chip(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    let _ = (b, id, name, icon, reason);
}

/// A static dropdown face (ComboBox): `field` gives the field face (Context Bar), otherwise a ghost face (tab strip);
/// the text, then the chevron; `w` fixes the width.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_face(b: &mut Builder<'_>, id: &str, label: &str, text: &str, chevron: IconId, field: bool, w: Option<f32>, reason: Option<&str>) {
    let _ = (b, id, label, text, chevron, field, w, reason);
}

/// Tool slot (components.json `tool_slot`: 36 x `h` (32, or 28 under FIT-R1), icon 20, radius 3, ToolSlot states; the
/// 4 px variant marker when the group has more than one tool).
#[allow(clippy::too_many_arguments)]
pub fn tool_slot(b: &mut Builder<'_>, id: &str, name: &str, value: &str, icon: IconId, h: f32, active: bool, marker: Option<IconId>, reason: Option<&str>) {
    let _ = (b, id, name, value, icon, h, active, marker, reason);
}

/// Icon rail button (components.json `icon_rail`: 32 x 32, icon 16).
pub fn icon_rail_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    let _ = (b, id, name, icon, reason);
}

/// A labelled button of the bottom icon rail (h 24, icon 14, caption).
pub fn rail_caption_button(b: &mut Builder<'_>, id: &str, name: &str, icon: IconId, reason: Option<&str>) {
    let _ = (b, id, name, icon, reason);
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
    /// The reason the disclosure, +N and panel menu are disabled.
    pub reason: Option<&'a str>,
}
pub fn group_header(b: &mut Builder<'_>, h: &GroupHeader<'_>) {
    let _ = (b, h.id);
}

/// Status readouts (components.json `status_bar`: gap 12, 1 x 14 separators in edge.subtle); (author_id, text) each, the
/// full text in the label. `last_w` fixes the last readout's width, which then ellipsizes.
pub fn status_readouts(b: &mut Builder<'_>, items: &[(&str, &str)], last_w: Option<f32>) {
    let _ = (b, items, last_w);
}

/// A ruler (components.json `ruler`, thickness 20) along `axis` (Row = horizontal) on surface.strip, document 0 at the
/// logical position `origin`, step 20, major every 5; `labels` false for the vertical ruler (no rotated text).
pub fn ruler(b: &mut Builder<'_>, id: &str, label: &str, axis: Axis, origin: f32, labels: bool) {
    let _ = (b, id, label, axis, origin, labels);
}
