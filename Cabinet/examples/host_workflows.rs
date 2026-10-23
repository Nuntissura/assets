//! In-memory host contract probe: no filesystem, watcher, worker or durable-storage effects.
use cabinet::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut source = Entry::new("src", None, "Source", EntryKind::Folder);
    source.children = ChildrenState::Unloaded;
    let mut cabinet = Cabinet::new(Snapshot {
        context: Context {
            scope: "authorized-demo".into(),
            generation: 1,
            revision: 1,
        },
        entries: vec![
            source,
            Entry::new("docs", None, "Documents", EntryKind::Folder),
        ],
        hierarchy_complete: false,
    })?;
    let mut settings = cabinet.settings().clone();
    settings.pinned_folders = vec!["not-loaded-folder".into()];
    settings
        .folder_sorts
        .insert("not-loaded-folder".into(), Sort::NameAscending);
    settings.hidden_patterns = vec!["*.tmp".into()];
    settings.watcher_exclusions = vec!["build/**".into()];
    cabinet.apply_settings(settings)?;
    assert!(cabinet
        .settings()
        .folder_sorts
        .contains_key("not-loaded-folder"));
    cabinet.pin_folder("src", true)?;
    cabinet.set_folder_sort("src", Some(Sort::NameDescending))?;
    cabinet.expand("src", true)?;
    let first = cabinet.request_child_page("src")?;
    cabinet.complete_child_page(ChildPage {
        request_id: first.id,
        context: first.context,
        parent: "src".into(),
        cursor: None,
        next_cursor: Some("page-2".into()),
        revision: 2,
        entries: vec![Entry::new(
            "readme",
            Some("src".into()),
            "README.md",
            EntryKind::Leaf,
        )],
    })?;
    assert_eq!(cabinet.next_page_cursor("src"), Some("page-2"));
    let failed = cabinet.request_child_page("src")?;
    cabinet.fail_child_page(failed.id, "demo page unavailable; retry allowed".into())?;
    assert!(matches!(
        cabinet.entry("src").unwrap().children,
        ChildrenState::Failed(_)
    ));
    let retry = cabinet.request_child_page("src")?;
    assert!(
        matches!(&retry.operation, Operation::LoadPage { cursor: Some(cursor), .. } if cursor == "page-2")
    );
    cabinet.complete_child_page(ChildPage {
        request_id: retry.id,
        context: retry.context,
        parent: "src".into(),
        cursor: Some("page-2".into()),
        next_cursor: None,
        revision: 3,
        entries: vec![Entry::new(
            "scratch",
            Some("src".into()),
            "scratch.tmp",
            EntryKind::Leaf,
        )],
    })?;
    assert_eq!(
        cabinet.entry("src").unwrap().children,
        ChildrenState::Loaded
    );
    assert!(!cabinet.rows().iter().any(|row| row.id == "scratch"));
    assert!(cabinet.entry("scratch").is_some());
    assert_eq!(cabinet.settings().watcher_exclusions, vec!["build/**"]);

    let ids = vec!["readme".into()];
    let placement = Placement::Into(Some("docs".into()));
    assert_eq!(
        cabinet.validate_move(&ids, &placement),
        Err(Error::IncompleteHierarchy)
    );
    let preflight = cabinet.request(Operation::MovePreflight {
        ids: ids.clone(),
        placement: placement.clone(),
    })?;
    let mut wrong_scope = preflight.context.clone();
    wrong_scope.scope = "other-account".into();
    assert_eq!(
        cabinet.accept_move_preflight(
            preflight.id,
            MoveGrant {
                context: wrong_scope,
                ids: ids.clone(),
                placement: placement.clone(),
                token: "demo-grant".into(),
            }
        ),
        Err(Error::StaleContext)
    );
    // The in-memory host stands in for an authorized full hierarchy check; this is no durable proof.
    cabinet.accept_move_preflight(
        preflight.id,
        MoveGrant {
            context: preflight.context,
            ids: ids.clone(),
            placement: placement.clone(),
            token: "demo-grant".into(),
        },
    )?;
    let moved = cabinet.request(Operation::MoveAuthorized {
        ids,
        placement,
        token: "demo-grant".into(),
    })?;
    let mut after = cabinet.snapshot().clone();
    after.context.revision += 1;
    let entry = after.entries.iter_mut().find(|e| e.id == "readme").unwrap();
    entry.parent = Some("docs".into());
    entry.revision += 1;
    cabinet.complete(Completion {
        request_id: moved.id,
        context: moved.context,
        result: Ok(after),
        undo_token: None,
    })?;
    assert_eq!(
        cabinet.entry("readme").unwrap().parent.as_deref(),
        Some("docs")
    );

    cabinet.set_filter("unloaded-match");
    assert!(cabinet.rows().is_empty());
    let search = cabinet.request(Operation::WorkspaceSearch {
        query: "unloaded-match".into(),
    })?;
    cabinet.complete_search(SearchResult {
        request_id: search.id,
        context: search.context,
        hits: vec![SearchHit {
            id: "unloaded-match".into(),
            path: "Archive/unloaded-match.md".into(),
        }],
    })?;
    assert_eq!(cabinet.search_result().unwrap().hits.len(), 1);
    assert!(cabinet.entry("unloaded-match").is_none());
    cabinet.set_filter("");
    // This demo host now supplies its complete in-memory hierarchy for the bulk operation.
    let mut complete_tree = cabinet.snapshot().clone();
    complete_tree.context.revision += 1;
    complete_tree.hierarchy_complete = true;
    cabinet.replace(complete_tree)?;
    let bulk = cabinet.request(Operation::Delete {
        ids: vec!["readme".into(), "scratch".into()],
    })?;
    cabinet.update_bulk_progress(BulkProgress {
        request_id: bulk.id,
        context: bulk.context.clone(),
        completed: 1,
        total: 2,
        message: "One item processed; host recovery available".into(),
        recovery_token: Some("host-recovery-demo".into()),
    })?;
    cabinet.complete(Completion {
        request_id: bulk.id,
        context: bulk.context,
        result: Err("Second item failed".into()),
        undo_token: None,
    })?;
    assert_eq!(
        cabinet.bulk_progress().unwrap().recovery_token.as_deref(),
        Some("host-recovery-demo")
    );
    assert!(cabinet.pending().is_none());
    println!("In-memory host boundaries exercised: pages/retry, authorized move, workspace search, recovery handle, partial preferences and separate visibility/watcher policies. No durable effects or worker-stop proof.");
    Ok(())
}
