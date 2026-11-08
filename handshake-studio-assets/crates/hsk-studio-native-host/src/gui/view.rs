//! View functions of the Layers panel (SMB-GUI-SPINE-1), the document list and the shared readouts: pure, inputs in,
//! nodes out, so a rebuild is a leaf change. Ids follow components.json (`studio.dock.<region>`,
//! `studio.group.<first-panel>`, `studio.tab.<panel>`, `studio.panel.<panel>`, `studio.panel.layers.row.<layer-id>`,
//! `studio.scroll.<owner>`, `studio.status`). The frame's bands are in [`super::chrome`].
use super::{BODY_ID, DOCK_ID, DOC_IMAGE, EMPTY_ID, GROUP_ID, LIST_ID, SCROLL_ID, TAB_ID, cut, row_id};
use hsk_studio_harness_view::view::LayerRow;
use kumihimo_ui::display::{DisplayList, IconId, Prim};
use kumihimo_ui::geom::Rect;
use kumihimo_ui::icons;
use kumihimo_ui::text::MAX_LABEL_BYTES;
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme, size, spacing};
use kumihimo_ui::tree::{Action, Align, Axis, Builder, LayoutSpec, Node, Paint, Role, Size};
use kumihimo_ui::widgets;

/// Most rows one list builds, whatever the height asks (a 16384 px window at scale 0.5 fits 1024).
const MAX_ROWS_BUILT: usize = 1024;

/// The eye of a visible row and the eye_off of a hidden one (components.json layer_row `hidden`: eye off), from the
/// designer's icon set.
pub fn eye(visible: bool) -> IconId {
    icons::ui(if visible { "eye" } else { "eye_off" }).unwrap_or(IconId(0))
}

fn column() -> LayoutSpec {
    LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }
}

/// Rows built for a list `list_h` logical px tall: every row at least partly visible (rows are a fixed 32).
pub fn rows_fit(list_h: f32) -> usize {
    if !list_h.is_finite() || list_h <= 0.0 {
        return 0;
    }
    ((list_h / size::ROW_LAYER_H).ceil() as usize).min(MAX_ROWS_BUILT)
}
/// Rows wholly visible: the page the wheel, the Page keys and the scrollbar count (at least 1).
pub fn rows_page(list_h: f32) -> usize {
    if !list_h.is_finite() || list_h <= 0.0 {
        return 1;
    }
    ((list_h / size::ROW_LAYER_H).floor() as usize).clamp(1, MAX_ROWS_BUILT)
}
/// The last first row: the list never scrolls past its end.
pub fn max_first(total: usize, list_h: f32) -> usize {
    total.saturating_sub(rows_page(list_h))
}

/// A standalone right dock with the Layers group only (the spine's shape, kept for bounded-build tests): the group
/// header, then [`layers_body`].
pub fn layers_dock(b: &mut Builder<'_>, rows: &[LayerRow], selected: Option<&str>, first: usize, body_h: f32) {
    b.begin(Node::new(DOCK_ID, Role::Group).label("Right dock").layout(column()));
    widgets::tab_row(b, GROUP_ID, &[(TAB_ID, "Layers")], 0);
    layers_body(b, rows, selected, first, body_h);
    b.end();
}

/// The Layers body (a TabPanel): the virtualised list and the scroll lane. Only the rows that fit are built, from
/// `first`; the list's value states first, last and total (STU-MDL-108: the tree never hides rows silently).
pub fn layers_body(b: &mut Builder<'_>, rows: &[LayerRow], selected: Option<&str>, first: usize, body_h: f32) {
    b.begin(
        Node::new(BODY_ID, Role::TabPanel)
            .label("Layers")
            .layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::SurfacePanel)),
    );
    if rows.is_empty() {
        empty_state(b, "No layers", "This document has no layers to list.");
    } else {
        let total = rows.len();
        let first = first.min(max_first(total, body_h));
        let built = rows_fit(body_h).min(total - first);
        let last = first + built;
        b.begin(
            Node::new(LIST_ID, Role::Tree)
                .label("Layers")
                .value(format!("rows {}-{} of {}", if built == 0 { first } else { first + 1 }, last, total))
                .actions(&[Action::Click])
                .layout(column())
                .flags(|f| f.clip = true),
        );
        for r in &rows[first..last] {
            widgets::layer_row(
                b,
                &row_id(&r.layer_id),
                cut(&r.name, MAX_LABEL_BYTES),
                ColorToken::TagNone,
                r.visible,
                selected == Some(r.layer_id.as_str()),
                u8::try_from(r.depth).unwrap_or(u8::MAX),
                None,
                eye(r.visible),
                &[],
            );
        }
        b.end();
        widgets::scrollbar(b, SCROLL_ID, "Layers", body_h, first, rows_page(body_h), total);
    }
    b.end();
}

