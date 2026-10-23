mod fixtures;
use fixtures::owner_fixture::*;
use fixtures::*;
use hsscrap::{interaction::Session, MediaKind, Override, Revision};
use rss_contracts::Error;
use rss_core::query::*;
use rss_wasm::RssBridge;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[test]
fn query_pages_and_current_denial_match_the_native_owner() {
    for size in [PageSize::Items25, PageSize::Items50, PageSize::Items100] {
        let mut native = QuerySession::new(query(size), bounds(), 500).unwrap();
        let mut db = FixtureDb::new(size.get() as u64 + 2);
        let mut bridge = RssBridge::new(&initial(INSTANCE, size)).unwrap();
        let ticket = native.load().unwrap();
        let first = db.page(&ticket);
        native.complete(&ticket, Ok(first.clone())).unwrap();
        let request = effect(&apply(&mut bridge, json!({"type":"load"})), "query");
        assert_eq!(
            request["request"],
            serde_json::to_value(ticket.request).unwrap()
        );
        let output = apply(
            &mut bridge,
            json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":first}}),
        );
        assert_eq!(
            output["view"]["rows"],
            serde_json::to_value(native.rows()).unwrap()
        );
        let next = native.load_next().unwrap();
        let request = effect(&apply(&mut bridge, json!({"type":"load_next"})), "query");
        assert_eq!(
            request["request"],
            serde_json::to_value(&next.request).unwrap()
        );
        let page = db.page(&next);
        native.complete(&next, Ok(page.clone())).unwrap();
        let output = apply(
            &mut bridge,
            json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":page}}),
        );
        assert_eq!(
            output["view"]["rows"],
            serde_json::to_value(native.rows()).unwrap()
        );
        let refresh = native.refresh().unwrap();
        let request = effect(&apply(&mut bridge, json!({"type":"refresh"})), "query");
        assert_eq!(
            native.complete(&refresh, Err(Error::Denied)),
            Err(Error::Denied)
        );
        let output = apply(
            &mut bridge,
            json!({"type":"complete_page","token":request["token"],"result":{"type":"error","error":Error::Denied}}),
        );
        assert_eq!(output["view"]["phase"], "revoked");
        assert!(output["view"]["rows"].as_array().unwrap().is_empty());
        assert_eq!(native.phase(), &QueryPhase::Revoked);
        assert_eq!(
            output["error"],
            serde_json::to_value(Error::Denied).unwrap()
        );
    }
}

