use handpick::*;

#[test]
fn writing_lookup_purpose_never_turns_navigation_or_actions_into_prose() {
    for purpose in [QueryPurpose::Navigation, QueryPurpose::Commands, QueryPurpose::Settings] {
        let mut s = session("purpose");
        assert!(s.set_query_purpose(purpose).unwrap());
        let fence = s.begin_lookup().unwrap();
        s.deliver(&fence, Channel::Suggestions, DeliveryStatus::Complete, vec![]).unwrap();
        assert!(!s.can_expand());
        assert!(!s.expand_if_empty(&fence).unwrap());
        assert!(!s.expand_for_prose(&fence, 200, 2).unwrap());
        assert!(!s.present_content(&fence, 200, 2, false).unwrap());
        assert!(!s.present_draft(&fence, 200, 2, false, "Explicit title").unwrap());
        s.expand().unwrap();
        assert!(s.present_content(&fence, 200, 2, false).unwrap());
        assert!(s.present_draft(&fence, 0, 1, true, "Explicit title").unwrap());
        assert!(!s.present_draft(&fence, 0, 1, true, "").unwrap());
    }
    let mut s = session("plain");
    assert_eq!(s.query_purpose(), QueryPurpose::SearchWrite);
    let fence = s.begin_lookup().unwrap();
    s.deliver(&fence, Channel::Suggestions, DeliveryStatus::Complete, vec![]).unwrap();
    assert!(s.expand_if_empty(&fence).unwrap());
    s.compact().unwrap();
    assert!(s.expand_for_prose(&fence, 160, 1).unwrap());
    s.compact().unwrap();
    assert!(s.present_content(&fence, 5, 2, false).unwrap());
}

#[test]
fn writing_purpose_change_fences_delivery_immediately_and_retains_native_draft() {
    let mut s = session("mode-freshness");
    let old = s.begin_lookup().unwrap();
    s.deliver(&old, Channel::Suggestions, DeliveryStatus::Complete, vec![]).unwrap();
    s.expand().unwrap();
    let editor = s.editor().map(|(id, revision)| (id.clone(), revision));
    assert!(s.set_query_purpose(QueryPurpose::Commands).unwrap());
    assert_eq!(s.query_purpose(), QueryPurpose::Commands);
    assert!(s.expanded());
    assert_eq!(s.editor().map(|(id, revision)| (id.clone(), revision)), editor);
    assert_eq!(s.validate(&old), Err(Error::Stale));
    assert_eq!(s.deliver(&old, Channel::Suggestions, DeliveryStatus::Complete, vec![]), Err(Error::Stale));
    assert_eq!(s.delivery(Channel::Suggestions).status(), DeliveryStatus::Pending);
    let fresh = s.fence().unwrap();
    assert!(!s.set_query_purpose(QueryPurpose::Commands).unwrap());
    assert_eq!(s.fence().unwrap(), fresh);
    s.edited(&fresh, fresh.revision + 1).unwrap();
    assert_eq!(s.validate(&fresh), Err(Error::Stale));
    let before_clear = s.fence().unwrap();
    s.present_content(&before_clear, 0, 1, true).unwrap();
    assert_eq!(s.validate(&before_clear), Err(Error::Stale));
    assert!(!s.expanded());
}

#[test]
fn writing_purpose_change_cannot_interrupt_ime_or_pending_save() {
    let mut s = session("mode-ime-save");
    let token = s.begin_composition(&s.fence().unwrap()).unwrap();
    let composing = s.fence().unwrap();
    assert_eq!(s.set_query_purpose(QueryPurpose::Navigation), Err(Error::Stale));
    assert_eq!(s.query_purpose(), QueryPurpose::SearchWrite);
    assert_eq!(s.fence().unwrap(), composing);
    s.end_composition(&token).unwrap();
    let submitted = intent(&s);
    s.submit(submitted.clone()).unwrap();
    assert_eq!(s.set_query_purpose(QueryPurpose::Settings), Err(Error::Stale));
    assert_eq!(s.pending_save(), Some(&submitted));
    assert_eq!(s.fence().unwrap(), composing);
    s.acknowledge(&SaveOutcome { submitted, content: ContentOutcome::Rejected }).unwrap();
    assert!(s.set_query_purpose(QueryPurpose::Settings).unwrap());
}

