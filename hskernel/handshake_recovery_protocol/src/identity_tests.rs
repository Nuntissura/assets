use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::json;

use crate::{AgentCapability, AgentLaneIdentity, AgentLaneKind, AttributionMode,
    LocalCloudAttribution, ModelProviderKind};

fn identity(kind: AgentLaneKind) -> AgentLaneIdentity {
    AgentLaneIdentity {
        lane_id: "lane".into(), actor_id: "actor".into(), lane_kind: kind,
        attribution: LocalCloudAttribution::local("runtime", "model"),
    }
}

#[derive(Debug)]
struct InjectedError {
    message: String,
    token: Box<u8>,
}

struct ObservedInput {
    name: &'static str,
    value: &'static str,
    events: Rc<RefCell<Vec<String>>>,
}

impl From<ObservedInput> for String {
    fn from(input: ObservedInput) -> Self {
        input.events.borrow_mut().push(format!("convert:{}", input.name));
        input.value.into()
    }
}

#[test]
fn lane_kinds_preserve_wire_names_and_skip_error_mapper_for_known_values() {
    for (kind, wire) in [
        (AgentLaneKind::Local, "local"), (AgentLaneKind::Cloud, "cloud"),
        (AgentLaneKind::Operator, "operator"), (AgentLaneKind::Validator, "validator"),
        (AgentLaneKind::IntegrationValidator, "integration_validator"),
        (AgentLaneKind::Indexer, "indexer"), (AgentLaneKind::Editor, "editor"),
        (AgentLaneKind::System, "system"),
    ] {
        assert_eq!(kind.as_str(), wire);
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<AgentLaneKind>(json!(wire)).unwrap(), kind);
        let calls = Cell::new(0);
        let parsed = AgentLaneKind::parse(wire, |message| {
            calls.set(calls.get() + 1);
            InjectedError { message, token: Box::new(0) }
        }).unwrap();
        assert_eq!(parsed, kind);
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn unknown_lane_mapper_receives_exact_text_once_and_returns_nonclone_error() {
    for value in ["", "LOCAL", " local ", "unknown"] {
        let calls = Cell::new(0);
        let token = Box::new(7);
        let address = &*token as *const u8;
        let error = AgentLaneKind::parse(value, |message| {
            calls.set(calls.get() + 1);
            InjectedError { message, token }
        }).unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(error.message, format!("unknown lane kind: {value}"));
        assert_eq!(&*error.token as *const u8, address);
    }
}

#[test]
fn constructor_converts_both_ids_before_validation_and_preserves_error_identity() {
    for fail_at in ["lane_id", "actor_id"] {
        let events = Rc::new(RefCell::new(Vec::new()));
        let lane = ObservedInput { name: "lane", value: " lane ", events: events.clone() };
        let actor = ObservedInput { name: "actor", value: " actor ", events: events.clone() };
        let token = Box::new(42);
        let address = &*token as *const u8;
        let mut failure = Some(InjectedError { message: "external-policy".into(), token });
        let error = AgentLaneIdentity::new(lane, actor, AgentLaneKind::Local,
            LocalCloudAttribution::local("runtime", "model"), |field, value| {
                events.borrow_mut().push(format!("validate:{field}:{value}"));
                if field == fail_at { Err(failure.take().unwrap()) } else { Ok(()) }
            }).unwrap_err();
        let mut expected = vec!["convert:lane", "convert:actor", "validate:lane_id: lane "];
        if fail_at == "actor_id" { expected.push("validate:actor_id: actor "); }
        assert_eq!(*events.borrow(), expected);
        assert_eq!(error.message, "external-policy");
        assert_eq!(&*error.token as *const u8, address);
    }
}

#[test]
fn constructor_preserves_ids_and_calls_supplied_validator_in_field_order() {
    let mut seen = Vec::new();
    let attribution = LocalCloudAttribution::local("runtime", "model");
    let lane = AgentLaneIdentity::new(" lane ", " actor ", AgentLaneKind::Editor,
        attribution.clone(), |field, value| {
            seen.push((field.to_owned(), value.to_owned()));
            Ok::<(), InjectedError>(())
        }).unwrap();
    assert_eq!(seen, vec![("lane_id".into(), " lane ".into()), ("actor_id".into(), " actor ".into())]);
    assert_eq!(lane.lane_id, " lane ");
    assert_eq!(lane.actor_id, " actor ");
    assert_eq!(lane.lane_kind, AgentLaneKind::Editor);
    assert_eq!(lane.attribution, attribution);
}

#[test]
fn constructor_allows_callers_to_retain_static_field_labels_without_cloning() {
    let mut labels: Vec<&'static str> = Vec::new();
    let lane = AgentLaneIdentity::new("lane", "actor", AgentLaneKind::Local,
        LocalCloudAttribution::local("runtime", "model"), |field, _value| {
            labels.push(field);
            Ok::<(), InjectedError>(())
        }).unwrap();
    assert_eq!(labels, vec!["lane_id", "actor_id"]);
    assert_eq!(lane.lane_id, "lane");
    assert_eq!(lane.actor_id, "actor");
}

#[test]
fn all_eight_lane_kinds_preserve_exact_ordered_capabilities() {
    use AgentCapability::*;
    for (kind, expected) in [
        (AgentLaneKind::Local, vec![ClaimWorktree, ClaimWorkspace, EditRichDocument, MutateGraph,
            RunQuietBackgroundWork, WriteLocalIndex, WriteMailbox, NavigateBackend, RecordCheckpoint]),
        (AgentLaneKind::Cloud, vec![ClaimWorkspace, RunQuietBackgroundWork, WriteMailbox, NavigateBackend, RecordCheckpoint]),
        (AgentLaneKind::Operator, vec![ClaimWorktree, ClaimWorkspace, EditRichDocument, MutateGraph,
            RunQuietBackgroundWork, WriteMailbox, NavigateBackend, RecordCheckpoint]),
        (AgentLaneKind::Validator, vec![InspectEvidence, NavigateBackend]),
        (AgentLaneKind::IntegrationValidator, vec![InspectEvidence, NavigateBackend]),
        (AgentLaneKind::Indexer, vec![ClaimWorkspace, RunQuietBackgroundWork, WriteLocalIndex, NavigateBackend, RecordCheckpoint]),
        (AgentLaneKind::Editor, vec![ClaimWorkspace, EditRichDocument, MutateGraph, RunQuietBackgroundWork, NavigateBackend, RecordCheckpoint]),
        (AgentLaneKind::System, vec![ClaimWorktree, ClaimWorkspace, EditRichDocument, MutateGraph,
            RunQuietBackgroundWork, WriteLocalIndex, WriteMailbox, NavigateBackend, RecordCheckpoint]),
    ] { assert_eq!(identity(kind).capabilities(), expected, "{kind:?}"); }
}

#[test]
fn capability_and_attribution_mode_wire_names_are_unchanged() {
    use AgentCapability::*;
    for (capability, wire) in [
        (ClaimWorktree, "claim_worktree"), (ClaimWorkspace, "claim_workspace"),
        (EditRichDocument, "edit_rich_document"), (InspectEvidence, "inspect_evidence"),
        (MutateGraph, "mutate_graph"), (RunQuietBackgroundWork, "run_quiet_background_work"),
        (WriteLocalIndex, "write_local_index"), (WriteMailbox, "write_mailbox"),
        (NavigateBackend, "navigate_backend"), (RecordCheckpoint, "record_checkpoint"),
    ] {
        assert_eq!(serde_json::to_value(capability).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<AgentCapability>(json!(wire)).unwrap(), capability);
    }
    for (mode, wire) in [(AttributionMode::Local, "local"), (AttributionMode::Cloud, "cloud"),
        (AttributionMode::Operator, "operator"), (AttributionMode::System, "system")] {
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<AttributionMode>(json!(wire)).unwrap(), mode);
    }
}

