use super::*;
#[test]
fn host_tag_catalog_cannot_survive_workspace_generation_change() {
    let ctx = egui::Context::default();
    let mut view = CabinetView::new("catalog-test");
    let mut cabinet = fixture();
    view.set_tag_catalog(
        cabinet.snapshot().context.clone(),
        vec![Tag {
            id: "private-tag".into(),
            label: "Private tag".into(),
            color: [10, 20, 30],
        }],
    );
    frame(&ctx, &mut view, &mut cabinet, vec![]);
    assert_eq!(view.state.available_tags.len(), 1);
    let mut next = cabinet.snapshot().clone();
    next.context.generation += 1;
    cabinet.replace(next).unwrap();
    frame(&ctx, &mut view, &mut cabinet, vec![]);
    assert!(view.state.available_tags.is_empty());
    assert!(view.state.catalog_context.is_none());
}
fn fixture() -> Cabinet {
    Cabinet::new(Snapshot {
        context: Context {
            scope: "test".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries: vec![
            Entry::new("folder", None, "Folder", EntryKind::Folder),
            Entry::new("leaf", Some("folder".into()), "Leaf", EntryKind::Leaf),
        ],
    })
    .unwrap()
}
fn frame(
    ctx: &egui::Context,
    view: &mut CabinetView,
    cabinet: &mut Cabinet,
    events: Vec<egui::Event>,
) -> ViewResponse {
    let mut result = None;
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(480.0, 420.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                result = Some(view.show(ui, cabinet));
            });
        },
    );
    result.unwrap()
}
fn click(
    ctx: &egui::Context,
    view: &mut CabinetView,
    cabinet: &mut Cabinet,
    pos: egui::Pos2,
) -> ViewResponse {
    frame(
        ctx,
        view,
        cabinet,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    frame(
        ctx,
        view,
        cabinet,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
}
#[test]
fn folder_plus_opens_real_input_and_enter_emits_create_without_mutating_source() {
    let ctx = egui::Context::default();
    let mut view = CabinetView::default();
    let mut cabinet = fixture();
    let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
    let pos = out.controls["new-folder"].center();
    click(&ctx, &mut view, &mut cabinet, pos);
    let input = frame(&ctx, &mut view, &mut cabinet, vec![]);
    assert!(input.controls.contains_key("inline-edit"));
    frame(
        &ctx,
        &mut view,
        &mut cabinet,
        vec![egui::Event::Text("Created".into())],
    );
    let result = frame(
        &ctx,
        &mut view,
        &mut cabinet,
        vec![egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    assert!(
        matches!(&result.requests[0].operation,Operation::Create{name,parent:None,..} if name=="Created")
    );
    assert_eq!(cabinet.snapshot().entries.len(), 2);
}
#[test]
fn disclosure_click_expands_and_row_click_selects_stable_identity() {
    let ctx = egui::Context::default();
    let mut view = CabinetView::default();
    let mut cabinet = fixture();
    let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
    let r = out.rows["folder"];
    click(
        &ctx,
        &mut view,
        &mut cabinet,
        r.left_center() + Vec2::new(10.0, 0.0),
    );
    assert!(cabinet.expanded("folder"));
    let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
    click(&ctx, &mut view, &mut cabinet, out.rows["leaf"].center());
    assert_eq!(cabinet.selected(), vec!["leaf".to_owned()]);
}
fn drag_fixture() -> Cabinet {
    Cabinet::new(Snapshot {
        context: Context {
            scope: "test".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries: vec![
            Entry::new("source", None, "Source", EntryKind::Folder),
            Entry::new("target", None, "Target", EntryKind::Folder),
            Entry::new("leaf", Some("source".into()), "Leaf", EntryKind::Leaf),
        ],
    })
    .unwrap()
}
fn press(ctx: &egui::Context, view: &mut CabinetView, cabinet: &mut Cabinet, pos: egui::Pos2) {
    frame(
        ctx,
        view,
        cabinet,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    frame(
        ctx,
        view,
        cabinet,
        vec![egui::Event::PointerMoved(pos + Vec2::new(12.0, 0.0))],
    );
}
fn release(
    ctx: &egui::Context,
    view: &mut CabinetView,
    cabinet: &mut Cabinet,
    pos: egui::Pos2,
) -> ViewResponse {
    frame(ctx, view, cabinet, vec![egui::Event::PointerMoved(pos)]);
    frame(
        ctx,
        view,
        cabinet,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
}
#[test]
fn actual_pointer_folder_and_leaf_drag_into_folder_emit_fenced_moves() {
    for source in ["source", "leaf"] {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = drag_fixture();
        cabinet.expand("source", true).unwrap();
        let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
        let from = layout.rows[source].center();
        let to = layout.rows["target"].center();
        press(&ctx, &mut view, &mut cabinet, from);
        let hover = frame(
            &ctx,
            &mut view,
            &mut cabinet,
            vec![egui::Event::PointerMoved(to)],
        );
        assert!(hover.drop_target.as_ref().is_some_and(|d| d.valid));
        let out = release(&ctx, &mut view, &mut cabinet, to);
        assert!(
                matches!(out.requests.first().map(|r|&r.operation),Some(Operation::Move{ids,placement:Placement::Into(Some(id))}) if ids==&vec![source.to_owned()]&&id=="target"),
                "source={source} expected=Into(target) requests={:?} drop={:?} layout={:?} hover={:?} release={:?}",out.requests,out.drop_target,layout.rows,hover.rows,out.rows
            );
        assert_eq!(
            cabinet.entry(source).unwrap().parent,
            if source == "leaf" {
                Some("source".into())
            } else {
                None
            }
        );
    }
}
#[test]
fn actual_pointer_drag_supports_manual_before_after_and_root() {
    for placement in [
        Placement::Before("target".into()),
        Placement::After("target".into()),
        Placement::Into(None),
    ] {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = drag_fixture();
        cabinet.expand("source", true).unwrap();
        let source = if matches!(&placement, Placement::Into(None)) {
            "leaf"
        } else {
            "source"
        };
        let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
        let from = layout.rows[source].center();
        let to = match &placement {
            Placement::Before(_) => layout.rows["target"].center_top() + Vec2::new(0.0, 2.0),
            Placement::After(_) => layout.rows["target"].center_bottom() - Vec2::new(0.0, 2.0),
            _ => layout.controls["root-drop"].center(),
        };
        press(&ctx, &mut view, &mut cabinet, from);
        let out = release(&ctx, &mut view, &mut cabinet, to);
        assert!(
                matches!(out.requests.first().map(|r|&r.operation),Some(Operation::Move{ids,placement:p}) if ids==&vec![source.to_owned()]&&p==&placement),
                "source={source} expected={placement:?} requests={:?} drop={:?} layout={:?} release={:?} controls={:?}",out.requests,out.drop_target,layout.rows,out.rows,out.controls
            );
    }
}
#[test]
fn actual_pointer_escape_and_stale_context_cancel_without_host_mutation() {
    for stale in [false, true] {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = drag_fixture();
        let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
        let from = layout.rows["source"].center();
        let to = layout.rows["target"].center();
        press(&ctx, &mut view, &mut cabinet, from);
        if stale {
            let mut snapshot = cabinet.snapshot().clone();
            snapshot.context.generation += 1;
            cabinet.replace(snapshot).unwrap();
            frame(&ctx, &mut view, &mut cabinet, vec![]);
        } else {
            frame(
                &ctx,
                &mut view,
                &mut cabinet,
                vec![egui::Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
        }
        let out = release(&ctx, &mut view, &mut cabinet, to);
        assert!(out.requests.is_empty());
        assert!(cabinet.pending().is_none());
        assert_eq!(cabinet.entry("source").unwrap().parent, None);
    }
}
#[test]
fn filtering_away_inline_rename_cancels_draft_and_reenables_tree_navigation() {
    let ctx = egui::Context::default();
    let mut view = CabinetView::default();
    let mut cabinet = fixture();
    frame(&ctx, &mut view, &mut cabinet, vec![]);
    view.begin_rename(&cabinet, "folder".into());
    frame(&ctx, &mut view, &mut cabinet, vec![]);
    cabinet.set_filter("no-match");
    let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
    assert!(!out.controls.contains_key("inline-edit"));
    assert!(view.state.edit.is_none());
    assert!(view.state.text.is_empty());
    assert!(out.error.as_ref().is_some_and(|e| e.contains("hidden")));
    assert!(out.requests.is_empty());
    cabinet.set_filter("");
    let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
    click(&ctx, &mut view, &mut cabinet, out.rows["folder"].center());
    let out = frame(
        &ctx,
        &mut view,
        &mut cabinet,
        vec![egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    assert!(matches!(&out.requests[0].operation,Operation::Open{id} if id=="folder"));
}
#[test]
fn lazy_folder_state_labels_include_actual_failure_reason() {
    let mut entry = Entry::new("f", None, "Folder", EntryKind::Folder);
    for (state, suffix) in [
        (ChildrenState::Unloaded, "Not loaded"),
        (ChildrenState::Loading, "Loading…"),
        (
            ChildrenState::Failed("Access expired".into()),
            "Failed: Access expired",
        ),
    ] {
        entry.children = state;
        assert_eq!(presentation_label(&entry), format!("Folder · {suffix}"));
    }
}
