mod fixture;
use fixture::*;
use hsscrap::interaction::*;
use hsscrap::operations::{finish_operation, pending_outcome, OwnerStepReceipt};
use hsscrap::ports::OwnerCommand;
use hsscrap::*;
use std::collections::BTreeSet;

fn committed(request: &SaveRequest) -> OperationOutcome {
    let owners: Vec<_> = request
        .plan
        .commands
        .iter()
        .enumerate()
        .map(|(index, _)| host("owner", &index.to_string()))
        .collect();
    let receipts: Vec<_> = owners
        .iter()
        .enumerate()
        .map(|(index, owner)| OwnerStepReceipt {
            operation_id: request.ticket.operation_id,
            intent_digest: request.plan.context.intent_digest.clone(),
            command_index: index,
            owner: owner.clone(),
            receipt: host("receipt", &index.to_string()),
        })
        .collect();
    finish_operation(&request.plan, &receipts, &owners, vec![], at()).unwrap()
}

#[test]
fn origins_unknown_and_clear_do_not_fabricate_source_values() {
    let value = inspection();
    let metadata = value.metadata().unwrap();
    assert_eq!(
        metadata.title.as_deref(),
        Some("A field guide worth keeping")
    );
    assert_eq!(metadata.description, None);
    assert_eq!(metadata.published_at, None);
    assert_eq!(metadata.author, None);
    assert_eq!(
        metadata_origin(&value.item.description_override, Some(&"source".to_owned())),
        MetadataOrigin::Cleared
    );
    assert_eq!(
        metadata_origin::<String>(&Override::Inherit, None),
        MetadataOrigin::Unknown
    );
    assert_eq!(value.effective_tags()[&tag("design")].len(), 2);
}

#[test]
fn cancel_discards_draft_without_owner_commands_or_snapshot_mutation() {
    let original = inspection();
    let mut session = Session::new(original.clone()).unwrap();
    session.begin_edit().unwrap();
    session
        .set_title(Override::Set("A local edit".into()))
        .unwrap();
    session
        .remove_tag(AssignmentId(uuid::Uuid::from_u128(22)))
        .unwrap();
    session
        .stage_tag(assignment(30, "later", TagTarget::Item(item_id())))
        .unwrap();
    assert!(session.dirty());
    assert!(session.cancel());
    assert_eq!(session.inspection(), Some(&original));
    assert!(session.pending().is_none());
    assert!(session.draft().is_none());
    assert_eq!(session.phase(), &Phase::Inspect);
}

#[test]
fn inherited_removal_keeps_the_assignment_owner_and_revision_fences() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    let id = AssignmentId(uuid::Uuid::from_u128(22));
    session.remove_tag(id).unwrap();
    let request = session.begin_save(context(40), &BTreeSet::new()).unwrap();
    assert_eq!(request.plan.commands, vec![OwnerCommand::RemoveTag(id)]);
    assert_eq!(request.removal_targets, vec![(id, project())]);
    assert_eq!(
        request.plan.context.expected_revisions,
        inspection().owner_fences
    );
    assert_eq!(
        session.inspection().unwrap().assignments,
        inspection().assignments
    );
    assert!(!session.cancel());
}

#[test]
fn uncertain_save_keeps_original_identity_and_rejects_new_save() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    session.set_title(Override::Set("Changed".into())).unwrap();
    let request = session.begin_save(context(41), &BTreeSet::new()).unwrap();
    assert!(session.complete(
        request.ticket,
        Err(Error::UnknownOutcome {
            operation_id: request.ticket.operation_id
        })
    ));
    assert_eq!(
        session.phase(),
        &Phase::Unknown(request.ticket.operation_id)
    );
    assert_eq!(session.pending(), Some(&request));
    assert!(session.begin_save(context(42), &BTreeSet::new()).is_err());
    assert!(matches!(
        session.reload(inspection()),
        Err(Error::UnknownOutcome { .. })
    ));
    assert!(!session.cancel());
    assert!(session.complete(request.ticket, Ok(committed(&request))));
    assert_eq!(session.phase(), &Phase::ReloadRequired);
    assert!(session.draft().is_none());
    assert!(session.begin_edit().is_err());
}

