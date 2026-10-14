use cabinet::*;

fn entry(id: &str, parent: Option<&str>, kind: EntryKind) -> Entry {
    Entry::new(id, parent.map(str::to_owned), id, kind)
}
fn snapshot(entries: Vec<Entry>) -> Snapshot {
    Snapshot {
        context: Context {
            scope: "workspace-a".into(),
            generation: 1,
            revision: 1,
        },
        entries,
        hierarchy_complete: true,
    }
}
fn tree() -> Snapshot {
    snapshot(vec![
        entry("a", None, EntryKind::Folder),
        entry("a1", Some("a"), EntryKind::Leaf),
        entry("a2", Some("a"), EntryKind::Folder),
        entry("a21", Some("a2"), EntryKind::Leaf),
        entry("b", None, EntryKind::Folder),
        entry("b1", Some("b"), EntryKind::Leaf),
    ])
}
mod independent_native_boundaries {
    use super::*;
    use cabinet_egui::{CabinetView, ViewResponse};
    use egui::{Context as GuiContext, Event, Key, PointerButton, Pos2, RawInput, Rect, Vec2};
    fn frame(
        ctx: &GuiContext,
        view: &mut CabinetView,
        c: &mut Cabinet,
        events: Vec<Event>,
    ) -> ViewResponse {
        let mut out = None;
        let _ = ctx.run(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(480.0, 400.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = Some(view.show(ui, c));
                });
            },
        );
        out.unwrap()
    }
    fn key(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    fn click(ctx: &GuiContext, view: &mut CabinetView, c: &mut Cabinet, pos: Pos2) {
        frame(
            ctx,
            view,
            c,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            c,
            vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
    }
    #[test]
    fn rename_draft_does_not_overwrite_external_revision() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(tree()).unwrap();
        let out = frame(&ctx, &mut view, &mut c, vec![]);
        click(&ctx, &mut view, &mut c, out.rows["a"].center());
        frame(&ctx, &mut view, &mut c, vec![key(Key::F2, true)]);
        let editing = frame(&ctx, &mut view, &mut c, vec![key(Key::F2, false)]);
        assert!(
            editing.controls.contains_key("inline-edit"),
            "precondition: real rename input mounted"
        );
        let mut refreshed = tree();
        refreshed.context.revision = 2;
        refreshed.entries[0].revision = 1;
        refreshed.entries[0].label = "external rename".into();
        c.replace(refreshed).unwrap();
        let out = frame(&ctx, &mut view, &mut c, vec![key(Key::Enter, true)]);
        assert!(out.requests.is_empty(), "stale draft must be cancelled or rejected, never rebound to the external writer's revision");
        assert_eq!(c.entry("a").unwrap().label, "external rename");
    }

    #[test]
    fn delete_confirmation_does_not_rebind_after_external_revision() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(tree()).unwrap();
        let out = frame(&ctx, &mut view, &mut c, vec![]);
        click(&ctx, &mut view, &mut c, out.rows["a"].center());
        frame(&ctx, &mut view, &mut c, vec![key(Key::Delete, true)]);
        let confirmation = frame(&ctx, &mut view, &mut c, vec![key(Key::Delete, false)]);
        assert!(
            confirmation.controls.contains_key("confirm-delete"),
            "precondition: real destructive confirmation mounted"
        );
        assert!(c.pending().is_none());
        let mut refreshed = tree();
        refreshed.context.revision = 2;
        refreshed.entries[0].revision = 1;
        refreshed.entries[0].label = "Changed by another actor".into();
        c.replace(refreshed.clone()).unwrap();
        let observed = frame(&ctx, &mut view, &mut c, vec![]);
        assert!(
            !observed.controls.contains_key("confirm-delete"),
            "a confirmation of older state must expire on canonical refresh"
        );
        assert!(observed.requests.is_empty());
        assert!(observed
            .error
            .as_ref()
            .is_some_and(|error| error.contains("delete confirmation cancelled")));
        let after_enter = frame(&ctx, &mut view, &mut c, vec![key(Key::Enter, true)]);
        assert!(
            !after_enter
                .requests
                .iter()
                .any(|request| matches!(request.operation, Operation::Delete { .. })),
            "Enter after an expired confirmation must never issue deletion"
        );
        assert_eq!(c.snapshot(), &refreshed);
    }
}
