//! View functions of the frame bands (SMB-GUI-FRAME-1 item 6): the menu bar, the document tab strip, the Context Bar,
//! the Tool Rail, the collapsed Browse and Time & Results docks, the Inspect Dock groups, the Viewport frame with its
//! rulers, the status bar and the observability nodes; SMB-GUI-MENU-1 (build-order B6) adds the menu popups, the
//! Layers context menu and the L0 and L1 tooltips ([`popups`], [`tip`]). Pure: facts in, nodes out; every fit decision (menu padding
//! and abbreviations, tab and title ellipses, +N overflow, the rail chevron) is made before the build, with Text, by
//! the frame. Every control the slice does not build keeps its normal look and carries the reason
//! `not_implemented_yet` as data; a click answers with it (lead correction 2026-11-08: B6's greyed items are menus). Ids follow components.json and the mock (`menu.<title>`, `shell.module-switch`,
//! `studio.toolgroup.<group>`, `studio.rail.<edge>.<panel>`, `studio.group.<first panel>`, `studio.tab.<panel>`).
use super::preset::{self, BOTTOM, LEFT, RAIL};
use super::view;
use super::{DOCK_ID, STATUS_ID, VIEWER_ID};
use hsk_studio_harness_view::view::LayerRow;
use kumihimo_ui::display::{IconId, Side};
use kumihimo_ui::icons;
use kumihimo_ui::tokens::{ColorToken, TextToken};
use kumihimo_ui::geom::Rect;
use kumihimo_ui::tree::{Action, Align, Axis, Builder, LayoutSpec, Node, NodeAvailability, Paint, Role, Size};
use kumihimo_ui::widgets::{self, GroupHeader, MenuRow, MenuTitle, TitleState, Tip, frame as fw};

/// STU-SHL-051 NOT_IMPLEMENTED_YET (14:31909): the reason code of every unbuilt control.
pub const NIY: &str = "not_implemented_yet";
const REASON: Option<&str> = Some(NIY);
/// The empty state line of a panel this host does not build yet.
pub const NOT_BUILT: &str = "Not built in this host yet";

/// A panel icon by the designer's name; an unknown name is IconId(0), which the icon source reports as missing.
pub fn ui_icon(name: &str) -> IconId {
    icons::ui(name).unwrap_or(IconId(0))
}
pub fn tool_icon(name: &str) -> IconId {
    icons::tool(name).unwrap_or(IconId(0))
}

fn row(h: Size) -> LayoutSpec {
    LayoutSpec { axis: Axis::Row, w: Size::Fill, h, cross: Align::Center, ..LayoutSpec::default() }
}
fn column() -> LayoutSpec {
    LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }
}
fn gap(b: &mut Builder<'_>, w: f32, h: f32) {
    b.leaf(Node::deco().size(Size::Fixed(w), Size::Fixed(h)));
}
/// `not_implemented_yet hidden=<ids>`: the value of a chevron or +N chip.
pub fn hidden_value(ids: &[String]) -> String {
    format!("{NIY} hidden={}", ids.join(","))
}

/// The menu bar's fit decisions (FIT-M1) and the window title.
pub struct MenuFacts<'a> {
    pub pad: f32,
    pub abbreviated: bool,
    /// Titles shown before the chevron.
    pub shown: usize,
    /// The window title as drawn (middle-ellipsized) and in full.
    pub title: &'a str,
    pub title_full: &'a str,
    /// Each title's B6 state, in STU-SHL-011 order (availability, greyed, open).
    pub states: &'a [TitleState],
}

/// The menu bar's chevron: it opens the list of the titles it hides.
pub const MORE_ID: &str = "shell.menu-bar.more";

pub fn menu_title_id(title: &str) -> String {
    format!("menu.{}", title.to_ascii_lowercase())
}

