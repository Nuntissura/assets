use handpick::*;

fn session() -> WritingSession {
    let mut session = WritingSession::new(ContextId::new("authorized-context").unwrap(),
        SessionId::new("native-writing").unwrap(), 8);
    session.bind(EditorId::new("notes-editor").unwrap(), 7).unwrap();
    session
}
fn snapshot(session: &WritingSession, before: &str) -> NativeCompletionSnapshot {
    NativeCompletionSnapshot::new(session.fence().unwrap(), CompletionSnapshotId::new("selection-1").unwrap(),
        before.to_owned(), NativeCompletionSelection::Caret).unwrap()
}
fn trigger(session: &WritingSession, before: &str, mode: CompletionInvocation) -> NativeCompletionTrigger {
    session.native_completion_trigger(&snapshot(session, before), mode).unwrap().unwrap()
}
fn key() -> ResultKey {
    ResultKey { provider: ProviderId::new("authorized-notes").unwrap(), result: ResultId::new("result-1").unwrap() }
}
fn action() -> Action { Action { id: ActionId::new("apply-completion").unwrap(), kind: ActionKind::ApplyCompletion } }
fn choose(session: &mut WritingSession) {
    let fence = session.fence().unwrap();
    session.deliver(&fence, Channel::Suggestions, DeliveryStatus::Complete, vec![SearchItem {
        key: key(), label: "Authorized result".into(), detail: None, actions: vec![action()],
    }]).unwrap();
    session.select(&fence, key()).unwrap();
}
fn reference() -> NativeCompletionChoice {
    NativeCompletionChoice { key: key(), value: NativeCompletionValue::Reference {
        target: NoteAddress::Note { note: NoteId::new("resolved-note").unwrap() }, label: "Owner label".into(), embed: false,
    }}
}

#[test]
fn portal_trigger_vectors_preserve_kind_query_and_original_extent() {
    let s = session();
    for (text, kind, query, replaced) in [
        ("type:no", NativeCompletionKind::Operator, "type:no", "type:no"),
        ("text TAG:proj", NativeCompletionKind::Operator, "TAG:proj", "TAG:proj"),
        ("in:bo", NativeCompletionKind::Operator, "in:bo", "in:bo"),
        ("is:un", NativeCompletionKind::Operator, "is:un", "is:un"),
        ("hello [[Road map", NativeCompletionKind::Note, "Road map", "[[Road map"),
        ("[[Road#intro", NativeCompletionKind::Note, "Road#intro", "[[Road#intro"),
        ("[[Road^stable", NativeCompletionKind::Note, "Road^stable", "[[Road^stable"),
        ("((some block", NativeCompletionKind::Block, "some block", "((some block"),
        ("text #project", NativeCompletionKind::Tag, "project", "#project"),
        ("text /meeting", NativeCompletionKind::Template, "meeting", "/meeting"),
        ("[[", NativeCompletionKind::Note, "", "[["),
        ("((", NativeCompletionKind::Block, "", "(("),
        ("#", NativeCompletionKind::Tag, "", "#"),
        ("/", NativeCompletionKind::Template, "", "/"),
        ("[[old [[new", NativeCompletionKind::Note, "old [[new", "[[old [[new"),
    ] {
        for mode in [CompletionInvocation::Automatic, CompletionInvocation::Manual] {
            let t = trigger(&s, text, mode);
            assert_eq!(t.kind(), kind, "{text}"); assert_eq!(t.query(), query);
            assert_eq!(t.replacement_range(), &NativeCompletionRange::BeforeCaret(ByteSpan {
                start: text.len() - replaced.len(), end: text.len(),
            }));
        }
    }
}

#[test]
fn manual_word_selection_empty_and_automatic_prose_are_distinct() {
    let s = session();
    for (text, query) in [("hello world", "world"), ("hello 猫🙂", "猫🙂"), ("", ""), ("hello ", "")] {
        assert!(s.native_completion_trigger(&snapshot(&s, text), CompletionInvocation::Automatic).unwrap().is_none());
        assert_eq!(trigger(&s, text, CompletionInvocation::Manual).query(), query);
    }
    let selected = NativeCompletionSnapshot::new(s.fence().unwrap(), CompletionSnapshotId::new("range-2").unwrap(),
        "prefix".into(), NativeCompletionSelection::Text("type:note and selected prose".into())).unwrap();
    assert!(s.native_completion_trigger(&selected, CompletionInvocation::Automatic).unwrap().is_none());
    let manual = s.native_completion_trigger(&selected, CompletionInvocation::Manual).unwrap().unwrap();
    assert_eq!(manual.kind(), NativeCompletionKind::Manual);
    assert_eq!(manual.query(), "type:note and selected prose");
    assert_eq!(manual.replacement_range(), &NativeCompletionRange::Selection);
    for text in ["[[closed]]", "((closed))", "[[new\nline", "x#tag", "x/template", "#two words", "/two words"] {
        assert!(s.native_completion_trigger(&snapshot(&s, text), CompletionInvocation::Automatic).unwrap().is_none(), "{text}");
    }
}