#[test]
fn writing_optional_title_preserves_explicit_metadata_and_pending_save() {
    assert_eq!(resolve_draft_title("  My idea  ", "first body"), "My idea");
    assert_eq!(resolve_draft_title("My idea", "edited body"), "My idea");
    assert_eq!(resolve_draft_title(" \n\t", "  first body  "), "first body");
    assert_eq!(resolve_draft_title("", ""), "");
    let mut session = session("title-save");
    let fence = session.fence().unwrap();
    session.present_draft(&fence, 0, 1, true, "Title only").unwrap();
    let editor = session.editor().map(|(id, revision)| (id.clone(), revision));
    session.submit(intent(&session)).unwrap();
    assert!(session.present_draft(&fence, 0, 1, true, "").unwrap());
    assert_eq!(session.editor().map(|(id, revision)| (id.clone(), revision)), editor);
    assert_eq!(session.fence().unwrap(), fence);
}

fn session(name: &str) -> WritingSession {
    let mut session = WritingSession::new(
        ContextId::new("private").unwrap(),
        SessionId::new(name).unwrap(),
        1,
    );
    session.bind(EditorId::new("editor-a").unwrap(), 1).unwrap();
    session
}
fn intent(session: &WritingSession) -> SaveIntent {
    SaveIntent {
        operation: OperationId::new("operation-a").unwrap(),
        fence: session.fence().unwrap(),
        destination: SaveDestination::Append {
            note: NoteId::new("note-a").unwrap(),
            expected_revision: RevisionId::new("revision-a").unwrap(),
        },
    }
}
fn committed(submitted: SaveIntent) -> SaveOutcome {
    SaveOutcome {
        submitted,
        content: ContentOutcome::Committed {
            note: NoteId::new("note-a").unwrap(),
            revision: RevisionId::new("revision-b").unwrap(),
            relationships: ProjectionReadiness::Failed,
            search: ProjectionReadiness::Pending,
        },
    }
}

#[test]
fn writing_lifecycle_replacement_and_revocation_reject_old_callbacks() {
    let mut old = session("old");
    let fence = old.fence().unwrap();
    let fresh = session("fresh");
    assert_eq!(fresh.validate(&fence), Err(Error::Stale));
    old.revoke();
    assert_eq!(old.validate(&fence), Err(Error::Revoked));
    assert!(old.editor().is_none());
    assert!(old.pins().is_empty());
}

#[test]
fn writing_occupied_restore_retains_owner_handles_and_fences_callbacks() {
    let mut s = session("s");
    let old = s.fence().unwrap();
    s.expand().unwrap();
    assert_eq!(s.fence().unwrap(), old);
    s.park(PinId::new("a").unwrap()).unwrap();
    s.bind(EditorId::new("editor-b").unwrap(), 7).unwrap();
    let before = s.fence().unwrap();
    assert_eq!(
        s.park(PinId::new("overflow").unwrap()),
        Err(Error::PinLimit)
    );
    assert_eq!(
        s.restore(&PinId::new("a").unwrap(), None),
        Err(Error::OccupiedDraft)
    );
    assert_eq!(s.fence().unwrap(), before);
    s.restore(&PinId::new("a").unwrap(), Some(PinId::new("b").unwrap()))
        .unwrap();
    assert_eq!(s.editor(), Some((&old.editor, 1)));
    assert_eq!(s.pins()[0].editor.as_str(), "editor-b");
    assert_eq!(s.pins()[0].revision, 7);
    assert_eq!(s.validate(&old), Err(Error::Stale));
    assert!(s.expanded());
}

