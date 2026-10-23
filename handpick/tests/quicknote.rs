use handpick::*;

fn session(capacity: usize) -> WritingSession {
    let mut session = WritingSession::new(
        ContextId::new("workspace").unwrap(),
        SessionId::new("session").unwrap(),
        capacity,
    );
    session.bind(EditorId::new("editor-a").unwrap(), 1).unwrap();
    session
}

fn pin(id: &str) -> PinId {
    PinId::new(id).unwrap()
}

fn save(session: &WritingSession) -> SaveIntent {
    SaveIntent {
        operation: OperationId::new("save-a").unwrap(),
        fence: session.fence().unwrap(),
        destination: SaveDestination::Edit {
            note: NoteId::new("note-a").unwrap(),
            expected_revision: RevisionId::new("old").unwrap(),
        },
    }
}

fn committed(submitted: SaveIntent, note: &str) -> SaveOutcome {
    SaveOutcome {
        submitted,
        content: ContentOutcome::Committed {
            note: NoteId::new(note).unwrap(),
            revision: RevisionId::new("new").unwrap(),
            relationships: ProjectionReadiness::Pending,
            search: ProjectionReadiness::Failed,
        },
    }
}

#[test]
fn bookmark_keeps_editor_open_and_close_reopen_retains_identity_and_new_revision() {
    let mut s = session(2);
    s.expand().unwrap();
    let before = s.fence().unwrap();
    assert_eq!(
        s.close_quicknote(&before).unwrap(),
        QuicknoteClose::NeedsSaveOrPin
    );
    assert_eq!(s.fence().unwrap(), before);
    s.bookmark(&before, pin("bookmark-a")).unwrap();
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-a");
    assert!(s.expanded());
    assert!(s.pins().is_empty());
    assert_eq!(s.validate(&before), Err(Error::Stale));
    s.edited(&s.fence().unwrap(), 2).unwrap();
    assert_eq!(s.bookmarks()[0].revision, 2);
    let closing = s.begin_lookup().unwrap();
    s.deliver(&closing, Channel::Search, DeliveryStatus::Complete, vec![])
        .unwrap();
    assert_eq!(s.close_quicknote(&closing).unwrap(), QuicknoteClose::Closed);
    assert!(s.editor().is_none());
    assert!(!s.expanded());
    assert_eq!(
        s.delivery(Channel::Search).status(),
        DeliveryStatus::Pending
    );
    assert_eq!(s.bookmarks().len(), 1);
    s.open_bookmark(&pin("bookmark-a")).unwrap();
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-a");
    assert_eq!(s.editor().unwrap().1, 2);
    assert!(s.expanded());
    assert_eq!(s.bookmarks().len(), 1);
    assert_eq!(s.validate(&closing), Err(Error::Stale));
}

#[test]
fn bookmark_restore_never_overwrites_occupied_draft_and_capacity_is_shared() {
    let mut s = session(2);
    s.bookmark(&s.fence().unwrap(), pin("a")).unwrap();
    s.close_quicknote(&s.fence().unwrap()).unwrap();
    s.bind(EditorId::new("editor-b").unwrap(), 1).unwrap();
    let before = s.fence().unwrap();
    assert_eq!(s.open_bookmark(&pin("a")), Err(Error::OccupiedDraft));
    assert_eq!(s.bookmark(&before, pin("a")), Err(Error::DuplicatePin));
    assert_eq!(s.fence().unwrap(), before);
    s.park(pin("parked-b")).unwrap();
    s.bind(EditorId::new("editor-c").unwrap(), 1).unwrap();
    assert_eq!(
        s.bookmark(&s.fence().unwrap(), pin("c")),
        Err(Error::PinLimit)
    );
    assert_eq!(s.park(pin("c")), Err(Error::PinLimit));
    assert_eq!(s.bookmarks().len(), 1);
    assert_eq!(s.pins().len(), 1);
    s.remove_bookmark(&pin("a")).unwrap();
    s.bookmark(&s.fence().unwrap(), pin("c")).unwrap();
    assert_eq!(
        s.restore(&pin("parked-b"), Some(pin("new-c"))),
        Err(Error::DuplicatePin)
    );
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-c");
}

