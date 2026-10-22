use cabinet::interaction::{Edit, InteractionState};
use cabinet::*;

fn fixture() -> Cabinet {
    Cabinet::new(Snapshot {
        context: Context {
            scope: "host".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries: vec![Entry::new("folder", None, "Folder", EntryKind::Folder)],
    })
    .unwrap()
}

#[test]
fn host_edit_rejects_invalid_input_retains_draft_and_emits_valid_request() {
    let mut cabinet = fixture();
    let mut state = InteractionState::default();
    state.sync(&cabinet);
    state.begin_create(Some("folder".into()));
    assert_eq!(state.submit_edit(&mut cabinet), Err(Error::InvalidName));
    assert!(matches!(state.edit, Some(Edit::Create { .. })));
    assert!(cabinet.pending().is_none());
    state.text = "New folder".into();
    let request = state.submit_edit(&mut cabinet).unwrap().unwrap();
    assert_eq!(
        request.operation,
        Operation::Create {
            parent: Some("folder".into()),
            name: "New folder".into(),
            kind: EntryKind::Folder,
        }
    );
    assert!(state.edit.is_none());
    assert!(state.text.is_empty());
    assert_eq!(cabinet.snapshot().entries.len(), 1);
}

#[test]
fn canonical_refresh_cancels_rename_before_a_host_can_submit_stale_text() {
    let mut cabinet = fixture();
    let mut state = InteractionState::default();
    state.sync(&cabinet);
    state.begin_rename(&cabinet, "folder".into());
    state.text = "Stale draft".into();
    let mut next = cabinet.snapshot().clone();
    next.context.revision += 1;
    next.entries[0].revision += 1;
    next.entries[0].label = "Canonical name".into();
    cabinet.replace(next).unwrap();
    assert!(state.sync(&cabinet));
    assert!(state.edit.is_none());
    assert!(state.text.is_empty());
    assert!(state.submit_edit(&mut cabinet).unwrap().is_none());
    assert_eq!(cabinet.entry("folder").unwrap().label, "Canonical name");
}

#[test]
fn account_switch_erases_host_drafts_queries_and_tag_catalog() {
    let mut cabinet = fixture();
    let mut state = InteractionState::default();
    state.sync(&cabinet);
    state.set_tag_catalog(
        cabinet.snapshot().context.clone(),
        vec![Tag {
            id: "private".into(),
            label: "Private".into(),
            color: [1, 2, 3],
        }],
    );
    state.quick_path = "Private path".into();
    state.workspace_query = "Private query".into();
    state.host_search = true;
    state.begin_rename(&cabinet, "folder".into());
    let mut next = cabinet.snapshot().clone();
    next.context.scope = "other-host".into();
    cabinet.replace(next).unwrap();
    state.sync(&cabinet);
    assert!(state.available_tags.is_empty());
    assert!(state.catalog_context.is_none());
    assert!(state.quick_path.is_empty());
    assert!(state.workspace_query.is_empty());
    assert!(!state.host_search);
    assert!(state.edit.is_none());
}

#[test]
fn incomplete_move_emits_preflight_and_lazy_expand_emits_load_without_io() {
    let mut cabinet = fixture();
    let mut next = cabinet.snapshot().clone();
    next.context.revision += 1;
    next.hierarchy_complete = false;
    next.entries[0].children = ChildrenState::Unloaded;
    cabinet.replace(next).unwrap();
    let mut state = InteractionState::default();
    let request = state
        .request(
            &mut cabinet,
            Operation::Move {
                ids: vec!["folder".into()],
                placement: Placement::Into(None),
            },
        )
        .unwrap();
    assert!(matches!(request.operation, Operation::MovePreflight { .. }));
    cabinet.abandon_pending(request.id).unwrap();
    let load = state.expand(&mut cabinet, "folder", true).unwrap().unwrap();
    assert!(matches!(load.operation, Operation::LoadPage { .. }));
    assert!(cabinet.expanded("folder"));
    assert_eq!(
        cabinet.snapshot().entries[0].children,
        ChildrenState::Unloaded
    );
}