/// The menu bar (surface.frame, bottom seam): the module switch with the module icon in accent.base, the 19 titles of
/// STU-SHL-011 (all in the tree, STU-SHL-012), the window title, the Operator's presence chip. No window buttons: the
/// OS frame owns them in this host (STU-SHL-004: a host stand-in of the shell's bar).
pub fn menu_bar(b: &mut Builder<'_>, m: &MenuFacts<'_>) {
    b.begin(Node::new("shell.menu-bar", Role::MenuBar).label("Menu bar").layout(row(Size::Fill)).paint(Paint::Fill(ColorToken::SurfaceFrame)).seam(Side::Bottom));
    b.begin(
        Node::new("shell.module-switch", Role::Button)
            .label("Switch module")
            .actions(&[Action::Click, Action::Focus])
            .reason(REASON)
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fixed(fw::MODULE_SWITCH_W), h: Size::Fill, main: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Surface { kind: kumihimo_ui::tokens::StyleKind::Ghost, radius: 0.0 }),
    );
    b.leaf(Node::deco().paint(Paint::TintedIcon { icon: ui_icon("module"), size: fw::RAIL_ICON, color: ColorToken::AccentBase }).size(Size::Fixed(fw::RAIL_ICON), Size::Fixed(fw::RAIL_ICON)));
    b.end();
    let ids: Vec<String> = preset::MENU.iter().map(|t| menu_title_id(t)).collect();
    let titles: Vec<MenuTitle<'_>> = preset::MENU
        .iter()
        .zip(&ids)
        .enumerate()
        .map(|(i, (t, id))| MenuTitle { id: id.as_str(), text: if m.abbreviated { preset::abbreviation(t) } else { t }, full: t, shown: i < m.shown })
        .collect();
    let hidden: Vec<String> = ids[m.shown.min(ids.len())..].to_vec();
    let value = format!("hidden={}", hidden.join(","));
    let chevron = (!hidden.is_empty()).then(|| (MORE_ID, value.as_str(), ui_icon("chev_down")));
    widgets::menu_title_row_states(b, &titles, m.states, m.pad, chevron);
    b.leaf(
        Node::new("shell.window-title", Role::Label)
            .label(m.title_full)
            .paint(Paint::Label { text: m.title.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Center, ellipsis: true })
            .size(Size::Fill, Size::Fill),
    );
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, gap: 4.0, pad: [4.0, 0.0, 8.0, 0.0], cross: Align::Center, ..LayoutSpec::default() }));
    widgets::presence_chip(b, "studio.presence.operator", "Operator", "O", ColorToken::PresenceOperator, REASON);
    b.end();
    b.end();
}

/// The tab strip's document tab.
pub struct TabFacts<'a> {
    pub full: &'a str,
    pub shown: &'a str,
    pub w: f32,
    pub unsaved: bool,
}

/// Width of the tab strip's right cluster at `caption_w` (`Layout`) and `preset_w` (the preset face).
pub fn tab_strip_cluster_w(caption_w: f32, preset_w: f32) -> f32 {
    fw::SEARCH_W + 6.0 + 1.0 + 6.0 + caption_w + 6.0 + preset_w + 6.0 + kumihimo_ui::tokens::size::BUTTON_ICON + 8.0
}
/// The new-document button and its 4 px lead.
pub const TAB_NEW_W: f32 = 4.0 + 24.0;

/// The document tab strip (surface.strip): the document tab, the new-document button, flexible space, the search
/// field, a divider, the caption `Layout`, the preset face, the panel-layout button.
pub fn tab_strip(b: &mut Builder<'_>, t: &TabFacts<'_>) {
    b.begin(Node::new("studio.tabstrip", Role::TabList).label("Documents").layout(row(Size::Fill)).paint(Paint::Fill(ColorToken::SurfaceStrip)));
    widgets::document_tab(b, "studio.tab.document", t.full, t.shown, t.w, true, t.unsaved, ("studio.tab.document.close", ui_icon("close")), REASON);
    gap(b, 4.0, 0.0);
    widgets::ghost_icon_button(b, "studio.tab.new", "New document", ui_icon("plus"), 24.0, 24.0, fw::ICON_MICRO, REASON);
    widgets::spacer(b);
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Hug, h: Size::Fill, gap: 6.0, pad: [0.0, 0.0, 8.0, 0.0], cross: Align::Center, ..LayoutSpec::default() }));
    widgets::search_field(b, "studio.search", "Search tools, commands, help", "Search tools, commands, help", "Ctrl K", ui_icon("search"), REASON);
    widgets::divider(b);
    b.leaf(Node::deco().paint(Paint::Label { text: "Layout".into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextTertiary), align: Align::Start, ellipsis: false }).size(Size::Hug, Size::Fill));
    widgets::dropdown_face(b, "studio.layout-preset.dropdown", "Layout preset", preset::LAYOUT_PRESET, ui_icon("chev_down"), false, None, REASON);
    widgets::ghost_icon_button(b, "studio.layout.panel-menu", "Panel layout", ui_icon("layout"), 24.0, 24.0, fw::ICON_SMALL, REASON);
    b.end();
    b.end();
}

/// What the Context Bar shows.
pub struct ContextFacts<'a> {
    pub tool_name: &'a str,
    pub tool_icon: IconId,
    /// `<kind> layer` of the selected row; None when nothing is selected (no placeholder).
    pub selection: Option<&'a str>,
}

/// The Context Bar (surface.panel, bottom seam): Tool Zone with the identity chip and the tool preset face, a zone
/// divider, no declared controls yet (B7), flexible space, a zone divider, the Selection Zone label.
pub fn context_bar(b: &mut Builder<'_>, c: &ContextFacts<'_>) {
    b.begin(
        Node::new("studio.options", Role::Toolbar)
            .label("Context Bar")
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, gap: widgets::component::CONTEXT_BAR_GAP, pad: [4.0, 0.0, 8.0, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfacePanel))
            .seam(Side::Bottom),
    );
    widgets::context_chip(b, "studio.options.identity", c.tool_name, c.tool_icon, REASON);
    widgets::dropdown_face(b, "studio.options.preset", "Tool preset", "Default", ui_icon("chev_down"), true, None, REASON);
    widgets::divider(b);
    widgets::spacer(b);
    widgets::divider(b);
    if let Some(s) = c.selection {
        b.leaf(
            Node::new("studio.options.selection", Role::Label)
                .label(s)
                .paint(Paint::Label { text: s.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: false })
                .size(Size::Hug, Size::Fill),
        );
    }
    b.end();
}

