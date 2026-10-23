mod fixture;
use fixture::*;
use hsscrap::*;
use rss_contracts::Error;
use rss_core::query::*;
use rss_core::{MemberRule, SourceGroup};
use std::collections::BTreeSet;

#[test]
fn bounded_pages_cover_frozen_canonical_set_while_arrivals_and_sort_keys_change() {
    for size in [PageSize::Items25, PageSize::Items50, PageSize::Items100] {
        let mut session = QuerySession::new(query(size), bounds(), 500).unwrap();
        let mut db = FixtureDb::new(123);
        let ticket = session.load().unwrap();
        let page = db.page(&ticket);
        assert_eq!(page.rows.len(), size.get());
        session.complete(&ticket, Ok(page)).unwrap();
        db.canonical.insert(0, row(500));
        db.canonical[1].order.utc_millis = Some(-1000);
        while session.has_next() {
            let ticket = session.load_next().unwrap();
            session.complete(&ticket, Ok(db.page(&ticket))).unwrap();
        }
        assert_eq!(
            session
                .rows()
                .iter()
                .map(|row| row.item_id)
                .collect::<Vec<_>>(),
            (1..=123).map(id::<ItemId>).collect::<Vec<_>>()
        );
    }
}

#[test]
fn refresh_retains_snapshot_and_anchor_until_explicit_apply() {
    let (mut session, _, mut db) = loaded(75);
    let position = ReadingPosition {
        item_id: id(10),
        offset_basis_points: 3721,
    };
    session.set_position(position).unwrap();
    let old = session.snapshot().unwrap().clone();
    let refresh = session.refresh().unwrap();
    db.canonical.push(row(99));
    let replacement = db.page(&refresh);
    let next = session.load_next().unwrap();
    session.complete(&next, Ok(db.page(&next))).unwrap();
    session.complete(&refresh, Ok(replacement)).unwrap();
    assert_eq!(session.snapshot(), Some(&old));
    assert_eq!(session.rows().len(), 50);
    assert_eq!(
        session.apply_refresh().unwrap(),
        RefreshApplied::Preserved(position)
    );
    assert_eq!(session.position(), Some(position));
    assert_ne!(session.snapshot(), Some(&old));
    assert_eq!(session.rows()[0].item_id, position.item_id);
}

#[test]
fn changed_reading_position_rejects_refresh_without_rewinding() {
    let (mut session, _, mut db) = loaded(75);
    session
        .set_position(ReadingPosition {
            item_id: id(2),
            offset_basis_points: 10,
        })
        .unwrap();
    let refresh = session.refresh().unwrap();
    let page = db.page(&refresh);
    let latest = ReadingPosition {
        item_id: id(20),
        offset_basis_points: 8000,
    };
    session.set_position(latest).unwrap();
    session.complete(&refresh, Ok(page)).unwrap();
    let before = session.rows().to_vec();
    assert_eq!(session.apply_refresh(), Err(Error::StaleResult));
    assert_eq!(session.rows(), before);
    assert_eq!(session.position(), Some(latest));
    assert!(session.refresh().is_ok());
}

#[test]
fn denial_clears_private_rows_and_old_profile_completion_cannot_revoke_new_view() {
    let (mut session, _, mut db) = loaded(50);
    let old = session.load_next().unwrap();
    let mut new = query(PageSize::Items25);
    new.profile = host("other-profile");
    new.authority_epoch = "authority-2".into();
    session.reset(new).unwrap();
    let current = session.load().unwrap();
    session.complete(&current, Ok(db.page(&current))).unwrap();
    assert_eq!(
        session.complete(&old, Err(Error::Denied)),
        Err(Error::StaleResult)
    );
    assert_eq!(session.rows().len(), 25);
    let next = session.load_next().unwrap();
    assert_eq!(
        session.complete(&next, Err(Error::Denied)),
        Err(Error::Denied)
    );
    assert!(session.rows().is_empty());
    assert!(session.snapshot().is_none());
    assert!(session.query().is_none());
    assert_eq!(session.phase(), &QueryPhase::Revoked);
}

#[test]
fn failed_reconnect_and_malformed_pages_never_advance_old_cursor() {
    for health in [
        PageHealth::Reconnect,
        PageHealth::Failed {
            reason: "offline".into(),
        },
    ] {
        let (mut session, _, mut db) = loaded(50);
        let before = session.rows().to_vec();
        let snapshot = session.snapshot().cloned();
        let ticket = session.load_next().unwrap();
        let mut page = db.page(&ticket);
        page.health = health.clone();
        assert!(session.complete(&ticket, Ok(page)).is_err());
        assert_eq!(session.rows(), before);
        assert_eq!(session.snapshot(), snapshot.as_ref());
        let retry = session.load_next().unwrap();
        assert_eq!(retry.request.after, ticket.request.after);
        let mut page = db.page(&retry);
        page.rows.clear();
        page.next = None;
        page.health = health;
        session.complete(&retry, Ok(page)).unwrap();
        assert_eq!(session.rows(), before);
        assert_eq!(session.snapshot(), snapshot.as_ref());
        assert!(session.has_next());
    }
}