#[test]
fn preview_player_and_replacement_effects_match_the_native_wall() {
    let (query, mut wall, _) = loaded(2);
    let candidates = vec![card(1, MediaKind::Video, 2), card(2, MediaKind::Video, 1)];
    install(&query, &mut wall, candidates.clone());
    let mut bridge = RssBridge::new(&initial(INSTANCE, PageSize::Items25)).unwrap();
    load(&mut bridge, 2);
    cards(&mut bridge, candidates);
    let preview = effect(
        &apply(
            &mut bridge,
            json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(1)}),
        ),
        "media",
    );
    let cancelled = apply(
        &mut bridge,
        json!({"type":"cancel","token":preview["token"]}),
    );
    assert_eq!(
        effect(&cancelled, "cancel_host_effect")["token"],
        preview["token"]
    );
    assert_eq!(cancelled["view"]["media"][0]["state"], "not_requested");
    assert_eq!(
        bridge
            .apply(&command(
                json!({"type":"complete_media","token":preview["token"],"state":"ready"})
            ))
            .unwrap_err(),
        "stale_ticket"
    );
    let retry = effect(
        &apply(
            &mut bridge,
            json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(1)}),
        ),
        "media",
    );
    assert_ne!(retry["token"], preview["token"]);
    apply(
        &mut bridge,
        json!({"type":"complete_media","token":retry["token"],"state":"ready"}),
    );
    assert!(wall.player().is_none());
    let native = wall.play(id(1)).unwrap();
    let output = apply(
        &mut bridge,
        json!({"type":"play","item_id":id::<rss_contracts::ItemId>(1)}),
    );
    let request = effect(&output, "media");
    let WallIntent::ResolveMedia(ticket) = native.last().unwrap() else {
        panic!("native playback intent")
    };
    assert_eq!(
        request["member"],
        serde_json::to_value(&ticket.member).unwrap()
    );
    assert_eq!(request["purpose"], "playback");
    wall.complete_media(ticket, MediaState::Ready).unwrap();
    apply(
        &mut bridge,
        json!({"type":"complete_media","token":request["token"],"state":"ready"}),
    );
    let native = wall.play(id(2)).unwrap();
    let output = apply(
        &mut bridge,
        json!({"type":"play","item_id":id::<rss_contracts::ItemId>(2)}),
    );
    let WallIntent::StopPlayback(stopped) = &native[0] else {
        panic!("native stop intent")
    };
    assert_eq!(
        effect(&output, "stop_playback")["member"],
        serde_json::to_value(stopped).unwrap()
    );
    assert_eq!(
        output["view"]["player"],
        serde_json::to_value(wall.player()).unwrap()
    );
    assert!(wall.request_preview(id(2)).unwrap().is_none());
    let rendering = apply(
        &mut bridge,
        json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(2)}),
    );
    assert!(rendering["effects"].as_array().unwrap().is_empty());
    assert_eq!(
        rendering["view"]["player"],
        serde_json::to_value(wall.player()).unwrap()
    );
    let revoked = apply(&mut bridge, json!({"type":"revoke"}));
    assert!(revoked["view"]["player"].is_null());
    assert!(revoked["view"]["cards"].as_array().unwrap().is_empty());
    assert!(revoked["effects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effect| effect["type"] == "stop_playback"));
}

#[test]
fn composed_hsscrap_draft_plan_unknown_outcome_and_decimal_revision_match_native() {
    let mut bridge = RssBridge::new(&initial(INSTANCE, PageSize::Items25)).unwrap();
    load(&mut bridge, 1);
    let mut candidate = card(1, MediaKind::Image, 1);
    candidate.item.revision = Revision(9_007_199_254_740_997);
    cards(&mut bridge, vec![candidate.clone()]);
    let request = effect(
        &apply(
            &mut bridge,
            json!({"type":"inspect","item_id":candidate.item.item_id}),
        ),
        "inspection",
    );
    let inspection = owner_fixture::inspection(&candidate);
    let mut session = Session::new(inspection.clone()).unwrap();
    let opened = apply(
        &mut bridge,
        json!({"type":"complete_inspection","token":request["token"],"details_instance_id":OTHER,
        "inspection":hsscrap_wasm::wire::InspectionWire::from(inspection.clone())}),
    );
    assert_eq!(
        opened["view"]["details"]["snapshot"]["view"]["inspection"]["item"]["revision"],
        "9007199254740997"
    );
    session.begin_edit().unwrap();
    bridge
        .details_apply(&details_command(
            &request["token"],
            json!({"type":"begin_edit"}),
        ))
        .unwrap();
    let value = Override::Set("Browser draft".into());
    session.set_title(value.clone()).unwrap();
    let output: Value = serde_json::from_str(
        &bridge
            .details_apply(&details_command(
                &request["token"],
                json!({"type":"set_title","value":value}),
            ))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        output["details"]["view"]["draft"]["title"],
        serde_json::to_value(&session.draft().unwrap().title).unwrap()
    );
    let context = hsscrap::ports::CommandContext {
        actor: host("actor"),
        scope: inspection.scope.clone(),
        operation_id: id(70000),
        intent_digest: String::new(),
        expected_revisions: inspection.owner_fences.clone(),
        bounds: bounds(),
    };
    let pending = session
        .begin_save(context.clone(), &BTreeSet::new())
        .unwrap();
    let output: Value = serde_json::from_str(
        &bridge
            .details_apply(&details_command(
                &request["token"],
                json!({"type":"begin_save","context":context}),
            ))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        output["details"]["view"]["pending"]["plan"],
        serde_json::to_value(&pending.plan).unwrap()
    );
    let token = output["details"]["view"]["pending"]["token"].clone();
    session.complete(
        pending.ticket,
        Err(hsscrap::Error::UnknownOutcome {
            operation_id: pending.ticket.operation_id,
        }),
    );
    let output: Value = serde_json::from_str(
        &bridge
            .details_apply(&details_command(
                &request["token"],
                json!({"type":"complete","token":token,"result":{"type":"unknown"}}),
            ))
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        session.phase(),
        hsscrap::interaction::Phase::Unknown(_)
    ));
    assert_eq!(output["details"]["view"]["phase"], "unknown");
    assert_eq!(
        bridge
            .apply(&command(json!({"type":"close_inspection"})))
            .unwrap_err(),
        "draft_pending"
    );
    apply(
        &mut bridge,
        json!({"type":"reset","query":query(PageSize::Items25)}),
    );
    assert_eq!(
        bridge
            .details_apply(&details_command(
                &request["token"],
                json!({"type":"cancel"})
            ))
            .unwrap_err(),
        "stale_ticket"
    );
}

