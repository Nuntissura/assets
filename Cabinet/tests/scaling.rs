use cabinet::*;

fn wide(size: usize) -> Snapshot {
    let mut entries = vec![Entry::new("folder", None, "Folder", EntryKind::Folder)];
    entries.extend((0..size).map(|i| {
        let mut e = Entry::new(
            format!("leaf-{i}"),
            Some("folder".into()),
            format!("Label-{i:06}"),
            EntryKind::Leaf,
        );
        e.order = i as i64;
        e
    }));
    Snapshot {
        context: Context {
            scope: "scaling-tests".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries,
    }
}
fn ids(c: &Cabinet) -> Vec<String> {
    c.rows().into_iter().map(|row| row.id).collect()
}

fn preview_request(id: u64) -> peak::PreviewRequest {
    peak::PreviewRequest {
        id,
        key: peak::Key {
            scope: "owner-a".into(),
            generation: 1,
            context_revision: 1,
            entry_id: "occurrence".into(),
            resource_id: "resource".into(),
            entry_revision: 1,
        },
        name: "image.png".into(),
        max_input_bytes: 1024,
        max_text_lines: 10,
        max_output_pixels: 16,
    }
}

#[test]
fn scheduler_stop_acknowledgement_is_required_before_reusing_worker_capacity() {
    let mut s = peak::PreviewScheduler::new(peak::SchedulerSettings {
        max_concurrent: 1,
        ..Default::default()
    })
    .unwrap();
    s.enqueue(preview_request(1), peak::Settings::default())
        .unwrap();
    let running = s.take_ready().pop().unwrap();
    assert!(
        !s.acknowledge_cancelled(running.token),
        "a healthy running worker cannot be released by cancellation acknowledgement"
    );
    s.cancel_all();
    assert_eq!(s.take_cancellations(), [running.token]);
    assert!(
        s.take_cancellations().is_empty(),
        "cancellation transport is emitted once"
    );
    s.enqueue(preview_request(2), peak::Settings::default())
        .unwrap();
    assert!(
        s.take_ready().is_empty(),
        "cancel intent does not prove the host worker stopped"
    );
    assert!(!s.acknowledge_cancelled(running.token + 10));
    assert!(s.acknowledge_cancelled(running.token));
    assert!(!s.acknowledge_cancelled(running.token));
    let next = s.take_ready().pop().unwrap();
    assert_eq!(next.request.id, 2);
    assert!(s
        .finish(
            running.token,
            peak::Content::Text {
                body: "late private content".into(),
                truncated: false
            }
        )
        .is_none());
    assert_eq!(
        s.diagnostics().active,
        1,
        "late result cannot release a replacement worker"
    );
}

#[test]
fn scheduler_rejects_malformed_output_and_never_replays_it_from_cache() {
    let mut s = peak::PreviewScheduler::default();
    s.enqueue(preview_request(1), peak::Settings::default())
        .unwrap();
    let job = s.take_ready().pop().unwrap();
    let completion = s
        .finish(
            job.token,
            peak::Content::Image {
                width: 4,
                height: 4,
                rgba: vec![0; 3],
            },
        )
        .unwrap();
    assert!(matches!(completion.content, peak::Content::Failed { .. }));
    assert_eq!(s.diagnostics().cached, 0);
    assert!(matches!(
        s.enqueue(preview_request(2), peak::Settings::default())
            .unwrap()
            .0,
        peak::Submission::Queued { .. }
    ));
}

#[test]
fn scheduler_cache_replay_fences_every_occurrence_identity_and_budget() {
    for changed_field in 0..7 {
        let mut s = peak::PreviewScheduler::default();
        let original = preview_request(1);
        s.enqueue(original.clone(), peak::Settings::default())
            .unwrap();
        let job = s.take_ready().pop().unwrap();
        s.finish(
            job.token,
            peak::Content::Text {
                body: "private".into(),
                truncated: false,
            },
        )
        .unwrap();
        let mut next = original;
        next.id = 2;
        match changed_field {
            0 => next.key.scope = "owner-b".into(),
            1 => next.key.generation += 1,
            2 => next.key.context_revision += 1,
            3 => next.key.entry_id = "other-occurrence".into(),
            4 => next.key.resource_id = "other-resource".into(),
            5 => next.key.entry_revision += 1,
            _ => next.max_input_bytes /= 2,
        }
        let (submission, replay) = s.enqueue(next, peak::Settings::default()).unwrap();
        assert!(
            matches!(submission, peak::Submission::Queued { .. }),
            "cache fence {changed_field} was ignored"
        );
        assert!(replay.is_none());
    }
}

#[test]
fn scheduler_configuration_rejection_is_atomic_and_shrinking_cache_evicts() {
    let mut s = peak::PreviewScheduler::default();
    for id in 1..=2 {
        let mut request = preview_request(id);
        request.key.entry_revision = id;
        s.enqueue(request, peak::Settings::default()).unwrap();
    }
    let before = s.settings().clone();
    let diagnostics = s.diagnostics();
    assert!(s
        .configure(peak::SchedulerSettings {
            max_queued: 1,
            ..Default::default()
        })
        .is_err());
    assert_eq!(s.settings(), &before);
    assert_eq!(s.diagnostics(), diagnostics);
    for job in s.take_ready() {
        s.finish(
            job.token,
            peak::Content::Text {
                body: "payload".into(),
                truncated: false,
            },
        )
        .unwrap();
    }
    assert_eq!(s.diagnostics().cached, 2);
    s.configure(peak::SchedulerSettings {
        max_cache_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(s.diagnostics().cached, 0);
    assert_eq!(s.diagnostics().cache_bytes, 0);
}

#[test]
fn scheduler_scope_change_erases_cache_and_late_private_jobs_cannot_repopulate_it() {
    for generation_change in [false, true] {
        let mut s = peak::PreviewScheduler::default();
        let first = preview_request(1);
        s.enqueue(first.clone(), peak::Settings::default()).unwrap();
        let cached_job = s.take_ready().pop().unwrap();
        s.finish(
            cached_job.token,
            peak::Content::Text {
                body: "account-a cached".into(),
                truncated: false,
            },
        )
        .unwrap();
        let mut running = first.clone();
        running.id = 2;
        running.key.entry_revision += 1;
        s.enqueue(running, peak::Settings::default()).unwrap();
        let running = s.take_ready().pop().unwrap();
        let mut next = first;
        next.id = 3;
        if generation_change {
            next.key.generation += 1;
        } else {
            next.key.scope = "owner-b".into();
        }
        s.enqueue(next, peak::Settings::default()).unwrap();
        assert_eq!(
            s.diagnostics().cached,
            0,
            "scope/generation switch must erase private cache storage"
        );
        assert_eq!(s.take_cancellations(), [running.token]);
        assert!(s
            .finish(
                running.token,
                peak::Content::Text {
                    body: "account-a late".into(),
                    truncated: false
                }
            )
            .is_none());
        assert_eq!(s.diagnostics().cache_bytes, 0);
    }
}

#[test]
fn warmed_projection_tracks_each_presentation_dependency() {
    let mut c = Cabinet::new(wide(1000)).unwrap();
    assert_eq!(ids(&c), ["folder"]);
    c.expand("folder", true).unwrap();
    let original = ids(&c);
    assert_eq!(original.len(), 1001);
    assert_eq!(ids(&c), original);
    c.set_folder_sort("folder", Some(Sort::NameDescending))
        .unwrap();
    assert_eq!(ids(&c)[1], "leaf-999");
    c.set_filter("Label-000500");
    assert_eq!(ids(&c), ["folder", "leaf-500"]);
    c.set_filter("");
    assert_eq!(ids(&c)[1], "leaf-999");
    c.set_folder_sort("folder", None).unwrap();
    assert_eq!(ids(&c), original);
    c.expand("folder", false).unwrap();
    assert_eq!(ids(&c), ["folder"]);
}

#[test]
fn shared_projection_is_reused_across_selection_and_unchanged_settings() {
    let mut c = Cabinet::new(wide(10_000)).unwrap();
    c.expand("folder", true).unwrap();
    let first = c.projected_rows();
    let rebuilds = c.diagnostics().projection_rebuilds;
    c.select("leaf-9999", SelectionModifiers::default())
        .unwrap();
    c.set_filter("");
    c.apply_settings(c.settings().clone()).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &c.projected_rows()));
    assert_eq!(
        c.diagnostics().projection_rebuilds,
        rebuilds,
        "selection and no-op settings cannot rebuild a canonical projection"
    );
}

#[test]
fn non_row_settings_do_not_invalidate_shared_projection() {
    let mut c = Cabinet::new(wide(10_000)).unwrap();
    c.expand("folder", true).unwrap();
    let first = c.projected_rows();
    let before = c.diagnostics().projection_rebuilds;
    let mut settings = c.settings().clone();
    settings.peak.width += 40.0;
    settings.peak.hover_delay_ms += 50;
    settings.double_click_ms += 20;
    settings.pinned_folders.push("folder".into());
    settings.watcher_exclusions.push("host-only-*".into());
    settings.show_tags = !settings.show_tags;
    c.apply_settings(settings).unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&first, &c.projected_rows()),
        "preview, input timing, decoration, pins and watcher policy cannot rebuild hierarchy rows"
    );
    assert_eq!(c.diagnostics().projection_rebuilds, before);
}