#[test]
fn duplicate_foreign_order_and_oversized_page_rejected_atomically() {
    for mutation in 0..6 {
        let (mut session, _, mut db) = loaded(50);
        let before = session.rows().to_vec();
        let ticket = session.load_next().unwrap();
        let mut page = db.page(&ticket);
        match mutation {
            0 => page.rows[0] = before[0].clone(),
            1 => page.rows[0].appearances[0].access_partition = ScopeRef("other-account".into()),
            2 => page.rows.swap(0, 1),
            3 => page.rows.push(row(99)),
            4 => page.snapshot.query.authority_epoch = "foreign".into(),
            _ => {
                page.rows[0].appearances[0].provider_entry_key =
                    "x".repeat(bounds().max_operation_bytes)
            }
        }
        assert!(session.complete(&ticket, Ok(page)).is_err());
        if mutation == 1 {
            assert!(session.rows().is_empty());
            assert_eq!(session.phase(), &QueryPhase::Revoked);
        } else {
            assert_eq!(session.rows(), before);
            assert_eq!(
                session.load_next().unwrap().request.after,
                ticket.request.after
            );
        }
    }
}

#[test]
fn aggregate_query_and_page_bytes_are_bounded_even_when_fields_fit() {
    let mut limited = bounds();
    limited.max_operation_bytes = 2000;
    let mut spec = query(PageSize::Items25);
    spec.filter.search = "x".repeat(1500);
    assert!(QuerySession::new(spec, limited.clone(), 100).is_err());
    let mut session = QuerySession::new(query(PageSize::Items25), limited, 100).unwrap();
    let mut db = FixtureDb::new(25);
    let ticket = session.load().unwrap();
    assert!(session.complete(&ticket, Ok(db.page(&ticket))).is_err());
    assert!(session.rows().is_empty());
}

#[test]
fn canonical_selected_collection_rules_remain_separate_and_deselected_focus_denied() {
    let (mut library, mut selection) = library();
    for name in ["one", "two"] {
        let group = SourceGroup {
            group_ref: host(name),
            owner_scope: selection.owner_scope.clone(),
            name: name.into(),
            description: String::new(),
            keywords: vec![name.into()],
            priority: 2,
            tag_refs: BTreeSet::new(),
            revision: Revision(1),
            common: Extensions::default(),
        };
        library = library
            .save_group(&selection.owner_scope, library.revision, group, &bounds())
            .unwrap();
        library = library
            .attach(
                &selection.owner_scope,
                library.revision,
                &host(name),
                &host("source"),
                &bounds(),
            )
            .unwrap();
        selection.selected_groups.insert(host(name));
    }
    library.member_rules.insert(
        (host("one"), host("source")),
        MemberRule {
            keywords: vec!["science".into()],
            prioritize: true,
        },
    );
    let build = |selection: &rss_core::ProfileSelection, focus| {
        WallQuery::from_library(
            &library,
            selection,
            Revision(2),
            "epoch".into(),
            &BTreeSet::from([host("source")]),
            BTreeSet::from([ScopeRef("public".into())]),
            focus,
            ContextFilter::AllAuthorized,
            WallFilter::default(),
            WallSort::SourcePriority,
            PageSize::Items25,
            &bounds(),
        )
    };
    let all = build(&selection, None).unwrap();
    assert_eq!(all.branches.len(), 3);
    let focused_group = host("one");
    let focused = build(&selection, Some(&focused_group)).unwrap();
    assert_eq!(focused.branches.len(), 1);
    assert!(focused.branches[0].prioritize);
    assert_eq!(focused.branches[0].source_keywords, ["science"]);
    selection.selected_groups.remove(&focused_group);
    assert_eq!(build(&selection, Some(&focused_group)), Err(Error::Denied));
}