#[test]
fn normalized_queries_never_rewrite_unicode_source_coordinates() {
    let s = session(); let text = "🙂 ![[Cafe\u{301}|My alias";
    let t = trigger(&s, text, CompletionInvocation::Automatic);
    assert_eq!(t.query(), "Cafe\u{301}"); assert_eq!(t.normalized_query(), "café");
    assert_eq!(t.label(), Some("My alias")); assert!(t.embed());
    assert_eq!(t.snapshot().text_before_caret(), text);
    assert_eq!(t.replacement_range(), &NativeCompletionRange::BeforeCaret(ByteSpan { start: 5, end: text.len() }));
    assert_eq!(trigger(&s, "\u{feff}#é", CompletionInvocation::Automatic).query(), "é");
    assert!(s.native_completion_trigger(&snapshot(&s, "\u{0085}#é"), CompletionInvocation::Automatic).unwrap().is_none());
    let block = trigger(&s, "!((猫|別名", CompletionInvocation::Automatic);
    assert!(block.embed()); assert_eq!(block.label(), Some("別名"));
}

#[test]
fn reference_lookup_preserves_note_heading_block_and_empty_tail_rules() {
    let s = session();
    assert_eq!(trigger(&s, "[[Road", CompletionInvocation::Automatic).lookup(), NativeCompletionLookup::Notes { title: "road".into() });
    assert_eq!(trigger(&s, "[[Road#Intro", CompletionInvocation::Automatic).lookup(), NativeCompletionLookup::Blocks {
        note_title: Some("road".into()), text: "intro".into(), headings_only: true,
    });
    assert_eq!(trigger(&s, "[[Road^id", CompletionInvocation::Automatic).lookup(), NativeCompletionLookup::Blocks {
        note_title: Some("road".into()), text: "id".into(), headings_only: false,
    });
    assert_eq!(trigger(&s, "((Road#Intro", CompletionInvocation::Automatic).lookup(), NativeCompletionLookup::Blocks {
        note_title: None, text: "intro".into(), headings_only: true,
    });
    assert_eq!(trigger(&s, "[[Road#", CompletionInvocation::Automatic).lookup(), NativeCompletionLookup::Blocks {
        note_title: Some("road".into()), text: "road#".into(), headings_only: true,
    });
}

#[test]
fn filter_choices_use_shared_grammar_and_quote_authorized_tag_names() {
    let s = session();
    for prefix in ["type:", "in:", "is:"] {
        for replacement in trigger(&s, prefix, CompletionInvocation::Automatic).operator_completions(&[]).unwrap() {
            let parsed = parse_query(&replacement).unwrap();
            assert!(parsed.unsupported.is_empty(), "{replacement}"); assert!(!parsed.incomplete, "{replacement}");
        }
    }
    let t = trigger(&s, "TAG:pro", CompletionInvocation::Automatic);
    assert_eq!(t.operator_completions(&["Project", "other"]).unwrap(), ["tag:Project "]);
    let t = trigger(&s, "tag:", CompletionInvocation::Automatic);
    let values = t.operator_completions(&["Project Ideas", "quote\" slash\\", "Café"]).unwrap();
    assert_eq!(values, ["tag:\"Project Ideas\" ", "tag:\"quote\\\" slash\\\\\" ", "tag:Café "]);
    assert_eq!(trigger(&s, "type:no", CompletionInvocation::Manual).parsed_query().unwrap().unsupported, ["type:no"]);
    assert_eq!(t.operator_completions(&vec!["tag"; MAX_COMPLETION_TAGS + 1]), Err(Error::PayloadLimit));
}

#[test]
fn tag_creation_requires_complete_authorized_coverage_and_nfc_distinct_name() {
    let s = session(); let t = trigger(&s, "#Cafe\u{301}", CompletionInvocation::Automatic);
    assert_eq!(t.tag_creation_name(&["Café"], CompletionTagCoverage::Complete).unwrap(), None);
    assert_eq!(t.tag_creation_name(&[], CompletionTagCoverage::Partial).unwrap(), None);
    assert_eq!(t.tag_creation_name(&[], CompletionTagCoverage::Complete).unwrap(), Some("Cafe\u{301}".into()));
    assert_eq!(trigger(&s, "#", CompletionInvocation::Automatic).tag_creation_name(&[], CompletionTagCoverage::Complete).unwrap(), None);
}