#[test]
fn bulk_success_clears_operation_progress_but_host_failure_preserves_recovery() {
    for outcome in ["acknowledge", "complete-success", "complete-failure"] {
        let mut c = Cabinet::new(wide(1)).unwrap();
        let before = c.snapshot().clone();
        let operation = if outcome == "acknowledge" {
            Operation::Download {
                id: "leaf-0".into(),
            }
        } else {
            Operation::Duplicate {
                id: "leaf-0".into(),
            }
        };
        let request = c.request(operation).unwrap();
        let progress = BulkProgress {
            request_id: request.id,
            context: request.context.clone(),
            completed: 1,
            total: 3,
            message: "host batch".into(),
            recovery_token: Some("host-recovery-token".into()),
        };
        c.update_bulk_progress(progress.clone()).unwrap();
        if outcome == "acknowledge" {
            c.acknowledge(request.id, &request.context).unwrap();
        } else {
            let result = if outcome == "complete-success" {
                let mut next = before.clone();
                next.context.revision += 1;
                Ok(next)
            } else {
                Err("host failed after partial work".into())
            };
            c.complete(Completion {
                request_id: request.id,
                context: request.context,
                result,
                undo_token: None,
            })
            .unwrap();
        }
        assert!(c.pending().is_none());
        if outcome == "complete-failure" {
            assert_eq!(c.bulk_progress(), Some(&progress));
            assert_eq!(c.snapshot(), &before);
            assert!(c.error().is_some());
        } else {
            assert!(
                c.bulk_progress().is_none(),
                "successful {outcome} retained stale progress/recovery"
            );
        }
    }
}