#[test]
fn selected_project_is_the_only_saved_date_and_context() {
    let mut spec = query(PageSize::Items25);
    let project = ProjectRef(host("project"));
    let scope = ScopeRef("project-scope".into());
    spec.context = ContextFilter::Project {
        project: project.clone(),
        scope: scope.clone(),
        folder: None,
    };
    spec.sort = WallSort::ProjectSaved;
    let mut session = QuerySession::new(spec, bounds(), 100).unwrap();
    let ticket = session.load().unwrap();
    let mut db = FixtureDb::new(1);
    let mut page = db.page(&ticket);
    page.rows[0].selected_context = Some(SelectedContext {
        membership: ProjectMembership {
            membership_id: id(100),
            item_id: id(1),
            project_ref: project,
            scope,
            saved_at: at(),
            created_here_at: at(),
            revision: Revision(1),
            lifecycle: Lifecycle::Active,
            removed_at: None,
            common: Extensions::default(),
        },
        folder: None,
    });
    let mut wrong = page.clone();
    wrong.rows[0]
        .selected_context
        .as_mut()
        .unwrap()
        .membership
        .project_ref = ProjectRef(host("other-project"));
    assert_eq!(session.complete(&ticket, Ok(wrong)), Err(Error::Denied));
    assert!(session.rows().is_empty());
    let ticket = session.reset(query(PageSize::Items25));
    assert!(ticket.is_ok());
    // Actual selected-context acceptance is a separate fresh authorized view.
    let mut spec = page.snapshot.query.clone();
    spec.authority_epoch = "reauthorized".into();
    session.reset(spec).unwrap();
    let ticket = session.load().unwrap();
    page.snapshot.query = ticket.request.query.clone();
    session.complete(&ticket, Ok(page)).unwrap();
    assert_eq!(
        session.rows()[0]
            .selected_context
            .as_ref()
            .unwrap()
            .membership
            .saved_at,
        at()
    );
}

#[test]
fn lazy_gallery_one_player_revision_change_and_stale_reply_effects() {
    let (session, mut wall, _) = loaded(3);
    install(
        &session,
        &mut wall,
        vec![card(1, MediaKind::Video, 2), card(2, MediaKind::Video, 1)],
    );
    assert!(wall.player().is_none());
    assert_eq!(
        wall.media_state(&MemberRef {
            item_id: id(1),
            member_id: id(100)
        }),
        MediaState::NotRequested
    );
    let first = wall.play(id(1)).unwrap();
    let WallIntent::ResolveMedia(old) = first[0].clone() else {
        panic!()
    };
    let second = wall.play(id(2)).unwrap();
    assert!(matches!(&second[0], WallIntent::StopPlayback(member) if member.item_id == id(1)));
    assert_eq!(
        wall.complete_media(&old, MediaState::Ready),
        Err(Error::StaleResult)
    );
    let mut changed = card(2, MediaKind::Video, 1);
    changed.item.revision = Revision(2);
    changed.members[0].observed_width = Some(900);
    let effects = install(&session, &mut wall, vec![changed]);
    assert!(effects.iter().any(
        |effect| matches!(effect, WallIntent::StopPlayback(member) if member.item_id == id(2))
    ));
    assert!(effects.contains(&WallIntent::InvalidateMedia { item_id: id(2) }));
    assert!(wall.player().is_none());
    wall.play(id(1)).unwrap();
    let effects = wall.select_member(id(1), id(101)).unwrap();
    assert!(matches!(&effects[0], WallIntent::StopPlayback(_)));
}

#[test]
fn same_item_new_authority_clears_media_cards_and_inspection_tickets() {
    let (mut session, mut wall, mut db) = loaded(3);
    let original = card(1, MediaKind::Video, 1);
    install(&session, &mut wall, vec![original.clone()]);
    let ticket = wall.request_cards(&session, vec![id(1)]).unwrap();
    let preview = wall.request_preview(id(1)).unwrap().unwrap();
    let WallIntent::Inspect(inspect) = wall.inspect(&session, id(1)).unwrap() else {
        panic!()
    };
    wall.play(id(1)).unwrap();
    let mut spec = query(PageSize::Items25);
    spec.profile = host("other-profile");
    spec.authority_epoch = "different".into();
    session.reset(spec).unwrap();
    let current = session.load().unwrap();
    session.complete(&current, Ok(db.page(&current))).unwrap();
    let effects = wall.reconcile(&session);
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, WallIntent::StopPlayback(_))));
    assert!(wall.card(id(1)).is_none());
    assert!(wall.inspection().is_none());
    assert_eq!(
        wall.complete_cards(&ticket, Ok(vec![original.clone()])),
        Err(Error::StaleResult)
    );
    let WallIntent::ResolveMedia(preview) = preview else {
        panic!()
    };
    assert_eq!(
        wall.complete_media(&preview, MediaState::Ready),
        Err(Error::StaleResult)
    );
    assert_eq!(
        wall.validate_inspection(&inspect, &inspection(&original)),
        Err(Error::StaleResult)
    );
}