/// The Tool Rail's fit decisions.
pub struct RailFacts {
    pub pitch: f32,
    pub shown: usize,
    pub active: usize,
}

/// A spacer that takes the rest of a column.
fn column_spacer(b: &mut Builder<'_>) {
    b.leaf(Node::deco().size(Size::Fixed(0.0), Size::Fill));
}

/// The Tool Rail (surface.frame, right seam): the slots in presets.raster.rail order from pad 2, the FIT-R2 chevron
/// above navigate when slots are hidden (its value lists the hidden slots' author ids; they stay invocable), then the
/// foot pinned to the rail's bottom: family browse, 4, the colour proxy, 6 (mock_lib.toolrail; the proxy's chips are
/// drawn by the frame over this panel: the fill chip is document colour, not a token).
pub fn tool_rail(b: &mut Builder<'_>, r: &RailFacts) {
    b.begin(
        Node::new("studio.panel.tools", Role::Toolbar)
            .label("Tools")
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, pad: [4.0, 2.0, 4.0, 0.0], cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfaceFrame))
            .seam(Side::Right)
            .flags(|f| f.clip = true),
    );
    let n = RAIL.len();
    let shown = r.shown.min(n);
    let hidden = n - shown;
    let head = if hidden > 0 { shown.saturating_sub(1) } else { shown };
    let slot = |b: &mut Builder<'_>, i: usize| {
        let g = &RAIL[i];
        widgets::tool_slot(b, &preset::slot_id(g.group), g.name, g.face_tool, tool_icon(g.icon), r.pitch, i == r.active, g.variants.then_some(icons::VARIANT_MARKER), None);
    };
    for i in 0..head {
        slot(b, i);
    }
    if hidden > 0 {
        let ids: Vec<String> = RAIL[head..n - 1].iter().map(|g| preset::slot_id(g.group)).collect();
        let value = hidden_value(&ids);
        widgets::ghost_icon_button(b, "studio.panel.tools.more", "More tools", ui_icon("chev_down"), kumihimo_ui::tokens::size::RAIL_TOOL_SLOT_W, r.pitch, fw::ICON_MICRO, Some(value.as_str()));
        if shown > 0 {
            slot(b, n - 1);
        }
    }
    column_spacer(b);
    widgets::ghost_icon_button(b, "studio.tool.family-browse", "All tools by family", ui_icon("presets"), kumihimo_ui::tokens::size::RAIL_TOOL_SLOT_W, kumihimo_ui::tokens::size::RAIL_TOOL_SLOT_H, fw::RAIL_ICON, REASON);
    gap(b, 0.0, 4.0);
    b.leaf(
        Node::new("studio.colour.proxy", Role::Group)
            .label("Fill and stroke")
            .actions(&[Action::Click, Action::Focus])
            .reason(REASON)
            .size(Size::Fixed(kumihimo_ui::tokens::size::RAIL_COLOUR_PROXY), Size::Fixed(kumihimo_ui::tokens::size::RAIL_COLOUR_PROXY)),
    );
    gap(b, 0.0, 6.0);
    b.end();
}

