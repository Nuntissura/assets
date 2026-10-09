use handpick::*;

fn context(name: &str) -> ContextId {
    ContextId::new(name).unwrap()
}
fn pin(name: &str) -> PinId {
    PinId::new(name).unwrap()
}
fn session() -> Session {
    Session::new(
        context("private"),
        Limits {
            max_text_bytes: 32,
            max_pins: 1,
            max_items: 4,
            max_item_bytes: 64,
            max_actions_per_item: 2,
        },
    )
}
fn item(name: &str) -> SearchItem {
    SearchItem {
        key: ResultKey {
            provider: ProviderId::new("notes").unwrap(),
            result: ResultId::new(name).unwrap(),
        },
        label: name.into(),
        detail: None,
        actions: vec![],
    }
}

#[test]
fn overflow_preserves_both_parked_and_occupied_text() {
    let mut s = session();
    s.set_draft("parked".into()).unwrap();
    s.pin(pin("p1")).unwrap();
    s.set_draft("occupied".into()).unwrap();
    let revision = s.draft_revision();
    assert_eq!(s.pin(pin("p2")), Err(Error::PinLimit));
    assert_eq!(s.set_draft("x".repeat(33)), Err(Error::TextLimit));
    assert_eq!(s.draft(), "occupied");
    assert_eq!(s.draft_revision(), revision);
    assert_eq!(s.pins()[0].tooltip_text(), "parked");
}

#[test]
fn restore_occupied_draft_is_an_atomic_swap_even_at_capacity() {
    let mut s = session();
    s.set_draft("first".into()).unwrap();
    s.pin(pin("p1")).unwrap();
    s.set_draft("second".into()).unwrap();
    assert_eq!(s.restore(&pin("p1"), None), Err(Error::OccupiedDraft));
    assert_eq!(
        s.restore(&pin("p1"), Some(pin("p1"))),
        Err(Error::DuplicatePin)
    );
    assert_eq!(s.draft(), "second");
    assert_eq!(s.pins()[0].tooltip_text(), "first");
    s.restore(&pin("p1"), Some(pin("p2"))).unwrap();
    assert_eq!(s.draft(), "first");
    assert_eq!(s.pins().len(), 1);
    assert_eq!(s.pins()[0].id(), &pin("p2"));
    assert_eq!(s.pins()[0].tooltip_text(), "second");
}

#[test]
fn revoked_scope_clears_drafts_pins_deliveries_and_selection() {
    let mut s = session();
    s.set_draft("secret".into()).unwrap();
    s.pin(pin("p1")).unwrap();
    s.set_draft("other secret".into()).unwrap();
    let old = s.begin().unwrap();
    s.deliver(
        &old,
        Channel::Search,
        DeliveryStatus::Complete,
        vec![item("secret")],
    )
    .unwrap();
    s.select(item("secret").key).unwrap();
    s.revoke();
    assert!(s.draft().is_empty());
    assert!(s.pins().is_empty());
    assert!(s.selected().is_none());
    assert!(s.delivery(Channel::Search).items().is_empty());
    assert_eq!(
        s.deliver(
            &old,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![item("secret")]
        ),
        Err(Error::Revoked)
    );
    s.switch_context(context("work")).unwrap();
    assert_eq!(
        s.deliver(
            &old,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![item("secret")]
        ),
        Err(Error::Stale)
    );
    s.switch_context(context("private")).unwrap();
    assert_eq!(
        s.deliver(
            &old,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![item("secret")]
        ),
        Err(Error::Stale)
    );
    assert!(s.draft().is_empty());
    assert!(s.pins().is_empty());
}

#[test]
fn late_generations_and_draft_changes_cannot_replace_current_results() {
    let mut s = session();
    let old = s.begin().unwrap();
    let current = s.begin().unwrap();
    assert!(current.generation > old.generation);
    s.deliver(
        &current,
        Channel::Suggestions,
        DeliveryStatus::Complete,
        vec![item("new")],
    )
    .unwrap();
    assert_eq!(
        s.deliver(
            &old,
            Channel::Suggestions,
            DeliveryStatus::Complete,
            vec![item("old")]
        ),
        Err(Error::Stale)
    );
    assert_eq!(s.delivery(Channel::Suggestions).items()[0].label, "new");
    s.set_draft("changed".into()).unwrap();
    assert_eq!(
        s.deliver(
            &current,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![item("new")]
        ),
        Err(Error::Stale)
    );
}

#[test]
fn selection_survives_reordering_but_never_retargets_missing_identity() {
    let mut s = session();
    let fence = s.begin().unwrap();
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Complete,
        vec![item("a"), item("b")],
    )
    .unwrap();
    s.select(item("a").key).unwrap();
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Complete,
        vec![item("b"), item("a")],
    )
    .unwrap();
    assert_eq!(s.selected(), Some(&item("a").key));
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Partial,
        vec![item("b")],
    )
    .unwrap();
    assert!(s.selected().is_none());
}

#[test]
fn completion_uses_utf8_boundaries_and_rejects_stale_or_oversize_edits() {
    let mut s = session();
    s.set_draft("é猫🙂 end".into()).unwrap();
    let fence = s.begin().unwrap();
    let edit = |start, end, replacement: &str| CompletionEdit {
        fence: fence.clone(),
        span: ByteSpan { start, end },
        replacement: replacement.into(),
    };
    assert_eq!(s.complete(edit(1, 2, "a")), Err(Error::InvalidSpan));
    assert_eq!(s.complete(edit(5, 6, "a")), Err(Error::InvalidSpan));
    assert_eq!(
        s.complete(edit(2, 5, &"x".repeat(33))),
        Err(Error::TextLimit)
    );
    assert_eq!(s.draft(), "é猫🙂 end");
    assert_eq!(s.complete(edit(2, 5, "犬")), Ok(5));
    assert_eq!(s.draft(), "é犬🙂 end");
    assert_eq!(s.complete(edit(2, 5, "鳥")), Err(Error::Stale));
}

#[test]
fn wire_version_identity_and_action_intent_are_explicit() {
    let contract = Envelope::v1(CompletionEdit {
        fence: Fence {
            context: context("work"),
            generation: 7,
            draft_revision: 3,
        },
        span: ByteSpan { start: 0, end: 2 },
        replacement: "é".into(),
    });
    let json = serde_json::to_string(&contract).unwrap();
    assert_eq!(
        serde_json::from_str::<Envelope<CompletionEdit>>(&json).unwrap(),
        contract
    );
    assert!(serde_json::from_str::<Envelope<CompletionEdit>>(
        &json.replace("handpick.v1", "handpick.v2")
    )
    .is_err());
    assert!(serde_json::from_str::<ContextId>("\"\"").is_err());
    assert!(ContextId::new("x".repeat(MAX_ID_BYTES + 1)).is_err());
}

#[test]
fn invalid_payload_admission_is_atomic_and_error_is_not_empty_success() {
    let mut s = session();
    let fence = s.begin().unwrap();
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Complete,
        vec![item("a")],
    )
    .unwrap();
    assert_eq!(
        s.deliver(
            &fence,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![item("b"), item("b")]
        ),
        Err(Error::DuplicateResult)
    );
    assert_eq!(s.delivery(Channel::Search).items()[0].label, "a");
    s.deliver(&fence, Channel::Search, DeliveryStatus::Failed, vec![])
        .unwrap();
    assert_eq!(s.delivery(Channel::Search).status(), DeliveryStatus::Failed);
    assert_eq!(
        s.delivery(Channel::Suggestions).status(),
        DeliveryStatus::Pending
    );
}
