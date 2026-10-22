use crate::{ContextMenu, ContextMenuItem, MenuItemKind};
use egui::accesskit;
use handshake_context_menu::navigation::{first_navigable, is_navigable, step_highlight};

impl ContextMenu {
    /// Open + render this menu as the context menu of `response` (secondary-click / right-click).
    ///
    /// This is the primary call site for a right-clickable surface: pass the `Response` of the widget
    /// the user can right-click (a tab chip, a pane body, a tree row). egui's
    /// [`egui::Popup::context_menu`] opens the popup on `response.secondary_clicked()`, anchors it at
    /// the pointer, paints it in `Order::Foreground` above all panes, clamps it on-screen, and closes
    /// it on Escape / click-outside / item-click with the correct one-frame grace.
    ///
    /// Returns `Some(item_id)` on the frame an enabled action leaf (top-level or inside a submenu) is
    /// confirmed, otherwise `None`. The caller maps the returned `&'static str` to a real action.
    pub fn show_on(&self, response: &egui::Response) -> Option<&'static str> {
        self.show_on_with_render_observation(response).0
    }

    pub fn show_on_with_render_observation(
        &self,
        response: &egui::Response,
    ) -> (Option<&'static str>, bool) {
        let mut activated = None;
        let mut rendered = false;
        egui::Popup::context_menu(response).show(|ui| {
            ui.set_min_width(180.0);
            activated = render_menu_root(ui, self.surface_id(), self.entries());
            rendered = true;
        });
        (activated, rendered)
    }

    /// Render this menu's items into an ALREADY-OPEN menu `Ui` (advanced / testing use).
    ///
    /// Most callers want [`Self::show_on`]. This entry point exists for callers that already manage the
    /// popup open-state themselves (e.g. a keyboard-opened menu via [`crate::request_open`] rendered inside
    /// their own [`egui::Popup`] body) and just need the typed-model painting + dispatch + AccessKit
    /// emission. Returns the confirmed item id this frame, or `None`.
    pub fn render_into(&self, ui: &mut egui::Ui) -> Option<&'static str> {
        render_menu_root(ui, self.surface_id(), self.entries())
    }
}

/// Per-open-menu keyboard-navigation state, stored in egui temp memory keyed by the menu popup's
/// `Ui` id. `highlight` is the index (into the FULL item list, including separators/disabled) of the
/// item the keyboard cursor currently sits on; `None` means "not yet anchored" (the just-opened
/// state, before the first paint requests focus on the first actionable item).
#[derive(Clone, Copy, Default)]
struct MenuNavState {
    highlight: Option<usize>,
}

