#![allow(dead_code)]
#[path = "../../fixtures/interaction/fixture.rs"]
pub mod owner_fixture;
use owner_fixture::*;
use rss_core::query::*;
use rss_wasm::RssBridge;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const INSTANCE: &str = "00000000-0000-0000-0000-00000000a001";
pub const OTHER: &str = "00000000-0000-0000-0000-00000000a002";

pub fn initial(instance: &str, size: PageSize) -> String {
    serde_json::to_string(&rss_wasm::wire::Initial {
        version: 1,
        instance_id: instance.into(),
        query: query(size),
        bounds: bounds(),
        maximum_loaded_items: 500,
    })
    .unwrap()
}
pub fn command(action: Value) -> String {
    json!({"version":1,"action":action}).to_string()
}
pub fn parsed(result: Result<String, String>) -> Value {
    match result {
        Ok(value) => json!({"ok":serde_json::from_str::<Value>(&value).unwrap()}),
        Err(error) => json!({"error":error}),
    }
}
pub fn apply(bridge: &mut RssBridge, action: Value) -> Value {
    serde_json::from_str(&bridge.apply(&command(action)).unwrap()).unwrap()
}
pub fn effect(output: &Value, kind: &str) -> Value {
    output["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|effect| effect["type"] == kind)
        .unwrap()
        .clone()
}
pub fn page(
    query: WallQuery,
    start: u64,
    count: u64,
    snapshot: &str,
    next: Option<&str>,
) -> WallPage {
    WallPage {
        snapshot: WallSnapshot {
            token: snapshot.into(),
            query,
        },
        rows: (start..start + count).map(row).collect(),
        next: next.map(|cursor| rss_contracts::WallCursor(cursor.into())),
        anchor: AnchorResolution::NotRequested,
        health: PageHealth::Ready,
    }
}
pub fn load(bridge: &mut RssBridge, count: u64) -> Value {
    let request = effect(&apply(bridge, json!({"type":"load"})), "query");
    let query: WallQuery = serde_json::from_value(request["request"]["query"].clone()).unwrap();
    apply(
        bridge,
        json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":page(query,1,count,"frozen-one",None)}}),
    )
}
pub fn cards(bridge: &mut RssBridge, cards: Vec<WallCard>) -> Value {
    let request = effect(
        &apply(
            bridge,
            json!({"type":"request_cards","items":cards.iter().map(|card| card.item.item_id).collect::<Vec<_>>()}),
        ),
        "cards",
    );
    apply(
        bridge,
        json!({"type":"complete_cards","token":request["token"],"result":{"type":"cards","cards":cards}}),
    )
}
pub fn details_command(token: &Value, action: Value) -> String {
    json!({"version":1,"token":token,"input":command(action)}).to_string()
}
pub fn library_request() -> Value {
    let (library, selection) = library();
    json!({"version":1,"scope":library.owner_scope,"library_revision":library.revision,
        "sources":library.sources.values().collect::<Vec<_>>(),"groups":[],"memberships":[],"rules":[],
        "selection":{"owner_scope":selection.owner_scope,"profile_ref":selection.profile_ref,
            "selected_groups":selection.selected_groups,"selected_sources":selection.selected_sources},
        "selection_revision":"9007199254740997","authority_epoch":"epoch","authorized_sources":[host("source")],
        "access_partitions":["public"],"focused_group":null,"context":"AllAuthorized","filter":WallFilter::default(),
        "sort":"Newest","page_size":"Items25","health":[],"bounds":bounds()})
}

pub fn oversized_library_inputs() -> Vec<Value> {
    let mut rule = library_request();
    rule["groups"] = json!([{"group_ref":host("unselected"),"owner_scope":"account","name":"Unused","description":"",
        "keywords":[],"priority":0,"tag_refs":[],"revision":"1","common":rss_contracts::Extensions::default()}]);
    rule["memberships"] = json!([{"group":host("unselected"),"source":host("source")}]);
    rule["rules"] = json!([{"group":host("unselected"),"source":host("source"),"keywords":["x".repeat(4097)],"prioritize":false}]);
    let mut result = vec![rule];
    for state in [
        rss_contracts::AcquisitionState::Failed {
            reason_code: "x".repeat(4097),
        },
        rss_contracts::AcquisitionState::Partial {
            observed_at: at(),
            reason_code: "x".repeat(4097),
        },
        rss_contracts::AcquisitionState::VerifiedEmpty {
            observed_at: at(),
            selected_source_identity: "x".repeat(4097),
        },
    ] {
        let mut health = library_request();
        health["health"] = json!([{"source":host("source"),"state":state}]);
        result.push(health);
    }
    result
}

#[derive(Serialize, Deserialize)]
pub struct Step {
    pub method: String,
    pub input: String,
    pub expected: Value,
}
#[derive(Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub initial: String,
    pub constructor_error: Option<String>,
    pub steps: Vec<Step>,
}
struct Recorder {
    bridge: RssBridge,
    scenario: Scenario,
}
impl Recorder {
    fn new(name: &str, size: PageSize) -> Self {
        let initial = initial(INSTANCE, size);
        Self {
            bridge: RssBridge::new(&initial).unwrap(),
            scenario: Scenario {
                name: name.into(),
                initial,
                constructor_error: None,
                steps: vec![],
            },
        }
    }
    fn run(&mut self, method: &str, input: String) -> Value {
        let output = parsed(match method {
            "apply" => self.bridge.apply(&input),
            "snapshot" => self.bridge.snapshot(),
            "details_apply" => self.bridge.details_apply(&input),
            "details_panel" => self.bridge.details_panel(&input),
            "compose_query" => RssBridge::compose_query(&input),
            _ => panic!("unknown test method"),
        });
        self.scenario.steps.push(Step {
            method: method.into(),
            input,
            expected: output.clone(),
        });
        output
    }
    fn action(&mut self, action: Value) -> Value {
        self.run("apply", command(action))["ok"].clone()
    }
    fn load(&mut self, count: u64) {
        let request = effect(&self.action(json!({"type":"load"})), "query");
        let query = serde_json::from_value(request["request"]["query"].clone()).unwrap();
        self.action(json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":page(query,1,count,"frozen-one",None)}}));
    }
    fn cards(&mut self, cards: Vec<WallCard>) {
        let items: Vec<_> = cards.iter().map(|card| card.item.item_id).collect();
        let request = effect(
            &self.action(json!({"type":"request_cards","items":items})),
            "cards",
        );
        self.action(json!({"type":"complete_cards","token":request["token"],"result":{"type":"cards","cards":cards}}));
    }
}

pub fn scenarios() -> Vec<Scenario> {
    let mut result = vec![];
    for size in [PageSize::Items25, PageSize::Items50, PageSize::Items100] {
        let mut run = Recorder::new(&format!("paging-{size:?}"), size);
        let request = effect(&run.action(json!({"type":"load"})), "query");
        let spec: WallQuery = serde_json::from_value(request["request"]["query"].clone()).unwrap();
        run.action(json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":page(spec.clone(),1,size.get() as u64,"frozen-one",Some("opaque-next"))}}));
        let next = effect(&run.action(json!({"type":"load_next"})), "query");
        run.action(json!({"type":"complete_page","token":next["token"],"result":{"type":"page","page":page(spec.clone(),size.get() as u64+1,2,"frozen-one",None)}}));
        let position = ReadingPosition {
            item_id: id(2),
            offset_basis_points: 2000,
        };
        run.action(json!({"type":"set_position","position":position}));
        let refresh = effect(&run.action(json!({"type":"refresh"})), "query");
        let mut replacement = page(spec, 2, 2, "frozen-two", None);
        replacement.anchor = AnchorResolution::Preserved(position);
        run.action(json!({"type":"complete_page","token":refresh["token"],"result":{"type":"page","page":replacement}}));
        run.action(json!({"type":"apply_refresh"}));
        result.push(run.scenario);
    }
    let mut run = Recorder::new("cancel-restart-and-denial", PageSize::Items25);
    let request = effect(&run.action(json!({"type":"load"})), "query");
    run.action(json!({"type":"cancel","token":request["token"]}));
    run.run("apply",command(json!({"type":"complete_page","token":request["token"],"result":{"type":"error","error":{"code":"network"}}})));
    let request = effect(&run.action(json!({"type":"load"})), "query");
    run.run("apply",command(json!({"type":"complete_page","token":format!("{OTHER}:1"),"result":{"type":"error","error":{"code":"denied"}}})));
    run.action(json!({"type":"complete_page","token":request["token"],"result":{"type":"error","error":{"code":"denied"}}}));
    run.action(json!({"type":"reset","query":query(PageSize::Items25)}));
    run.action(json!({"type":"load"}));
    result.push(run.scenario);

    let mut run = Recorder::new("lazy-media-and-one-player", PageSize::Items25);
    run.load(2);
    run.cards(vec![
        card(1, hsscrap::MediaKind::Video, 2),
        card(2, hsscrap::MediaKind::Video, 1),
    ]);
    let preview = effect(
        &run.action(json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(1)})),
        "media",
    );
    let cancelled = run.action(json!({"type":"cancel","token":preview["token"]}));
    assert_eq!(cancelled["view"]["media"][0]["state"], "not_requested");
    run.run(
        "apply",
        command(json!({"type":"complete_media","token":preview["token"],"state":"ready"})),
    );
    let preview = effect(
        &run.action(json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(1)})),
        "media",
    );
    let play = effect(
        &run.action(json!({"type":"play","item_id":id::<rss_contracts::ItemId>(1)})),
        "media",
    );
    run.run(
        "apply",
        command(json!({"type":"complete_media","token":preview["token"],"state":"ready"})),
    );
    run.action(json!({"type":"complete_media","token":play["token"],"state":"ready"}));
    let second = effect(
        &run.action(json!({"type":"play","item_id":id::<rss_contracts::ItemId>(2)})),
        "media",
    );
    let rendering = run.action(json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(2)}));
    assert!(rendering["effects"].as_array().unwrap().is_empty());
    run.action(json!({"type":"complete_media","token":second["token"],"state":"failed"}));
    run.action(json!({"type":"select_member","item_id":id::<rss_contracts::ItemId>(1),"member_id":id::<hsscrap::MemberId>(101)}));
    run.action(json!({"type":"preview","item_id":id::<rss_contracts::ItemId>(1)}));
    run.action(json!({"type":"revoke"}));
    result.push(run.scenario);

    let mut run = Recorder::new("released-member-media-retries-and-stale-tickets", PageSize::Items25);
    run.load(2);
    run.cards(vec![card(1,hsscrap::MediaKind::Video,2),card(2,hsscrap::MediaKind::Video,1)]);
    let member=json!({"item_id":id::<rss_contracts::ItemId>(1),"member_id":id::<hsscrap::MemberId>(100)});
    let other=json!({"item_id":id::<rss_contracts::ItemId>(2),"member_id":id::<hsscrap::MemberId>(200)});
    for state in [None,Some("ready"),Some("unavailable"),Some("failed")] {
        let old=effect(&run.action(json!({"type":"preview","item_id":member["item_id"]})),"media");
        if let Some(state)=state {run.action(json!({"type":"complete_media","token":old["token"],"state":state}));}
        let released=run.action(json!({"type":"release_media","member":member}));
        assert_eq!(effect(&released,"release_media")["member"],member);
        assert_eq!(released["view"]["media"][0]["state"],"not_requested");
        if state.is_none(){assert_eq!(effect(&released,"cancel_host_effect")["token"],old["token"]);}
        run.action(json!({"type":"release_media","member":member}));
        let fresh=effect(&run.action(json!({"type":"preview","item_id":member["item_id"]})),"media");
        assert_ne!(old["token"],fresh["token"]);
        let stale=run.run("apply",command(json!({"type":"complete_media","token":old["token"],"state":"ready"})));
        assert_eq!(stale["error"],"stale_ticket");
        let stale=run.run("apply",command(json!({"type":"cancel","token":old["token"]})));
        assert_eq!(stale["error"],"stale_ticket");
        run.action(json!({"type":"cancel","token":fresh["token"]}));
    }
    let playing=effect(&run.action(json!({"type":"play","item_id":other["item_id"]})),"media");
    let unrelated=run.action(json!({"type":"release_media","member":member}));
    assert_eq!(unrelated["view"]["player"],other);
    let missing=run.action(json!({"type":"release_media","member":{"item_id":member["item_id"],"member_id":other["member_id"]}}));
    assert!(missing["error"].is_object());
    assert_eq!(missing["view"]["player"],other);
    let stopped=run.action(json!({"type":"release_media","member":other}));
    assert_eq!(effect(&stopped,"stop_playback")["member"],other);
    assert_eq!(effect(&stopped,"cancel_host_effect")["token"],playing["token"]);
    assert!(stopped["view"]["player"].is_null());
    run.action(json!({"type":"revoke"}));
    let denied=run.action(json!({"type":"release_media","member":member}));
    assert!(denied["error"].is_object());
    result.push(run.scenario);

    let mut run = Recorder::new("hss-details-context-and-large-revision", PageSize::Items25);
    run.load(1);
    let mut item = card(1, hsscrap::MediaKind::Image, 1);
    item.item.revision = rss_contracts::Revision(9_007_199_254_740_997);
    run.cards(vec![item.clone()]);
    let obsolete_inspection = effect(
        &run.action(json!({"type":"inspect","item_id":item.item.item_id})),
        "inspection",
    );
    let replacement = run.action(json!({"type":"inspect","item_id":item.item.item_id}));
    assert_eq!(
        effect(&replacement, "cancel_host_effect")["token"],
        obsolete_inspection["token"]
    );
    let inspection = effect(&replacement, "inspection");
    let native = owner_fixture::inspection(&item);
    let wire = hsscrap_wasm::wire::InspectionWire::from(native.clone());
    let stale = run.run("apply",command(json!({"type":"complete_inspection","token":obsolete_inspection["token"],"details_instance_id":OTHER,
        "inspection":wire.clone()})));
    assert_eq!(stale["error"], "stale_ticket");
    run.action(json!({"type":"complete_inspection","token":inspection["token"],"details_instance_id":OTHER,"inspection":wire}));
    let mut foreign = hsscrap_wasm::wire::InspectionWire::from(native.clone());
    foreign.scope = rss_contracts::ScopeRef("foreign".into());
    run.run(
        "details_apply",
        details_command(
            &inspection["token"],
            json!({"type":"reload","inspection":foreign}),
        ),
    );
    run.run("details_panel",json!({"version":1,"token":inspection["token"],"input":json!({"version":1,"width":390.0,"height":1000.0}).to_string()}).to_string());
    run.run(
        "details_apply",
        details_command(&inspection["token"], json!({"type":"begin_edit"})),
    );
    run.run("apply",command(json!({"type":"complete_inspection","token":inspection["token"],"details_instance_id":OTHER,
        "inspection":hsscrap_wasm::wire::InspectionWire::from(native.clone())})));
    run.run(
        "apply",
        command(json!({"type":"inspect","item_id":item.item.item_id})),
    );
    run.run(
        "details_apply",
        details_command(
            &inspection["token"],
            json!({"type":"set_title","value":hsscrap::Override::Set("Browser edit".to_owned())}),
        ),
    );
    let context = hsscrap::ports::CommandContext {
        actor: host("actor"),
        scope: native.scope.clone(),
        operation_id: id(60000),
        intent_digest: String::new(),
        expected_revisions: native.owner_fences.clone(),
        bounds: bounds(),
    };
    let pending = run.run(
        "details_apply",
        details_command(
            &inspection["token"],
            json!({"type":"begin_save","context":context}),
        ),
    );
    let save_token = pending["ok"]["details"]["view"]["pending"]["token"].clone();
    run.run(
        "details_apply",
        details_command(
            &inspection["token"],
            json!({"type":"complete","token":save_token,"result":{"type":"unknown"}}),
        ),
    );
    run.run("apply", command(json!({"type":"close_inspection"})));
    run.action(json!({"type":"reset","query":query(PageSize::Items25)}));
    run.run(
        "details_apply",
        details_command(&inspection["token"], json!({"type":"cancel"})),
    );
    result.push(run.scenario);

    let mut run = Recorder::new("strict-wire-admission", PageSize::Items25);
    run.run(
        "apply",
        "{\"version\":1,\"version\":1,\"action\":{\"type\":\"revoke\"}}".into(),
    );
    for number in [
        "9007199254740992",
        "9007199254740992.0",
        "9.007199254740992e15",
        "-9007199254740992.0",
    ] {
        run.run(
            "apply",
            format!("{{\"version\":1,\"action\":{{\"type\":\"revoke\"}},\"number\":{number}}}"),
        );
    }
    run.run(
        "apply",
        json!({"version":2,"action":{"type":"revoke"}}).to_string(),
    );
    run.run(
        "apply",
        format!("{{\"x\":{}0{}}}", "[".repeat(129), "]".repeat(129)),
    );
    run.run("apply", " ".repeat(1024 * 1024 + 1));
    run.run("snapshot", String::new());
    result.push(run.scenario);

    let mut run = Recorder::new("canonical-library-and-manager", PageSize::Items25);
    run.run("compose_query", library_request().to_string());
    let mut tagged = library_request();
    tagged["sources"][0]["extensions"] = json!({"rss.source_tags":{"version":1,"tag_refs":[host("shared-tag")]},
        "portal.connection_selection":{"connection_id":"preserved","future":true}});
    let tagged_output = run.run("compose_query", tagged.to_string());
    assert_eq!(tagged_output["ok"]["manager"]["sources"][0]["source"]["extensions"], tagged["sources"][0]["extensions"]);
    tagged["sources"][0]["extensions"]["rss.source_tags"]["unknown"] = json!("reject without dropping");
    assert_eq!(run.run("compose_query", tagged.to_string())["error"], "invalid_source_tags");
    let mut foreign = library_request();
    foreign["selection"]["owner_scope"] = json!("foreign");
    run.run("compose_query", foreign.to_string());
    for oversized in oversized_library_inputs() {
        let output = run.run("compose_query", oversized.to_string());
        assert_eq!(output["error"], "bounds");
    }
    result.push(run.scenario);

    let mut run = Recorder::new("bounded-handles", PageSize::Items100);
    run.load(40);
    let mut first = Value::Null;
    for number in 1..=32 {
        let request = effect(
            &run.action(
                json!({"type":"request_cards","items":[id::<rss_contracts::ItemId>(number)]}),
            ),
            "cards",
        );
        if number == 1 {
            first = request["token"].clone();
        }
    }
    let exhausted = run.run(
        "apply",
        command(json!({"type":"request_cards","items":[id::<rss_contracts::ItemId>(33)]})),
    );
    assert_eq!(exhausted["error"], "backpressure");
    run.action(json!({"type":"cancel","token":first}));
    run.action(json!({"type":"request_cards","items":[id::<rss_contracts::ItemId>(33)]}));
    result.push(run.scenario);
    let mut run = Recorder::new("bounded-retained-window", PageSize::Items25);
    let mut configured: Value = serde_json::from_str(&run.scenario.initial).unwrap();
    configured["bounds"]["max_text_bytes"] = json!(65536);
    run.scenario.initial = configured.to_string();
    run.bridge = RssBridge::new(&run.scenario.initial).unwrap();
    for index in 0..3 {
        let request = effect(
            &run.action(json!({"type":if index == 0 { "load" } else { "load_next" }})),
            "query",
        );
        let query = serde_json::from_value(request["request"]["query"].clone()).unwrap();
        let next = format!("large-page-{}", index + 1);
        let mut page = page(query, index * 25 + 1, 25, "large-frozen", Some(&next));
        for row in &mut page.rows {
            row.appearances[0]
                .common
                .extensions
                .insert("test.payload".into(), json!("x".repeat(30000)));
        }
        let completed = run.run("apply",command(json!({"type":"complete_page","token":request["token"],"result":{"type":"page","page":page}})));
        if index == 2 {
            assert_eq!(completed["error"], "bounds");
            run.action(json!({"type":"cancel","token":request["token"]}));
        } else {
            assert_eq!(
                completed["ok"]["view"]["rows"].as_array().unwrap().len(),
                ((index + 1) * 25) as usize
            );
        }
    }
    run.run("snapshot", String::new());
    result.push(run.scenario);
    for (name, mutate) in [
        ("unsupported-constructor-version", 0),
        ("invalid-constructor-bounds", 1),
    ] {
        let mut input: Value = serde_json::from_str(&initial(INSTANCE, PageSize::Items25)).unwrap();
        if mutate == 0 {
            input["version"] = json!(99);
        } else {
            input["maximum_loaded_items"] = json!(1025);
        }
        let input = input.to_string();
        let error = RssBridge::new(&input).err().expect("rejected constructor");
        assert_eq!(
            error,
            if mutate == 0 {
                "unsupported_version"
            } else {
                "bounds"
            }
        );
        result.push(Scenario {
            name: name.into(),
            initial: input,
            constructor_error: Some(error),
            steps: vec![],
        });
    }
    result
}