#[test]
fn writing_commit_with_failed_projection_preserves_newer_writing() {
    let mut s = session("s");
    let save = intent(&s);
    s.submit(save.clone()).unwrap();
    s.edited(&save.fence, 2).unwrap();
    let newer = s.fence().unwrap();
    assert_eq!(
        s.acknowledge(&committed(save)),
        Ok(SaveDisposition::OlderRevisionCommitted)
    );
    assert_eq!(s.fence().unwrap(), newer);
    assert!(s.pending_save().is_none());
}

#[test]
fn writing_unknown_commit_requires_reconciliation_and_wrong_target_rejects() {
    let mut s = session("s");
    let save = intent(&s);
    s.submit(save.clone()).unwrap();
    assert_eq!(
        s.acknowledge(&SaveOutcome {
            submitted: save.clone(),
            content: ContentOutcome::Unknown
        }),
        Ok(SaveDisposition::ReconcileBeforeRetry)
    );
    assert_eq!(s.submit(save.clone()), Err(Error::Stale));
    let mut wrong = committed(save.clone());
    if let ContentOutcome::Committed { note, .. } = &mut wrong.content {
        *note = NoteId::new("wrong-note").unwrap();
    }
    assert_eq!(s.acknowledge(&wrong), Err(Error::Stale));
    assert_eq!(s.pending_save(), Some(&save));
    assert_eq!(
        s.acknowledge(&committed(save)),
        Ok(SaveDisposition::CurrentRevisionCommitted)
    );
    assert!(s.editor().is_some());
}

#[test]
fn writing_insert_requires_anchor_and_rejects_stale_revision_without_loss() {
    let mut s = session("s");
    let mut save = intent(&s);
    save.destination = SaveDestination::Insert {
        target: NoteAddress::Note {
            note: NoteId::new("note-a").unwrap(),
        },
        expected_revision: RevisionId::new("revision-a").unwrap(),
        side: InsertionSide::After,
    };
    assert_eq!(s.submit(save.clone()), Err(Error::InvalidSpan));
    assert!(s.pending_save().is_none());
    save.destination = SaveDestination::Insert {
        target: NoteAddress::Block {
            note: NoteId::new("note-a").unwrap(),
            block: BlockId::new("nested-a").unwrap(),
        },
        expected_revision: RevisionId::new("revision-a").unwrap(),
        side: InsertionSide::After,
    };
    s.edited(&save.fence, 2).unwrap();
    assert_eq!(s.submit(save.clone()), Err(Error::Stale));
    save.fence = s.fence().unwrap();
    s.submit(save).unwrap();
}

#[test]
fn writing_continuation_keeps_canonical_identity_and_rejects_self_lineage() {
    let mut s = session("continuation");
    let mut save = intent(&s);
    save.destination = SaveDestination::Update {
        note: NoteId::new("note-a").unwrap(),
        expected_revision: RevisionId::new("revision-a").unwrap(),
        prior_operation: save.operation.clone(),
    };
    assert_eq!(s.submit(save.clone()), Err(Error::InvalidSpan));
    assert!(s.pending_save().is_none());
    if let SaveDestination::Update { prior_operation, .. } = &mut save.destination {
        *prior_operation = OperationId::new("operation-prior").unwrap();
    }
    let wire = serde_json::to_string(&save).unwrap();
    assert_eq!(serde_json::from_str::<SaveIntent>(&wire).unwrap(), save);
    s.submit(save.clone()).unwrap();
    let mut wrong = committed(save.clone());
    if let ContentOutcome::Committed { note, .. } = &mut wrong.content { *note = NoteId::new("other-note").unwrap(); }
    assert_eq!(s.acknowledge(&wrong), Err(Error::Stale));
    assert_eq!(s.pending_save(), Some(&save));
    assert_eq!(s.acknowledge(&committed(save)), Ok(SaveDisposition::CurrentRevisionCommitted));
}