/// Top-level menu entry point: drive keyboard navigation (ArrowDown/ArrowUp move a wrapping
/// highlight over the actionable items; Enter/Space confirm the highlighted item) and then paint +
/// dispatch the items. Submenu bodies reuse plain [`render_items`] (their hover/keyboard behavior is
/// egui's own; multi-level keyboard nav is out of MT-019 scope).
///
/// The highlight is realized by calling `request_focus` on the highlighted item's [`egui::Response`]
/// inside [`render_items`]: egui then (a) paints the focus/selection background, (b) sets the
/// AccessKit tree focus to that node so an out-of-process model can read the cursor, and (c) turns a
/// subsequent Enter/Space into a `FAKE_PRIMARY_CLICKED` so the existing `response.clicked()` dispatch
/// path fires with no special-casing.
///
/// We still `consume_key` the arrow events here, but note what that does and does NOT do: egui reads
/// the arrow `focus_direction` from the RAW event queue at `begin_pass`, BEFORE this closure runs, so
/// consuming the key inside the closure does NOT suppress egui's own directional focus. Instead our
/// manual highlight + `request_focus` is the single thing that drives the visible cursor and the
/// wrapping behavior egui's directional focus lacks; egui's redundant directional move (if any) lands
/// on the same focusable items and is harmless here. The `consume_key` keeps the arrow events from
/// leaking out to other widgets/handlers in the same frame.
fn render_menu_root(
    ui: &mut egui::Ui,
    surface_id: &str,
    items: &[ContextMenuItem],
) -> Option<&'static str> {
    // Expose the popup body as the canonical AccessKit menu container. Leaf nodes below it are
    // MenuItems; the parent role preserves the complete context-menu topology for model steering.
    let menu_id = ui.id();
    let menu_label = format!("{surface_id} context menu");
    let menu_author = format!("ctx-menu.surface.{surface_id}");
    ui.ctx().accesskit_node_builder(menu_id, move |node| {
        node.set_role(accesskit::Role::Menu);
        node.set_author_id(menu_author);
        node.set_label(menu_label);
    });
    let nav_id = ui.id().with("ctx-menu-nav");
    let mut nav: MenuNavState = ui
        .ctx()
        .data_mut(|d| d.get_temp::<MenuNavState>(nav_id).unwrap_or_default());

    // Anchor on open: the first frame the menu paints, no item is highlighted yet — put the cursor
    // on the first actionable item so arrow keys have a starting point and the menu shows a cursor.
    let highlight_valid = nav
        .highlight
        .is_some_and(|i| items.get(i).is_some_and(is_navigable));
    if !highlight_valid {
        nav.highlight = first_navigable(items);
    }

    // Read + consume arrow keys and move the wrapping highlight. Our manual step (with request_focus
    // in render_items) is what actually drives the cursor AND wraps at the ends; egui's built-in
    // directional focus does not wrap. NOTE: consume_key here does NOT prevent egui's directional
    // focus — egui captures the arrow focus_direction from the raw event queue in begin_pass, before
    // this closure runs, so its directional move (if it happens) is already decided. That redundant
    // move is harmless (it lands on the same focusable items); consume_key just stops the arrow events
    // from leaking to other widgets/handlers in the same frame.
    if let Some(current) = nav.highlight {
        let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
        let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
        if down {
            nav.highlight = Some(step_highlight(items, current, true));
        } else if up {
            nav.highlight = Some(step_highlight(items, current, false));
        }
    }

    ui.ctx().data_mut(|d| d.insert_temp(nav_id, nav));

    render_items(ui, surface_id, items, nav.highlight)
}

/// Paint a list of items into an open menu `Ui`, dispatch the confirmed leaf, and emit AccessKit
/// nodes. Recurses ONE level for submenus (V1 scope). `surface_id` is currently informational
/// (author_ids are already globally namespaced via `ctx-menu.{item.id}`), but is threaded through so a
/// future multi-surface deployment can prefix ids per surface without touching call sites.
fn render_items(
    ui: &mut egui::Ui,
    surface_id: &str,
    items: &[ContextMenuItem],
    highlight: Option<usize>,
) -> Option<&'static str> {
    let _ = surface_id;
    let mut activated: Option<&'static str> = None;
    for (idx, item) in items.iter().enumerate() {
        let is_highlighted = highlight == Some(idx);
        match &item.kind {
            MenuItemKind::Separator => {
                ui.separator();
            }
            MenuItemKind::Action => {
                if render_leaf(ui, item, is_highlighted) {
                    activated = Some(item.id);
                    ui.close();
                }
            }
            MenuItemKind::Submenu(children) => {
                // egui's `menu_button` becomes a nested submenu when called inside an open menu Ui
                // (opens on hover, paints in its own Foreground Area so it is not clipped by the parent
                // — red-team R5). The submenu header is itself a MenuItem node carrying its author_id.
                // Nested items carry no keyboard highlight (multi-level keyboard nav is out of MT-019
                // scope); their navigation is egui's own hover/focus behavior.
                let inner = ui.menu_button(item.label, |ui| {
                    render_items(ui, surface_id, children, None)
                });
                if is_highlighted {
                    // The keyboard cursor sits on the submenu header: focus + highlight it so Enter
                    // opens it and the cursor is visible/AccessKit-discoverable like a leaf.
                    inner.response.clone().highlight().request_focus();
                }
                name_menu_node(
                    ui,
                    inner.response.id,
                    &item.author_id(),
                    item.label,
                    item.enabled,
                    item.disabled_reason,
                );
                if let Some(Some(child_id)) = inner.inner {
                    activated = Some(child_id);
                    ui.close();
                }
            }
        }
    }
    activated
}