/// A dock root (components.json dock_region: role Region, `studio.dock.<region>`), filling its region.
fn dock_root(b: &mut Builder<'_>, id: &str, label: &str, fill: ColorToken, seam: Side, axis: Axis) {
    b.begin(
        Node::new(id, Role::Region)
            .label(label)
            .layout(LayoutSpec { axis, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(fill))
            .seam(seam)
            .flags(|f| f.clip = true),
    );
}

/// An icon rail (components.json icon_rail: role Toolbar) inside a dock root.
fn icon_rail(b: &mut Builder<'_>, id: &str, label: &str, axis: Axis, pad: [f32; 4], gap_px: f32) {
    b.begin(
        Node::new(id, Role::Toolbar)
            .label(label)
            .layout(LayoutSpec { axis, w: Size::Fill, h: Size::Fill, gap: gap_px, pad, cross: Align::Center, ..LayoutSpec::default() }),
    );
}

/// The Browse Dock collapsed to its icon rail (surface.strip, right seam): expand, a 20 x 1 divider, one button per
/// panel of presets.raster.left.
pub fn browse_rail(b: &mut Builder<'_>) {
    dock_root(b, "studio.dock.left", "Browse Dock", ColorToken::SurfaceStrip, Side::Right, Axis::Column);
    icon_rail(b, "studio.rail.left", "Browse Dock rail", Axis::Column, [0.0; 4], 0.0);
    widgets::ghost_icon_button(b, "studio.rail.left.expand", "Expand Browse Dock", ui_icon("dbl_right"), fw::RAIL_BUTTON, fw::RAIL_BUTTON, fw::ICON_SMALL, REASON);
    b.leaf(Node::deco().paint(Paint::Fill(ColorToken::EdgeSubtle)).size(Size::Fixed(fw::RAIL_DIVIDER_W), Size::Fixed(1.0)));
    gap(b, 0.0, 4.0);
    for p in LEFT {
        widgets::icon_rail_button(b, &format!("studio.rail.left.{}", p.id), p.name, ui_icon(p.icon), REASON);
    }
    b.end();
    b.end();
}

/// The Time & Results Dock collapsed to its labelled rail (surface.strip, top seam).
pub fn time_rail(b: &mut Builder<'_>) {
    dock_root(b, "studio.dock.bottom", "Time & Results Dock", ColorToken::SurfaceStrip, Side::Top, Axis::Row);
    icon_rail(b, "studio.rail.bottom", "Time & Results Dock rail", Axis::Row, [4.0, 0.0, 4.0, 0.0], 2.0);
    for p in BOTTOM {
        widgets::rail_caption_button(b, &format!("studio.rail.bottom.{}", p.id), p.name, ui_icon(p.icon), REASON);
    }
    widgets::spacer(b);
    widgets::ghost_icon_button(b, "studio.rail.bottom.expand", "Expand Time & Results Dock", ui_icon("dbl_up"), 24.0, 24.0, fw::ICON_SMALL, REASON);
    b.end();
    b.end();
}

/// One dock group as the frame decided it.
pub struct GroupFacts<'a> {
    pub tabs: &'a [(&'a str, &'a str)],
    pub active: usize,
    pub shown: &'a [usize],
    pub collapsed: bool,
    pub focused: bool,
    /// The group's height; None fills the dock (the expanded Browse Dock's one group).
    pub h: Option<f32>,
    /// The +N list is open (D-F4).
    pub open: bool,
}

/// What the Layers body needs (the spine's panel, unchanged).
pub struct LayersFacts<'a> {
    pub rows: &'a [LayerRow],
    pub selected: Option<&'a str>,
    pub first: usize,
    pub body_h: f32,
}

/// One ScrubValue as the frame decided it (SMB-GUI-GESTURE-GATE-1): its owner-based id, label, the value it shows
/// (the in-gesture value during a gesture) rendered at the precision, the unit, and the numeric node metadata.
pub struct FieldFacts<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub text: &'a str,
    pub unit: &'a str,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// What the Properties body shows: the selected layer's opacity field; None keeps the empty state (no selection).
pub struct PropertiesFacts<'a> {
    pub opacity: Option<FieldFacts<'a>>,
}

/// The Properties body with the selected layer's Appearance (mock_raster.py properties: `.pad` 6 22 6 12, `.pr` 24 high
/// with a 96 px label and 8 px gap): the opacity ScrubValue (components.json scrub_value: 72 x 24, field style, value
/// right-aligned with the unit in text.tertiary, pad 6; on hover and while scrubbing the 2 px accent gutter on its left,
/// STU-SHL-186). Its grab rect is the node's bounds (STU-SHL-196), 12 px from the dock's left edge and clear of the 10 px
/// scroll lane on the right (STU-SHL-103, -105). The node value is the rendered string with its unit (STU-SHL-193).
fn properties_body(b: &mut Builder<'_>, f: &FieldFacts<'_>) {
    b.begin(
        Node::new("studio.panel.properties", Role::TabPanel)
            .label("Properties")
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, pad: [12.0, 6.0, 22.0, 6.0], cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfacePanel)),
    );
    b.leaf(
        Node::deco()
            .paint(Paint::Label { text: "Appearance".into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextPrimary), align: Align::Start, ellipsis: true })
            .size(Size::Fill, Size::Fixed(kumihimo_ui::tokens::size::FIELD_H)),
    );
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(kumihimo_ui::tokens::size::FIELD_H), cross: Align::Center, ..LayoutSpec::default() }));
    b.leaf(
        Node::deco()
            .paint(Paint::Label { text: f.label.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: true })
            .size(Size::Fixed(96.0), Size::Fill),
    );
    gap(b, 8.0, 0.0);
    let hot = {
        let ui = b.interaction();
        ui.hovered.as_deref() == Some(f.id) || ui.pressed.as_deref() == Some(f.id)
    };
    let mut field = Node::new(f.id, Role::Slider)
        .label(f.label)
        .value(format!("{}{}", f.text, f.unit))
        .actions(&[Action::Focus, Action::Increment, Action::Decrement, Action::SetValue])
        .layout(LayoutSpec {
            axis: Axis::Row,
            w: Size::Fixed(kumihimo_ui::widgets::component::SLIDER_FIELD_W),
            h: Size::Fixed(kumihimo_ui::tokens::size::FIELD_H),
            pad: [0.0, 0.0, 6.0, 0.0],
            cross: Align::Center,
            ..LayoutSpec::default()
        })
        .paint(Paint::Surface { kind: kumihimo_ui::tokens::StyleKind::Field, radius: kumihimo_ui::tokens::radius::R2_FIELD });
    field.numeric = Some(kumihimo_ui::tree::Numeric { value: f.value, min: f.min, max: f.max, step: f.step });
    b.begin(field);
    b.leaf(Node::deco().paint(if hot { Paint::Fill(ColorToken::AccentBase) } else { Paint::None }).size(Size::Fixed(2.0), Size::Fill));
    b.leaf(Node::deco().paint(Paint::Value { text: f.text.into(), unit: Some(f.unit.into()) }).size(Size::Fill, Size::Fill));
    b.end();
    b.end();
    b.end();
}