#[test]
fn inspection_reload_cannot_widen_selected_context() {
    let mut bridge = RssBridge::new(&initial(INSTANCE, PageSize::Items25)).unwrap();
    load(&mut bridge, 1);
    let candidate = card(1, MediaKind::Image, 1);
    cards(&mut bridge, vec![candidate.clone()]);
    let request = effect(
        &apply(
            &mut bridge,
            json!({"type":"inspect","item_id":candidate.item.item_id}),
        ),
        "inspection",
    );
    let inspection = owner_fixture::inspection(&candidate);
    let replacement = apply(
        &mut bridge,
        json!({"type":"inspect","item_id":candidate.item.item_id}),
    );
    assert_eq!(
        effect(&replacement, "cancel_host_effect")["token"],
        request["token"]
    );
    assert_eq!(bridge.apply(&command(json!({"type":"complete_inspection","token":request["token"],"details_instance_id":OTHER,
        "inspection":hsscrap_wasm::wire::InspectionWire::from(inspection.clone())}))).unwrap_err(),"stale_ticket");
    let request = effect(&replacement, "inspection");
    apply(
        &mut bridge,
        json!({"type":"complete_inspection","token":request["token"],"details_instance_id":OTHER,
        "inspection":hsscrap_wasm::wire::InspectionWire::from(inspection.clone())}),
    );
    let before = bridge.snapshot().unwrap();
    let mut foreign = hsscrap_wasm::wire::InspectionWire::from(inspection);
    foreign.scope = rss_contracts::ScopeRef("foreign".into());
    assert_eq!(
        bridge
            .details_apply(&details_command(
                &request["token"],
                json!({"type":"reload","inspection":foreign})
            ))
            .unwrap_err(),
        "inspection_context"
    );
    assert_eq!(bridge.snapshot().unwrap(), before);
}

#[test]
fn strict_raw_admission_and_cross_instance_results_preserve_current_state() {
    let mut bridge = RssBridge::new(&initial(INSTANCE, PageSize::Items25)).unwrap();
    let before = bridge.snapshot().unwrap();
    for number in [
        "9007199254740992",
        "9007199254740992.0",
        "9.007199254740992e15",
        "-9007199254740992.0",
    ] {
        assert_eq!(
            bridge
                .apply(&format!(
                    "{{\"version\":1,\"action\":{{\"type\":\"revoke\"}},\"unsafe\":{number}}}"
                ))
                .unwrap_err(),
            "numeric_range"
        );
        assert_eq!(bridge.snapshot().unwrap(), before);
    }
    assert_eq!(
        bridge
            .apply("{\"version\":1,\"version\":1,\"action\":{\"type\":\"revoke\"}}")
            .unwrap_err(),
        "invalid_json"
    );
    let mut other = RssBridge::new(&initial(OTHER, PageSize::Items25)).unwrap();
    let old = effect(&apply(&mut bridge, json!({"type":"load"})), "query");
    apply(&mut other, json!({"type":"load"}));
    assert_eq!(other.apply(&command(json!({"type":"complete_page","token":old["token"],"result":{"type":"error","error":Error::Denied}}))).unwrap_err(),"stale_ticket");
    assert_eq!(
        serde_json::from_str::<Value>(&other.snapshot().unwrap()).unwrap()["view"]["phase"],
        "loading"
    );
    apply(&mut bridge, json!({"type":"cancel","token":old["token"]}));
    assert_eq!(bridge.apply(&command(json!({"type":"complete_page","token":old["token"],"result":{"type":"error","error":Error::Denied}}))).unwrap_err(),"stale_ticket");
}

#[test]
fn canonical_library_projection_matches_native_query_and_manager() {
    let input = library_request();
    let output: Value =
        serde_json::from_str(&RssBridge::compose_query(&input.to_string()).unwrap()).unwrap();
    let (library, selection) = library();
    let native = WallQuery::from_library(
        &library,
        &selection,
        Revision(9_007_199_254_740_997),
        "epoch".into(),
        &BTreeSet::from([host("source")]),
        BTreeSet::from([rss_contracts::ScopeRef("public".into())]),
        None,
        ContextFilter::AllAuthorized,
        WallFilter::default(),
        WallSort::Newest,
        PageSize::Items25,
        &bounds(),
    )
    .unwrap();
    assert_eq!(output["query"], serde_json::to_value(native).unwrap());
    assert_eq!(output["manager"]["sources"][0]["selected"], true);
    assert_eq!(output["manager"]["sources"][0]["visible"], true);
    assert_eq!(output["query"]["selection_revision"], "9007199254740997");
    for oversized in oversized_library_inputs() {
        assert_eq!(
            RssBridge::compose_query(&oversized.to_string()).unwrap_err(),
            "bounds"
        );
    }
}
