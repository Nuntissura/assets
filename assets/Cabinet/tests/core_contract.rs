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
fn ids(c: &Cabinet) -> Vec<String> {
    c.rows().iter().map(|r| r.id.clone()).collect()
}
fn plain() -> SelectionModifiers {
    SelectionModifiers::default()
}
fn acknowledge(c: &mut Cabinet, request: &Request, next: Snapshot, token: Option<&str>) {
    c.complete(Completion {
        request_id: request.id,
        context: request.context.clone(),
        result: Ok(next),
        undo_token: token.map(str::to_owned),
    })
    .unwrap();
}

#[test]
fn arbitrary_depth_reveal_keeps_every_ancestor_and_exact_depth() {
    let mut entries = vec![entry("n0", None, EntryKind::Folder)];
    for i in 1..=64 {
        entries.push(entry(
            &format!("n{i}"),
            Some(&format!("n{}", i - 1)),
            if i == 64 {
                EntryKind::Leaf
            } else {
                EntryKind::Folder
            },
        ));
    }
    let mut c = Cabinet::new(snapshot(entries)).unwrap();
    c.reveal("n64").unwrap();
    let rows = c.rows();
    assert_eq!(
        rows.len(),
        65,
        "there must be no inherited nesting truncation"
    );
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.id, format!("n{i}"));
        assert_eq!(row.depth, i);
    }
}

#[test]
fn invalid_refreshes_are_atomic_and_do_not_destroy_navigation() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.expand("a", true).unwrap();
    c.select("a1", plain()).unwrap();
    let original = c.snapshot().clone();
    let original_rows = ids(&c);
    let mut duplicate = original.clone();
    duplicate.entries.push(duplicate.entries[0].clone());
    let mut missing = original.clone();
    missing.entries[1].parent = Some("absent".into());
    let mut cyclic = original.clone();
    cyclic.entries[0].parent = Some("a2".into());
    for invalid in [duplicate, missing, cyclic] {
        assert!(c.replace(invalid).is_err());
        assert_eq!(c.snapshot(), &original);
        assert_eq!(ids(&c), original_rows);
        assert_eq!(c.selected(), vec!["a1"]);
        assert!(c.expanded("a"));
    }
}

#[test]
fn refresh_retains_ids_but_removes_deleted_selection() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.expand("a", true).unwrap();
    c.select("a1", plain()).unwrap();
    let mut next = tree();
    next.context.revision = 2;
    next.entries[0].label = "Renamed".into();
    next.entries[0].revision = 1;
    c.replace(next.clone()).unwrap();
    assert!(c.expanded("a"));
    assert_eq!(c.selected(), vec!["a1"]);
    next.context.revision = 3;
    next.entries.retain(|e| e.id != "a1");
    c.replace(next).unwrap();
    assert!(c.selected().is_empty());
    assert!(c.focused().is_none() || c.entry(c.focused().unwrap()).is_some());
}

#[test]
fn context_switch_drops_private_state_and_rejects_old_completion() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.expand("a", true).unwrap();
    c.select("a1", plain()).unwrap();
    c.set_filter("a");
    let request = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "next".into(),
        })
        .unwrap();
    let mut other = tree();
    other.context.scope = "workspace-b".into();
    other.context.generation = 2;
    c.replace(other.clone()).unwrap();
    assert!(c.selected().is_empty());
    assert_eq!(c.filter(), "");
    assert!(!c.expanded("a"));
    assert!(c.pending().is_none());
    assert!(!c.can_undo());
    assert!(!c.can_redo());
    assert!(c
        .complete(Completion {
            request_id: request.id,
            context: request.context,
            result: Ok(tree()),
            undo_token: Some("old".into())
        })
        .is_err());
    assert_eq!(c.snapshot(), &other);
}

#[test]
fn keyboard_range_and_toggle_selection_use_visible_order() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.expand("a", true).unwrap();
    c.select("a2", plain()).unwrap();
    c.navigate(
        Navigation::Down,
        SelectionModifiers {
            range: true,
            toggle: false,
        },
    )
    .unwrap();
    assert_eq!(c.selected(), vec!["a2", "a1"]);
    assert_eq!(c.focused(), Some("a1"));
    c.select(
        "b",
        SelectionModifiers {
            toggle: true,
            range: false,
        },
    )
    .unwrap();
    assert_eq!(c.selected(), vec!["a2", "a1", "b"]);
    c.select(
        "a1",
        SelectionModifiers {
            toggle: true,
            range: false,
        },
    )
    .unwrap();
    assert_eq!(c.selected(), vec!["a2", "b"]);
    c.navigate(Navigation::Home, plain()).unwrap();
    assert_eq!(c.focused(), Some("a"));
    c.navigate(Navigation::End, plain()).unwrap();
    assert_eq!(c.focused(), Some("b"));
}