#[test]
fn ime_and_pending_unknown_save_keep_bookmark_and_close_state_unchanged() {
    let mut s = session(1);
    let token = s.begin_composition(&s.fence().unwrap()).unwrap();
    let before = s.fence().unwrap();
    assert_eq!(s.bookmark(&before, pin("a")), Err(Error::Stale));
    assert_eq!(s.close_quicknote(&before), Err(Error::Stale));
    s.end_composition(&token).unwrap();
    s.bookmark(&s.fence().unwrap(), pin("a")).unwrap();
    let intent = save(&s);
    s.submit(intent.clone()).unwrap();
    assert_eq!(s.close_quicknote(&s.fence().unwrap()), Err(Error::Stale));
    assert_eq!(s.open_bookmark(&pin("a")), Err(Error::Stale));
    assert_eq!(s.remove_bookmark(&pin("a")), Err(Error::Stale));
    assert_eq!(
        s.acknowledge(&SaveOutcome {
            submitted: intent.clone(),
            content: ContentOutcome::Unknown
        })
        .unwrap(),
        SaveDisposition::ReconcileBeforeRetry
    );
    assert_eq!(s.close_quicknote(&s.fence().unwrap()), Err(Error::Stale));
    assert_eq!(s.pending_save(), Some(&intent));
    s.acknowledge(&SaveOutcome {
        submitted: intent,
        content: ContentOutcome::Rejected,
    })
    .unwrap();
    assert_eq!(
        s.close_quicknote(&s.fence().unwrap()).unwrap(),
        QuicknoteClose::Closed
    );
}

#[test]
fn pending_and_unknown_save_preserve_active_editor_across_legacy_park_and_restore() {
    let mut s = session(4);
    s.bookmark(&s.fence().unwrap(), pin("bookmark-a")).unwrap();
    s.close_quicknote(&s.fence().unwrap()).unwrap();
    s.bind(EditorId::new("editor-b").unwrap(), 2).unwrap();
    s.park(pin("parked-b")).unwrap();
    s.bind(EditorId::new("editor-c").unwrap(), 3).unwrap();
    s.expand().unwrap();
    let intent = save(&s);
    s.submit(intent.clone()).unwrap();
    let before = s.fence().unwrap();
    let pins = s.pins().to_vec();
    let bookmarks = s.bookmarks().to_vec();
    for unknown in [false, true] {
        if unknown {
            assert_eq!(
                s.acknowledge(&SaveOutcome {
                    submitted: intent.clone(),
                    content: ContentOutcome::Unknown,
                })
                .unwrap(),
                SaveDisposition::ReconcileBeforeRetry
            );
        }
        assert_eq!(s.park(pin("parked-c")), Err(Error::Stale));
        assert_eq!(
            s.restore(&pin("parked-b"), Some(pin("replacement-c"))),
            Err(Error::Stale)
        );
        assert_eq!(s.editor().unwrap().0.as_str(), "editor-c");
        assert_eq!(s.editor().unwrap().1, 3);
        assert_eq!(s.fence().unwrap(), before);
        assert_eq!(s.validate(&before), Ok(()));
        assert_eq!(s.pins(), pins.as_slice());
        assert_eq!(s.bookmarks(), bookmarks.as_slice());
        assert_eq!(s.pending_save(), Some(&intent));
        assert!(s.expanded());
    }
}

#[test]
fn definitive_save_outcomes_reenable_legacy_park_and_occupied_restore() {
    for commit in [false, true] {
        let mut s = session(3);
        s.park(pin("parked-a")).unwrap();
        s.bind(EditorId::new("editor-b").unwrap(), 2).unwrap();
        let intent = save(&s);
        s.submit(intent.clone()).unwrap();
        s.acknowledge(&SaveOutcome {
            submitted: intent.clone(),
            content: ContentOutcome::Unknown,
        })
        .unwrap();
        let outcome = if commit {
            committed(intent, "note-a")
        } else {
            SaveOutcome {
                submitted: intent,
                content: ContentOutcome::Rejected,
            }
        };
        assert_eq!(
            s.acknowledge(&outcome).unwrap(),
            if commit {
                SaveDisposition::CurrentRevisionCommitted
            } else {
                SaveDisposition::RetainForRetry
            }
        );
        assert!(s.pending_save().is_none());
        let before = s.fence().unwrap();
        s.restore(&pin("parked-a"), Some(pin("parked-b"))).unwrap();
        assert_eq!(s.editor().unwrap().0.as_str(), "editor-a");
        assert_eq!(s.validate(&before), Err(Error::Stale));
        s.park(pin("parked-a-again")).unwrap();
        assert!(s.editor().is_none());
        s.restore(&pin("parked-b"), None).unwrap();
        assert_eq!(s.editor().unwrap().0.as_str(), "editor-b");
        assert_eq!(s.editor().unwrap().1, 2);
    }
}

