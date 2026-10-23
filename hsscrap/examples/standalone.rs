//! Synthetic standalone consumer. Hosts still authorize reads and execute owner plans.
use hsscrap::interaction::{Inspection, Phase, Session, TargetAccess};
use hsscrap::*;
use hsscrap_gui::Inspector;
use hsscrap_wasm::{wire::InspectionWire, HsscrapBridge};
use serde_json::{json, Value};

fn main() {
    let bounds = Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 100,
        max_page_items: 25,
        max_operation_bytes: 32768,
    };
    let original = "https://EXAMPLE.org:443/a%2fb?q=1#x";
    let normalized = identity::normalize_url(original, &bounds).unwrap();
    assert_eq!(normalized.original, original);
    assert_eq!(normalized.identity_url, "https://example.org/a%2Fb?q=1#x");
    let item_id = ItemId("00000000-0000-0000-0000-000000000001".parse().unwrap());
    let owner = HostRef {
        host_instance_id: "standalone-example".into(),
        owner_kind: "item".into(),
        native_id: "1".into(),
    };
    let scope = ScopeRef("example".into());
    let revision = Revision(9_007_199_254_740_993);
    let now = Instant::parse("2026-10-23T00:00:00Z").unwrap();
    let inspection = Inspection {
        scope: scope.clone(),
        item: Item {
            item_id,
            owner_scope: scope.clone(),
            revision,
            first_capture_id: CaptureId("00000000-0000-0000-0000-000000000002".parse().unwrap()),
            selected_observation_id: None,
            gallery_observation_id: None,
            member_order: vec![],
            title_override: Override::Set("Local title".into()),
            description_override: Override::Inherit,
            cover_override: Override::Inherit,
            created_at: now.clone(),
            updated_at: now,
            lifecycle: Lifecycle::Active,
            deleted_at: None,
            common: Extensions::default(),
        },
        selected_observation: None,
        captures: vec![],
        memberships: vec![],
        assignments: vec![],
        targets: vec![TargetAccess {
            target: TagTarget::Item(item_id),
            scope,
            label: "This item".into(),
            writable: true,
        }],
        tag_choices: vec![],
        can_edit_metadata: true,
        item_owner: owner.clone(),
        owner_fences: vec![ExpectedRevision { owner, revision }],
        bounds,
    };
    let mut session = Session::new(inspection.clone()).unwrap();
    session.begin_edit().unwrap();
    session
        .set_title(Override::Set("Unsaved draft".into()))
        .unwrap();
    assert!(session.dirty());
    assert!(session.cancel());
    assert_eq!(session.phase(), &Phase::Inspect);
    assert_eq!(
        session.inspection().unwrap().item.title_override,
        Override::Set("Local title".into())
    );

    let mut inspector = Inspector::new("standalone", inspection.clone()).unwrap();
    let mut has_edit = false;
    let _frame = egui::Context::default().run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            has_edit = inspector.show(ui).controls.contains_key("edit");
        });
    });
    assert!(has_edit);
    let initial = json!({
        "version": 1,
        "instance_id": "00000000-0000-0000-0000-000000000071",
        "inspection": InspectionWire::from(inspection),
    });
    let mut bridge = HsscrapBridge::new(&initial.to_string()).unwrap();
    let snapshot: Value = serde_json::from_str(&bridge.snapshot().unwrap()).unwrap();
    assert_eq!(
        snapshot["view"]["inspection"]["item"]["revision"],
        "9007199254740993"
    );
    let panel: Value = serde_json::from_str(
        &bridge
            .panel(&json!({"version":1,"width":390,"height":844}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert!(panel["controls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|control| control["id"] == "edit"));
    let revoked: Value = serde_json::from_str(
        &bridge
            .apply(&json!({"version":1,"action":{"type":"revoke"}}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert!(revoked["view"]["inspection"].is_null());
    println!(
        "{}",
        json!({
            "status":"pass", "data":"synthetic-standalone",
            "checks":["url_normalization","native_edit_cancel","native_gui",
                      "bridge_exact_revision","bridge_panel","bridge_revoke"]
        })
    );
}