#[test]
fn filter_includes_ancestors_without_mutating_expansion() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.set_filter("a21");
    assert_eq!(ids(&c), vec!["a", "a2", "a21"]);
    assert_eq!(
        c.rows().iter().map(|r| r.depth).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    c.set_filter("");
    assert_eq!(ids(&c), vec!["a", "b"]);
    assert!(!c.expanded("a"));
    assert!(!c.expanded("a2"));
}

#[test]
fn sort_changes_presentation_only_and_preserves_hierarchy() {
    let mut s = tree();
    s.entries[0].order = 9;
    s.entries[4].order = 1;
    let mut c = Cabinet::new(s.clone()).unwrap();
    assert_eq!(ids(&c), vec!["b", "a"]);
    c.set_sort(Sort::NameAscending);
    assert_eq!(ids(&c), vec!["a", "b"]);
    c.set_sort(Sort::NameDescending);
    assert_eq!(ids(&c), vec!["b", "a"]);
    assert_eq!(c.snapshot(), &s, "sorting must not author host data");
}

#[test]
fn moves_fail_closed_on_cycles_permissions_and_incomplete_hierarchy() {
    let c = Cabinet::new(tree()).unwrap();
    assert!(c
        .validate_move(&["a".into()], &Placement::Into(Some("a2".into())))
        .is_err());
    let mut normalized = Cabinet::new(tree()).unwrap();
    let normalized_request = normalized
        .request(Operation::Move {
            ids: vec!["a".into(), "a2".into()],
            placement: Placement::Into(Some("b".into())),
        })
        .unwrap();
    assert_eq!(
        normalized_request.operation,
        Operation::Move {
            ids: vec!["a".into()],
            placement: Placement::Into(Some("b".into()))
        },
        "overlapping subtree selections must not double-move descendants"
    );
    assert!(c
        .validate_move(&["a1".into()], &Placement::Into(Some("b1".into())))
        .is_err());
    assert!(c
        .validate_move(
            &["a1".into(), "a21".into()],
            &Placement::Into(Some("b".into()))
        )
        .is_ok());
    let mut denied = tree();
    denied.entries[1].capabilities.move_entry = false;
    assert!(Cabinet::new(denied)
        .unwrap()
        .validate_move(
            &["a1".into(), "a21".into()],
            &Placement::Into(Some("b".into()))
        )
        .is_err());
    let mut denied_target = tree();
    denied_target.entries[4].capabilities.accept_children = false;
    assert!(Cabinet::new(denied_target)
        .unwrap()
        .validate_move(&["a1".into()], &Placement::Into(Some("b".into())))
        .is_err());
    let mut incomplete = tree();
    incomplete.hierarchy_complete = false;
    assert!(Cabinet::new(incomplete)
        .unwrap()
        .validate_move(&["a".into()], &Placement::Into(Some("b".into())))
        .is_err());
}

#[test]
fn same_parent_reorder_is_a_request_and_keeps_target_revision() {
    let mut s = tree();
    s.entries[1].revision = 7;
    let mut anchor = entry("a3", Some("a"), EntryKind::Leaf);
    anchor.revision = 9;
    s.entries.push(anchor);
    let mut c = Cabinet::new(s.clone()).unwrap();
    let req = c
        .request(Operation::Move {
            ids: vec!["a1".into()],
            placement: Placement::After("a3".into()),
        })
        .unwrap();
    assert_eq!(req.expected_revisions.get("a1"), Some(&7));
    assert_eq!(
        req.expected_revisions.get("a3"),
        Some(&9),
        "ordering anchor must participate in concurrency checks"
    );
    assert_eq!(c.snapshot(), &s, "requests never pretend a move persisted");
}

#[test]
fn failed_write_preserves_data_and_does_not_invent_undo() {
    let mut c = Cabinet::new(tree()).unwrap();
    let before = c.snapshot().clone();
    let req = c
        .request(Operation::Delete {
            ids: vec!["a".into()],
        })
        .unwrap();
    assert!(c
        .request(Operation::Rename {
            id: "b".into(),
            name: "B".into()
        })
        .is_err());
    c.complete(Completion {
        request_id: req.id,
        context: req.context,
        result: Err("permission revoked".into()),
        undo_token: None,
    })
    .unwrap();
    assert_eq!(c.snapshot(), &before);
    assert!(c.pending().is_none());
    assert!(c.error().is_some());
    assert!(!c.can_undo());
    assert!(!c.can_redo());
}

