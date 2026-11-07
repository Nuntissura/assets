//! View functions of the frame's panels: pure, inputs in, nodes out, so a rebuild is a leaf change. Ids follow
//! components.json (`studio.viewer.<kind>`, `studio.dock.<region>`, `studio.group.<first-panel>`,
//! `studio.tab.<panel>`, `studio.panel.<panel>`, `studio.panel.layers.row.<layer-id>`, `studio.scroll.<owner>`,
//! `studio.status`).
use super::{BODY_ID, DOCK_ID, DOC_IMAGE, EMPTY_ID, GROUP_ID, LIST_ID, SCROLL_ID, STATUS_ID, TAB_ID, VIEWER_ID, cut, row_id};
use crate::scene::CHROME;
use hsk_studio_harness_view::view::LayerRow;
use kumihimo_ui::display::{DisplayList, IconId, Prim};
use kumihimo_ui::gallery::ICON_EYE;
use kumihimo_ui::geom::Rect;
use kumihimo_ui::text::MAX_LABEL_BYTES;
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme, size, spacing};
use kumihimo_ui::tree::{Action, Align, Axis, Builder, LayoutSpec, Node, Paint, Role, Size};
use kumihimo_ui::widgets;

/// A hidden layer's visibility slot draws no glyph (components.json layer_row `hidden`: eye off).
pub const ICON_NONE: IconId = IconId(0);
/// Most rows one list builds, whatever the height asks (a 16384 px window at scale 0.5 fits 1024).
const MAX_ROWS_BUILT: usize = 1024;

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

/// The right dock: the group header (tab row with the Layers tab), then the Layers body: the virtualised list and
/// the scroll lane. Only the rows that fit are built, from `first`; the list's value states first, last and total
/// (STU-MDL-108: the tree never hides rows silently).
pub fn layers_dock(b: &mut Builder<'_>, rows: &[LayerRow], selected: Option<&str>, first: usize, body_h: f32) {
    b.begin(Node::new(DOCK_ID, Role::Group).label("Right dock").layout(column()));
    widgets::tab_row(b, GROUP_ID, &[(TAB_ID, "Layers")], 0);
    b.begin(
        Node::new(BODY_ID, Role::Group)
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
                if r.visible { ICON_EYE } else { ICON_NONE },
                &[],
            );
        }
        b.end();
        widgets::scrollbar(b, SCROLL_ID, "Layers", body_h, first, rows_page(body_h), total);
    }
    b.end();
    b.end();
}

/// The right dock when its tree was refused (`duplicate_id` when two layer ids meet after lowercasing): the empty
/// state names the code.
pub fn dock_refused(b: &mut Builder<'_>, code: &str) {
    b.begin(Node::new(DOCK_ID, Role::Group).label("Right dock").layout(column()));
    widgets::tab_row(b, GROUP_ID, &[(TAB_ID, "Layers")], 0);
    b.begin(Node::new(BODY_ID, Role::Group).label("Layers").layout(column()).paint(Paint::Fill(ColorToken::SurfacePanel)));
    empty_state(b, "Layers cannot be listed", &format!("Refused: {code}"));
    b.end();
    b.end();
}

fn line(text: &str, token: TextToken, color: ColorToken) -> Node {
    Node::deco()
        .paint(Paint::Label { text: text.into(), token, weight: 0, color: Some(color), align: Align::Start, ellipsis: true })
        .size(Size::Fill, Size::Fixed(token.style().line))
}

/// components.json `empty_state`: one line naming what is missing, one line on how to get it (pad 16).
pub fn empty_state(b: &mut Builder<'_>, title: &str, body: &str) {
    b.begin(
        Node::new(EMPTY_ID, Role::Group)
            .label(title)
            .value(body)
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, gap: spacing::S2, pad: [spacing::S6; 4], cross: Align::Start, ..LayoutSpec::default() }),
    );
    b.leaf(line(title, TextToken::Body, ColorToken::TextPrimary));
    b.leaf(line(body, TextToken::Body, ColorToken::TextSecondary));
    b.end();
}