#[test]
fn bulk_progress_cannot_attach_to_a_readonly_navigation_request() {
    let mut c = Cabinet::new(wide(2)).unwrap();
    let request = c
        .request(Operation::Open {
            id: "leaf-0".into(),
        })
        .unwrap();
    assert!(c
        .update_bulk_progress(BulkProgress {
            request_id: request.id,
            context: request.context,
            completed: 0,
            total: 100,
            message: "Misclassified write".into(),
            recovery_token: Some("opaque-host-handle".into())
        })
        .is_err());
    assert!(c.bulk_progress().is_none());
    assert_eq!(c.pending().unwrap().id, request.id);
}

#[test]
fn visual_suppression_removes_action_selection_but_watcher_exclusion_does_not() {
    let mut c = Cabinet::new(wide(10)).unwrap();
    c.expand("folder", true).unwrap();
    c.select("leaf-9", SelectionModifiers::default()).unwrap();
    let mut settings = c.settings().clone();
    settings.watcher_exclusions.push("Label-000009".into());
    c.apply_settings(settings.clone()).unwrap();
    assert_eq!(
        c.selected(),
        ["leaf-9"],
        "watcher exclusion is host policy and cannot hide UI action selection"
    );
    assert!(ids(&c).contains(&"leaf-9".into()));
    settings.hidden_patterns.push("Label-000009".into());
    c.apply_settings(settings).unwrap();
    assert!(!ids(&c).contains(&"leaf-9".into()));
    assert!(
        c.selected().is_empty(),
        "hidden selected entries must not remain implicit destructive action targets"
    );
    assert_ne!(c.focused(), Some("leaf-9"));
    assert!(
        c.entry("leaf-9").is_some(),
        "UI suppression cannot delete canonical data"
    );
}

#[test]
fn delta_rejection_is_atomic_and_valid_delta_updates_live_index_projection() {
    let mut c = Cabinet::new(wide(1000)).unwrap();
    c.expand("folder", true).unwrap();
    c.select("leaf-999", SelectionModifiers::default()).unwrap();
    let warm = c.projected_rows();
    let before = c.snapshot().clone();
    let mut changed = c.entry("leaf-999").unwrap().clone();
    changed.label = "Changed".into();
    changed.revision += 1;
    let mut batch = DeltaBatch {
        base: before.context.clone(),
        revision: 2,
        expected_revisions: [("leaf-999".into(), u64::MAX)].into_iter().collect(),
        upserts: vec![changed.clone()],
        removed: vec![],
        hierarchy_complete: None,
    };
    assert!(matches!(
        c.apply_delta(batch.clone()),
        Err(Error::StaleContext)
    ));
    assert_eq!(c.snapshot(), &before);
    assert!(std::sync::Arc::ptr_eq(&warm, &c.projected_rows()));
    batch
        .expected_revisions
        .insert("leaf-999".into(), c.entry("leaf-999").unwrap().revision);
    batch.upserts.push(changed.clone());
    assert!(matches!(
        c.apply_delta(batch.clone()),
        Err(Error::DuplicateId(_))
    ));
    assert_eq!(c.snapshot(), &before);
    batch.upserts.pop();
    c.apply_delta(batch.clone()).unwrap();
    assert_eq!(c.entry("leaf-999").unwrap(), &changed);
    assert_eq!(c.selected(), ["leaf-999"]);
    assert!(!std::sync::Arc::ptr_eq(&warm, &c.projected_rows()));
    assert!(
        matches!(c.apply_delta(batch), Err(Error::StaleContext)),
        "replaying an applied batch cannot mutate the next revision"
    );
}

#[test]
fn partial_kind_changes_prune_known_leaf_preferences_but_preserve_absent_folders() {
    for use_delta in [false, true] {
        let mut snapshot = wide(0);
        snapshot.hierarchy_complete = false;
        let mut c = Cabinet::new(snapshot).unwrap();
        let mut settings = c.settings().clone();
        settings
            .folder_sorts
            .insert("folder".into(), Sort::NameDescending);
        settings
            .folder_sorts
            .insert("absent".into(), Sort::ModifiedAscending);
        settings.pinned_folders = vec!["folder".into(), "absent".into()];
        c.apply_settings(settings).unwrap();
        let mut leaf = c.entry("folder").unwrap().clone();
        leaf.kind = EntryKind::Leaf;
        leaf.revision += 1;
        if use_delta {
            c.apply_delta(DeltaBatch {
                base: c.snapshot().context.clone(),
                revision: 2,
                expected_revisions: [("folder".into(), 0)].into_iter().collect(),
                upserts: vec![leaf],
                removed: vec![],
                hierarchy_complete: None,
            })
            .unwrap();
        } else {
            let mut next = c.snapshot().clone();
            next.context.revision = 2;
            next.entries = vec![leaf];
            c.replace(next).unwrap();
        }
        assert_eq!(c.entry("folder").unwrap().kind, EntryKind::Leaf);
        assert!(
            !c.settings().folder_sorts.contains_key("folder"),
            "known leaf preference survived; delta={use_delta}"
        );
        assert_eq!(
            c.settings().folder_sorts.get("absent"),
            Some(&Sort::ModifiedAscending)
        );
        assert_eq!(c.settings().pinned_folders, ["absent"]);
        c.apply_settings(c.settings().clone())
            .expect("canonical refresh must leave valid persisted settings");
    }
}

