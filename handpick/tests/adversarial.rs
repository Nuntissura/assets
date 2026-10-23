use handpick::*;

fn session() -> Session {
    Session::new(
        ContextId::new("private").unwrap(),
        Limits {
            max_text_bytes: 16,
            max_pins: 2,
            max_items: 2,
            max_item_bytes: 8,
            max_actions_per_item: 1,
        },
    )
}
fn pin(id: &str) -> PinId {
    PinId::new(id).unwrap()
}
fn item(id: &str) -> SearchItem {
    SearchItem {
        key: ResultKey {
            provider: ProviderId::new("notes").unwrap(),
            result: ResultId::new(id).unwrap(),
        },
        label: "猫".into(),
        detail: Some("é".into()),
        actions: vec![],
    }
}

#[test]
fn rejected_payload_retains_selection_and_prior_failure_state() {
    let mut s = session();
    let fence = s.begin().unwrap();
    let original = item("original");
    s.deliver(
        &fence,
        Channel::Search,
        DeliveryStatus::Partial,
        vec![original.clone()],
    )
    .unwrap();
    s.select(original.key.clone()).unwrap();
    let mut oversized = item("oversized");
    oversized.detail = Some("🙂🙂".into()); // 3 + 8 bytes exceeds 8, despite three characters.
    assert_eq!(
        s.deliver(
            &fence,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![oversized]
        ),
        Err(Error::PayloadLimit)
    );
    assert_eq!(
        s.delivery(Channel::Search).status(),
        DeliveryStatus::Partial
    );
    assert_eq!(s.delivery(Channel::Search).items(), &[original.clone()]);
    assert_eq!(s.selected(), Some(&original.key));
    let mut too_many_actions = item("actions");
    let action = Action {
        id: ActionId::new("open").unwrap(),
        kind: ActionKind::Open,
    };
    too_many_actions.actions = vec![action.clone(), action];
    assert_eq!(
        s.deliver(
            &fence,
            Channel::Search,
            DeliveryStatus::Complete,
            vec![too_many_actions]
        ),
        Err(Error::PayloadLimit)
    );
    assert_eq!(s.selected(), Some(&original.key));
}

#[test]
fn pin_restore_cycle_cannot_revive_same_text_completion_fence() {
    let mut s = session();
    s.set_draft("é🙂".into()).unwrap();
    let before = s.begin().unwrap();
    s.pin(pin("original")).unwrap();
    s.restore(&pin("original"), None).unwrap();
    assert_eq!(s.draft(), "é🙂");
    assert_eq!(
        s.complete(CompletionEdit {
            fence: before,
            span: ByteSpan { start: 0, end: 2 },
            replacement: "x".into(),
        }),
        Err(Error::Stale)
    );
    assert_eq!(s.draft(), "é🙂");
    assert!(s.pins().is_empty());
}

#[test]
fn decoded_completion_boundary_rejection_preserves_current_round() {
    let mut s = session();
    s.set_draft("猫🙂".into()).unwrap();
    let fence = s.begin().unwrap();
    let mut edit = CompletionEdit {
        fence: fence.clone(),
        span: ByteSpan { start: 1, end: 3 },
        replacement: "x".into(),
    };
    let wire = serde_json::to_string(&Envelope::v1(edit.clone())).unwrap();
    let decoded: Envelope<CompletionEdit> = serde_json::from_str(&wire).unwrap();
    assert_eq!(s.complete(decoded.payload), Err(Error::InvalidSpan));
    edit.span = ByteSpan { start: 3, end: 7 };
    edit.replacement = "é".into();
    assert_eq!(s.complete(edit), Ok(5));
    assert_eq!(s.draft(), "猫é");
    assert_eq!(
        s.deliver(&fence, Channel::Search, DeliveryStatus::Complete, vec![]),
        Err(Error::Stale)
    );
}

#[test]
fn unicode_identity_budget_is_checked_at_deserialization() {
    let exact = "é".repeat(MAX_ID_BYTES / 2);
    let identity: ContextId =
        serde_json::from_str(&serde_json::to_string(&exact).unwrap()).unwrap();
    assert_eq!(identity.as_str(), exact);
    let too_long = format!("{exact}x");
    assert!(serde_json::from_str::<ContextId>(&serde_json::to_string(&too_long).unwrap()).is_err());
}
