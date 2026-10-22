//! SMB-FOLIO-PARENT-PAGES: `hsk.studio.parent_page_records@1`. Byte-identical wire round trip and
//! every refusal code derived from the rule named next to it.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn slyr(n: u32) -> String {
    identity("SLYR", n)
}
struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}
fn wire_identical<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> T {
    let first = serde_json::to_vec(v).expect("encode");
    let back: T = serde_json::from_slice(&first).expect("decode");
    assert_eq!(&back, v);
    assert_eq!(serde_json::to_vec(&back).expect("encode again"), first);
    back
}

fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn page(key: &str) -> Value {
    let e = json!({"top": pt(36.0), "right": pt(36.0), "bottom": pt(36.0), "left": pt(36.0)});
    json!({"page_id": key, "width": pt(612.0), "height": pt(792.0), "side": "right_hand",
        "margins": e, "bleed": null})
}
fn document() -> StudioDocument {
    let layers: Vec<Value> = (1..=8)
        .map(|n| {
            json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),
                "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})
        })
        .collect();
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),
            "name":"spread 1","parent":null,
            "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(1224.0),"height":pt(792.0)},
            "pages":[page("p1"),page("p2")]}],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}

fn parent(id: &str, based_on: Option<&str>, items: &[(&str, u32)]) -> Value {
    let mut v = json!({"schema_id":"hsk.studio.parent_page@1","parent_id":id,"name":id,
        "parent_page_items":items.iter().map(|(i,l)| json!({"item_id":i,"layer_id":slyr(*l)})).collect::<Vec<_>>()});
    if let Some(b) = based_on {
        v["based_on"] = json!(b);
    }
    v
}
fn records(parents: Vec<Value>, bindings: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.parent_page_records@1","document_id":identity("SDOC",1),
        "parents":parents,"page_bindings":bindings})
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioParentPageRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_parent_page_records(
        &r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}
fn code(v: &Value) -> Code {
    check(v).expect_err("refused").code
}
fn bound() -> Value {
    json!({"page_id":"p1","applied_parent":"B","override_list":[
        {"override_kind":"replace","parent_item_id":"num","replacement_layer_id":slyr(5)},
        {"override_kind":"suppress","parent_item_id":"logo"}]})
}

/// A parent based on a parent, per-page binding with Replace and Suppress of items inherited from
/// the parent and from its ancestor round-trips byte-identically; an empty set encodes only its
/// header; a page bound to no parent carries no `applied_parent` key.
#[test]
fn parent_pages_round_trip_and_resolve_inherited_items() {
    let v = records(
        vec![
            parent("A", None, &[("logo", 1), ("rule", 2)]),
            parent("B", Some("A"), &[("num", 3)]),
        ],
        vec![bound(), json!({"page_id":"p2"})],
    );
    assert!(check(&v).expect("valid").is_empty());
    let r: StudioParentPageRecords = serde_json::from_value(v).unwrap();
    let back = wire_identical(&r);
    assert_eq!(back.parent("B").unwrap().based_on.as_deref(), Some("A"));
    assert_eq!(back.binding("p1").unwrap().override_list.len(), 2);
    assert_eq!(back.binding("p2").unwrap().applied_parent, None);
    assert!(!serde_json::to_string(back.binding("p2").unwrap()).unwrap().contains("applied_parent"));
    let empty = StudioParentPageRecords::new(identity("SDOC", 1));
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({"schema_id":"hsk.studio.parent_page_records@1","document_id":identity("SDOC",1)})
    );
    assert!(parent_page_records_schema().as_value().is_object());
}