#[test]
fn reference_plan_preserves_alias_embed_fence_and_native_document_ownership() {
    let mut s = session(); choose(&mut s);
    let snap = snapshot(&s, "🙂 ![[Road|My title");
    let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
    let before = s.fence().unwrap();
    let plan = s.plan_native_completion(&t, &snap, reference()).unwrap();
    assert_eq!(s.fence().unwrap(), before); assert_eq!(s.editor().unwrap().1, 7);
    assert_eq!(plan.replacement_range(), t.replacement_range());
    assert!(matches!(plan.choice().value, NativeCompletionValue::Reference { ref label, embed: true, .. } if label == "My title"));
    assert!(matches!(plan.action().action, WritingAction::Reference { embed: true, .. }));
    assert!(plan.validate(&s, &snap).is_ok());
}

#[test]
fn operator_tag_template_and_heading_plans_are_typed_owner_intents() {
    let mut s = session(); choose(&mut s);
    for (text, value) in [
        ("type:no", NativeCompletionValue::Text { action: action(), text: "type:note ".into() }),
        ("#proj", NativeCompletionValue::Tag { action: action(), label: "Project".into() }),
        ("/meeting", NativeCompletionValue::Template),
        ("[[Road#Intro", NativeCompletionValue::Reference { target: NoteAddress::Heading {
            note: NoteId::new("note").unwrap(), block: BlockId::new("heading").unwrap(),
        }, label: "Intro".into(), embed: false }),
    ] {
        let snap = snapshot(&s, text); let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
        assert!(s.plan_native_completion(&t, &snap, NativeCompletionChoice { key: key(), value }).is_ok());
    }
    let snap = snapshot(&s, "((block"); let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
    assert_eq!(s.plan_native_completion(&t, &snap, reference()), Err(Error::InvalidSpan));
}

#[test]
fn changed_selection_or_snapshot_bytes_cannot_reuse_the_same_revision() {
    let mut s = session(); choose(&mut s); let snap = snapshot(&s, "[[Road");
    let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
    let plan = s.plan_native_completion(&t, &snap, reference()).unwrap();
    let moved = NativeCompletionSnapshot::new(s.fence().unwrap(), CompletionSnapshotId::new("selection-2").unwrap(),
        "[[Road".into(), NativeCompletionSelection::Caret).unwrap();
    assert_eq!(plan.validate(&s, &moved), Err(Error::Stale));
    let changed = NativeCompletionSnapshot::new(s.fence().unwrap(), snap.id().clone(), "[[Other".into(), NativeCompletionSelection::Caret).unwrap();
    assert_eq!(s.plan_native_completion(&t, &changed, reference()), Err(Error::Stale));
    let selected = NativeCompletionSnapshot::new(s.fence().unwrap(), snap.id().clone(), "[[Road".into(), NativeCompletionSelection::Text("Road".into())).unwrap();
    assert_eq!(plan.validate(&s, &selected), Err(Error::Stale));
}

#[test]
fn edits_lookup_ime_rebind_revocation_and_wrong_results_reject_old_plans() {
    for change in 0..6 {
        let mut s = session(); choose(&mut s); let snap = snapshot(&s, "[[Road");
        let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
        let plan = s.plan_native_completion(&t, &snap, reference()).unwrap();
        match change {
            0 => s.edited(snap.fence(), 8).unwrap(),
            1 => { s.begin_lookup().unwrap(); },
            2 => { s.begin_composition(snap.fence()).unwrap(); },
            3 => s.revoke(),
            4 => { s.park(PinId::new("parked").unwrap()).unwrap(); s.bind(EditorId::new("other-editor").unwrap(), 7).unwrap(); },
            _ => { let mut wrong = reference(); wrong.key.result = ResultId::new("not-selected").unwrap();
                assert_eq!(s.plan_native_completion(&t, &snap, wrong), Err(Error::Stale)); continue; },
        }
        assert!(plan.validate(&s, &snap).is_err());
        assert!(s.native_completion_trigger(&snap, CompletionInvocation::Manual).is_err());
    }
}