pub fn group_id(first_tab: &str) -> String {
    format!("studio.group.{first_tab}")
}
pub fn tab_id(panel: &str) -> String {
    format!("studio.tab.{panel}")
}
/// The +N chip of a group and the id of one row of its open list.
pub fn more_id(first_tab: &str) -> String {
    format!("{}.more", group_id(first_tab))
}
pub fn more_row_id(first_tab: &str, panel: &str) -> String {
    format!("{}.more.{panel}", group_id(first_tab))
}

/// One dock group: (for every group but the top one of a dock) a 1 px lead under the group seam so the header keeps
/// all 26 px (D-F11), the header with disclosure, tabs, +N and panel menu, then the open +N list, or the active tab's
/// body: Layers as the spine built it, Properties with the selected layer's opacity field while a layer is selected,
/// every other panel the empty state naming it in words.
fn group_box(b: &mut Builder<'_>, g: &GroupFacts<'_>, top: bool, layers: Option<&LayersFacts<'_>>, props: Option<&PropertiesFacts<'_>>) {
    let Some(&(first, _)) = g.tabs.first() else { return };
    let h = g.h.map_or(Size::Fill, Size::Fixed);
    let mut boxed = Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h, cross: Align::Start, ..LayoutSpec::default() }).flags(|f| f.clip = true);
    if !top {
        boxed = boxed.seam(Side::Top);
    }
    b.begin(boxed);
    if !top {
        gap(b, 0.0, 1.0);
    }
    let gid = group_id(first);
    let tab_ids: Vec<String> = g.tabs.iter().map(|(id, _)| tab_id(id)).collect();
    let tabs: Vec<(&str, &str)> = tab_ids.iter().zip(g.tabs).map(|(id, (_, name))| (id.as_str(), *name)).collect();
    let hidden: Vec<usize> = (0..g.tabs.len()).filter(|i| !g.shown.contains(i)).collect();
    let hidden_ids: Vec<String> = hidden.iter().map(|&i| tab_ids[i].clone()).collect();
    let more_value = format!("hidden={}", hidden_ids.join(","));
    let (dis, more, menu) = (format!("{gid}.disclosure"), more_id(first), format!("{gid}.menu"));
    widgets::group_header(
        b,
        &GroupHeader {
            id: &gid,
            tabs: &tabs,
            shown: g.shown,
            active: g.active,
            focused: g.focused,
            collapsed: g.collapsed,
            disclosure: (&dis, ui_icon(if g.collapsed { "chev_right" } else { "chev_down" })),
            more: (!hidden.is_empty()).then(|| (more.as_str(), more_value.as_str(), ui_icon("chev_down"))),
            menu: (&menu, ui_icon("menu")),
            reason: REASON,
        },
    );
    if !g.collapsed {
        if g.open && !hidden.is_empty() {
            // The +N list (D-F4): one row per hidden tab; a row selects that tab and closes the list.
            b.begin(Node::new(format!("{gid}.more.list"), Role::Group).label("More tabs").layout(column()).paint(Paint::Fill(ColorToken::SurfacePanel)));
            for &i in &hidden {
                let (panel, name) = g.tabs[i];
                b.begin(
                    Node::new(more_row_id(first, panel), Role::MenuItem)
                        .label(name)
                        .actions(&[Action::Click, Action::Focus])
                        .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(kumihimo_ui::tokens::size::MENU_ITEM_H), pad: [8.0, 0.0, 8.0, 0.0], cross: Align::Center, ..LayoutSpec::default() })
                        .paint(Paint::Surface { kind: kumihimo_ui::tokens::StyleKind::Ghost, radius: 0.0 }),
                );
                b.leaf(Node::deco().paint(Paint::Label { text: name.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextPrimary), align: Align::Start, ellipsis: false }).size(Size::Hug, Size::Fill));
                b.end();
            }
            b.end();
        } else {
            let (panel, name) = g.tabs[g.active.min(g.tabs.len() - 1)];
            let field = props.and_then(|p| p.opacity.as_ref()).filter(|_| panel == "properties");
            match (layers.filter(|_| panel == "layers"), field) {
                (Some(l), _) => view::layers_body(b, l.rows, l.selected, l.first, l.body_h),
                (None, Some(f)) => properties_body(b, f),
                (None, None) => {
                    b.begin(Node::new(format!("studio.panel.{panel}"), Role::TabPanel).label(name).layout(column()).paint(Paint::Fill(ColorToken::SurfacePanel)));
                    view::empty_state_at(b, &format!("studio.panel.{panel}.empty"), name, NOT_BUILT, NIY);
                    b.end();
                }
            }
        }
    }
    b.end();
}

