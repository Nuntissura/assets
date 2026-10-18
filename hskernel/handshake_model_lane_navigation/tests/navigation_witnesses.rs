use handshake_model_lane_navigation::*;
use serde_json::{json, Value};

fn scalar(id: &str, seq: i64) -> ModelLaneNavigationScalar {
    ModelLaneNavigationScalar {
        aggregate_id: id.into(), run_id: "run-1".into(),
        event_ledger_event_id: format!("KE-{id}"), event_ledger_seq: seq,
        ordering_seq: Some(seq),
    }
}

fn source(kind: &str, id: &str) -> ModelLaneNavigationSource {
    (kind.to_owned(), scalar(id, 1), None).into()
}

fn assemble(rows: Vec<ModelLaneNavigationSource>) -> ModelLaneNavigationProjection {
    assemble_navigation_projection("model_lane.navigation.run", "run", "run-1", None, "run-1", rows)
        .expect("component projection")
}

#[test]
fn legacy_tuple_assembly_preserves_scalar_wire_and_omits_witnesses() {
    let projection = assemble_navigation_projection(
        "model_lane.navigation.run", "run", "run-1", None, "run-1",
        vec![("run".to_owned(), scalar("run-1", 1), None::<String>),
             ("context_artifact".to_owned(), scalar("artifact-1", 2), None),
             ("mt_runtime_status".to_owned(), scalar("status-1", 3), None)],
    ).unwrap();
    let wire = serde_json::to_value(projection).unwrap();
    assert_eq!(wire["schema_id"], "hsk.model_lane_navigation@2");
    assert!(wire["run"].get("memory_pack").is_none());
    assert_eq!(wire["artifacts"][0], serde_json::to_value(scalar("artifact-1", 2)).unwrap());
    assert_eq!(wire["mt_runtime_statuses"][0], serde_json::to_value(scalar("status-1", 3)).unwrap());
    assert!(wire["flight_recorder_refs"].as_array().unwrap().is_empty());
}

#[test]
fn optional_witnesses_decode_missing_and_null_and_serialize_absence() {
    for explicit_null in [false, true] {
        let mut run = json!({"run_id":"run-1", "source":scalar("run-1", 1)});
        let mut artifact = serde_json::to_value(scalar("artifact-1", 2)).unwrap();
        let mut status = serde_json::to_value(scalar("status-1", 3)).unwrap();
        if explicit_null {
            run["memory_pack"] = Value::Null;
            artifact["aliases"] = Value::Null;
            status["micro_task_id"] = Value::Null;
        }
        let run: ModelLaneNavigationRun = serde_json::from_value(run).unwrap();
        let artifact: ModelLaneNavigationArtifact = serde_json::from_value(artifact).unwrap();
        let status: ModelLaneNavigationMtRuntimeStatus = serde_json::from_value(status).unwrap();
        assert!(run.memory_pack.is_none());
        assert!(artifact.aliases.is_none());
        assert!(status.micro_task_id.is_none());
        assert!(serde_json::to_value(run).unwrap().get("memory_pack").is_none());
        assert!(serde_json::to_value(artifact).unwrap().get("aliases").is_none());
        assert!(serde_json::to_value(status).unwrap().get("micro_task_id").is_none());
    }
}

#[test]
fn assembly_retains_exact_host_supplied_witness_values() {
    let hash = "ABCDEF0123456789".repeat(4);
    let memory = ModelLaneNavigationMemoryPack { reference: "memory-pack://Case/Run".into(), hash: hash.clone() };
    let aliases = ModelLaneNavigationArtifactAliases {
        artifact_ref: "artifact://Case/Payload".into(), artifact_binding_id: "Binding-1".into(),
        artifact_manifest_ref: "manifest://Case/Manifest".into(), artifact_payload_ref: "artifact://Case/Payload".into(),
        artifact_sha256: hash.clone(), content_hash: hash,
    };
    let mut run = source("run", "run-1"); run.memory_pack = Some(memory.clone());
    let mut artifact = source("context_artifact", "Binding-1"); artifact.artifact_aliases = Some(aliases.clone());
    let mut status = source("mt_runtime_status", "status-1"); status.micro_task_id = Some("MT-Case".into());
    let projection = assemble(vec![run, artifact, status]);
    assert_eq!(projection.run.as_ref().unwrap().memory_pack.as_ref(), Some(&memory));
    assert_eq!(projection.artifacts[0].aliases.as_ref(), Some(&aliases));
    assert_eq!(projection.mt_runtime_statuses[0].micro_task_id.as_deref(), Some("MT-Case"));
    let wire = serde_json::to_value(&projection).unwrap();
    assert_eq!(wire["run"]["memory_pack"], serde_json::to_value(memory).unwrap());
    assert_eq!(wire["artifacts"][0]["aliases"], serde_json::to_value(aliases).unwrap());
    assert!(wire["artifacts"][0].get("source").is_none());
    assert_eq!(serde_json::from_value::<ModelLaneNavigationProjection>(wire).unwrap(), projection);
}

#[test]
fn flattened_scalar_duplicate_fields_are_rejected() {
    let duplicate = r#"{"aggregate_id":"a","aggregate_id":"forged","run_id":"run-1","event_ledger_event_id":"KE-a","event_ledger_seq":1,"ordering_seq":1}"#;
    assert!(serde_json::from_str::<ModelLaneNavigationArtifact>(duplicate).is_err());
    assert!(serde_json::from_str::<ModelLaneNavigationMtRuntimeStatus>(duplicate).is_err());
}

#[test]
fn witnesses_cannot_be_assigned_to_another_record_kind() {
    let mut memory = source("lane", "lane-1");
    memory.memory_pack = Some(ModelLaneNavigationMemoryPack { reference:"memory-pack://one".into(), hash:"a".repeat(64) });
    let mut aliases = source("context_handoff", "handoff-1");
    aliases.artifact_aliases = Some(ModelLaneNavigationArtifactAliases {
        artifact_ref:"a".into(), artifact_binding_id:"b".into(), artifact_manifest_ref:"m".into(),
        artifact_payload_ref:"p".into(), artifact_sha256:"a".repeat(64), content_hash:"a".repeat(64),
    });
    let mut status = source("diagnostic_tier", "tier-1"); status.micro_task_id = Some("MT-1".into());
    for wrong in [memory, aliases, status] {
        let result = assemble_navigation_projection("route", "run", "run-1", None, "run-1", vec![source("run", "run-1"), wrong]);
        assert!(matches!(result, Err(ModelLaneNavigationError::InvalidInput(_))));
    }
}

#[test]
fn assembly_sorts_and_deduplicates_source_evidence_and_error_codes() {
    let mut lane = source("lane", "lane-1"); lane.error_code = Some("Z-CODE".into());
    let mut recovery = source("recovery_event", "recovery-1"); recovery.error_code = Some("A-CODE".into());
    let projection = assemble(vec![source("run", "run-1"), lane.clone(), recovery, lane]);
    assert_eq!(projection.error_codes, vec!["A-CODE", "Z-CODE"]);
    assert_eq!(projection.event_ledger_refs, vec![
        "eventledger://kernel/KE-lane-1", "eventledger://kernel/KE-recovery-1",
        "eventledger://kernel/KE-run-1", "eventledger://kernel/seq/1",
    ]);
    assert!(projection.flight_recorder_refs.is_empty());
}