/// Cycles (self and mutual), dangling references and overrides of items that are not on the parent
/// chain are refused with the code of the rule.
#[test]
fn cycle_dangling_and_override_refusals() {
    let a = || parent("A", None, &[("logo", 1)]);
    // based_on loops: self, mutual.
    assert_eq!(code(&records(vec![parent("A", Some("A"), &[])], vec![])), Code::RenderCycle);
    assert_eq!(
        code(&records(vec![parent("A", Some("B"), &[]), parent("B", Some("A"), &[])], vec![])),
        Code::RenderCycle
    );
    // Dangling: based_on, item layer, page, applied parent, replacement layer.
    assert_eq!(code(&records(vec![parent("A", Some("Z"), &[])], vec![])), Code::DanglingReference);
    assert_eq!(code(&records(vec![parent("A", None, &[("x", 99)])], vec![])), Code::DanglingReference);
    assert_eq!(
        code(&records(vec![a()], vec![json!({"page_id":"nope"})])),
        Code::DanglingReference
    );
    assert_eq!(
        code(&records(vec![a()], vec![json!({"page_id":"p1","applied_parent":"Z"})])),
        Code::DanglingReference
    );
    let replace = |parent_item: &str, layer: u32| {
        json!({"page_id":"p1","applied_parent":"A","override_list":[
            {"override_kind":"replace","parent_item_id":parent_item,"replacement_layer_id":slyr(layer)}]})
    };
    assert_eq!(code(&records(vec![a()], vec![replace("logo", 99)])), Code::DanglingReference);
    // Override of an item that is not on the parent chain (also an item of a descendant).
    assert_eq!(code(&records(vec![a()], vec![replace("ghost", 5)])), Code::DanglingReference);
    let child_only = records(vec![a(), parent("B", Some("A"), &[("num", 3)])], vec![replace("num", 5)]);
    assert_eq!(code(&child_only), Code::DanglingReference);
    // Override without a parent; duplicate override; layer reuse.
    assert_eq!(
        code(&records(vec![a()], vec![json!({"page_id":"p1","override_list":[
            {"override_kind":"suppress","parent_item_id":"logo"}]})])),
        Code::InvalidField
    );
    let twice = json!({"page_id":"p1","applied_parent":"A","override_list":[
        {"override_kind":"suppress","parent_item_id":"logo"},
        {"override_kind":"suppress","parent_item_id":"logo"}]});
    assert_eq!(code(&records(vec![a()], vec![twice])), Code::DuplicateIdentity);
    assert_eq!(code(&records(vec![a()], vec![replace("logo", 1)])), Code::DuplicateIdentity);
    // Duplicate parent, item and page binding; document binding.
    assert_eq!(code(&records(vec![a(), a()], vec![])), Code::DuplicateIdentity);
    assert_eq!(
        code(&records(vec![parent("A", None, &[("x", 1), ("x", 2)])], vec![])),
        Code::DuplicateIdentity
    );
    assert_eq!(
        code(&records(vec![], vec![json!({"page_id":"p1"}), json!({"page_id":"p1"})])),
        Code::DuplicateIdentity
    );
    let mut other = records(vec![], vec![]);
    other["document_id"] = json!(identity("SDOC", 2));
    assert_eq!(code(&other), Code::InvalidField);
}

/// Hostile input: unknown fields and unknown override kinds do not decode; budgets and
/// cancellation are honoured.
#[test]
fn hostile_wire_budget_and_cancel() {
    let mut extra = records(vec![parent("A", None, &[])], vec![]);
    extra["parents"][0]["surprise"] = json!(1);
    assert!(serde_json::from_value::<StudioParentPageRecords>(extra).is_err());
    let unknown = records(
        vec![parent("A", None, &[("logo", 1)])],
        vec![json!({"page_id":"p1","applied_parent":"A","override_list":[
            {"override_kind":"move","parent_item_id":"logo"}]})],
    );
    assert!(serde_json::from_value::<StudioParentPageRecords>(unknown).is_err());

    let r: StudioParentPageRecords = serde_json::from_value(records(
        vec![parent("A", None, &[("logo", 1)])],
        vec![bound_simple()],
    ))
    .unwrap();
    let tight = Budget {
        nodes: 1,
        ..Budget::default()
    };
    let refused = validate_parent_page_records(
        &r,
        &document(),
        tight,
        &CancellationToken::default(),
        &Caller,
    )
    .expect_err("budget");
    assert_eq!(refused.code, Code::BudgetExceeded);
    let token = CancellationToken::default();
    token.cancel();
    let canceled =
        validate_parent_page_records(&r, &document(), Budget::default(), &token, &Caller)
            .expect_err("cancel");
    assert_eq!(canceled.code, Code::Canceled);
}
fn bound_simple() -> Value {
    json!({"page_id":"p1","applied_parent":"A"})
}