/// The Inspect Dock (surface.panel, left seam): three groups, a top seam on each but the first.
pub fn inspect_dock(b: &mut Builder<'_>, groups: &[GroupFacts<'_>], layers: &LayersFacts<'_>, props: &PropertiesFacts<'_>) {
    dock_root(b, DOCK_ID, "Inspect Dock", ColorToken::SurfacePanel, Side::Left, Axis::Column);
    for (gi, g) in groups.iter().enumerate() {
        group_box(b, g, gi == 0, Some(layers), Some(props));
    }
    b.end();
}

/// The Browse Dock expanded under FIT-L1 (260): one group of the six presets.raster.left panels, through the same group
/// view, empty states as bodies.
pub fn browse_dock(b: &mut Builder<'_>, g: &GroupFacts<'_>) {
    dock_root(b, "studio.dock.left", "Browse Dock", ColorToken::SurfacePanel, Side::Right, Axis::Column);
    group_box(b, g, true, None, None);
    b.end();
}

/// The Inspect Dock collapsed to its 32 px icon rail (FIT-W5, docking.edge_states right collapsed): expand, a 20 x 1
/// divider, one button per group (its first panel).
pub fn inspect_rail(b: &mut Builder<'_>) {
    dock_root(b, DOCK_ID, "Inspect Dock", ColorToken::SurfaceStrip, Side::Left, Axis::Column);
    icon_rail(b, "studio.rail.right", "Inspect Dock rail", Axis::Column, [0.0; 4], 0.0);
    widgets::ghost_icon_button(b, "studio.rail.right.expand", "Expand Inspect Dock", ui_icon("dbl_left"), fw::RAIL_BUTTON, fw::RAIL_BUTTON, fw::ICON_SMALL, REASON);
    b.leaf(Node::deco().paint(Paint::Fill(ColorToken::EdgeSubtle)).size(Size::Fixed(fw::RAIL_DIVIDER_W), Size::Fixed(1.0)));
    gap(b, 0.0, 4.0);
    for g in preset::RIGHT {
        if let Some(&(id, name)) = g.tabs.first() {
            widgets::icon_rail_button(b, &format!("studio.rail.right.{id}"), name, ui_icon("presets"), REASON);
        }
    }
    b.end();
    b.end();
}

/// What the Viewport frame shows besides the document (drawn after this panel).
pub struct ViewportFacts<'a> {
    pub file: &'a str,
    pub size: Option<(u32, u32)>,
    pub revision: u64,
    pub render_code: Option<&'a str>,
    /// Logical position of document 0 on each axis (the rulers' origin).
    pub origin: (f32, f32),
}

/// The Viewport (components.json `viewer`, role Canvas, canvas.surround): the 20 x 20 ruler corner, the rulers on
/// surface.strip with seams and labels (the vertical ruler's rotated), the canvas area under them. A render refusal
/// shows its code, never a stale frame. Zoom and pan are B11.
pub fn viewport(b: &mut Builder<'_>, v: &ViewportFacts<'_>) {
    let value = match (v.size, v.render_code) {
        (_, Some(code)) => format!("render refused: {code}"),
        (Some((w, h)), None) => format!("{w} x {h} px, revision {}", v.revision),
        (None, None) => "no frame".to_owned(),
    };
    b.begin(Node::new(VIEWER_ID, Role::Canvas).label(v.file).value(value).layout(column()).paint(Paint::Fill(ColorToken::CanvasSurround)).flags(|f| f.clip = true));
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(kumihimo_ui::tokens::size::RULER_THICKNESS), cross: Align::Start, ..LayoutSpec::default() }));
    let t = kumihimo_ui::tokens::size::RULER_THICKNESS;
    b.leaf(Node::deco().paint(Paint::Fill(ColorToken::SurfaceStrip)).seam(Side::Right).seam(Side::Bottom).size(Size::Fixed(t), Size::Fixed(t)));
    widgets::ruler(b, "studio.viewer.ruler.x", "Horizontal ruler", Axis::Row, v.origin.0, true);
    b.end();
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
    widgets::ruler(b, "studio.viewer.ruler.y", "Vertical ruler", Axis::Column, v.origin.1, true);
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Center, main: Align::Center, ..LayoutSpec::default() }));
    if let Some(code) = v.render_code {
        b.leaf(
            Node::deco()
                .paint(Paint::Label { text: format!("Render refused: {code}"), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Center, ellipsis: true })
                .size(Size::Fill, Size::Fixed(TextToken::Body.style().line)),
        );
    }
    b.end();
    b.end();
    b.end();
}