#[test]
fn only_matching_current_commit_allows_unbookmarked_close() {
    let mut s = session(1);
    let intent = save(&s);
    s.submit(intent.clone()).unwrap();
    assert_eq!(
        s.acknowledge(&committed(intent.clone(), "wrong-note")),
        Err(Error::Stale)
    );
    assert_eq!(s.pending_save(), Some(&intent));
    s.edited(&s.fence().unwrap(), 2).unwrap();
    assert_eq!(
        s.acknowledge(&committed(intent, "note-a")).unwrap(),
        SaveDisposition::OlderRevisionCommitted
    );
    assert!(s.committed_note().is_none());
    assert_eq!(
        s.close_quicknote(&s.fence().unwrap()).unwrap(),
        QuicknoteClose::NeedsSaveOrPin
    );
    let intent = save(&s);
    s.submit(intent.clone()).unwrap();
    assert_eq!(
        s.acknowledge(&committed(intent, "note-a")).unwrap(),
        SaveDisposition::CurrentRevisionCommitted
    );
    assert_eq!(s.committed_note().unwrap().0.as_str(), "note-a");
    assert_eq!(
        s.close_quicknote(&s.fence().unwrap()).unwrap(),
        QuicknoteClose::Closed
    );
}

#[test]
fn recovered_canonical_note_has_no_invented_prior_capture_operation() {
    let mut s = WritingSession::new(
        ContextId::new("workspace").unwrap(),
        SessionId::new("recovered").unwrap(),
        1,
    );
    s.bind_committed(
        EditorId::new("editor").unwrap(),
        7,
        NoteId::new("existing").unwrap(),
        RevisionId::new("canonical-4").unwrap(),
    )
    .unwrap();
    assert_eq!(s.committed_note().unwrap().1.as_str(), "canonical-4");
    s.edited(&s.fence().unwrap(), 8).unwrap();
    assert!(s.committed_note().is_none());
    let intent = SaveIntent {
        operation: OperationId::new("fresh-operation").unwrap(),
        fence: s.fence().unwrap(),
        destination: SaveDestination::Edit {
            note: NoteId::new("existing").unwrap(),
            expected_revision: RevisionId::new("canonical-4").unwrap(),
        },
    };
    assert_eq!(
        serde_json::from_str::<SaveIntent>(&serde_json::to_string(&intent).unwrap()).unwrap(),
        intent
    );
    s.submit(intent.clone()).unwrap();
    assert_eq!(
        s.acknowledge(&committed(intent, "existing")).unwrap(),
        SaveDisposition::CurrentRevisionCommitted
    );
    assert_eq!(
        s.close_quicknote(&s.fence().unwrap()).unwrap(),
        QuicknoteClose::Closed
    );
}

#[test]
fn canonical_readback_adopts_the_same_active_snapshot_without_rebinding_or_losing_bookmark() {
    let mut s = session(1);
    s.expand().unwrap();
    s.bookmark(&s.fence().unwrap(), pin("a")).unwrap();
    let before = s.fence().unwrap();
    s.bind_committed(
        EditorId::new("editor-a").unwrap(),
        1,
        NoteId::new("note-a").unwrap(),
        RevisionId::new("stored-1").unwrap(),
    )
    .unwrap();
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-a");
    assert_eq!(s.editor().unwrap().1, 1);
    assert!(s.expanded());
    assert_eq!(s.bookmarks().len(), 1);
    assert_eq!(s.committed_note().unwrap().1.as_str(), "stored-1");
    assert_eq!(s.validate(&before), Err(Error::Stale));
    s.remove_bookmark(&pin("a")).unwrap();
    assert_eq!(
        s.close_quicknote(&s.fence().unwrap()).unwrap(),
        QuicknoteClose::Closed
    );
}

#[test]
fn canonical_readback_reopens_only_the_exact_bookmarked_revision_and_preserves_legacy_park() {
    let mut s = session(2);
    s.bookmark(&s.fence().unwrap(), pin("a")).unwrap();
    s.close_quicknote(&s.fence().unwrap()).unwrap();
    let bind = |s: &mut WritingSession, revision| {
        s.bind_committed(
            EditorId::new("editor-a").unwrap(),
            revision,
            NoteId::new("note-a").unwrap(),
            RevisionId::new("stored-1").unwrap(),
        )
    };
    assert_eq!(bind(&mut s, 2), Err(Error::Stale));
    assert!(s.editor().is_none());
    bind(&mut s, 1).unwrap();
    assert_eq!(s.bookmarks().len(), 1);
    assert!(!s.expanded());
    s.close_quicknote(&s.fence().unwrap()).unwrap();
    s.bind(EditorId::new("editor-b").unwrap(), 1).unwrap();
    let before = s.fence().unwrap();
    assert_eq!(bind(&mut s, 1), Err(Error::OccupiedDraft));
    assert_eq!(s.fence().unwrap(), before);
    s.park(pin("parked-b")).unwrap();
    assert_eq!(
        s.bind_committed(
            EditorId::new("editor-b").unwrap(),
            1,
            NoteId::new("note-b").unwrap(),
            RevisionId::new("stored-1").unwrap()
        ),
        Err(Error::DuplicatePin)
    );
    assert_eq!(s.pins().len(), 1);
    s.restore(&pin("parked-b"), None).unwrap();
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-b");
}