#[test]
fn partial_refresh_preserves_unloaded_folder_preferences_explicit_deletion_removes_them() {
    let mut snapshot = wide(0);
    snapshot.hierarchy_complete = false;
    let mut c = Cabinet::new(snapshot).unwrap();
    let mut settings = c.settings().clone();
    settings
        .folder_sorts
        .insert("unloaded".into(), Sort::NameDescending);
    settings.pinned_folders.push("unloaded".into());
    c.apply_settings(settings).unwrap();
    let mut next = c.snapshot().clone();
    next.context.revision += 1;
    c.replace(next).unwrap();
    assert_eq!(
        c.settings().folder_sorts.get("unloaded"),
        Some(&Sort::NameDescending)
    );
    assert_eq!(c.settings().pinned_folders, ["unloaded"]);
    let folder = Entry::new("unloaded", None, "Unloaded", EntryKind::Folder);
    c.apply_delta(DeltaBatch {
        base: c.snapshot().context.clone(),
        revision: 3,
        expected_revisions: Default::default(),
        upserts: vec![folder],
        removed: vec![],
        hierarchy_complete: None,
    })
    .unwrap();
    let revision = c.entry("unloaded").unwrap().revision;
    c.apply_delta(DeltaBatch {
        base: c.snapshot().context.clone(),
        revision: 4,
        expected_revisions: [("unloaded".into(), revision)].into_iter().collect(),
        upserts: vec![],
        removed: vec!["unloaded".into()],
        hierarchy_complete: None,
    })
    .unwrap();
    assert!(!c.settings().folder_sorts.contains_key("unloaded"));
    assert!(c.settings().pinned_folders.is_empty());
}

#[test]
fn host_search_is_distinct_from_loaded_filter_and_fences_late_context_results() {
    let mut c = Cabinet::new(wide(10)).unwrap();
    let first = c
        .request(Operation::WorkspaceSearch {
            query: "remote".into(),
        })
        .unwrap();
    let mut next = c.snapshot().clone();
    next.context.generation += 1;
    c.replace(next).unwrap();
    let current = c
        .request(Operation::WorkspaceSearch {
            query: "current".into(),
        })
        .unwrap();
    assert!(c
        .complete_search(SearchResult {
            request_id: first.id,
            context: first.context,
            hits: vec![SearchHit {
                id: "remote-only".into(),
                path: "private/path".into()
            }]
        })
        .is_err());
    assert!(c.search_result().is_none());
    assert_eq!(c.pending(), Some(&current));
    c.complete_search(SearchResult {
        request_id: current.id,
        context: current.context,
        hits: vec![SearchHit {
            id: "not-loaded".into(),
            path: "Remote/File".into(),
        }],
    })
    .unwrap();
    assert!(
        c.entry("not-loaded").is_none(),
        "host hit does not fabricate a loaded canonical entry"
    );
    c.set_filter("Remote");
    assert!(
        c.rows().is_empty(),
        "local filtering must not silently use host search results"
    );
    assert_eq!(c.search_result().unwrap().hits[0].id, "not-loaded");
}

#[test]
fn child_page_wrong_parent_and_cursor_are_atomic_and_retry_keeps_cursor() {
    let mut snapshot = wide(0);
    snapshot.hierarchy_complete = false;
    snapshot.entries[0].children = ChildrenState::Unloaded;
    let mut c = Cabinet::new(snapshot).unwrap();
    let request = c.request_child_page("folder").unwrap();
    let before = c.snapshot().clone();
    let mut page = ChildPage {
        request_id: request.id,
        context: request.context.clone(),
        parent: "folder".into(),
        cursor: None,
        next_cursor: Some("page-2".into()),
        revision: 2,
        entries: vec![Entry::new(
            "new",
            Some("wrong-parent".into()),
            "New",
            EntryKind::Leaf,
        )],
    };
    assert!(matches!(
        c.complete_child_page(page.clone()),
        Err(Error::InvalidParent(_))
    ));
    assert_eq!(c.snapshot(), &before);
    assert_eq!(c.pending(), Some(&request));
    page.entries[0].parent = Some("folder".into());
    page.cursor = Some("wrong-cursor".into());
    assert!(c.complete_child_page(page.clone()).is_err());
    assert_eq!(c.snapshot(), &before);
    page.cursor = None;
    c.complete_child_page(page.clone()).unwrap();
    assert_eq!(c.next_page_cursor("folder"), Some("page-2"));
    assert!(
        c.complete_child_page(page).is_err(),
        "completed page cannot be replayed"
    );
    let retry = c.request_child_page("folder").unwrap();
    assert_eq!(
        retry.operation,
        Operation::LoadPage {
            id: "folder".into(),
            cursor: Some("page-2".into())
        }
    );
    c.fail_child_page(retry.id, "temporary host failure".into())
        .unwrap();
    assert!(matches!(
        c.entry("folder").unwrap().children,
        ChildrenState::Failed(_)
    ));
    assert_eq!(c.next_page_cursor("folder"), Some("page-2"));
    let retried = c.request_child_page("folder").unwrap();
    assert_eq!(retried.operation, retry.operation);
    c.complete_child_page(ChildPage {
        request_id: retried.id,
        context: retried.context,
        parent: "folder".into(),
        cursor: Some("page-2".into()),
        next_cursor: None,
        revision: 3,
        entries: vec![Entry::new(
            "last",
            Some("folder".into()),
            "Last",
            EntryKind::Leaf,
        )],
    })
    .unwrap();
    assert_eq!(c.entry("folder").unwrap().children, ChildrenState::Loaded);
    assert!(c.entry("new").is_some());
    assert!(c.entry("last").is_some());
}