#[test]
fn conflict_keeps_draft_until_explicit_reload_and_stale_completion_is_ignored() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    session.set_title(Override::Set("My draft".into())).unwrap();
    let request = session.begin_save(context(43), &BTreeSet::new()).unwrap();
    assert!(session.complete(
        request.ticket,
        Err(Error::Conflict {
            field: "item_revision".into()
        })
    ));
    assert!(matches!(session.phase(), Phase::Conflict(_)));
    assert_eq!(
        session.draft().unwrap().title,
        Override::Set("My draft".into())
    );
    assert!(session.set_title(Override::Clear).is_err());
    let mut fresh = inspection();
    fresh.item.title_override = Override::Set("Owner's current title".into());
    fresh.item.revision = Revision(3);
    fresh.owner_fences[0].revision = Revision(3);
    session.reload(fresh).unwrap();
    assert!(!session.complete(request.ticket, Ok(pending_outcome(&request.plan))));
    assert_eq!(
        session
            .inspection()
            .unwrap()
            .metadata()
            .unwrap()
            .title
            .as_deref(),
        Some("Owner's current title")
    );
}

#[test]
fn revocation_erases_data_and_invalidates_even_reused_operation_ids() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    session.set_title(Override::Clear).unwrap();
    let old = session.begin_save(context(44), &BTreeSet::new()).unwrap();
    session.revoke();
    assert_eq!(session.phase(), &Phase::Revoked);
    assert!(
        session.inspection().is_none() && session.draft().is_none() && session.pending().is_none()
    );
    session.reload(inspection()).unwrap();
    session.begin_edit().unwrap();
    session.set_description(Override::Inherit).unwrap();
    let fresh = session.begin_save(context(44), &BTreeSet::new()).unwrap();
    assert_ne!(old.ticket, fresh.ticket);
    assert!(!session.complete(old.ticket, Ok(pending_outcome(&old.plan))));
    assert_eq!(session.pending(), Some(&fresh));
}

#[test]
fn unauthorized_targets_and_changed_fences_cannot_produce_a_save() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    assert_eq!(
        session.remove_tag(AssignmentId(uuid::Uuid::from_u128(23))),
        Err(Error::Denied)
    );
    assert_eq!(
        session.stage_tag(assignment(
            31,
            "later",
            TagTarget::Item(ItemId(uuid::Uuid::from_u128(999)))
        )),
        Err(Error::Denied)
    );
    session.set_title(Override::Set("Changed".into())).unwrap();
    let mut stale = context(45);
    stale.expected_revisions[0].revision = Revision(1);
    assert!(matches!(
        session.begin_save(stale, &BTreeSet::new()),
        Err(Error::Conflict { .. })
    ));
    assert_eq!(session.phase(), &Phase::Editing);
    assert!(session.pending().is_none());
}

#[test]
fn canonical_tags_deduplicate_by_identity_and_support_undo() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    session
        .remove_tag(AssignmentId(uuid::Uuid::from_u128(21)))
        .unwrap();
    session
        .stage_tag(assignment(32, "design", TagTarget::Item(item_id())))
        .unwrap();
    assert!(session.draft().unwrap().removals.is_empty());
    assert!(session.draft().unwrap().additions.is_empty());
    session
        .stage_tag(assignment(33, "other-design", TagTarget::Item(item_id())))
        .unwrap();
    assert_eq!(
        session.draft().unwrap().additions[0].tag_ref,
        tag("other-design")
    );
    session
        .remove_tag(AssignmentId(uuid::Uuid::from_u128(33)))
        .unwrap();
    assert!(!session.dirty());
}

