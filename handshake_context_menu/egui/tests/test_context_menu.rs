//! WP-KERNEL-011 MT-019 — context-menu infrastructure, driven LIVE through egui_kittest.
//!
//! These tests exercise the shared [`handshake_native::context_menu`] primitive on a REAL
//! secondary-clickable egui surface (not a synthetic state poke), proving the behaviors the MT-019
//! contract asks for end-to-end:
//!
//! - secondary-click (right-click) on a surface OPENS the context menu at the pointer;
//! - clicking an enabled item DISPATCHES its stable id back to the caller and CLOSES the menu;
//! - a disabled item renders + is addressable but cannot be confirmed (no fake-enable);
//! - a submenu item opens a nested child menu whose leaves are reachable + dispatch;
//! - Escape CLOSES the menu without firing an action;
//! - clicking elsewhere (away) CLOSES the menu;
//! - every rendered item carries a `Role::MenuItem` AccessKit node with a stable `ctx-menu.*`
//!   author_id (out-of-process steering; MT-025 interactive-naming invariant);
//! - the menu is CLOSED by default, so it adds nothing to the default-frame accessibility tree.
//!
//! The surface under right-click is given its own labelled AccessKit node ("RightClickSurface") so the
//! kittest harness can address it the same out-of-process way a swarm agent would, then drive a
//! genuine `click_secondary()` pointer event — the real trigger path, not a memory poke.

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use handshake_context_menu_egui::{ContextMenu, ContextMenuItem};

const SURFACE_LABEL: &str = "RightClickSurface";

/// Build a harness whose UI is a single right-clickable surface that opens the given menu and records
/// the activated item id into the shared `Arc<Mutex<...>>` so a test can assert what was dispatched.
fn surface_harness(
    menu: ContextMenu,
    captured: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
) -> Harness<'static> {
    Harness::builder().build_ui(move |ui| {
        // A real allocated, secondary-clickable rect — the surface the user right-clicks. We give it a
        // stable AccessKit label so the harness can find + right-click it out-of-process.
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(200.0, 80.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            ui.painter()
                .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), SURFACE_LABEL)
        });

        if let Some(id) = menu.show_on(&response) {
            *captured.lock().unwrap() = Some(id);
        }
    })
}

fn tab_menu() -> ContextMenu {
    ContextMenu::new("tab")
        .item(ContextMenuItem::action("tab.pin", "Pin").with_shortcut("Ctrl+Shift+P"))
        .item(ContextMenuItem::action("tab.close", "Close").with_shortcut("Ctrl+W"))
        .separator()
        .item(
            ContextMenuItem::action("tab.close-others", "Close Others")
                .disabled("Needs more than one tab"),
        )
        .item(ContextMenuItem::submenu(
            "tab.move-to",
            "Move to",
            vec![
                ContextMenuItem::action("tab.move-to.pane-b", "Pane B"),
                ContextMenuItem::action("tab.move-to.pane-c", "Pane C"),
            ],
        ))
}

/// Every live author-id-carrying node: (author_id, role, label).
fn live_author_nodes(harness: &Harness<'_>) -> Vec<(String, String, Option<String>)> {
    let mut found = Vec::new();
    for node in harness.root().children_recursive() {
        let ak = node.accesskit_node();
        if let Some(author_id) = ak.author_id() {
            found.push((author_id.to_owned(), format!("{:?}", ak.role()), ak.label()));
        }
    }
    found
}

// ── Closed by default: no context-menu nodes in the default frame ───────────────────────────────────

#[test]
fn context_menu_is_closed_by_default() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    let nodes = live_author_nodes(&harness);
    assert!(
        !nodes.iter().any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "no context-menu items in the default (closed) frame: {nodes:?}"
    );
    assert!(
        captured.lock().unwrap().is_none(),
        "nothing dispatched on an idle frame"
    );
}

// ── Secondary-click opens the menu; its items become addressable MenuItem nodes ──────────────────────

#[test]
fn secondary_click_opens_menu_with_named_items() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    // egui materializes the popup's items the frame after it is opened in memory; run once more so the
    // just-opened popup is laid out and its leaves enter the accessibility tree.
    harness.run();

    let nodes = live_author_nodes(&harness);
    for leaf in [
        "ctx-menu.tab.pin",
        "ctx-menu.tab.close",
        "ctx-menu.tab.close-others",
    ] {
        let found = nodes
            .iter()
            .find(|(a, _, _)| a == leaf)
            .unwrap_or_else(|| panic!("open menu leaf {leaf} missing/anonymous: {nodes:?}"));
        assert_eq!(found.1, "MenuItem", "{leaf} role is MenuItem");
    }
    // No item was confirmed merely by opening the menu.
    assert!(
        captured.lock().unwrap().is_none(),
        "opening the menu fired no action"
    );
}