#[cfg(feature = "serde")]
#[test]
fn additive_settings_load_legacy_json_without_erasing_existing_preferences() {
    let mut settings = Settings::default();
    settings
        .folder_sorts
        .insert("folder".into(), Sort::ModifiedAscending);
    settings.double_click_ms = 475;
    settings.peak.hover_delay_ms = 900;
    let mut legacy = serde_json::to_value(&settings).unwrap();
    for key in ["pinned_folders", "hidden_patterns", "watcher_exclusions"] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let restored: Settings = serde_json::from_value(legacy.clone())
        .expect("new optional preferences must default for an existing stored settings document");
    assert_eq!(restored.folder_sorts, settings.folder_sorts);
    assert_eq!(restored.double_click_ms, 475);
    assert_eq!(restored.peak.hover_delay_ms, 900);
    assert!(restored.pinned_folders.is_empty());
    assert!(restored.hidden_patterns.is_empty());
    assert!(restored.watcher_exclusions.is_empty());
    legacy["hidden_patterns"] = serde_json::json!(17);
    assert!(
        serde_json::from_value::<Settings>(legacy).is_err(),
        "missing optional value may default; malformed supplied data must not silently default"
    );
}

#[test]
fn incomplete_move_preflight_canonicalizes_sources_and_never_bypasses_loaded_rights() {
    let mut snapshot = wide(2);
    snapshot.hierarchy_complete = false;
    let mut c = Cabinet::new(snapshot.clone()).unwrap();
    let placement = Placement::Into(None);
    let request = c
        .request(Operation::MovePreflight {
            ids: vec!["folder".into(), "leaf-0".into(), "folder".into()],
            placement: placement.clone(),
        })
        .unwrap();
    assert_eq!(
        request.operation,
        Operation::MovePreflight {
            ids: vec!["folder".into()],
            placement: placement.clone()
        },
        "host preflight must receive unique canonical subtree roots"
    );
    let mut grant = MoveGrant {
        context: request.context.clone(),
        ids: vec!["folder".into()],
        placement: placement.clone(),
        token: "host-proof".into(),
    };
    grant.context.revision += 1;
    assert!(c.accept_move_preflight(request.id, grant.clone()).is_err());
    assert_eq!(c.pending(), Some(&request));
    grant.context = request.context.clone();
    c.accept_move_preflight(request.id, grant.clone()).unwrap();
    let authorized = c
        .request(Operation::MoveAuthorized {
            ids: grant.ids.clone(),
            placement: placement.clone(),
            token: grant.token.clone(),
        })
        .unwrap();
    assert_eq!(authorized.context, request.context);
    assert_eq!(
        authorized.expected_revisions.get("leaf-0"),
        Some(&snapshot.entries[1].revision)
    );

    snapshot.entries[2].capabilities.move_entry = false;
    let mut denied = Cabinet::new(snapshot).unwrap();
    assert!(
        matches!(denied.request(Operation::MovePreflight { ids: vec!["folder".into()], placement: placement.clone() }), Err(Error::PermissionDenied(id)) if id == "leaf-1")
    );
    assert!(denied.pending().is_none());
    assert!(denied
        .request(Operation::MoveAuthorized {
            ids: vec!["folder".into()],
            placement,
            token: "invented-token".into()
        })
        .is_err());
}

#[test]
fn readonly_request_fence_size_is_independent_of_unrelated_loaded_entries() {
    let mut c = Cabinet::new(wide(100_000)).unwrap();
    let request = c
        .request(Operation::Open {
            id: "leaf-99999".into(),
        })
        .unwrap();
    assert_eq!(
        request.expected_revisions.get("leaf-99999"),
        Some(&c.entry("leaf-99999").unwrap().revision)
    );
    assert!(
        request.expected_revisions.len() <= 2,
        "read-only one-resource request must not duplicate 100001 unrelated revision records"
    );
    assert_eq!(request.context, c.snapshot().context);
}

#[test]
fn lightweight_acknowledgement_is_exact_context_and_readonly_only() {
    let mut c = Cabinet::new(wide(10_000)).unwrap();
    let request = c
        .request(Operation::Download {
            id: "leaf-9999".into(),
        })
        .unwrap();
    let before = c.snapshot().clone();
    let warm = c.projected_rows();
    let mut wrong = request.context.clone();
    wrong.generation += 1;
    assert!(c.acknowledge(request.id, &wrong).is_err());
    assert!(c.acknowledge(request.id + 1, &request.context).is_err());
    assert_eq!(c.pending(), Some(&request));
    c.acknowledge(request.id, &request.context).unwrap();
    assert_eq!(c.snapshot(), &before);
    assert!(std::sync::Arc::ptr_eq(&warm, &c.projected_rows()));
    assert!(c.pending().is_none());
    let rename = c
        .request(Operation::Rename {
            id: "leaf-9999".into(),
            name: "Changed".into(),
        })
        .unwrap();
    assert!(
        c.acknowledge(rename.id, &rename.context).is_err(),
        "write cannot be acknowledged without canonical host result"
    );
    assert_eq!(c.pending(), Some(&rename));
    assert_eq!(c.entry("leaf-9999").unwrap().label, "Label-009999");
}