#[test]
fn undo_redo_are_host_acknowledged_compensations() {
    let mut c = Cabinet::new(tree()).unwrap();
    let original = tree();
    let req = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "renamed".into(),
        })
        .unwrap();
    let mut renamed = original.clone();
    renamed.context.revision = 2;
    renamed.entries[0].label = "renamed".into();
    renamed.entries[0].revision = 1;
    acknowledge(&mut c, &req, renamed.clone(), Some("undo-rename"));
    assert!(c.can_undo());
    let undo = c.request_undo().unwrap();
    assert_eq!(
        undo.operation,
        Operation::Undo {
            token: "undo-rename".into()
        }
    );
    assert_eq!(c.snapshot(), &renamed);
    let mut restored = original;
    restored.context.revision = 3;
    restored.entries[0].revision = 2;
    acknowledge(&mut c, &undo, restored.clone(), Some("redo-rename"));
    assert!(c.can_redo());
    let redo = c.request_redo().unwrap();
    assert_eq!(
        redo.operation,
        Operation::Redo {
            token: "redo-rename".into()
        }
    );
    assert_eq!(c.snapshot(), &restored);
    renamed.context.revision = 4;
    renamed.entries[0].revision = 3;
    acknowledge(&mut c, &redo, renamed, Some("undo-again"));
    assert!(c.can_undo());
    assert!(!c.can_redo());
}

#[test]
fn stale_revision_completion_cannot_overwrite_newer_canonical_refresh() {
    let mut c = Cabinet::new(tree()).unwrap();
    let req = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "late".into(),
        })
        .unwrap();
    let mut newer = tree();
    newer.context.revision = 5;
    newer.entries[0].label = "external edit".into();
    newer.entries[0].revision = 1;
    c.replace(newer.clone()).unwrap();
    let mut stale = tree();
    stale.context.revision = 2;
    stale.entries[0].label = "late".into();
    assert!(c
        .complete(Completion {
            request_id: req.id,
            context: req.context,
            result: Ok(stale),
            undo_token: Some("stale-token".into())
        })
        .is_err());
    assert_eq!(c.snapshot(), &newer);
    assert!(!c.can_undo());
}

#[test]
fn snapshots_with_same_context_revision_cannot_change_payload() {
    let mut c = Cabinet::new(tree()).unwrap();
    let before = c.snapshot().clone();
    let mut divergent = before.clone();
    divergent.entries[0].label = "unversioned mutation".into();
    assert!(c.replace(divergent).is_err());
    assert_eq!(c.snapshot(), &before);
    let mut unversioned_entry = before.clone();
    unversioned_entry.context.revision = 2;
    unversioned_entry.entries[0].label = "missing entry revision".into();
    assert!(c.replace(unversioned_entry).is_err());
    assert_eq!(c.snapshot(), &before);
    let mut lower_entry_revision = before.clone();
    lower_entry_revision.context.revision = 2;
    lower_entry_revision.entries[0].revision = 7;
    c.replace(lower_entry_revision.clone()).unwrap();
    let mut stale_entry = lower_entry_revision.clone();
    stale_entry.context.revision = 3;
    stale_entry.entries[0].revision = 6;
    assert!(c.replace(stale_entry).is_err());
    assert_eq!(c.snapshot(), &lower_entry_revision);
}

#[test]
fn recursive_delete_cannot_bypass_a_descendant_permission() {
    let mut s = tree();
    s.entries[3].capabilities.delete = false;
    let mut c = Cabinet::new(s.clone()).unwrap();
    assert!(c
        .request(Operation::Delete {
            ids: vec!["a".into()]
        })
        .is_err());
    assert!(c.pending().is_none());
    assert_eq!(c.snapshot(), &s);
}

#[test]
fn failed_undo_keeps_original_host_token_for_retry() {
    let mut c = Cabinet::new(tree()).unwrap();
    let req = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "renamed".into(),
        })
        .unwrap();
    let mut next = tree();
    next.context.revision = 2;
    next.entries[0].label = "renamed".into();
    next.entries[0].revision = 1;
    acknowledge(&mut c, &req, next.clone(), Some("compensate"));
    let undo = c.request_undo().unwrap();
    c.complete(Completion {
        request_id: undo.id,
        context: undo.context,
        result: Err("temporary failure".into()),
        undo_token: None,
    })
    .unwrap();
    assert_eq!(c.snapshot(), &next);
    assert!(c.can_undo());
    assert!(!c.can_redo());
    assert_eq!(
        c.request_undo().unwrap().operation,
        Operation::Undo {
            token: "compensate".into()
        }
    );
}

