use serde::{Deserialize, Serialize};

pub const SURFACE_CONTRACT_ID: &str = "native_swarm_lane_diagnostics";
pub const PROJECTION_SCHEMA_ID: &str = "hsk.model_lane_diagnostics_projection@3";
pub const SURFACE_AUTHOR_ID: &str = "swarm-lane-diagnostics.surface";
pub const RUN_FILTER_AUTHOR_ID: &str = "swarm-lane-diagnostics.filter.run";
pub const LANE_FILTER_AUTHOR_ID: &str = "swarm-lane-diagnostics.filter.lane";
pub const MESSAGE_FILTER_AUTHOR_ID: &str = "swarm-lane-diagnostics.filter.message";
pub const REFRESH_AUTHOR_ID: &str = "swarm-lane-diagnostics.action.refresh";
pub const ERROR_AUTHOR_ID: &str = "swarm-lane-diagnostics.error";
pub const FRESHNESS_AUTHOR_ID: &str = "swarm-lane-diagnostics.freshness";
pub const PRIVACY_OWNER_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.owner-account";
pub const PRIVACY_PRINCIPAL_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.actor-principal";
pub const PRIVACY_SESSION_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.authenticated-session";
pub const PRIVACY_ACCESS_SPACE_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.access-space";
pub const PRIVACY_WORKSPACE_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.workspace";
pub const PRIVACY_VISIBILITY_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.visibility";
pub const PRIVACY_DENIAL_AUTHOR_ID: &str = "swarm-lane-diagnostics.privacy.denial-posture";
pub const EMPTY_MESSAGES_AUTHOR_ID: &str = "swarm-lane-diagnostics.messages.empty";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsProjection {
    pub schema_id: String,
    pub surface_contract_id: String,
    pub run: SwarmLaneDiagnosticsRun,
    pub lanes: Vec<SwarmLaneDiagnosticsLane>,
    pub messages: Vec<SwarmLaneDiagnosticsMessage>,
    pub diagnostic_tiers: Vec<SwarmLaneDiagnosticsTier>,
    pub mt_runtime_statuses: Vec<SwarmLaneDiagnosticsMtStatus>,
    pub routing_executions: Vec<SwarmLaneRoutingExecutionDiagnostics>,
    pub active_lease_count: usize,
    pub reclaimable_lease_ids: Vec<String>,
    pub orphan_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_scope: Option<SwarmLaneDiagnosticsResourceScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsResourceScope {
    pub owner_account_fingerprint: String,
    pub actor_principal_fingerprint: String,
    pub authenticated_session_fingerprint: String,
    pub access_space_fingerprint: String,
    pub workspace_fingerprint: String,
    pub visibility: String,
    pub denial_posture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsRun {
    pub run_id: String,
    pub trace_id: String,
    pub run_span_id: String,
    #[serde(default)]
    pub coordinator_session_id: String,
    #[serde(default)]
    pub routing_policy: String,
    #[serde(default)]
    pub artifact_namespace: String,
    pub projection_plan_ref: Option<String>,
    pub consent_receipt_ref: Option<String>,
    pub work_packet_id: Option<String>,
    pub micro_task_id: Option<String>,
    pub task_board_id: Option<String>,
    #[serde(default)]
    pub owner_session: String,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub flight_recorder_correlation_id: String,
    pub context_bundle_id: String,
    pub memory_pack_ref: String,
    pub memory_pack_hash: String,
    pub locus_ref: Option<String>,
    pub loom_ref: Option<String>,
    pub fems_ref: Option<String>,
    pub status: String,
    pub recovery_hint_ref: Option<String>,
    pub selected_model_id: Option<String>,
    pub candidate_model_ids: Vec<String>,
    #[serde(default)]
    pub budget_summary_ref: String,
    #[serde(default)]
    pub determinism_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsLane {
    pub lane_id: String,
    pub kind: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub backend: String,
    pub status: String,
    pub recovery_state: String,
    pub model_id: Option<String>,
    pub model_display_name: String,
    pub model_stable_anchor: Option<String>,
    pub model_anchor_unavailable_reason: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub model_session_id: String,
    #[serde(default)]
    pub adapter_id: String,
    pub provider_kind: String,
    pub runtime_binding: String,
    #[serde(default)]
    pub launch_authority: String,
    pub capability_token_ids: Vec<String>,
    pub effective_capability_snapshot_ref: Option<String>,
    pub capability_negotiation_ref: Option<String>,
    pub provider_feature_profile_ref: Option<String>,
    pub requested_execution_policy_ref: Option<String>,
    pub effective_execution_policy_ref: Option<String>,
    pub projection_plan_ref: Option<String>,
    pub consent_receipt_ref: Option<String>,
    pub tool_gate_decision_refs: Vec<String>,
    pub trace_id: String,
    pub lane_span_id: String,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub flight_recorder_correlation_id: String,
    pub last_activity_utc: Option<String>,
    pub message_count: usize,
    pub payload_error_count: usize,
    pub orphan_state: String,
    pub cancellation_ref: Option<String>,
    pub reclaim_policy_ref: Option<String>,
    pub terminal_status_mapping_ref: Option<String>,
    pub process_ownership_ref: Option<String>,
    pub no_os_process_reason_ref: Option<String>,
    pub last_runtime_status_ref: Option<String>,
    pub last_recovery_event_ref: Option<String>,
    pub failstate_code: Option<String>,
    pub startup_failure_ref: Option<String>,
    pub reason_ref: Option<String>,
    pub recovery_hint_ref: Option<String>,
    pub work_packet_id: Option<String>,
    pub micro_task_id: Option<String>,
    pub task_board_id: Option<String>,
    #[serde(default)]
    pub owner_session: String,
    pub locus_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsMessage {
    pub message_id: String,
    pub from_lane_id: String,
    #[serde(default)]
    pub to_lane: String,
    pub routing_target_role: Option<String>,
    pub routing_target_session: Option<String>,
    pub routing_correlation_id: Option<String>,
    #[serde(default)]
    pub routing_requires_ack: bool,
    pub routing_ack_for: Option<String>,
    pub kind: String,
    pub authority: String,
    pub promotion_state: String,
    pub payload_ref: String,
    pub payload_sha256: String,
    pub artifact_ref: Option<String>,
    pub promotion_decision_id: Option<String>,
    pub promotion_gate_ref: Option<String>,
    pub promotion_receipt_ref: Option<String>,
    pub validator_verdict_ref: Option<String>,
    pub operator_decision_ref: Option<String>,
    pub promoted_artifact_sha256: Option<String>,
    pub promoted_artifact_version: Option<String>,
    #[serde(default)]
    pub tool_gate_decision_refs: Vec<String>,
    #[serde(default)]
    pub coordinator_session_id: String,
    pub work_packet_id: Option<String>,
    pub micro_task_id: Option<String>,
    pub task_board_id: Option<String>,
    #[serde(default)]
    pub owner_session: String,
    pub trace_id: String,
    pub message_span_id: String,
    pub parent_span_id: Option<String>,
    pub linked_span_contexts: Vec<String>,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub flight_recorder_correlation_id: String,
    pub locus_ref: Option<String>,
    pub loom_ref: Option<String>,
    pub fems_ref: Option<String>,
    pub proposal_ref: Option<String>,
    pub crdt_update_ref: Option<String>,
    pub crdt_base_snapshot_ref: Option<String>,
    pub crdt_state_vector: Option<String>,
    pub crdt_proposal_ref: Option<String>,
    pub crdt_stale_base_ref: Option<String>,
    pub payload_error: Option<String>,
    pub reason_ref: Option<String>,
    pub recovery_hint_ref: Option<String>,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsTier {
    pub tier: String,
    pub state: String,
    pub reason: String,
    pub evidence_ref: String,
    pub follow_up_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneDiagnosticsMtStatus {
    pub micro_task_id: String,
    pub status: String,
    pub proof_status_ref: Option<String>,
    pub hbr_status_ref: Option<String>,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneRoutingOutboxDiagnostics {
    pub command_id: String,
    pub status: String,
    pub fencing_token: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at_unix_ms: Option<u64>,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneRoutingStageDiagnostics {
    pub execution_id: String,
    pub stage_id: String,
    pub state: String,
    pub attempt: u32,
    pub dispatch_target: String,
    pub dependency_stage_ids: Vec<String>,
    pub expected_run_id: String,
    pub expected_lane_id: String,
    pub expected_model_id: String,
    pub expected_provider: Option<String>,
    pub instance_id: Option<String>,
    pub lane_id: Option<String>,
    pub input_refs: Vec<String>,
    pub output_ref: Option<String>,
    pub output_message_ref: Option<String>,
    pub authority_request_message_ref: Option<String>,
    pub output_sha256: Option<String>,
    pub authority_ref: Option<String>,
    pub lease_owner: Option<String>,
    pub fencing_token: Option<String>,
    pub lease_expires_at_unix_ms: Option<u64>,
    pub lease_expired: bool,
    pub detail: Option<String>,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub updated_at_unix_ms: u64,
    pub outbox: SwarmLaneRoutingOutboxDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmLaneRoutingExecutionDiagnostics {
    pub execution_id: String,
    pub run_id: String,
    pub selecting_decision_id: String,
    pub selecting_decision_event_id: String,
    pub selecting_decision_event_seq: i64,
    pub trace_id: String,
    pub run_span_id: String,
    pub coordinator_session_id: String,
    pub locus_ref: String,
    pub work_packet_id: String,
    pub micro_task_id: Option<String>,
    pub task_board_id: String,
    pub owner_session: String,
    pub canonical_graph_sha256: String,
    pub canonical_launch_plan_sha256: String,
    pub cloud_consent_receipt_ref: Option<String>,
    pub validator_authority_ref: Option<String>,
    pub operator_authority_ref: Option<String>,
    pub initial_input_ref: Option<String>,
    pub initial_input_sha256: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub cancel_reason: Option<String>,
    pub revision: u64,
    pub stages: Vec<SwarmLaneRoutingStageDiagnostics>,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
}

#[cfg(test)]
pub(crate) fn test_projection() -> SwarmLaneDiagnosticsProjection {
    // Synthetic pure unit data, not a backend projection or authentication evidence.
    SwarmLaneDiagnosticsProjection {
        schema_id: PROJECTION_SCHEMA_ID.into(),
        surface_contract_id: SURFACE_CONTRACT_ID.into(),
        run: SwarmLaneDiagnosticsRun {
            run_id: "unit-run".into(),
            trace_id: "unit-trace".into(),
            run_span_id: "unit-run-span".into(),
            coordinator_session_id: "unit-coordinator".into(),
            routing_policy: "unit-routing".into(),
            artifact_namespace: "unit-artifacts".into(),
            projection_plan_ref: Some("unit-projection-plan".into()),
            consent_receipt_ref: Some("unit-consent".into()),
            work_packet_id: Some("unit-wp".into()),
            micro_task_id: Some("unit-mt".into()),
            task_board_id: Some("unit-board".into()),
            owner_session: "unit-owner".into(),
            event_ledger_event_id: "unit-event".into(),
            event_ledger_seq: 1,
            flight_recorder_correlation_id: "unit-recorder".into(),
            context_bundle_id: "unit-context".into(),
            memory_pack_ref: "unit-memory".into(),
            memory_pack_hash: "b".repeat(64),
            locus_ref: Some("unit-locus".into()),
            loom_ref: Some("unit-loom".into()),
            fems_ref: Some("unit-fems".into()),
            status: "running".into(),
            recovery_hint_ref: Some("unit-recovery".into()),
            selected_model_id: Some("unit-model".into()),
            candidate_model_ids: vec!["unit-model".into()],
            budget_summary_ref: "unit-budget".into(),
            determinism_mode: "unit-determinism".into(),
        },
        lanes: vec![],
        messages: vec![],
        diagnostic_tiers: ["flight_recorder", "internal_diagnostics", "palmistry"]
            .into_iter()
            .map(|tier| SwarmLaneDiagnosticsTier {
                tier: tier.into(),
                state: "wired".into(),
                reason: "unit-reason".into(),
                evidence_ref: "unit-evidence".into(),
                follow_up_ref: None,
            })
            .collect(),
        mt_runtime_statuses: vec![SwarmLaneDiagnosticsMtStatus {
            micro_task_id: "unit-mt".into(),
            status: "pending".into(),
            proof_status_ref: Some("unit-proof".into()),
            hbr_status_ref: Some("unit-hbr".into()),
            event_ledger_event_id: "unit-mt-event".into(),
            event_ledger_seq: 1,
        }],
        routing_executions: vec![],
        active_lease_count: 0,
        reclaimable_lease_ids: vec![],
        orphan_state: "none".into(),
        resource_scope: Some(SwarmLaneDiagnosticsResourceScope {
            owner_account_fingerprint: "a".repeat(64),
            actor_principal_fingerprint: "a".repeat(64),
            authenticated_session_fingerprint: "a".repeat(64),
            access_space_fingerprint: "a".repeat(64),
            workspace_fingerprint: "a".repeat(64),
            visibility: "private_exact_scope_only".into(),
            denial_posture: "foreign_scope_is_absent_restricted_metadata_withheld".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn projection_dto_preserves_complete_json_round_trip() {
        let projection = super::test_projection();
        let wire = serde_json::to_value(&projection).expect("serialize pure unit projection");
        let decoded: super::SwarmLaneDiagnosticsProjection =
            serde_json::from_value(wire.clone()).expect("deserialize pure unit projection");
        assert_eq!(decoded, projection);
        assert_eq!(serde_json::to_value(decoded).expect("reserialize DTO"), wire);
    }
}