#[test]
fn item_only_inspection_rejects_inherited_unselected_context() {
    let (session, mut wall, _) = loaded(1);
    let card = card(1, MediaKind::Link, 0);
    install(&session, &mut wall, vec![card.clone()]);
    let WallIntent::Inspect(ticket) = wall.inspect(&session, id(1)).unwrap() else {
        panic!()
    };
    let mut inspection = inspection(&card);
    wall.validate_inspection(&ticket, &inspection).unwrap();
    inspection.targets.push(hsscrap::interaction::TargetAccess {
        target: TagTarget::Project(ProjectRef(host("unselected"))),
        scope: ScopeRef("account".into()),
        label: "Other project".into(),
        writable: true,
    });
    assert_eq!(
        wall.validate_inspection(&ticket, &inspection),
        Err(Error::Denied)
    );
}

#[test]
fn stop_during_pending_playback_allows_a_new_preview_and_rejects_late_decode() {
    let (session, mut wall, _) = loaded(1);
    install(&session, &mut wall, vec![card(1, MediaKind::Video, 1)]);
    let WallIntent::ResolveMedia(preview) = wall.request_preview(id(1)).unwrap().unwrap() else {
        panic!()
    };
    let effects = wall.play(id(1)).unwrap();
    let WallIntent::ResolveMedia(play) = &effects[0] else {
        panic!()
    };
    assert!(matches!(
        wall.stop_player(),
        Some(WallIntent::StopPlayback(_))
    ));
    assert!(wall.request_preview(id(1)).unwrap().is_some());
    assert_eq!(
        wall.complete_media(&preview, MediaState::Ready),
        Err(Error::StaleResult)
    );
    assert_eq!(
        wall.complete_media(play, MediaState::Ready),
        Err(Error::StaleResult)
    );
}

#[test]
fn matching_selected_observation_from_foreign_partition_never_enters_card_cache() {
    let (session, mut wall, _) = loaded(2);
    let safe = card(1, MediaKind::Link, 0);
    let mut foreign = card(2, MediaKind::Link, 0);
    foreign.item.title_override = Override::Inherit;
    foreign.item.selected_observation_id = Some(id(90_000));
    foreign.observation = Some(MetadataObservation {
        observation_id: id(90_000),
        item_id: foreign.item.item_id,
        stream_id: id(90_001),
        sequence: Revision(1),
        baseline_observation_id: None,
        capture_id: Some(foreign.item.first_capture_id),
        appearance_ref: None,
        producer: "Foreign publisher".into(),
        access_partition: ScopeRef("foreign-private".into()),
        observed_at: at(),
        source_revision: None,
        canonical_url_assertion: None,
        provider: None,
        host_site: "example.org".into(),
        title: Some("PRIVATE_PARTITION_TITLE".into()),
        description: Some("PRIVATE_PARTITION_DESCRIPTION".into()),
        author: None,
        published_at: None,
        provider_labels: vec![],
        gallery_snapshot: None,
        source_payload_digest: None,
        status: Availability {
            state: AvailabilityState::Available,
            checked_at: None,
            reason_code: None,
            retry_after: None,
        },
        common: Extensions::default(),
    });
    foreign.validate(&bounds()).unwrap();
    assert_eq!(
        foreign.item.selected_observation_id,
        foreign
            .observation
            .as_ref()
            .map(|observation| observation.observation_id)
    );
    let ticket = wall.request_cards(&session, vec![id(1), id(2)]).unwrap();
    assert_eq!(
        wall.complete_cards(&ticket, Ok(vec![safe, foreign.clone()])),
        Err(Error::Denied)
    );
    // Validate the complete batch before mutation: even the earlier safe row was not cached.
    assert!(wall.card(id(1)).is_none());
    assert!(wall
        .card(id(2))
        .and_then(|card| card.title().ok().flatten())
        .is_none());
    assert!(wall.selected_member(id(2)).is_none());
    assert!(wall.player().is_none());

    // The same selected ObservationId is admissible only in an authorized partition.
    let (_, mut authorized, _) = loaded(2);
    foreign.observation.as_mut().unwrap().access_partition = ScopeRef("public".into());
    foreign.observation.as_mut().unwrap().title = Some("PUBLIC_TITLE".into());
    foreign.observation.as_mut().unwrap().description = Some("PUBLIC_DESCRIPTION".into());
    let ticket = authorized.request_cards(&session, vec![id(2)]).unwrap();
    authorized
        .complete_cards(&ticket, Ok(vec![foreign]))
        .unwrap();
    assert_eq!(
        authorized.card(id(2)).unwrap().title().unwrap().as_deref(),
        Some("PUBLIC_TITLE")
    );
}