#[test]
fn writing_ime_blocks_actions_save_and_pin_without_changing_editor() {
    let mut s = session("s");
    let save = intent(&s);
    let token = s.begin_composition(&save.fence).unwrap();
    let composing_fence = s.fence().unwrap();
    assert_eq!(
        s.validate_action(&WritingActionIntent {
            fence: save.fence.clone(),
            action: WritingAction::Save {
                destination: save.destination.clone()
            }
        }),
        Err(Error::Stale)
    );
    assert_eq!(s.park(PinId::new("a").unwrap()), Err(Error::Stale));
    assert_eq!(s.submit(save.clone()), Err(Error::Stale));
    assert_eq!(s.fence().unwrap(), composing_fence);
    s.edited(&composing_fence, 2).unwrap();
    s.end_composition(&token).unwrap();
    let token2 = s.begin_composition(&s.fence().unwrap()).unwrap();
    assert_eq!(s.end_composition(&token), Err(Error::Stale));
    s.end_composition(&token2).unwrap();
}

#[test]
fn writing_provider_query_is_explicit_bounded_and_lifecycle_scoped() {
    let s = session("s");
    let mut provider = WritingProvider {
        provider: ProviderId::new("notes").unwrap(),
        canonical_set: RevisionId::new("owner-authorized-notes").unwrap(),
        capabilities: vec![CapabilityDeclaration {
            capability: WritingCapability::Search,
            support: CapabilitySupport::Unsupported,
        }],
        max_query_bytes: 8,
        max_page_items: 2,
    };
    let mut query = CompositionQuery {
        fence: s.fence().unwrap(),
        query: "selected".into(),
        provider: provider.provider.clone(),
        page_items: 2,
        cursor: None,
    };
    assert_eq!(provider.validate_query(&s, &query), Err(Error::Revoked));
    provider.capabilities[0].support = CapabilitySupport::Supported {
        contract: RevisionId::new("notes.search.v1").unwrap(),
    };
    provider.validate_query(&s, &query).unwrap();
    query.query.push('!');
    assert_eq!(
        provider.validate_query(&s, &query),
        Err(Error::PayloadLimit)
    );
    query.query = "selected".into();
    assert_eq!(
        provider.validate_query(&session("replacement"), &query),
        Err(Error::Stale)
    );
    assert_eq!(s.fence().unwrap(), query.fence);
}