#[test]
fn canonical_readback_rejects_changed_native_revision_ime_pending_save_and_revocation() {
    let mut s = session(1);
    let bind = |s: &mut WritingSession, revision| {
        s.bind_committed(
            EditorId::new("editor-a").unwrap(),
            revision,
            NoteId::new("note-a").unwrap(),
            RevisionId::new("stored-1").unwrap(),
        )
    };
    let before = s.fence().unwrap();
    assert_eq!(bind(&mut s, 2), Err(Error::Stale));
    assert_eq!(s.fence().unwrap(), before);
    let token = s.begin_composition(&before).unwrap();
    assert_eq!(bind(&mut s, 1), Err(Error::Stale));
    s.end_composition(&token).unwrap();
    let intent = save(&s);
    s.submit(intent.clone()).unwrap();
    assert_eq!(bind(&mut s, 1), Err(Error::Stale));
    assert_eq!(s.pending_save(), Some(&intent));
    s.revoke();
    assert_eq!(bind(&mut s, 1), Err(Error::Revoked));
}

#[test]
fn empty_quicknote_body_collapses_without_discarding_native_handle_or_bookmark() {
    let mut s = session(1);
    s.bookmark(&s.fence().unwrap(), pin("a")).unwrap();
    s.present_draft(&s.fence().unwrap(), 0, 1, true, "Header remains")
        .unwrap();
    assert!(s.expanded());
    let before = s.fence().unwrap();
    assert!(!s.present_quicknote(&before, true).unwrap());
    assert_eq!(s.editor().unwrap().0.as_str(), "editor-a");
    assert_eq!(s.bookmarks().len(), 1);
    assert_eq!(s.validate(&before), Err(Error::Stale));
    s.open_bookmark(&pin("a")).unwrap();
    assert!(s.expanded());
    s.revoke();
    assert!(s.bookmarks().is_empty());
    assert_eq!(s.open_bookmark(&pin("a")), Err(Error::Revoked));
}

#[test]
fn original_query_entry_requires_fresh_complete_empty_or_explicit_newline() {
    let mut s = Session::new(
        ContextId::new("workspace").unwrap(),
        Limits {
            max_text_bytes: 4096,
            max_pins: 1,
            max_items: 8,
            max_item_bytes: 4096,
            max_actions_per_item: 8,
        },
    );
    s.set_draft("a new thought".into()).unwrap();
    let fence = s.begin().unwrap();
    for status in [
        DeliveryStatus::Pending,
        DeliveryStatus::Partial,
        DeliveryStatus::Offline,
        DeliveryStatus::Failed,
        DeliveryStatus::StaleIndex,
    ] {
        s.deliver(&fence, Channel::Search, status, vec![]).unwrap();
        assert!(!s
            .can_start_quicknote(
                &fence,
                Channel::Search,
                QuicknoteTrigger::CompleteEmpty,
                false
            )
            .unwrap());
        assert!(s
            .can_start_quicknote(&fence, Channel::Search, QuicknoteTrigger::NewLine, false)
            .unwrap());
    }
    s.deliver(&fence, Channel::Search, DeliveryStatus::Complete, vec![])
        .unwrap();
    assert!(s
        .can_start_quicknote(
            &fence,
            Channel::Search,
            QuicknoteTrigger::CompleteEmpty,
            false
        )
        .unwrap());
    assert!(!s
        .can_start_quicknote(&fence, Channel::Search, QuicknoteTrigger::NewLine, true)
        .unwrap());
    assert_eq!(s.draft(), "a new thought");
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Complete,
        vec![SearchItem {
            key: ResultKey {
                provider: ProviderId::new("notes").unwrap(),
                result: ResultId::new("match").unwrap(),
            },
            label: "Existing thought".into(),
            detail: None,
            actions: vec![],
        }],
    )
    .unwrap();
    assert!(!s
        .can_start_quicknote(
            &fence,
            Channel::Search,
            QuicknoteTrigger::CompleteEmpty,
            false
        )
        .unwrap());
    for query in [
        "> missing command",
        "type:settings missing",
        "type:folder missing",
        "type:",
        "",
        "\"unclosed",
    ] {
        s.set_draft(query.into()).unwrap();
        assert_eq!(
            s.can_start_quicknote(
                &fence,
                Channel::Search,
                QuicknoteTrigger::CompleteEmpty,
                false
            ),
            Err(Error::Stale)
        );
        let fresh = s.begin().unwrap();
        s.deliver(&fresh, Channel::Search, DeliveryStatus::Complete, vec![])
            .unwrap();
        assert!(!s
            .can_start_quicknote(
                &fresh,
                Channel::Search,
                QuicknoteTrigger::CompleteEmpty,
                false
            )
            .unwrap());
    }
}