// ── Clicking an enabled item dispatches its id and closes the menu ──────────────────────────────────

#[test]
fn clicking_item_dispatches_id_and_closes() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // Click the "Close" leaf — the genuine pointer path.
    harness.get_by_label("Close").click();
    harness.run();

    assert_eq!(
        *captured.lock().unwrap(),
        Some("tab.close"),
        "clicking Close dispatched the stable item id"
    );

    // R6/MC6: the menu closed after the click (its leaves are gone from the live tree).
    harness.run();
    let nodes = live_author_nodes(&harness);
    assert!(
        !nodes.iter().any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "the context menu closed after an item was clicked: {nodes:?}"
    );
}

// ── Disabled item renders + is addressable but cannot be confirmed ──────────────────────────────────

#[test]
fn disabled_item_renders_but_does_not_fire() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // The disabled leaf is present + addressable in the open menu.
    let nodes = live_author_nodes(&harness);
    assert!(
        nodes
            .iter()
            .any(|(a, _, _)| a == "ctx-menu.tab.close-others"),
        "disabled 'Close Others' is present + addressable: {nodes:?}"
    );

    // Clicking it fires nothing (egui ignores clicks on a disabled widget — no fake-enable).
    harness.get_by_label("Close Others").click();
    harness.run();
    assert!(
        captured.lock().unwrap().is_none(),
        "the disabled item did not dispatch an action"
    );
}

// ── Submenu opens a nested child menu whose leaves dispatch ─────────────────────────────────────────

#[test]
fn submenu_opens_and_child_dispatches() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // The submenu header is itself an addressable MenuItem node.
    let nodes = live_author_nodes(&harness);
    assert!(
        nodes
            .iter()
            .any(|(a, r, _)| a == "ctx-menu.tab.move-to" && r == "MenuItem"),
        "submenu header 'Move to' is an addressable MenuItem: {nodes:?}"
    );

    // Hover/open the submenu, then click a child. egui opens a submenu on hover; clicking its button
    // also opens it, after which the child leaves materialize.
    harness.get_by_label("Move to").click();
    harness.run();
    harness.run();

    let nodes = live_author_nodes(&harness);
    assert!(
        nodes
            .iter()
            .any(|(a, _, _)| a == "ctx-menu.tab.move-to.pane-b"),
        "submenu child 'Pane B' is reachable once the submenu is open: {nodes:?}"
    );

    harness.get_by_label("Pane B").click();
    harness.run();
    assert_eq!(
        *captured.lock().unwrap(),
        Some("tab.move-to.pane-b"),
        "clicking the submenu child dispatched its stable id"
    );
}

// ── Escape closes the menu without firing an action ─────────────────────────────────────────────────

#[test]
fn escape_closes_menu_without_action() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();
    assert!(
        live_author_nodes(&harness)
            .iter()
            .any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "menu open before Escape"
    );

    harness.key_press(egui::Key::Escape);
    harness.run();
    harness.run();

    let nodes = live_author_nodes(&harness);
    assert!(
        !nodes.iter().any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "Escape closed the context menu: {nodes:?}"
    );
    assert!(captured.lock().unwrap().is_none(), "Escape fired no action");
}

// ── Clicking away (outside the menu) closes it ──────────────────────────────────────────────────────

#[test]
fn click_away_closes_menu() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();
    assert!(
        live_author_nodes(&harness)
            .iter()
            .any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "menu open before click-away"
    );

    // A primary click on the surface (outside the menu popup rect) dismisses the menu. The one-frame
    // grace means the OPENING right-click does not self-dismiss; this is a SEPARATE later click.
    harness.get_by_label(SURFACE_LABEL).click();
    harness.run();
    harness.run();

    let nodes = live_author_nodes(&harness);
    assert!(
        !nodes.iter().any(|(a, _, _)| a.starts_with("ctx-menu.")),
        "clicking away closed the context menu: {nodes:?}"
    );
}

// ── Keyboard navigation: ArrowDown moves a highlight; Enter confirms it (AC#6 / proof_target #3) ─────