#[test]
fn writing_wire_distinguishes_occurrence_target_and_unsupported_capability() {
    let evidence = RelationshipEvidence::Linked {
        relationship: RelationshipId::new("edge-a").unwrap(),
        source_note: NoteId::new("source-a").unwrap(),
        source_block: Some(BlockId::new("occurrence-a").unwrap()),
        source_revision: RevisionId::new("hash-a").unwrap(),
        target: NoteAddress::Block {
            note: NoteId::new("target-a").unwrap(),
            block: BlockId::new("target-block").unwrap(),
        },
        context: Some(RelationshipContext {
            source_title: "Source".into(),
            snippet: Some("Source occurrence context".into()),
            source_hash: Some(RevisionId::new("hash-a").unwrap()),
        }),
    };
    let json = serde_json::to_value(Envelope::v1(evidence)).unwrap();
    assert_eq!(json["payload"]["source_block"], "occurrence-a");
    assert_eq!(json["payload"]["target"]["block"], "target-block");
    assert!(serde_json::from_str::<NoteAddress>(
        r#"{"kind":"block","note":"a","block":"","extra":true}"#
    )
    .is_err());
    assert!(serde_json::from_str::<CapabilitySupport>(r#"{"state":"supported"}"#).is_err());
}

#[test]
fn writing_lookup_generation_and_json_counters_preserve_identity() {
    let mut s = session("s");
    let first = s.begin_lookup().unwrap();
    let second = s.begin_lookup().unwrap();
    assert_eq!(s.validate(&first), Err(Error::Stale));
    s.validate(&second).unwrap();
    let mut wide = second;
    wide.revision = 9_007_199_254_740_993;
    let json = serde_json::to_value(&wide).unwrap();
    assert_eq!(json["revision"], "9007199254740993");
    assert_eq!(serde_json::from_value::<WritingFence>(json).unwrap(), wide);
}

#[test]
fn writing_ack_during_ime_never_retires_preedit_and_lookup_does_not_change_save_revision() {
    let mut s = session("s");
    let save = intent(&s);
    s.submit(save.clone()).unwrap();
    let token = s.begin_composition(&s.fence().unwrap()).unwrap();
    assert_eq!(
        s.acknowledge(&committed(save)),
        Ok(SaveDisposition::RetainActiveComposition)
    );
    s.end_composition(&token).unwrap();
    let save = intent(&s);
    s.submit(save.clone()).unwrap();
    s.begin_lookup().unwrap();
    assert_eq!(
        s.acknowledge(&committed(save)),
        Ok(SaveDisposition::CurrentRevisionCommitted)
    );
}

#[test]
fn writing_relationship_pages_require_current_freshness_and_canonical_count_evidence() {
    let s = session("s");
    let provider = ProviderId::new("loom").unwrap();
    let mut page = RelationshipPage {
        fence: s.fence().unwrap(),
        provider: provider.clone(),
        projection_revision: None,
        freshness: ProjectionReadiness::Ready,
        status: DeliveryStatus::Complete,
        counts: RelationshipCounts::PageOnly,
        next_cursor: None,
        rows: vec![],
    };
    assert_eq!(page.validate(&s, &provider, 10), Err(Error::Stale));
    page.projection_revision = Some(RevisionId::new("projection-a").unwrap());
    page.validate(&s, &provider, 10).unwrap();
    page.freshness = ProjectionReadiness::Pending;
    page.counts = RelationshipCounts::CanonicalAuthorized {
        source_notes: 1,
        relationships: 2,
    };
    assert_eq!(page.validate(&s, &provider, 10), Err(Error::Stale));
    page.counts = RelationshipCounts::AuthorizedProjection {
        source_notes: 1,
        relationships: 2,
    };
    page.validate(&s, &provider, 10).unwrap();
}

#[test]
fn writing_results_preserve_stable_selection_and_admit_only_completed_empty_expansion() {
    let mut s = session("results");
    let first = s.begin_lookup().unwrap();
    let item = |id: &str| SearchItem {
        key: ResultKey {
            provider: ProviderId::new("notes").unwrap(),
            result: ResultId::new(id).unwrap(),
        },
        label: id.into(),
        detail: None,
        actions: vec![],
    };
    let a = item("a");
    let b = item("b");
    s.deliver(
        &first,
        Channel::Suggestions,
        DeliveryStatus::Complete,
        vec![a.clone(), b.clone()],
    )
    .unwrap();
    s.select(&first, a.key.clone()).unwrap();
    s.deliver(
        &first,
        Channel::Suggestions,
        DeliveryStatus::Complete,
        vec![b.clone(), a.clone()],
    )
    .unwrap();
    assert_eq!(s.selected(), Some(&a.key));
    assert!(!s.expand_if_empty(&first).unwrap());
    let second = s.begin_lookup().unwrap();
    assert!(s.selected().is_none());
    assert_eq!(
        s.deliver(
            &first,
            Channel::Suggestions,
            DeliveryStatus::Complete,
            vec![]
        ),
        Err(Error::Stale)
    );
    for status in [
        DeliveryStatus::Pending,
        DeliveryStatus::Offline,
        DeliveryStatus::Partial,
        DeliveryStatus::Failed,
        DeliveryStatus::StaleIndex,
    ] {
        s.deliver(&second, Channel::Suggestions, status, vec![])
            .unwrap();
        assert!(!s.expand_if_empty(&second).unwrap());
    }
    s.deliver(
        &second,
        Channel::Suggestions,
        DeliveryStatus::Complete,
        vec![],
    )
    .unwrap();
    assert!(s.expand_if_empty(&second).unwrap());
    s.deliver(
        &second,
        Channel::Suggestions,
        DeliveryStatus::Complete,
        vec![a.clone()],
    )
    .unwrap();
    assert!(s.expanded());
    s.compact().unwrap();
    assert!(!s.expanded());
    s.select(&second, a.key.clone()).unwrap();
    assert_eq!(
        s.deliver(
            &second,
            Channel::Suggestions,
            DeliveryStatus::Complete,
            vec![a.clone(), a]
        ),
        Err(Error::DuplicateResult)
    );
    assert!(s.selected().is_some());
    s.revoke();
    assert!(s.delivery(Channel::Suggestions).items().is_empty());
    assert!(s.selected().is_none());
}

#[test]
fn writing_overflow_is_independent_of_lookup_and_preserves_native_identity() {
    for status in [DeliveryStatus::Pending, DeliveryStatus::Complete, DeliveryStatus::Partial, DeliveryStatus::Failed, DeliveryStatus::Offline] {
        let mut s = session("overflow");
        let fence = s.begin_lookup().unwrap();
        s.deliver(&fence, Channel::Suggestions, status, vec![]).unwrap();
        assert!(!s.expand_for_overflow(&fence, false, true).unwrap());
        assert!(!s.expand_for_overflow(&fence, true, false).unwrap());
        if status == DeliveryStatus::Complete {
            s.deliver(&fence, Channel::Suggestions, status, vec![SearchItem {
                key: ResultKey { provider: ProviderId::new("notes").unwrap(), result: ResultId::new("match").unwrap() },
                label: "Match".into(), detail: None, actions: vec![],
            }]).unwrap();
        }
        let editor = s.editor().map(|(id, revision)| (id.clone(), revision));
        assert!(s.expand_for_overflow(&fence, true, true).unwrap());
        assert!(s.expanded());
        assert_eq!(s.fence().unwrap(), fence);
        assert_eq!(s.editor().map(|(id, revision)| (id.clone(), revision)), editor);
        assert_eq!(s.delivery(Channel::Suggestions).status(), status);
        s.compact().unwrap();
        assert!(!s.expanded());
    }
}

#[test]
fn writing_overflow_respects_lookup_modes_ime_and_pending_save() {
    for purpose in [QueryPurpose::Navigation, QueryPurpose::Commands, QueryPurpose::Settings] {
        let mut s = session("overflow-mode");
        s.set_query_purpose(purpose).unwrap();
        assert!(!s.expand_for_overflow(&s.fence().unwrap(), true, true).unwrap());
        assert!(!s.expanded());
    }
    let mut s = session("overflow-guard");
    let token = s.begin_composition(&s.fence().unwrap()).unwrap();
    assert!(!s.expand_for_overflow(&s.fence().unwrap(), true, true).unwrap());
    s.end_composition(&token).unwrap();
    let submitted = intent(&s);
    s.submit(submitted.clone()).unwrap();
    assert!(!s.expand_for_overflow(&s.fence().unwrap(), true, true).unwrap());
    assert_eq!(s.pending_save(), Some(&submitted));
    s.acknowledge(&SaveOutcome { submitted, content: ContentOutcome::Rejected }).unwrap();
    assert!(s.expand_for_overflow(&s.fence().unwrap(), true, true).unwrap());
}

#[test]
fn writing_overflow_rejects_stale_and_revoked_observations() {
    let mut s = session("overflow-stale");
    let old = s.fence().unwrap();
    s.edited(&old, old.revision + 1).unwrap();
    assert_eq!(s.expand_for_overflow(&old, true, true), Err(Error::Stale));
    assert!(!s.expanded());
    let current = s.fence().unwrap();
    s.revoke();
    assert_eq!(s.expand_for_overflow(&current, true, true), Err(Error::Revoked));
}