#[test]
fn active_ime_and_pending_save_do_not_admit_new_completion_snapshots() {
    let mut s = session(); let token = s.begin_composition(&s.fence().unwrap()).unwrap();
    let composing = snapshot(&s, "[[Road");
    assert_eq!(s.native_completion_trigger(&composing, CompletionInvocation::Manual), Err(Error::Stale));
    s.end_composition(&token).unwrap();
    s.submit(SaveIntent { operation: OperationId::new("save-1").unwrap(), fence: s.fence().unwrap(),
        destination: SaveDestination::Create { folder: None } }).unwrap();
    assert_eq!(s.native_completion_trigger(&snapshot(&s, "[[Road"), CompletionInvocation::Manual), Err(Error::Stale));
}

#[test]
fn snapshot_query_and_choice_limits_reject_without_editor_changes() {
    let mut s = session(); choose(&mut s); let before = s.fence().unwrap();
    assert_eq!(NativeCompletionSnapshot::new(before.clone(), CompletionSnapshotId::new("oversize").unwrap(),
        "x".repeat(MAX_COMPLETION_SNAPSHOT_BYTES), NativeCompletionSelection::Text("x".into())), Err(Error::TextLimit));
    assert_eq!(s.native_completion_trigger(&snapshot(&s, &format!("[[{}", "x".repeat(MAX_COMPLETION_QUERY_BYTES + 1))),
        CompletionInvocation::Manual), Err(Error::TextLimit));
    let snap = snapshot(&s, "type:no"); let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
    let mut wrong_action = action(); wrong_action.kind = ActionKind::RunCommand;
    let choice = NativeCompletionChoice { key: key(), value: NativeCompletionValue::Text { action: wrong_action, text: "type:note ".into() } };
    assert_eq!(s.plan_native_completion(&t, &snap, choice), Err(Error::PayloadLimit));
    let mut unoffered = action(); unoffered.id = ActionId::new("different-action").unwrap();
    let choice = NativeCompletionChoice { key: key(), value: NativeCompletionValue::Text { action: unoffered, text: "type:note ".into() } };
    assert_eq!(s.plan_native_completion(&t, &snap, choice), Err(Error::Stale));
    assert_eq!(s.fence().unwrap(), before);
}

#[test]
fn empty_prefix_completion_keeps_provider_authority_and_page_bounds() {
    let s = session(); let t = trigger(&s, "[[", CompletionInvocation::Manual);
    let provider = WritingProvider { provider: key().provider,
        canonical_set: RevisionId::new("authorized-set-revision").unwrap(), max_query_bytes: 4096, max_page_items: 25,
        capabilities: vec![CapabilityDeclaration { capability: WritingCapability::Search,
            support: CapabilitySupport::Supported { contract: RevisionId::new("search-contract").unwrap() } }],
    };
    let query = t.composition_query(provider.provider.clone(), 10, None);
    assert!(query.query.is_empty());
    assert!(provider.validate_native_completion_query(&s, &t, &query).is_ok());
    assert!(provider.validate_query(&s, &query).is_err(), "existing prose-query contract must stay unchanged");
    let mut wrong = query.clone(); wrong.provider = ProviderId::new("foreign-provider").unwrap();
    assert_eq!(provider.validate_native_completion_query(&s, &t, &wrong), Err(Error::Stale));
    wrong = query.clone(); wrong.query = "different".into();
    assert_eq!(provider.validate_native_completion_query(&s, &t, &wrong), Err(Error::Stale));
    wrong = query.clone(); wrong.page_items = 26;
    assert_eq!(provider.validate_native_completion_query(&s, &t, &wrong), Err(Error::PayloadLimit));
    let mut unavailable = provider.clone(); unavailable.capabilities[0].support = CapabilitySupport::Unavailable;
    assert_eq!(unavailable.validate_native_completion_query(&s, &t, &query), Err(Error::Revoked));
}

#[test]
fn slash_template_source_retains_heading_block_and_owner_note_constraints() {
    let mut s = session(); choose(&mut s);
    for (text, expected) in [
        ("/Road", NativeTemplateSource::Note { title: "road".into() }),
        ("/Road#Intro", NativeTemplateSource::Blocks { note_title: "road".into(), text: "intro".into(), headings_only: true }),
        ("/Road^stable", NativeTemplateSource::Blocks { note_title: "road".into(), text: "stable".into(), headings_only: false }),
        ("/Road#", NativeTemplateSource::Blocks { note_title: "road".into(), text: "road#".into(), headings_only: true }),
        ("/^stable", NativeTemplateSource::Blocks { note_title: "".into(), text: "stable".into(), headings_only: false }),
    ] {
        let snap = snapshot(&s, text);
        let t = s.native_completion_trigger(&snap, CompletionInvocation::Automatic).unwrap().unwrap();
        assert_eq!(t.kind(), NativeCompletionKind::Template);
        assert_eq!(t.lookup(), NativeCompletionLookup::Templates { source: expected });
        assert_eq!(t.replacement_range(), &NativeCompletionRange::BeforeCaret(ByteSpan { start: 0, end: text.len() }));
        let plan = s.plan_native_completion(&t, &snap, NativeCompletionChoice { key: key(), value: NativeCompletionValue::Template }).unwrap();
        assert_eq!(plan.action().action, WritingAction::Template { template: key() });
    }
}