#[test]
fn arrow_down_then_enter_confirms_third_actionable_item() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // The actionable items in order are: [0] Pin, [1] Close, [2] Move to (submenu header). The
    // separator and disabled "Close Others" are NOT navigable and must be skipped. On open the
    // highlight anchors on Pin; two ArrowDowns land on the THIRD actionable item ("Move to").
    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::ArrowDown);
    harness.run();

    // The third actionable item now carries the keyboard cursor: its AccessKit node reads focused.
    let focused: Vec<String> = harness
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().is_focused())
        .filter_map(|n| n.accesskit_node().author_id().map(|a| a.to_owned()))
        .collect();
    assert!(
        focused.contains(&"ctx-menu.tab.move-to".to_owned()),
        "after 2x ArrowDown the cursor (focus) is on the third actionable item: {focused:?}"
    );

    // Enter confirms the highlighted item. "Move to" is a submenu header; confirming it opens the
    // submenu rather than dispatching, so to prove Enter CONFIRMS A LEAF we step back to "Close"
    // (the second actionable item) and press Enter there.
    harness.key_press(egui::Key::ArrowUp);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();

    assert_eq!(
        *captured.lock().unwrap(),
        Some("tab.close"),
        "Enter on the highlighted leaf dispatched its stable id"
    );
}

/// proof_target #3 — the LITERAL one-gesture proof: in a THREE-LEAF menu (no submenu, no separator,
/// no disabled entry to split the path), opening anchors the highlight on the first leaf, then
/// `ArrowDown` x2 + `Enter` dispatches the THIRD leaf's id as a single uninterrupted gesture (no
/// step-back). This is distinct from `arrow_down_then_enter_confirms_third_actionable_item`, whose
/// third actionable item is a submenu header (Enter there opens a submenu rather than dispatching),
/// forcing that test to step back one item before confirming. Here all three are dispatchable leaves,
/// so ArrowDown-ArrowDown-Enter proves end-to-end that the keyboard cursor reaches and confirms the
/// third leaf without any intervening correction.
#[test]
fn arrow_down_twice_then_enter_dispatches_third_leaf() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let triple_menu = ContextMenu::new("triple")
        .item(ContextMenuItem::action("triple.one", "One"))
        .item(ContextMenuItem::action("triple.two", "Two"))
        .item(ContextMenuItem::action("triple.three", "Three"));
    let mut harness = surface_harness(triple_menu, captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // On open the highlight anchors on the first leaf ("One"). Two ArrowDowns advance the cursor
    // One -> Two -> Three, landing on the THIRD leaf — all three are plain action leaves, so nothing
    // is skipped and nothing splits the gesture.
    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::ArrowDown);
    harness.run();

    // Enter confirms the highlighted third leaf directly (it is an action, not a submenu header), so
    // ArrowDown x2 + Enter is one continuous gesture that dispatches the third leaf's stable id.
    harness.key_press(egui::Key::Enter);
    harness.run();

    assert_eq!(
        *captured.lock().unwrap(),
        Some("triple.three"),
        "ArrowDown x2 + Enter dispatched the THIRD leaf as one gesture (no step-back)"
    );
}

#[test]
fn arrow_up_wraps_to_last_actionable_item() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    // On open the cursor anchors on the first actionable item (Pin). A single ArrowUp must WRAP to
    // the last actionable item ("Move to"), not stay put / not move to a disabled or separator entry.
    harness.key_press(egui::Key::ArrowUp);
    harness.run();

    let focused: Vec<String> = harness
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().is_focused())
        .filter_map(|n| n.accesskit_node().author_id().map(|a| a.to_owned()))
        .collect();
    assert!(
        focused.contains(&"ctx-menu.tab.move-to".to_owned()),
        "ArrowUp from the first item wraps the cursor to the last actionable item: {focused:?}"
    );
    assert!(
        captured.lock().unwrap().is_none(),
        "navigation alone fired no action"
    );
}

// ── No anonymous MenuItem nodes while the menu is open (MT-025 invariant) ────────────────────────────

#[test]
fn open_menu_items_are_all_named() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut harness = surface_harness(tab_menu(), captured.clone());
    harness.run();

    harness.get_by_label(SURFACE_LABEL).click_secondary();
    harness.run();
    harness.run();

    let menu_item_count = harness
        .root()
        .children_recursive()
        .filter(|n| format!("{:?}", n.accesskit_node().role()) == "MenuItem")
        .count();
    let named_menu_items = harness
        .root()
        .children_recursive()
        .filter(|n| {
            let ak = n.accesskit_node();
            format!("{:?}", ak.role()) == "MenuItem" && ak.author_id().is_some()
        })
        .count();
    assert_eq!(
        menu_item_count, named_menu_items,
        "every live context-menu MenuItem node carries an author_id (none anonymous)"
    );
    assert!(
        menu_item_count >= 3,
        "at least the three top-level leaves; got {menu_item_count}"
    );
}