#[test]
fn scoped_write_fences_include_source_subtree_and_destination_references() {
    let mut snapshot = wide(2);
    let mut destination = Entry::new("destination", None, "Destination", EntryKind::Folder);
    destination.revision = 23;
    snapshot.entries.push(destination);
    let mut unrelated = Entry::new("unrelated", None, "Unrelated", EntryKind::Leaf);
    unrelated.revision = 99;
    snapshot.entries.push(unrelated);
    let mut c = Cabinet::new(snapshot.clone()).unwrap();
    let moved = c
        .request(Operation::Move {
            ids: vec!["leaf-0".into()],
            placement: Placement::Into(Some("destination".into())),
        })
        .unwrap();
    for id in ["leaf-0", "folder", "destination"] {
        assert_eq!(
            moved.expected_revisions.get(id),
            Some(&c.entry(id).unwrap().revision),
            "move fence omitted affected reference {id}"
        );
    }
    assert!(!moved.expected_revisions.contains_key("unrelated"));
    let mut c = Cabinet::new(snapshot).unwrap();
    let deleted = c
        .request(Operation::Delete {
            ids: vec!["folder".into()],
        })
        .unwrap();
    for id in ["folder", "leaf-0", "leaf-1"] {
        assert_eq!(
            deleted.expected_revisions.get(id),
            Some(&c.entry(id).unwrap().revision),
            "delete fence omitted affected descendant {id}"
        );
    }
    assert!(!deleted.expected_revisions.contains_key("unrelated"));
}

#[test]
fn invalid_refresh_preserves_warm_projection_private_state_and_preferences() {
    let mut c = Cabinet::new(wide(1000)).unwrap();
    c.expand("folder", true).unwrap();
    c.select("leaf-999", SelectionModifiers::default()).unwrap();
    c.set_folder_sort("folder", Some(Sort::NameDescending))
        .unwrap();
    let before = c.snapshot().clone();
    let rows = ids(&c);
    let settings = c.settings().clone();
    let mut invalid = before.clone();
    invalid.context.revision += 1;
    invalid.entries[501].parent = Some("absent".into());
    invalid.entries[501].revision += 1;
    assert!(c.replace(invalid).is_err());
    assert_eq!(c.snapshot(), &before);
    assert_eq!(ids(&c), rows);
    assert_eq!(c.selected(), ["leaf-999"]);
    assert_eq!(c.settings(), &settings);
    assert!(c.expanded("folder"));
}

#[test]
fn refresh_changes_filter_membership_without_stale_cached_entries() {
    let mut c = Cabinet::new(wide(1000)).unwrap();
    c.set_filter("Label-000999");
    assert_eq!(ids(&c), ["folder", "leaf-999"]);
    let mut next = c.snapshot().clone();
    next.context.revision += 1;
    next.entries[1000].revision += 1;
    next.entries[1000].label = "Renamed".into();
    c.replace(next).unwrap();
    assert!(ids(&c).is_empty());
    assert_eq!(c.entry("leaf-999").unwrap().label, "Renamed");
    c.set_filter("Renamed");
    assert_eq!(ids(&c), ["folder", "leaf-999"]);
}

#[test]
fn deep_cycle_rejection_does_not_require_recursive_validation() {
    let depth: usize = 100_000;
    let mut s = wide(0);
    s.entries = (0..depth)
        .map(|i| {
            Entry::new(
                format!("n{i}"),
                i.checked_sub(1).map(|p| format!("n{p}")),
                "Node",
                EntryKind::Folder,
            )
        })
        .collect();
    let mut c = Cabinet::new(s).unwrap();
    assert_eq!(c.rows().len(), 1);
    let mut cyclic = c.snapshot().clone();
    cyclic.context.revision += 1;
    cyclic.entries[0].parent = Some(format!("n{}", depth - 1));
    cyclic.entries[0].revision += 1;
    assert!(matches!(c.replace(cyclic), Err(Error::Cycle(_))));
    assert_eq!(c.entry("n0").unwrap().parent, None);
    assert_eq!(c.rows().len(), 1);
}

#[cfg(feature = "egui")]
#[test]
fn viewport_controls_remain_bounded_and_far_focus_is_reachable() {
    use cabinet::egui_view::CabinetView;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(100_000)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = CabinetView::new("independent-large-tree");
    let render = |view: &mut CabinetView, c: &mut Cabinet, time| {
        let mut response = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(700.0, 500.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| response = Some(view.show(ui, c)));
            },
        );
        response.unwrap()
    };
    let first = render(&mut view, &mut c, 0.0);
    assert!(
        first.rows.len() < 100,
        "row controls must scale with viewport, not 100001 canonical nodes"
    );
    assert!(
        first.text_layouts < 100,
        "offscreen label layout cannot bypass control virtualization"
    );
    assert!(!first.rows.contains_key("leaf-99999"));
    let rebuilds = c.diagnostics().projection_rebuilds;
    render(&mut view, &mut c, 0.05);
    assert_eq!(
        c.diagnostics().projection_rebuilds,
        rebuilds,
        "unchanged native frames reuse the projection"
    );
    c.reveal("leaf-99999").unwrap();
    render(&mut view, &mut c, 0.1);
    let after = render(&mut view, &mut c, 0.2);
    assert!(after.rows.len() < 100);
    assert!(
        after.rows.contains_key("leaf-99999"),
        "focused row outside original viewport must become mounted"
    );
    assert_eq!(c.focused(), Some("leaf-99999"));
}

