mod model;
pub mod navigation;

pub use model::{ContextMenu, ContextMenuItem, MenuItemKind};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{first_navigable, step_highlight};

    fn sample_menu() -> ContextMenu {
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

    /// The typed model preserves item identity, kind, enabled state, and submenu nesting.
    #[test]
    fn model_carries_typed_items() {
        let menu = sample_menu();
        let entries = menu.entries();
        assert_eq!(
            entries.len(),
            5,
            "two actions + separator + disabled + submenu"
        );

        assert_eq!(entries[0].id, "tab.pin");
        assert_eq!(entries[0].shortcut_hint, Some("Ctrl+Shift+P"));
        assert!(entries[0].enabled);
        assert!(matches!(entries[0].kind, MenuItemKind::Action));

        assert!(matches!(entries[2].kind, MenuItemKind::Separator));

        assert!(!entries[3].enabled, "Close Others is disabled");
        assert_eq!(entries[3].disabled_reason, Some("Needs more than one tab"));

        match &entries[4].kind {
            MenuItemKind::Submenu(children) => {
                assert_eq!(children.len(), 2, "one-level submenu has two children");
                assert_eq!(children[0].id, "tab.move-to.pane-b");
            }
            other => panic!("expected submenu, got {other:?}"),
        }
    }

    /// AccessKit author_ids are the stable `ctx-menu.{id}` form and unique across the whole menu
    /// (including submenu children) — the out-of-process address + the MT-025 gate key.
    #[test]
    fn author_ids_are_namespaced_and_unique() {
        let menu = sample_menu();
        let mut ids = Vec::new();
        for item in menu.entries() {
            if matches!(item.kind, MenuItemKind::Separator) {
                continue;
            }
            ids.push(item.author_id());
            if let MenuItemKind::Submenu(children) = &item.kind {
                for child in children {
                    ids.push(child.author_id());
                }
            }
        }
        assert!(ids.contains(&"ctx-menu.tab.close".to_owned()));
        assert!(ids.contains(&"ctx-menu.tab.move-to.pane-b".to_owned()));
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "every context-menu author_id is unique: {ids:?}"
        );
    }

    /// Keyboard nav anchors on the first ACTIONABLE item (index 0 here is an enabled action).
    #[test]
    fn first_navigable_picks_first_actionable() {
        let menu = sample_menu();
        // sample: [pin(action), close(action), separator, close-others(disabled), move-to(submenu)]
        assert_eq!(first_navigable(menu.entries()), Some(0));

        // If the first entries are a separator + disabled, the anchor skips to the first live one.
        let menu = ContextMenu::new("s")
            .separator()
            .item(ContextMenuItem::action("a.x", "X").disabled("nope"))
            .item(ContextMenuItem::action("a.y", "Y"));
        assert_eq!(
            first_navigable(menu.entries()),
            Some(2),
            "skips separator + disabled"
        );
    }

    /// ArrowDown/ArrowUp skip separators + disabled items and wrap at both ends.
    #[test]
    fn step_highlight_skips_and_wraps() {
        let menu = sample_menu();
        let items = menu.entries();
        // Indices: 0 pin, 1 close, 2 separator, 3 close-others(disabled), 4 move-to(submenu).
        // Navigable set in order: 0 -> 1 -> 4 -> (wrap) 0.
        assert_eq!(step_highlight(items, 0, true), 1, "down: pin -> close");
        assert_eq!(
            step_highlight(items, 1, true),
            4,
            "down: close -> move-to (skips separator + disabled)"
        );
        assert_eq!(
            step_highlight(items, 4, true),
            0,
            "down wraps: move-to -> pin"
        );

        // ArrowUp is the mirror, including wrap from the first item to the last navigable.
        assert_eq!(
            step_highlight(items, 0, false),
            4,
            "up wraps: pin -> move-to"
        );
        assert_eq!(
            step_highlight(items, 4, false),
            1,
            "up: move-to -> close (skips disabled + separator)"
        );
        assert_eq!(step_highlight(items, 1, false), 0, "up: close -> pin");
    }

    /// A menu with a single navigable item keeps the highlight on it (wrap to self, no panic).
    #[test]
    fn step_highlight_single_item_is_stable() {
        let menu = ContextMenu::new("s")
            .item(ContextMenuItem::action("a.only", "Only"))
            .separator();
        let items = menu.entries();
        assert_eq!(step_highlight(items, 0, true), 0);
        assert_eq!(step_highlight(items, 0, false), 0);
    }
}