/// The Layers body when its tree was refused (`duplicate_id` when two layer ids meet after lowercasing): the empty
/// state names the code.
pub fn dock_refused(b: &mut Builder<'_>, code: &str) {
    b.begin(Node::new(DOCK_ID, Role::Group).label("Inspect Dock").layout(column()).paint(Paint::Fill(ColorToken::SurfacePanel)));
    widgets::tab_row(b, GROUP_ID, &[(TAB_ID, "Layers")], 0);
    b.begin(Node::new(BODY_ID, Role::TabPanel).label("Layers").layout(column()).paint(Paint::Fill(ColorToken::SurfacePanel)));
    empty_state(b, "Layers cannot be listed", &format!("Refused: {code}"));
    b.end();
    b.end();
}

fn line(text: &str, token: TextToken, color: ColorToken) -> Node {
    Node::deco()
        .paint(Paint::Label { text: text.into(), token, weight: 0, color: Some(color), align: Align::Start, ellipsis: true })
        .size(Size::Fill, Size::Fixed(token.style().line))
}

/// components.json `empty_state` of the Layers panel: one line naming what is missing, one line on how to get it.
pub fn empty_state(b: &mut Builder<'_>, title: &str, body: &str) {
    empty_state_at(b, EMPTY_ID, title, body);
}

/// components.json `empty_state` with its own id (pad 16).
pub fn empty_state_at(b: &mut Builder<'_>, id: &str, title: &str, body: &str) {
    b.begin(
        Node::new(id, Role::Group)
            .label(title)
            .value(body)
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, gap: spacing::S2, pad: [spacing::S6; 4], cross: Align::Start, ..LayoutSpec::default() }),
    );
    b.leaf(line(title, TextToken::Body, ColorToken::TextPrimary));
    b.leaf(line(body, TextToken::Body, ColorToken::TextSecondary));
    b.end();
}

/// The document rect in the canvas area: doc px = logical px (zoom 100 %, B11), centred on whole px.
pub fn image_rect(area: Rect, width: u32, height: u32) -> Rect {
    let (w, h) = (width as f32, height as f32);
    Rect::xywh(area.x0 + ((area.w() - w) / 2.0).floor(), area.y0 + ((area.h() - h) / 2.0).floor(), w, h)
}

/// The document over its transparency grid, clipped to the canvas area under the rulers (straight alpha, paint.rs
/// Image).
pub fn document_list(area: Rect, width: u32, height: u32, theme: Theme) -> DisplayList {
    let r = image_rect(area, width, height);
    DisplayList {
        items: vec![
            Prim::PushScissor(area),
            Prim::Checker { rect: r, a: ColorToken::CanvasCheckerA.resolve(theme), b: ColorToken::CanvasCheckerB.resolve(theme), cell: 8.0 },
            Prim::Image { rect: r, image: DOC_IMAGE },
            Prim::PopScissor,
        ],
    }
}

/// One status readout: its full text in the label; `fill` takes the room left and ellipsizes.
pub fn readout(b: &mut Builder<'_>, id: &str, text: &str, fill: bool) {
    b.leaf(
        Node::new(id, Role::Label)
            .label(text)
            .paint(Paint::Label { text: text.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: fill })
            .size(if fill { Size::Fill } else { Size::Hug }, Size::Fixed(TextToken::Body.style().line)),
    );
}