#[cfg(feature = "egui")]
#[test]
fn real_workspace_search_input_survives_frames_and_clears_on_account_switch() {
    use cabinet::egui_view::{CabinetView, ViewResponse};
    fn frame(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        events: Vec<egui::Event>,
    ) -> ViewResponse {
        let mut response = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(800.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| response = Some(view.show(ui, c)));
            },
        );
        response.unwrap()
    }
    fn click(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            c,
            time,
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
            c,
            time + 0.01,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
    let ctx = egui::Context::default();
    let mut view = CabinetView::new("independent-search");
    let mut c = Cabinet::new(wide(10)).unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["host-search-mode"].center(),
    );
    let search = frame(&ctx, &mut view, &mut c, 0.05, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        search.controls["filter"].center(),
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.15,
        vec![egui::Event::Text("remote-pattern".into())],
    );
    let settled = frame(&ctx, &mut view, &mut c, 0.2, vec![]);
    assert_eq!(
        c.filter(),
        "",
        "host query must not become a loaded-only filter"
    );
    let submitted = click(
        &ctx,
        &mut view,
        &mut c,
        0.25,
        settled.controls["search-submit"].center(),
    );
    assert_eq!(submitted.requests.len(), 1);
    assert_eq!(
        submitted.requests[0].operation,
        Operation::WorkspaceSearch {
            query: "remote-pattern".into()
        }
    );
    let mut next = c.snapshot().clone();
    next.context.scope = "other-account".into();
    c.replace(next).unwrap();
    let switched = frame(&ctx, &mut view, &mut c, 0.3, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.35,
        switched.controls["host-search-mode"].center(),
    );
    let cleared = frame(&ctx, &mut view, &mut c, 0.4, vec![]);
    let disabled = click(
        &ctx,
        &mut view,
        &mut c,
        0.45,
        cleared.controls["search-submit"].center(),
    );
    assert!(
        disabled.requests.is_empty(),
        "previous account's query cannot be submitted after switching"
    );
    assert!(c.pending().is_none());
}

#[cfg(feature = "egui")]
mod native_probe {
    use super::*;
    use cabinet::egui_view::{CabinetView, ViewResponse};
    pub fn frame(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        events: Vec<egui::Event>,
    ) -> ViewResponse {
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(800.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| out = Some(view.show(ui, c)));
            },
        );
        out.unwrap()
    }
    pub fn click(
        ctx: &egui::Context,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            c,
            time,
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
            c,
            time + 0.01,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
}

#[cfg(feature = "egui")]
#[test]
fn native_scheduler_reconfiguration_preserves_active_tokens_and_rejects_invalid_limits_atomically()
{
    use native_probe::frame;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(1)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet::egui_view::CabinetView::new("reconfigure-active-worker");
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 1,
        ..Default::default()
    })
    .unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    let hover = layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        vec![egui::Event::PointerMoved(hover)],
    );
    frame(&ctx, &mut view, &mut c, 0.6, vec![]);
    let dispatch = frame(&ctx, &mut view, &mut c, 1.9, vec![]);
    assert_eq!(dispatch.preview_jobs.len(), 1);
    let token = dispatch.preview_jobs[0].token;
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 2,
        max_cache_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(view
        .enable_scheduler(peak::SchedulerSettings {
            max_concurrent: 0,
            ..Default::default()
        })
        .is_err());
    assert!(
        view.complete_scheduled(
            token,
            peak::Content::Text {
                body: "owned-worker".into(),
                truncated: false
            }
        ),
        "reconfiguration must preserve the issued worker token"
    );
    assert!(!view.complete_scheduled(
        token,
        peak::Content::Unsupported {
            reason: "duplicate".into()
        }
    ));
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.0,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    frame(&ctx, &mut view, &mut c, 2.4, vec![]);
    assert_eq!(view.peak.stage(), peak::Stage::Hidden);
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.5,
        vec![egui::Event::PointerMoved(hover)],
    );
    let fresh = frame(&ctx, &mut view, &mut c, 4.3, vec![]);
    assert_eq!(
        fresh.preview_jobs.len(),
        1,
        "rejected configuration must not restore the default cache budget"
    );
    assert_ne!(fresh.preview_jobs[0].token, token);
    assert!(fresh.preview_requests.is_empty());
}