#[test]
fn local_attribution_preserves_original_fields() {
    let attribution = LocalCloudAttribution::local(" runtime ", " model ");
    assert_eq!(serde_json::to_value(attribution).unwrap(), json!({
        "mode": "local", "provider": "local_runtime", "runtime": " runtime ",
        "model_label": " model ", "credential_ref": null, "provider_metadata": {}
    }));
}

#[test]
fn cloud_constructor_calls_mandatory_scrubber_once_with_exact_metadata() {
    let metadata = json!({"nested": {"token": "input"}, "array": [1, null, true]});
    let untouched = metadata.clone();
    let calls = Cell::new(0);
    let token = Box::new("consumed-once");
    let attribution = LocalCloudAttribution::cloud(ModelProviderKind::Anthropic, " model ",
        " credential ", metadata.clone(), |input| {
            calls.set(calls.get() + 1);
            assert_eq!(input, untouched);
            let marker = *token;
            drop(token);
            json!({"external-policy": marker})
        });
    assert_eq!(calls.get(), 1);
    assert_eq!(metadata, untouched);
    assert_eq!(serde_json::to_value(attribution).unwrap(), json!({
        "mode": "cloud", "provider": "anthropic", "runtime": null, "model_label": " model ",
        "credential_ref": " credential ", "provider_metadata": {"external-policy": "consumed-once"}
    }));
}

#[test]
fn attribution_persistence_scrubs_one_metadata_clone_and_preserves_input() {
    let mut original = LocalCloudAttribution::local("runtime", "model");
    original.provider_metadata = json!({"raw": [1, 2]});
    let before = original.clone();
    let calls = Cell::new(0);
    let consumed_once = Box::new(1);
    let scrubbed = original.scrubbed_for_persistence(|mut input| {
        drop(consumed_once);
        calls.set(calls.get() + 1);
        assert_eq!(input, before.provider_metadata);
        input["raw"] = json!("external-policy");
        input
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(original, before);
    let mut expected = before;
    expected.provider_metadata = json!({"raw": "external-policy"});
    assert_eq!(scrubbed, expected);
}

#[test]
fn identity_persistence_scrubs_one_nested_metadata_clone_and_preserves_input() {
    let mut original = identity(AgentLaneKind::Cloud);
    original.attribution.provider_metadata = json!({"raw": [1, 2]});
    let before = original.clone();
    let calls = Cell::new(0);
    let consumed_once = Box::new(1);
    let scrubbed = original.scrubbed_for_persistence(|input| {
        drop(consumed_once);
        calls.set(calls.get() + 1);
        assert_eq!(input, before.attribution.provider_metadata);
        json!({"policy-result": "retained"})
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(original, before);
    let mut expected = before;
    expected.attribution.provider_metadata = json!({"policy-result": "retained"});
    assert_eq!(scrubbed, expected);
}

#[test]
fn identity_wire_roundtrip_and_attribution_metadata_default_preserve_serde_behavior() {
    let original = identity(AgentLaneKind::System);
    let wire = serde_json::to_value(&original).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 4);
    assert_eq!(wire["lane_kind"], "system");
    assert_eq!(serde_json::from_value::<AgentLaneIdentity>(wire).unwrap(), original);
    let missing = json!({"mode": "operator", "provider": null, "runtime": null,
        "model_label": "operator", "credential_ref": null});
    let attribution: LocalCloudAttribution = serde_json::from_value(missing).unwrap();
    assert_eq!(attribution.provider_metadata, serde_json::Value::Null);
}
