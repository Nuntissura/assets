//! Lane SMB-TYPE-AUTHORED-TOKEN-1 (STU-TYP-130): the per-document composition token record. Studio's
//! own bounds and refusals; vendor parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

const DOC: &str = "SDOC-019a0000-0000-7000-8000-000000000001";
const LAYER: &str = "SLYR-019a0000-0000-7000-8000-000000000002";
const STORY_A: &str = "STXT-019a0000-0000-7000-8000-00000000000a";
const STORY_B: &str = "STXT-019a0000-0000-7000-8000-00000000000b";
const V1: &str = STUDIO_COMPOSITION_TOKENS[0];
const V2: &str = STUDIO_COMPOSITION_TOKENS[1];
const V3: &str = STUDIO_COMPOSITION_TOKENS[2];

fn token() -> CancellationToken {
    CancellationToken::default()
}
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id": "hsk.studio.document@1", "document_id": DOC, "revision": 1, "geometry_unit": "pt", "colour_mode": 3, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null, "working_space_spot": null, "blending_space": null,
        "artboards": [], "page_spreads": [],
        "layers": [{"schema_id": "hsk.studio.layer@1", "layer_id": LAYER, "name": "t", "parent": null, "kind": "text", "payload": {"payload_kind": "text", "story_id": STORY_B}, "visible": true}],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []},
    }))
    .unwrap()
}
fn upgraded() -> StudioCompositionTokenRecord {
    let mut r = StudioCompositionTokenRecord::new(DOC, V3);
    r.upgrades = vec![
        StudioCompositionUpgrade { from_token: V1.into(), to_token: V2.into(), story_ids: vec![STORY_A.into()] },
        StudioCompositionUpgrade { from_token: V2.into(), to_token: V3.into(), story_ids: vec![] },
    ];
    r
}
fn parse(bytes: &[u8]) -> Result<StudioCompositionTokenRecord, Diagnostic> {
    parse_composition_token_record(bytes, &document(), Budget::default(), &token())
}
fn wire(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = serde_json::to_value(upgraded()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

#[test]
fn the_list_runs_oldest_first_and_an_absent_token_reads_as_the_first() {
    assert_eq!(ABSENT_COMPOSITION_TOKEN, V1);
    assert_eq!(CURRENT_COMPOSITION_TOKEN, V3);
    assert_eq!(composition_token_rank(V1), Some(0));
    assert_eq!(composition_token_rank(V3), Some(2));
    assert_eq!(composition_token_rank("hsk-type-shaped-runs@4/future"), None);
    assert!(STUDIO_COMPOSITION_TOKENS.iter().all(|t| t.len() <= MAX_COMPOSITION_TOKEN_BYTES));
}

#[test]
fn a_record_round_trips_with_its_upgrades() {
    let bytes = serde_json::to_vec(&upgraded()).unwrap();
    assert_eq!(parse(&bytes).unwrap(), upgraded());
    let fresh = serde_json::to_value(StudioCompositionTokenRecord::new(DOC, V3)).unwrap();
    assert_eq!(fresh, json!({"schema_id": "hsk.studio.composition_token@1", "document_id": DOC, "composition_version": V3}));
    assert_eq!(parse(&serde_json::to_vec(&fresh).unwrap()).unwrap().composition_version, V3);
}

#[test]
fn a_well_formed_token_this_build_does_not_know_is_refused_typed() {
    let future = "hsk-type-shaped-runs@4/future";
    let bytes = serde_json::to_vec(&StudioCompositionTokenRecord::new(DOC, future)).unwrap();
    assert_eq!(parse(&bytes).unwrap_err().code, Code::UnknownCompositionToken);
    // An upgrade that ends in an unknown token is the same refusal, not a broken chain.
    let bytes = wire(|v| {
        v["composition_version"] = json!(future);
        v["upgrades"][1]["to_token"] = json!(future);
    });
    assert_eq!(parse(&bytes).unwrap_err().code, Code::UnknownCompositionToken);
}

#[test]
fn hostile_records_are_refused_typed() {
    let cases: Vec<(&str, Vec<u8>, Code)> = vec![
        ("an unknown field", wire(|v| v["vendor"] = json!(1)), Code::InvalidJson),
        ("an unknown upgrade field", wire(|v| v["upgrades"][0]["vendor"] = json!(1)), Code::InvalidJson),
        ("no token", wire(|v| drop(v.as_object_mut().unwrap().remove("composition_version"))), Code::InvalidJson),
        ("a token of the wrong type", wire(|v| v["composition_version"] = json!(3)), Code::InvalidJson),
        ("another schema", wire(|v| v["schema_id"] = json!("hsk.studio.composition_token@2")), Code::InvalidJson),
        ("another document", wire(|v| v["document_id"] = json!("SDOC-019a0000-0000-7000-8000-000000000009")), Code::InvalidField),
        ("a document id of the wrong kind", wire(|v| v["document_id"] = json!(LAYER)), Code::InvalidId),
        ("an empty token", wire(|v| v["composition_version"] = json!("")), Code::InvalidField),
        ("a token over the limit", wire(|v| v["composition_version"] = json!("x".repeat(MAX_COMPOSITION_TOKEN_BYTES + 1))), Code::InvalidField),
        ("a token with a space", wire(|v| v["composition_version"] = json!("hsk type")), Code::InvalidField),
        ("a token with a control byte", wire(|v| v["composition_version"] = json!("hsk\u{7}type")), Code::InvalidField),
        ("a token past ASCII", wire(|v| v["composition_version"] = json!("hsk-\u{e9}")), Code::InvalidField),
        ("an upgrade token over the limit", wire(|v| v["upgrades"][0]["from_token"] = json!("x".repeat(MAX_COMPOSITION_TOKEN_BYTES + 1))), Code::InvalidField),
        ("a broken upgrade chain", wire(|v| v["upgrades"][1]["from_token"] = json!(V1)), Code::InvalidField),
        ("a last upgrade that is not the token", wire(|v| v["composition_version"] = json!(V2)), Code::InvalidField),
        ("an upgrade to itself", wire(|v| {
            v["upgrades"] = json!([{"from_token": V3, "to_token": V3}]);
        }), Code::InvalidField),
        ("an upgrade to an older token", wire(|v| {
            v["composition_version"] = json!(V1);
            v["upgrades"] = json!([{"from_token": V3, "to_token": V1}]);
        }), Code::InvalidField),
        ("stories out of order", wire(|v| v["upgrades"][0]["story_ids"] = json!([STORY_B, STORY_A])), Code::InvalidField),
        ("a story twice", wire(|v| v["upgrades"][0]["story_ids"] = json!([STORY_A, STORY_A])), Code::InvalidField),
        ("a story id that is no key", wire(|v| v["upgrades"][0]["story_ids"] = json!(["a b"])), Code::InvalidId),
        ("more upgrades than the limit", wire(|v| {
            let hop = |n: usize| json!({"from_token": format!("t{n}"), "to_token": format!("t{}", n + 1)});
            v["upgrades"] = json!((0..=MAX_COMPOSITION_UPGRADES).map(hop).collect::<Vec<_>>());
        }), Code::BudgetExceeded),
        ("more stories than the limit", wire(|v| {
            let names: Vec<String> = (0..=MAX_COMPOSITION_UPGRADE_STORIES).map(|n| format!("s{n:05}")).collect();
            v["upgrades"][0]["story_ids"] = json!(names);
        }), Code::BudgetExceeded),
        ("a repeated key", format!("{{\"document_id\":\"{DOC}\",\"document_id\":\"{DOC}\"}}").into_bytes(), Code::DuplicateField),
        ("no JSON at all", b"{not json".to_vec(), Code::InvalidJson),
        ("nesting past the depth budget", "[".repeat(64).into_bytes(), Code::BudgetExceeded),
        ("bytes past the input limit", vec![b' '; MAX_COMPOSITION_TOKEN_INPUT_BYTES + 1], Code::BudgetExceeded),
    ];
    for (what, bytes, code) in cases {
        assert_eq!(parse(&bytes).map_err(|d| d.code), Err(code), "{what}");
    }
}

#[test]
fn a_cancel_is_a_cancel() {
    let canceled = token();
    canceled.cancel();
    let bytes = serde_json::to_vec(&upgraded()).unwrap();
    assert_eq!(parse_composition_token_record(&bytes, &document(), Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
    assert_eq!(composition_affected_stories(&document(), None, Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn the_stories_of_a_token_change_are_the_documents_and_the_text_entrys_sorted_and_unique() {
    let doc = document();
    let budget = Budget::default();
    assert_eq!(composition_affected_stories(&doc, None, budget, &token()).unwrap(), [STORY_B]);
    let text = json!({"schema": "hsk.harness.text_layers@1", "layers": [], "styles": {}, "stories": [{"story_id": STORY_B, "text_kind": "area"}, {"story_id": STORY_A}]});
    let text = serde_json::to_vec(&text).unwrap();
    assert_eq!(composition_affected_stories(&doc, Some(&text), budget, &token()).unwrap(), [STORY_A, STORY_B]);
    // A text entry the diff cannot read is refused, never a shorter list.
    assert_eq!(composition_affected_stories(&doc, Some(b"{not json"), budget, &token()).unwrap_err().code, Code::InvalidJson);
    assert_eq!(composition_affected_stories(&doc, Some(b"{\"stories\":[{}]}"), budget, &token()).unwrap_err().code, Code::InvalidJson);
    // More than one upgrade may name is refused, so the record it would write stays within its bound.
    let many: Vec<Value> = (0..=MAX_COMPOSITION_UPGRADE_STORIES).map(|n| json!({"story_id": format!("s{n:05}")})).collect();
    let many = serde_json::to_vec(&json!({"stories": many})).unwrap();
    assert_eq!(composition_affected_stories(&doc, Some(&many), budget, &token()).unwrap_err().code, Code::BudgetExceeded);
}