/// The status bar's readouts at the right, in order (D-F7): document size, zoom, revision, renderer state, device.
pub const READOUTS: [&str; 5] = ["studio.status.size", "studio.status.zoom", "studio.status.revision", "studio.status.chrome", "studio.status.device"];
/// FIT-S1: the order readouts drop in when the bar is short (device first; zoom last).
pub const DROP_ORDER: [usize; 5] = [4, 3, 2, 0, 1];

/// What the status bar reads out.
pub struct StatusFacts<'a> {
    /// The hintline: the active tool's name (no gesture map yet).
    pub hint: &'a str,
    /// The last outcome in words (drawn) and as `command code` (the node's value).
    pub command: &'a str,
    pub command_code: &'a str,
    /// The texts of [`READOUTS`], in order; `shown[i]` false drops readout i (FIT-S1): it stays in the tree with no box.
    pub readouts: [&'a str; 5],
    pub shown: [bool; 5],
    /// The renderer state (`cpu painter`, or `vulkan ui pass` with `--chrome vulkan`: scene::ChromeMode::code), readout
    /// studio.status.chrome.
    pub chrome: &'a str,
}

/// The status bar (surface.frame, top seam, h 24, gap 12): the hintline (tool name, then the last outcome in words) at
/// the left; the readouts at the right, separated by 1 x 14 edge.subtle lines; dropped readouts keep their node.
pub fn status(b: &mut Builder<'_>, s: &StatusFacts<'_>) {
    b.begin(
        Node::new(STATUS_ID, Role::Status)
            .label("Status")
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, gap: fw::STATUS_GAP, pad: [8.0, 0.0, 8.0, 0.0], cross: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfaceFrame))
            .seam(Side::Top)
            .flags(|f| f.clip = true),
    );
    view::readout(b, "studio.status.hint", s.hint, false);
    b.leaf(
        Node::new("studio.status.command", Role::Label)
            .label(s.command)
            .value(s.command_code)
            .paint(Paint::Label { text: s.command.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: true })
            .size(Size::Fill, Size::Fixed(TextToken::Body.style().line)),
    );
    let mut texts = s.readouts;
    texts[3] = s.chrome;
    let shown: Vec<(&str, &str)> = (0..5).filter(|&i| s.shown[i]).map(|i| (READOUTS[i], texts[i])).collect();
    widgets::status_readouts(b, &shown, None);
    for i in (0..5).filter(|&i| !s.shown[i]) {
        b.leaf(Node::new(READOUTS[i], Role::Label).label(texts[i]).size(Size::Fixed(0.0), Size::Fixed(0.0)));
    }
    b.end();
}

/// The unpainted observability nodes: studio.dock.state (STU-MDL-106), studio.tool.active (STU-MDL-116) and
/// studio.gesture.active (STU-SHL-108, STU-MDL-106).
pub fn observe(b: &mut Builder<'_>, dock_state: &str, tool: &str, gesture: &str) {
    b.leaf(Node::new("studio.dock.state", Role::Label).label("Dock state").value(dock_state));
    b.leaf(Node::new("studio.tool.active", Role::Label).label("Active tool").value(tool));
    // STU-SHL-108, STU-MDL-106: `<author_id> <kind>` of the widget owning the pointer, or `none`.
    b.leaf(Node::new(super::GESTURE_ID, Role::Label).label("Active gesture").value(gesture));
}

// ---- B6: menu popups, the Layers context menu and tooltips (SMB-GUI-MENU-1).

/// One popup row as the frame decided it (ids, texts and the availability record come from the menu tree).
#[derive(Clone, Debug, PartialEq)]
pub struct PopupRow {
    /// `menu.<command id>`, `menu.<id>.remedy`, or `studio.menu.more.<title>` in the chevron's list.
    pub id: String,
    pub label: String,
    pub chord: Option<String>,
    /// The reason text under a greyed row; None: enabled.
    pub reason: Option<String>,
    pub availability: Option<NodeAvailability>,
    /// The reason line (STU-SHL-019 `<STATE> <CODE>: <text>[; remedy <id>]`) of a greyed row.
    pub value: Option<String>,
    pub submenu: bool,
    pub open: bool,
    pub hot: bool,
    /// A toggle row (MenuItemCheckBox: the panels, GPU render, status log) and whether it is on.
    pub toggle: bool,
    pub checked: bool,
    pub shown: bool,
    pub remedy: bool,
    /// Top of the row, logical px (0 when not shown).
    pub y: f32,
}