#[test]
fn selected_text_reference_split_matches_portal_non_dotall_line_terminators() {
    let mut s = session(); choose(&mut s);
    for separator in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for raw in [format!("Road{separator}#Intro"), format!("Road#{separator}Intro"),
            format!("Road#Intro{separator}"), format!("Road{separator}^stable")] {
            let snap = NativeCompletionSnapshot::new(s.fence().unwrap(), CompletionSnapshotId::new("selected-lines").unwrap(),
                String::new(), NativeCompletionSelection::Text(raw.clone())).unwrap();
            let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
            assert_eq!(t.lookup(), NativeCompletionLookup::Notes { title: raw.to_lowercase() });
            assert_eq!(t.replacement_range(), &NativeCompletionRange::Selection);
            let plan = s.plan_native_completion(&t, &snap, reference()).unwrap();
            assert_eq!(plan.snapshot().selection(), &NativeCompletionSelection::Text(raw));
        }
    }
    // ECMAScript dot accepts these whitespace characters; they are not LineTerminators.
    for separator in ["\t", "\u{0085}", "\u{00a0}", "\u{feff}"] {
        let raw = format!("Road{separator}#Intro");
        let snap = NativeCompletionSnapshot::new(s.fence().unwrap(), CompletionSnapshotId::new("selected-single-line").unwrap(),
            String::new(), NativeCompletionSelection::Text(raw)).unwrap();
        let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
        assert_eq!(t.lookup(), NativeCompletionLookup::Blocks { note_title: Some(format!("road{separator}")), text: "intro".into(), headings_only: true });
    }
}

#[test]
fn plan_revalidation_requires_the_exact_still_offered_action_at_same_key_and_fence() {
    for tag in [false, true] {
        for replacement in 0..3 {
            let mut s = session(); choose(&mut s);
            let snap = snapshot(&s, if tag { "#project" } else { "type:no" });
            let t = s.native_completion_trigger(&snap, CompletionInvocation::Manual).unwrap().unwrap();
            let value = if tag { NativeCompletionValue::Tag { action: action(), label: "Project".into() } }
                else { NativeCompletionValue::Text { action: action(), text: "type:note ".into() } };
            let plan = s.plan_native_completion(&t, &snap, NativeCompletionChoice { key: key(), value }).unwrap();
            let mut changed = action(); changed.id = ActionId::new("replacement-action").unwrap();
            let actions = match replacement { 0 => vec![], 1 => vec![changed], _ => vec![changed, action()] };
            s.deliver(snap.fence(), Channel::Suggestions, DeliveryStatus::Complete, vec![SearchItem {
                key: key(), label: "Same result with refreshed actions".into(), detail: None, actions,
            }]).unwrap();
            assert_eq!(s.fence().unwrap(), *snap.fence());
            assert_eq!(s.selected(), Some(&key()));
            if replacement == 2 { assert!(plan.validate(&s, &snap).is_ok()); }
            else { assert_eq!(plan.validate(&s, &snap), Err(Error::Stale)); }
        }
    }
}

#[test]
fn tag_filter_insertions_round_trip_the_exact_normalized_authorized_name() {
    use unicode_normalization::UnicodeNormalization;
    let s = session(); let t = trigger(&s, "tag:", CompletionInvocation::Manual);
    for name in ["a\\b", "quote\" slash\\", "\"", "ends\"", "\\", "literal\\u0041", "a\tb",
        "a\nb", "a\rb", "a\u{0008}b", "a\u{000c}b", "a\u{0000}b", "a\u{001f}b", "#leading", "#", "Café", "Cafe\u{301}", "猫🙂"] {
        let replacements = t.operator_completions(&[name]).unwrap();
        assert_eq!(replacements.len(), 1);
        let query = parse_query(&replacements[0].nfc().collect::<String>()).unwrap();
        assert!(!query.incomplete, "{name:?}: {:?}", replacements[0]);
        assert!(query.unsupported.is_empty(), "{name:?}");
        assert!(query.words.is_empty(), "{name:?}");
        assert_eq!(query.tag, Some(name.nfc().collect::<String>().to_lowercase()), "{name:?}");
    }
}