/// What the Viewport shows.
pub struct ViewportFacts<'a> {
    pub file: &'a str,
    pub size: Option<(u32, u32)>,
    pub revision: u64,
    pub render_code: Option<&'a str>,
}

/// The Viewport (components.json `viewer`, fill canvas.surround). The document image is drawn by
/// [`document_list`] after this panel: Prim::Checker under Prim::Image, 1:1 at scale 1, centred and clipped. A
/// render refusal shows its code, never a stale frame. Zoom and pan are B11.
pub fn viewport(b: &mut Builder<'_>, v: &ViewportFacts<'_>) {
    let value = match (v.size, v.render_code) {
        (_, Some(code)) => format!("render refused: {code}"),
        (Some((w, h)), None) => format!("{w} x {h} px, revision {}", v.revision),
        (None, None) => "no frame".to_owned(),
    };
    b.begin(
        Node::new(VIEWER_ID, Role::Image)
            .label(v.file)
            .value(value)
            .layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Center, main: Align::Center, ..LayoutSpec::default() })
            .paint(Paint::Fill(ColorToken::CanvasSurround))
            .flags(|f| f.clip = true),
    );
    if let Some(code) = v.render_code {
        b.leaf(line(&format!("Render refused: {code}"), TextToken::Body, ColorToken::TextSecondary).size(Size::Fill, Size::Fixed(TextToken::Body.style().line)));
    }
    b.end();
}

/// The document rect in the Viewport: doc px = logical px, centred on whole px.
pub fn image_rect(viewport: Rect, width: u32, height: u32) -> Rect {
    let (w, h) = (width as f32, height as f32);
    Rect::xywh(viewport.x0 + ((viewport.w() - w) / 2.0).floor(), viewport.y0 + ((viewport.h() - h) / 2.0).floor(), w, h)
}

/// The document over its transparency grid, clipped to the Viewport (straight alpha, paint.rs Image).
pub fn document_list(viewport: Rect, width: u32, height: u32, theme: Theme) -> DisplayList {
    let r = image_rect(viewport, width, height);
    DisplayList {
        items: vec![
            Prim::PushScissor(viewport),
            Prim::Checker { rect: r, a: ColorToken::CanvasCheckerA.resolve(theme), b: ColorToken::CanvasCheckerB.resolve(theme), cell: 8.0 },
            Prim::Image { rect: r, image: DOC_IMAGE },
            Prim::PopScissor,
        ],
    }
}

/// What the status bar reads out.
pub struct StatusFacts<'a> {
    /// Device and backend (AC-BEHAV-07).
    pub device: &'a str,
    pub revision: u64,
    /// The last command and its code.
    pub command: &'a str,
}

fn readout(b: &mut Builder<'_>, id: &str, text: &str, fill: bool) {
    b.leaf(
        Node::new(id, Role::Label)
            .label(text)
            .paint(Paint::Label { text: text.into(), token: TextToken::Body, weight: 0, color: Some(ColorToken::TextSecondary), align: Align::Start, ellipsis: fill })
            .size(if fill { Size::Fill } else { Size::Hug }, Size::Fixed(TextToken::Body.style().line)),
    );
}

/// The status bar (components.json `status_bar`: h 24, gap 12): the chrome statement, the last command with its
/// code, the document revision, the device and backend.
pub fn status(b: &mut Builder<'_>, s: &StatusFacts<'_>) {
    b.begin(
        Node::new(STATUS_ID, Role::Group)
            .label("Status")
            .layout(LayoutSpec {
                axis: Axis::Row,
                w: Size::Fill,
                h: Size::Fixed(size::FRAME_STATUS_BAR_H),
                gap: spacing::S5,
                pad: [spacing::S4, 0.0, spacing::S4, 0.0],
                cross: Align::Center,
                ..LayoutSpec::default()
            })
            .paint(Paint::Fill(ColorToken::SurfaceStrip)),
    );
    readout(b, "studio.status.chrome", CHROME, true);
    readout(b, "studio.status.command", s.command, false);
    readout(b, "studio.status.revision", &format!("revision {}", s.revision), false);
    readout(b, "studio.status.device", s.device, false);
    b.end();
}