#[cfg(feature = "egui")]
#[test]
fn native_preview_worker_completion_replays_through_scoped_scheduler_cache() {
    use native_probe::frame;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(wide(2)).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet::egui_view::CabinetView::new("independent-scheduled-preview");
    view.enable_scheduler(peak::SchedulerSettings {
        max_concurrent: 1,
        ..Default::default()
    })
    .unwrap();
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    let hover = layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        vec![egui::Event::PointerMoved(hover)],
    );
    frame(&ctx, &mut view, &mut c, 0.6, vec![]);
    let dispatched = frame(&ctx, &mut view, &mut c, 1.9, vec![]);
    assert!(
        dispatched.preview_requests.is_empty(),
        "scheduler mode must not duplicate legacy worker dispatch"
    );
    assert_eq!(dispatched.preview_jobs.len(), 1);
    let job = &dispatched.preview_jobs[0];
    assert_eq!(job.request.key.entry_id, "leaf-0");
    assert!(view.complete_scheduled(
        job.token,
        peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        }
    ));
    assert!(!view.complete_scheduled(
        job.token,
        peak::Content::Text {
            body: "duplicate".into(),
            truncated: false
        }
    ));
    assert_eq!(
        view.peak.content(),
        Some(&peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        })
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.0,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    frame(&ctx, &mut view, &mut c, 2.4, vec![]);
    assert_eq!(view.peak.stage(), peak::Stage::Hidden);
    frame(
        &ctx,
        &mut view,
        &mut c,
        2.5,
        vec![egui::Event::PointerMoved(hover)],
    );
    let cached = frame(&ctx, &mut view, &mut c, 4.3, vec![]);
    assert!(cached.preview_jobs.is_empty());
    assert!(cached.preview_requests.is_empty());
    assert_eq!(
        view.peak.content(),
        Some(&peak::Content::Text {
            body: "worker-produced".into(),
            truncated: false
        })
    );
    assert!(
        c.pending().is_none(),
        "preview workers cannot occupy the durable operation slot"
    );
    frame(
        &ctx,
        &mut view,
        &mut c,
        4.4,
        vec![egui::Event::PointerMoved(egui::pos2(790.0, 640.0))],
    );
    let mut other = c.snapshot().clone();
    other.context.scope = "other-account".into();
    c.replace(other).unwrap();
    frame(&ctx, &mut view, &mut c, 4.5, vec![]);
    let mut original = c.snapshot().clone();
    original.context.scope = "scaling-tests".into();
    c.replace(original).unwrap();
    c.expand("folder", true).unwrap();
    let returned_layout = frame(&ctx, &mut view, &mut c, 4.6, vec![]);
    let returned_hover = returned_layout.rows["leaf-0"].left_center() + egui::vec2(50.0, 0.0);
    frame(
        &ctx,
        &mut view,
        &mut c,
        4.7,
        vec![egui::Event::PointerMoved(returned_hover)],
    );
    assert_eq!(
        view.peak.key().map(|key| key.entry_id.as_str()),
        Some("leaf-0"),
        "privacy probe must hover an actually mounted leaf after scope reset"
    );
    let returned = frame(&ctx, &mut view, &mut c, 6.5, vec![]);
    assert_eq!(
        returned.preview_jobs.len(),
        1,
        "A -> B -> A without hovering B must purge A's stored cache on the first context switch"
    );
    assert!(view.peak.content().is_none());
}

#[cfg(feature = "egui")]
#[test]
fn real_paging_menu_emits_fenced_load_page() {
    use native_probe::{click, frame};
    let mut snapshot = wide(0);
    snapshot.hierarchy_complete = false;
    snapshot.entries[0].children = ChildrenState::Unloaded;
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(snapshot).unwrap();
    let mut view = cabinet::egui_view::CabinetView::new("independent-page-menu");
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["options:folder"].center(),
    );
    let menu = frame(&ctx, &mut view, &mut c, 0.06, vec![]);
    let page = click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        menu.controls["menu-load-page:folder"].center(),
    );
    assert_eq!(page.requests.len(), 1);
    assert_eq!(
        page.requests[0].operation,
        Operation::LoadPage {
            id: "folder".into(),
            cursor: None
        }
    );
    assert_eq!(page.requests[0].context, c.snapshot().context);
    assert_eq!(
        page.requests[0].expected_revisions.get("folder"),
        Some(&c.entry("folder").unwrap().revision)
    );
}

#[cfg(feature = "egui")]
#[test]
fn real_incomplete_move_menu_requires_host_grant_before_authorized_dispatch() {
    use native_probe::{click, frame};
    let mut snapshot = wide(2);
    snapshot.hierarchy_complete = false;
    snapshot.entries.push(Entry::new(
        "destination",
        None,
        "Destination",
        EntryKind::Folder,
    ));
    let ctx = egui::Context::default();
    let mut c = Cabinet::new(snapshot).unwrap();
    c.expand("folder", true).unwrap();
    let mut view = cabinet::egui_view::CabinetView::new("independent-preflight-menu");
    let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.02,
        layout.controls["options:leaf-0"].center(),
    );
    let menu = frame(&ctx, &mut view, &mut c, 0.06, vec![]);
    click(
        &ctx,
        &mut view,
        &mut c,
        0.1,
        menu.controls["menu-move:leaf-0"].center(),
    );
    let destinations = frame(&ctx, &mut view, &mut c, 0.15, vec![]);
    let preflight = click(
        &ctx,
        &mut view,
        &mut c,
        0.2,
        destinations.controls["move-destination:destination"].center(),
    );
    assert_eq!(preflight.requests.len(), 1);
    let request = &preflight.requests[0];
    let placement = Placement::Into(Some("destination".into()));
    assert_eq!(
        request.operation,
        Operation::MovePreflight {
            ids: vec!["leaf-0".into()],
            placement: placement.clone()
        }
    );
    assert_eq!(c.entry("leaf-0").unwrap().parent, Some("folder".into()));
    c.accept_move_preflight(
        request.id,
        MoveGrant {
            context: request.context.clone(),
            ids: vec!["leaf-0".into()],
            placement: placement.clone(),
            token: "trusted-host-proof".into(),
        },
    )
    .unwrap();
    let approved = frame(&ctx, &mut view, &mut c, 0.25, vec![]);
    assert!(
        approved.requests.is_empty(),
        "grant acceptance alone cannot dispatch a durable move"
    );
    let confirm = approved.controls["confirm-authorized-move"];
    assert!(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::Vec2::new(800.0,650.0)).contains_rect(confirm),"host-approved confirmation must be visible and clickable within the viewport; got {confirm:?}");
    let authorized = click(&ctx, &mut view, &mut c, 0.3, confirm.center());
    assert_eq!(authorized.requests.len(), 1);
    assert_eq!(
        authorized.requests[0].operation,
        Operation::MoveAuthorized {
            ids: vec!["leaf-0".into()],
            placement,
            token: "trusted-host-proof".into()
        }
    );
    assert_eq!(
        c.entry("leaf-0").unwrap().parent,
        Some("folder".into()),
        "authorized intention still awaits host canonical result"
    );
}
