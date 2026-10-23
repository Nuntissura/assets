use serde_json::{json, Value};

use crate::{validate_swarm_dashboard_projection, ParallelSwarmDashboardProjectionV1};

fn projection() -> ParallelSwarmDashboardProjectionV1 {
    serde_json::from_value(json!({
        "schema_id": "hsk.parallel_swarm.dashboard_projection@1",
        "workspace_id": "workspace",
        "generated_at_utc": "2026-10-18T12:34:56.123456789Z",
        "filters": {"workspace_id": "workspace", "wp_id": null, "mt_id": null, "limit": 1},
        "projection_contract": {
            "projection_only": true, "authority_mutation_allowed": false,
            "ui_state_authoritative": false,
            "source_component": "parallel_swarm_state_recovery",
            "source_tables": ["knowledge_agent_worktree_claims", "knowledge_agent_role_mailbox_handoffs",
                "knowledge_agent_state_recovery_checkpoints", "knowledge_agent_recovery_receipts",
                "knowledge_parallel_indexing_lease_queue", "knowledge_agent_quiet_background_work", "kernel_event_ledger"],
            "source_event_aggregates": ["parallel_swarm_claim", "parallel_swarm_claim_reclaim",
                "parallel_swarm_handoff", "parallel_swarm_checkpoint", "parallel_swarm_recovery",
                "parallel_indexing_lease", "parallel_swarm_quiet_background_work"]
        },
        "source_watermark": {"source_component": "parallel_swarm_state_recovery", "event_count": 0,
            "max_event_created_at_utc": null, "events": [], "aggregate_counts": [], "missing_event_refs": []},
        "totals": {"claims": 0, "active_claims": 0, "stale_active_claims": 0,
            "mailbox_handoffs": 0, "recovery_checkpoints": 0, "recovery_receipts": 0,
            "indexing_leases": 0, "acquired_indexing_leases": 0, "quiet_background_work": 0,
            "events": 0, "warnings": 0, "claims_by_status": {}, "handoffs_by_status": {},
            "leases_by_status": {}, "quiet_work_by_kind": {}},
        "lanes": [], "claims": [], "mailbox_handoffs": [], "recovery_checkpoints": [],
        "recovery_receipts": [], "indexing_leases": [], "quiet_background_work": [], "warnings": []
    })).unwrap()
}

fn claim_projection() -> ParallelSwarmDashboardProjectionV1 {
    let mut projection = projection();
    projection.claims.push(serde_json::from_value(json!({
        "claim_id": "claim", "wp_id": "WP-1", "mt_id": "MT-007", "scope_kind": "worktree",
        "scope_id": "scope", "lane_id": "lane", "actor_id": "actor", "lane_kind": "local",
        "status": "active", "reason": "test", "claimed_at_utc": "2026-10-18T12:00:00Z",
        "expires_at_utc": "2026-10-18T13:00:00Z", "released_at_utc": null, "stale": false,
        "source_refs": [{"table_name": "knowledge_agent_worktree_claims", "row_id": "claim",
            "row_source_ref": "surreal://knowledge_agent_worktree_claims/claim",
            "event_ledger_event_id": "KE-claim", "event_source_ref": "event-ledger://KE-claim",
            "event_aggregate_type": "parallel_swarm_claim", "event_aggregate_id": "claim"}]
    })).unwrap());
    projection.source_watermark.events.push(serde_json::from_value(json!({
        "event_id": "KE-claim", "source_component": "parallel_swarm_state_recovery",
        "aggregate_type": "parallel_swarm_claim", "aggregate_id": "claim",
        "created_at_utc": "2026-10-18T12:00:00Z"
    })).unwrap());
    projection.source_watermark.aggregate_counts.push(serde_json::from_value(json!({
        "aggregate_type": "parallel_swarm_claim", "count": 1
    })).unwrap());
    projection.source_watermark.event_count = 1;
    projection.totals.claims = 1;
    projection.totals.events = 1;
    projection
}

#[test]
fn dashboard_projection_wire_contract_roundtrips_without_losing_source_refs() {
    let projection = claim_projection();
    assert_eq!(validate_swarm_dashboard_projection(&projection), Ok(()));
    let wire = serde_json::to_value(&projection).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 15);
    assert_eq!(wire["generated_at_utc"], "2026-10-18T12:34:56.123456789Z");
    assert_eq!(wire["claims"][0]["source_refs"][0]["event_source_ref"], "event-ledger://KE-claim");
    assert_eq!(serde_json::from_value::<ParallelSwarmDashboardProjectionV1>(wire).unwrap(), projection);
}

#[test]
fn projection_contract_failures_accumulate_in_existing_order() {
    let mut projection = projection();
    projection.schema_id.clear();
    let contract = &mut projection.projection_contract;
    contract.projection_only = false;
    contract.authority_mutation_allowed = true;
    contract.ui_state_authoritative = true;
    contract.source_component.clear();
    contract.source_tables.clear();
    contract.source_event_aggregates.clear();
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "schema_id must be hsk.parallel_swarm.dashboard_projection@1",
        "projection contract must set projection_only=true",
        "projection contract must set authority_mutation_allowed=false",
        "projection contract must set ui_state_authoritative=false",
        "projection source_component must be parallel_swarm_state_recovery",
        "projection contract missing source table knowledge_agent_worktree_claims",
        "projection contract missing source table knowledge_agent_role_mailbox_handoffs",
        "projection contract missing source table knowledge_agent_state_recovery_checkpoints",
        "projection contract missing source table knowledge_agent_recovery_receipts",
        "projection contract missing source table knowledge_parallel_indexing_lease_queue",
        "projection contract missing source table knowledge_agent_quiet_background_work",
        "projection contract missing source table kernel_event_ledger",
        "projection contract missing source event aggregate parallel_swarm_claim",
        "projection contract missing source event aggregate parallel_swarm_claim_reclaim",
        "projection contract missing source event aggregate parallel_swarm_handoff",
        "projection contract missing source event aggregate parallel_swarm_checkpoint",
        "projection contract missing source event aggregate parallel_swarm_recovery",
        "projection contract missing source event aggregate parallel_indexing_lease",
        "projection contract missing source event aggregate parallel_swarm_quiet_background_work",
    ]);
}