#[test]
fn oversized_metadata_is_rejected_before_owner_dispatch() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    assert!(matches!(
        session.set_description(Override::Set("x".repeat(bounds().max_text_bytes + 1))),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(!session.dirty());
    assert!(session.pending().is_none());
}

#[test]
fn authorized_profile_scope_can_inspect_account_owned_item_and_context_tags() {
    let mut value = inspection();
    value.scope = ScopeRef("work-profile".into());
    value.item.owner_scope = ScopeRef("account-root".into());
    value.targets[0].scope = ScopeRef("account-root".into());
    value.assignments[0].scope = ScopeRef("account-root".into());
    for target in &mut value.targets[1..] {
        target.scope = ScopeRef("work-profile".into());
    }
    for assignment in &mut value.assignments[1..] {
        assignment.scope = ScopeRef("work-profile".into());
    }
    value.memberships[0].scope = ScopeRef("work-profile".into());
    let mut session = Session::new(value.clone()).unwrap();
    assert_eq!(
        session.inspection().unwrap().effective_tags()[&tag("design")].len(),
        2
    );
    session.begin_edit().unwrap();
    let id = AssignmentId(uuid::Uuid::from_u128(22));
    session.remove_tag(id).unwrap();
    let mut command = context(46);
    command.scope = ScopeRef("work-profile".into());
    let request = session.begin_save(command, &BTreeSet::new()).unwrap();
    assert_eq!(request.removal_targets, vec![(id, project())]);
    value.assignments[1].scope = ScopeRef("another-profile".into());
    assert!(matches!(Session::new(value), Err(Error::Denied)));
}

#[test]
fn malformed_or_mismatched_committed_responses_keep_draft_and_pending_operation() {
    let cases: &[(&str, fn(&mut OperationOutcome))] = &[
        ("missing commit time", |value| value.committed_at = None),
        ("missing receipts", |value| value.owner_receipt_refs.clear()),
        ("partial receipts", |value| {
            value.owner_receipt_refs.pop();
        }),
        ("duplicate receipts", |value| {
            value.owner_receipt_refs[1] = value.owner_receipt_refs[0].clone()
        }),
        ("unresolvable receipt", |value| {
            value.owner_receipt_refs[0].native_id.clear()
        }),
        ("oversized receipt", |value| {
            value.owner_receipt_refs[0].native_id = "x".repeat(bounds().max_text_bytes + 1)
        }),
        ("oversized result reference", |value| {
            value
                .result_refs
                .push(host("result", &"x".repeat(bounds().max_text_bytes + 1)))
        }),
        ("duplicate revision owner", |value| {
            value
                .expected_revisions
                .push(value.expected_revisions[0].clone())
        }),
        ("changed revision", |value| {
            value.expected_revisions[0].revision = Revision(99)
        }),
        ("missing revision owner", |value| {
            value.expected_revisions.pop();
        }),
        ("extra revision owner", |value| {
            value.expected_revisions.push(ExpectedRevision {
                owner: host("owner", "unexpected"),
                revision: Revision(1),
            })
        }),
        ("changed revision owner", |value| {
            value.expected_revisions[0].owner = host("owner", "another")
        }),
        ("invalid revision", |value| {
            value.expected_revisions[0].revision = Revision(0)
        }),
        ("other operation", |value| {
            value.operation_id = OperationId(uuid::Uuid::from_u128(999))
        }),
        ("other scope", |value| {
            value.scope = ScopeRef("another-profile".into())
        }),
        ("other intent", |value| {
            value.intent_digest = "different-intent".into()
        }),
        ("unsupported required feature", |value| {
            value
                .common
                .required_features
                .insert("hsscrap.unknown".into());
        }),
        ("oversized aggregate", |value| {
            value.common.extensions.insert(
                "hsscrap.large".into(),
                serde_json::Value::Array(
                    (0..9)
                        .map(|_| serde_json::Value::String("x".repeat(bounds().max_text_bytes)))
                        .collect(),
                ),
            );
        }),
    ];
    for (name, alter) in cases {
        let original = inspection();
        let mut session = Session::new(original.clone()).unwrap();
        session.begin_edit().unwrap();
        session
            .set_title(Override::Set("Keep this draft".into()))
            .unwrap();
        session
            .remove_tag(AssignmentId(uuid::Uuid::from_u128(22)))
            .unwrap();
        let draft = session.draft().unwrap().clone();
        let request = session.begin_save(context(47), &BTreeSet::new()).unwrap();
        let mut response = committed(&request);
        alter(&mut response);
        assert!(session.complete(request.ticket, Ok(response)), "{name}");
        assert_eq!(
            session.phase(),
            &Phase::Unknown(request.ticket.operation_id),
            "{name}"
        );
        assert_eq!(session.draft(), Some(&draft), "{name}");
        assert_eq!(session.pending(), Some(&request), "{name}");
        assert_eq!(session.inspection(), Some(&original), "{name}");
        assert!(!session.cancel(), "{name}");
        assert!(
            matches!(
                session.reload(inspection()),
                Err(Error::UnknownOutcome { .. })
            ),
            "{name}"
        );
        assert!(
            session.complete(request.ticket, Ok(committed(&request))),
            "{name}"
        );
        assert_eq!(session.phase(), &Phase::ReloadRequired, "{name}");
    }
}

#[test]
fn valid_committed_outcome_compares_revision_guards_by_owner_not_vector_order() {
    let mut session = Session::new(inspection()).unwrap();
    session.begin_edit().unwrap();
    session
        .set_title(Override::Set("Saved title".into()))
        .unwrap();
    let supported = BTreeSet::from(["hsscrap.known".into()]);
    let request = session.begin_save(context(48), &supported).unwrap();
    let mut response = committed(&request);
    response.expected_revisions.reverse();
    response
        .common
        .required_features
        .insert("hsscrap.known".into());
    assert!(session.complete(request.ticket, Ok(response)));
    assert_eq!(session.phase(), &Phase::ReloadRequired);
    assert!(session.draft().is_none() && session.pending().is_none());
    assert!(session.begin_edit().is_err());
    assert_eq!(
        session.inspection().unwrap().item.title_override,
        inspection().item.title_override
    );
}