#[test]
fn moving_folder_cannot_bypass_unselected_descendant_permission() {
    let mut s = tree();
    s.entries[3].capabilities.move_entry = false;
    let mut c = Cabinet::new(s.clone()).unwrap();
    assert!(c
        .request(Operation::Move {
            ids: vec!["a".into()],
            placement: Placement::Into(Some("b".into()))
        })
        .is_err());
    assert_eq!(c.snapshot(), &s);
    assert!(c.pending().is_none());
}

#[test]
fn invalid_success_can_be_abandoned_without_claiming_host_effects() {
    let mut c = Cabinet::new(tree()).unwrap();
    let before = c.snapshot().clone();
    let req = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "new".into(),
        })
        .unwrap();
    let mut invalid = tree();
    invalid.context.revision = 2;
    invalid.entries.push(invalid.entries[0].clone());
    assert!(c
        .complete(Completion {
            request_id: req.id,
            context: req.context.clone(),
            result: Ok(invalid),
            undo_token: Some("untrusted".into())
        })
        .is_err());
    assert_eq!(c.snapshot(), &before);
    assert_eq!(c.pending(), Some(&req));
    assert!(c.error().is_some());
    assert!(c.abandon_pending(req.id + 1).is_err());
    assert_eq!(c.pending(), Some(&req));
    c.abandon_pending(req.id).unwrap();
    assert!(c.pending().is_none());
    assert_eq!(c.snapshot(), &before);
    assert!(!c.can_undo());
    assert!(c.request(Operation::Open { id: "a1".into() }).is_ok());
}

#[test]
fn filter_does_not_leave_invisible_entries_in_action_selection() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.select("a", plain()).unwrap();
    c.select(
        "b",
        SelectionModifiers {
            toggle: true,
            range: false,
        },
    )
    .unwrap();
    c.set_filter("b");
    let visible: Vec<_> = ids(&c);
    assert!(
        c.selected().iter().all(|id| visible.contains(id)),
        "drag/delete selection must not silently include entries hidden by the active filter"
    );
}

#[test]
fn sorted_views_reject_manual_sibling_reorder_but_allow_folder_destination() {
    let mut c = Cabinet::new(tree()).unwrap();
    c.set_sort(Sort::NameAscending);
    assert!(
        c.validate_move(&["a".into()], &Placement::After("b".into()))
            .is_err(),
        "a between-row marker must not claim placement the active sort cannot preserve"
    );
    assert!(c
        .validate_move(&["a1".into()], &Placement::Into(Some("b".into())))
        .is_ok());
}

#[test]
fn folders_first_manual_view_rejects_incompatible_cross_kind_marker() {
    let c = Cabinet::new(tree()).unwrap();
    assert!(c.validate_move(&["a1".into()], &Placement::Before("a2".into())).is_err(), "manual order still groups folders before leaves, so a leaf-before-folder marker is misleading");
}

#[test]
fn canonical_refresh_reports_superseded_host_operation_uncertainty() {
    let mut c = Cabinet::new(tree()).unwrap();
    let req = c
        .request(Operation::Rename {
            id: "a".into(),
            name: "in flight".into(),
        })
        .unwrap();
    let mut next = tree();
    next.context.revision = 2;
    next.entries[4].revision = 1;
    next.entries[4].label = "other actor".into();
    c.replace(next.clone()).unwrap();
    assert_eq!(c.snapshot(), &next);
    assert!(c.pending().is_none());
    assert!(c.error().is_some(), "dropping the pending receipt must not silently imply that the host operation was cancelled");
    assert!(c
        .complete(Completion {
            request_id: req.id,
            context: req.context,
            result: Err("late failure".into()),
            undo_token: None
        })
        .is_err());
}

#[test]
fn multi_entry_payload_preserves_display_order_instead_of_identifier_order() {
    let mut first = entry("z-id", None, EntryKind::Folder);
    first.label = "Alpha".into();
    let mut second = entry("a-id", None, EntryKind::Folder);
    second.label = "Zulu".into();
    let mut c = Cabinet::new(snapshot(vec![second, first])).unwrap();
    c.set_sort(Sort::NameAscending);
    c.select("z-id", plain()).unwrap();
    c.select(
        "a-id",
        SelectionModifiers {
            toggle: true,
            range: false,
        },
    )
    .unwrap();
    assert_eq!(c.selected(), vec!["z-id", "a-id"]);
    let req = c
        .request(Operation::Move {
            ids: c.selected(),
            placement: Placement::Into(None),
        })
        .unwrap();
    assert!(
        matches!(req.operation, Operation::Move { ids, .. } if ids == vec!["z-id", "a-id"]),
        "multi-drop must preserve the user's displayed ordering"
    );
}
