use handpick::*;

fn session() -> Session {
    Session::new(
        ContextId::new("private").unwrap(),
        Limits {
            max_text_bytes: 4096,
            max_pins: 2,
            max_items: 8,
            max_item_bytes: 4096,
            max_actions_per_item: 8,
        },
    )
}

#[test]
fn source_shapes_and_exact_outer_spaces_preserve_literal_input() {
    let mut session = session();
    for (candidate, form, destination) in [
        ("https://www.", SourceForm::Prefix, "https://www."),
        ("www.", SourceForm::Prefix, "https://www."),
        ("example.com", SourceForm::Url, "https://example.com/"),
        ("example.org", SourceForm::Url, "https://example.org/"),
        (
            "www.Example.dev/path?x=1#part",
            SourceForm::Url,
            "https://www.example.dev/path?x=1#part",
        ),
        (
            "https://example.net/path",
            SourceForm::Url,
            "https://example.net/path",
        ),
        (
            "http://example.com:8080/",
            SourceForm::Url,
            "http://example.com:8080/",
        ),
    ] {
        for (before, after) in [("", ""), (" ", ""), ("", " "), (" ", " ")] {
            let literal = format!("{before}{candidate}{after}");
            session.set_draft(literal.clone()).unwrap();
            let fence = session.fence();
            assert_eq!(
                session.input_state(&fence, false).unwrap(),
                InputState::Source {
                    candidate: candidate.into(),
                    form,
                    can_add_source: true,
                }
            );
            assert_eq!(
                session.source_intent(&fence, false).unwrap().as_deref(),
                Some(destination)
            );
            assert_eq!(session.draft(), literal);
            assert_eq!(session.fence(), fence);
        }
    }
}

#[test]
fn extra_whitespace_or_prose_is_never_source_intent() {
    let mut session = session();
    for literal in [
        "  example.com",
        "example.com  ",
        "\texample.com",
        "example.com\t",
        "example.com\n",
        "https://example.com\nmore",
        "https://example.com notes",
        "notes https://example.com",
        "example.com example.org",
        "\u{a0}example.com",
        "example.com\u{a0}",
        "hello",
        "https://",
        "not a URL",
        "person@example.com",
    ] {
        session.set_draft(literal.into()).unwrap();
        let fence = session.fence();
        assert_eq!(
            session.input_state(&fence, false).unwrap(),
            InputState::Quicknote,
            "{literal:?}"
        );
        assert_eq!(session.source_intent(&fence, false).unwrap(), None);
        assert_eq!(session.draft(), literal);
    }
}

#[test]
fn url_to_prose_and_clear_reset_without_editor_or_pin_side_effects() {
    let mut session = session();
    session.set_draft("parked text".into()).unwrap();
    session.pin(PinId::new("bookmark").unwrap()).unwrap();
    session.set_draft("www.".into()).unwrap();
    session.set_draft(String::new()).unwrap();
    assert_eq!(
        session.input_state(&session.fence(), false).unwrap(),
        InputState::Neutral
    );
    assert_eq!(
        session.source_intent(&session.fence(), false).unwrap(),
        None
    );
    session.set_draft("www.".into()).unwrap();
    let source = session.fence();
    session.set_draft("www. a thought".into()).unwrap();
    assert_eq!(
        session.input_state(&session.fence(), false).unwrap(),
        InputState::Quicknote
    );
    assert_eq!(session.input_state(&source, false), Err(Error::Stale));
    session.set_draft(String::new()).unwrap();
    assert_eq!(
        session.input_state(&session.fence(), false).unwrap(),
        InputState::Neutral
    );
    assert_eq!(
        session.source_intent(&session.fence(), false).unwrap(),
        None
    );
    assert_eq!(session.pins()[0].tooltip_text(), "parked text");
    session
        .restore(&PinId::new("bookmark").unwrap(), None)
        .unwrap();
    assert_eq!(session.draft(), "parked text");
}

#[test]
fn source_actions_require_current_context_and_no_active_composition() {
    let mut session = session();
    session.set_draft("example.com".into()).unwrap();
    let fence = session.fence();
    assert!(matches!(
        session.input_state(&fence, true).unwrap(),
        InputState::Source {
            can_add_source: false,
            ..
        }
    ));
    assert_eq!(session.source_intent(&fence, true).unwrap(), None);
    session
        .switch_context(ContextId::new("work").unwrap())
        .unwrap();
    assert_eq!(session.source_intent(&fence, false), Err(Error::Stale));
    assert_eq!(
        session.input_state(&session.fence(), false).unwrap(),
        InputState::Neutral
    );
    session
        .switch_context(ContextId::new("private").unwrap())
        .unwrap();
    assert_eq!(session.source_intent(&fence, false), Err(Error::Stale));
    session.revoke();
    assert_eq!(
        session.input_state(&session.fence(), false),
        Err(Error::Revoked)
    );
}

#[test]
fn source_suppresses_automatic_quicknote_but_preserves_explicit_write_and_query_modes() {
    let mut session = session();
    for literal in ["www.", "example.com", "https://example.com"] {
        session.set_draft(literal.into()).unwrap();
        let fence = session.begin().unwrap();
        session
            .deliver(&fence, Channel::Search, DeliveryStatus::Complete, vec![])
            .unwrap();
        assert!(!session
            .can_start_quicknote(
                &fence,
                Channel::Search,
                QuicknoteTrigger::CompleteEmpty,
                false
            )
            .unwrap());
        assert!(session
            .can_start_quicknote(&fence, Channel::Search, QuicknoteTrigger::NewLine, false)
            .unwrap());
    }
    session
        .set_draft("https://example.com a thought".into())
        .unwrap();
    let fence = session.begin().unwrap();
    assert_eq!(
        session.input_state(&fence, false).unwrap(),
        InputState::Quicknote
    );
    assert!(!session
        .can_start_quicknote(
            &fence,
            Channel::Search,
            QuicknoteTrigger::CompleteEmpty,
            false
        )
        .unwrap());
    session
        .deliver(&fence, Channel::Search, DeliveryStatus::Complete, vec![])
        .unwrap();
    assert!(session
        .can_start_quicknote(
            &fence,
            Channel::Search,
            QuicknoteTrigger::CompleteEmpty,
            false
        )
        .unwrap());
    for literal in ["> settings", "type:settings", "in:body notes"] {
        session.set_draft(literal.into()).unwrap();
        let fence = session.begin().unwrap();
        session
            .deliver(&fence, Channel::Search, DeliveryStatus::Complete, vec![])
            .unwrap();
        assert!(!session
            .can_start_quicknote(
                &fence,
                Channel::Search,
                QuicknoteTrigger::CompleteEmpty,
                false
            )
            .unwrap());
    }
}