#[test]
fn limited_rows_do_not_redefine_canonical_totals() {
    let mut projection = claim_projection();
    projection.totals.claims = 2;
    assert_eq!(projection.claims.len(), 1);
    assert_eq!(validate_swarm_dashboard_projection(&projection), Ok(()));
}

#[test]
fn warnings_and_watermark_counts_are_checked_before_source_rows() {
    let mut projection = projection();
    projection.totals.warnings = 1;
    projection.source_watermark.event_count = 1;
    projection.source_watermark.missing_event_refs.push("KE-missing".into());
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "totals.warnings must match warnings length",
        "source_watermark.event_count must match source_watermark.events length",
        "source_watermark.missing_event_refs must be empty for a valid projection",
    ]);
}

#[test]
fn duplicate_watermark_events_and_aggregate_counts_are_not_accepted() {
    let mut projection = claim_projection();
    let event = projection.source_watermark.events[0].clone();
    projection.source_watermark.events.push(event);
    projection.source_watermark.event_count = 2;
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "source_watermark.events contains duplicate event KE-claim",
        "source_watermark.aggregate_counts must match source_watermark.events",
    ]);
}

#[test]
fn watermark_component_and_row_aggregate_must_match() {
    let mut projection = claim_projection();
    projection.source_watermark.events[0].source_component = "other".into();
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "watermark event KE-claim has invalid source_component",
        "claim claim EventLedger watermark aggregate mismatch",
    ]);
}

#[test]
fn source_ref_failures_preserve_table_row_uri_and_event_error_order() {
    let mut projection = claim_projection();
    let source = &mut projection.claims[0].source_refs[0];
    source.table_name.clear();
    source.row_id.clear();
    source.row_source_ref.clear();
    source.event_source_ref = None;
    source.event_aggregate_type = Some("other".into());
    source.event_aggregate_id = Some("other".into());
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "claim claim has empty source table",
        "claim claim source table must be knowledge_agent_worktree_claims",
        "claim claim has mismatched source row_id",
        "claim claim has mismatched row source ref",
        "claim claim has invalid row source ref",
        "claim claim has invalid event source ref",
        "claim claim has invalid event aggregate_type",
        "claim claim has mismatched event aggregate_id",
        "claim claim EventLedger watermark aggregate mismatch",
    ]);
}

#[test]
fn row_event_refs_cannot_dangle_or_omit_aggregate_identity() {
    let mut projection = claim_projection();
    projection.claims[0].source_refs[0].event_ledger_event_id = None;
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(), vec![
        "claim claim has no EventLedger source ref",
        "claim claim has dangling event source ref",
        "claim claim has aggregate identity without EventLedger ref",
    ]);
    let mut projection = claim_projection();
    projection.claims[0].source_refs[0].event_aggregate_id = None;
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(),
        vec!["claim claim missing EventLedger aggregate identity"]);
}

#[test]
fn source_event_must_be_present_in_the_declared_watermark() {
    let mut projection = claim_projection();
    projection.source_watermark.events.clear();
    projection.source_watermark.aggregate_counts.clear();
    projection.source_watermark.event_count = 0;
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(),
        vec!["claim claim EventLedger ref missing from watermark"]);
}

#[test]
fn missing_source_refs_produce_row_error_without_spurious_followups() {
    let mut projection = claim_projection();
    projection.claims[0].source_refs.clear();
    assert_eq!(validate_swarm_dashboard_projection(&projection).unwrap_err(),
        vec!["claim claim has no source_refs"]);
}

#[test]
fn claim_reclaim_aggregate_is_a_supported_existing_source() {
    let mut projection = claim_projection();
    projection.claims[0].source_refs[0].event_aggregate_type = Some("parallel_swarm_claim_reclaim".into());
    projection.source_watermark.events[0].aggregate_type = "parallel_swarm_claim_reclaim".into();
    projection.source_watermark.aggregate_counts[0].aggregate_type = "parallel_swarm_claim_reclaim".into();
    assert_eq!(validate_swarm_dashboard_projection(&projection), Ok(()));
}

#[test]
fn source_ref_wire_preserves_optional_event_identity_as_null() {
    let source: crate::SwarmDashboardSourceRefV1 = serde_json::from_value(json!({
        "table_name": "table", "row_id": "row", "row_source_ref": "surreal://table/row",
        "event_ledger_event_id": null, "event_source_ref": null,
        "event_aggregate_type": null, "event_aggregate_id": null
    })).unwrap();
    let wire: Value = serde_json::to_value(source).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 7);
    assert!(wire["event_ledger_event_id"].is_null());
    assert!(wire["event_source_ref"].is_null());
    assert!(wire["event_aggregate_type"].is_null());
    assert!(wire["event_aggregate_id"].is_null());
}