/// One open popup: its author id (`studio.menu.<title>[.<submenu>]`, `studio.ctx.layers`), label, box and rows; or,
/// for a node with no children, one greyed line saying why (`empty`: id, label, reason text, reason line).
#[derive(Clone, Debug, PartialEq)]
pub struct PopupLevel {
    pub id: String,
    pub label: String,
    pub rect: Rect,
    pub rows: Vec<PopupRow>,
    pub empty: Option<(String, String, String, String)>,
    /// The scroll lane when not every row fits (components.json `menu_popup.pointer`): whether rows lie above the
    /// first shown one and below the last. None: every row shows.
    pub scroll: Option<(bool, bool)>,
}

/// Every open popup, left to right, and their union (the popup panel's viewport).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PopupFacts {
    pub rect: Rect,
    pub levels: Vec<PopupLevel>,
}

/// The popup panel: each popup in its column at its offset inside the union (cascades open at the parent's right
/// edge; the frame moved them into the window).
pub fn popups(b: &mut Builder<'_>, p: &PopupFacts) {
    let (check, chevron) = (ui_icon("check"), ui_icon("chev_right"));
    b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
    let mut x = p.rect.x0;
    for l in &p.levels {
        let lead = (l.rect.x0 - x).max(0.0);
        if lead > 0.0 {
            gap(b, lead, 0.0);
        }
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(l.rect.w()), h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
        let top = (l.rect.y0 - p.rect.y0).max(0.0);
        if top > 0.0 {
            gap(b, 0.0, top);
        }
        widgets::menu_popup_begin(b, &l.id, &l.label, l.rect.w(), l.rect.h());
        if let Some((id, label, reason, line)) = &l.empty {
            b.begin(
                Node::new(id.as_str(), Role::Label)
                    .label(label.as_str())
                    .value(line.as_str())
                    .flags(|f| f.disabled = true)
                    .layout(LayoutSpec {
                        axis: Axis::Column,
                        w: Size::Fill,
                        h: Size::Fixed(widgets::menu_row_h(true)),
                        pad: [kumihimo_ui::tokens::size::MENU_GUTTER_ICON, 0.0, widgets::popup::PAD_R, 0.0],
                        cross: Align::Start,
                        ..LayoutSpec::default()
                    }),
            );
            b.leaf(Node::deco().paint(Paint::Label { text: label.clone(), token: TextToken::Body, weight: 0, color: None, align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fixed(kumihimo_ui::tokens::size::MENU_ITEM_H)));
            b.leaf(Node::deco().paint(Paint::Label { text: reason.clone(), token: TextToken::Caption, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: true }).size(Size::Fill, Size::Fixed(widgets::popup::REASON_LINE)));
            b.end();
        }
        if let Some((up, _)) = l.scroll {
            widgets::menu_scroll_button(b, &format!("{}.scroll-up", l.id), "Scroll up", ui_icon("chev_up"), up);
        }
        for r in &l.rows {
            let row = MenuRow {
                id: &r.id,
                label: &r.label,
                chord: r.chord.as_deref(),
                reason: r.reason.as_deref(),
                availability: r.availability.clone(),
                value: r.value.as_deref(),
                submenu: r.submenu,
                open: r.open,
                hot: r.hot,
                toggle: r.toggle,
                checked: r.checked,
                shown: r.shown,
                remedy: r.remedy,
            };
            widgets::menu_item(b, &row, check, chevron);
        }
        if let Some((_, down)) = l.scroll {
            widgets::menu_scroll_button(b, &format!("{}.scroll-down", l.id), "Scroll down", ui_icon("chev_down"), down);
        }
        b.end();
        b.end();
        x = l.rect.x1;
    }
    b.end();
}

/// The tooltip's author id (components.json `tooltip_l0`, `tooltip_l1`) and its remedy button's.
pub const TIP_ID: &str = "studio.tooltip";
pub const TIP_REMEDY_ID: &str = "studio.tooltip.remedy";

/// A tooltip as the frame placed it.
#[derive(Clone, Debug, PartialEq)]
pub struct TipFacts {
    pub rect: Rect,
    pub level: u8,
    pub name: String,
    pub chord: Option<String>,
    pub summary: Option<String>,
    pub reason: Option<String>,
    pub remedy: Option<String>,
    pub value: Option<String>,
}

/// The tooltip panel: one tip filling its box.
pub fn tip(b: &mut Builder<'_>, t: &TipFacts) {
    let tip = Tip {
        id: TIP_ID,
        level: t.level,
        name: &t.name,
        chord: t.chord.as_deref(),
        summary: t.summary.as_deref(),
        reason: t.reason.as_deref(),
        remedy: t.remedy.as_deref().map(|l| (TIP_REMEDY_ID, l)),
        value: t.value.as_deref(),
    };
    widgets::tooltip(b, &tip, t.rect.w(), t.rect.h());
}