/// Render one leaf (enabled or disabled) and return true iff it was confirmed this frame. A disabled
/// leaf is rendered greyed with its disclosed reason on hover and never returns true (no fake-enable).
fn render_leaf(ui: &mut egui::Ui, item: &ContextMenuItem, is_highlighted: bool) -> bool {
    let mut button = egui::Button::new(item.label).min_size(egui::vec2(ui.available_width(), 0.0));
    if let Some(hint) = item.shortcut_hint {
        button = button.shortcut_text(hint);
    }
    if item.enabled {
        let response = ui.add(button);
        if is_highlighted {
            // Realize the keyboard cursor: focus drives the selection background, the AccessKit
            // tree focus (so an out-of-process model sees the cursor), and egui's Enter/Space ->
            // FAKE_PRIMARY_CLICKED so `clicked()` below confirms the highlighted item. `highlight()`
            // additionally forces the active/selection fill even on the frame focus is requested.
            response.clone().highlight().request_focus();
        }
        name_menu_node(ui, response.id, &item.author_id(), item.label, true, None);
        response.clicked()
    } else {
        let response = ui.add_enabled(false, button);
        let response = match item.disabled_reason {
            Some(reason) => response.on_disabled_hover_text(reason),
            None => response,
        };
        name_menu_node(
            ui,
            response.id,
            &item.author_id(),
            item.label,
            false,
            item.disabled_reason,
        );
        false
    }
}

/// Attach a stable `author_id` + `Role::MenuItem` + label to a leaf/submenu node's LIVE AccessKit
/// node, exactly mirroring the original native menu bar's `name_node`. The node is egui's real
/// per-frame node for `widget_id`, so the values land in the live tree a model reads out-of-process.
/// Items are dynamic, so they live in egui's hashed id space (not a fixed-band NodeId).
fn name_menu_node(
    ui: &mut egui::Ui,
    widget_id: egui::Id,
    author_id: &str,
    label: &str,
    enabled: bool,
    disabled_reason: Option<&'static str>,
) {
    let author_id = author_id.to_owned();
    let label = label.to_owned();
    // MT-154 AC-154-7: a freshly opened popup's first LIVE frame is egui's invisible sizing pass. Its
    // widgets are registered disabled (UiBuilder::invisible) at provisional geometry, and the next
    // frame hit-tests pointer input against exactly those disabled rects, so a click aimed at them is
    // swallowed and `show_on` never confirms the item. Advertising Click there told steering callers
    // the item was actionable one frame too early. A snapshot-capture pass renders on a fresh context
    // (pass 0) whose single sizing pass IS the published tree, so it keeps the typed availability.
    let provisional = ui.is_sizing_pass() && ui.ctx().cumulative_pass_nr() > 0;
    ui.ctx().accesskit_node_builder(widget_id, move |node| {
        node.set_role(accesskit::Role::MenuItem);
        node.set_author_id(author_id);
        node.set_label(label);
        if enabled && provisional {
            node.set_disabled();
            node.remove_action(accesskit::Action::Click);
        } else if enabled {
            // Snapshot capture may render an inactive source pane under an egui disabled parent even
            // though the retained typed menu entry is actionable in its originating pane. The menu
            // model is the authority for leaf availability, so preserve that truth in AccessKit.
            node.clear_disabled();
            node.add_action(accesskit::Action::Click);
        } else {
            node.set_disabled();
            if let Some(reason) = disabled_reason {
                // Argus/UIA consumers need the same explanation the pointer hover receives. AccessKit's
                // value is preserved in the canonical snapshot and is queryable without visual hover.
                node.set_value(reason);
            }
            // egui's disabled Button remains perceivable but still inherits the Button widget's
            // default Click action. Remove it explicitly so AccessKit and out-of-process steering
            // cannot advertise or invoke an action the typed menu model has disabled.
            node.remove_action(accesskit::Action::Click);
        }
    });
}
