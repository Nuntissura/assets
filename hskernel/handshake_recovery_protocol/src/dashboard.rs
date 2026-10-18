use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::RecoveryResumePointer;

pub const PARALLEL_SWARM_DASHBOARD_SCHEMA_ID: &str = "hsk.parallel_swarm.dashboard_projection@1";

const PARALLEL_SWARM_SOURCE_COMPONENT: &str = "parallel_swarm_state_recovery";

const PARALLEL_SWARM_DASHBOARD_SOURCE_TABLES: &[&str] = &[
    "knowledge_agent_worktree_claims",
    "knowledge_agent_role_mailbox_handoffs",
    "knowledge_agent_state_recovery_checkpoints",
    "knowledge_agent_recovery_receipts",
    "knowledge_parallel_indexing_lease_queue",
    "knowledge_agent_quiet_background_work",
    "kernel_event_ledger",
];

const PARALLEL_SWARM_DASHBOARD_EVENT_AGGREGATES: &[&str] = &[
    "parallel_swarm_claim",
    "parallel_swarm_claim_reclaim",
    "parallel_swarm_handoff",
    "parallel_swarm_checkpoint",
    "parallel_swarm_recovery",
    "parallel_indexing_lease",
    "parallel_swarm_quiet_background_work",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelSwarmDashboardProjectionV1 {
    pub schema_id: String,
    pub workspace_id: String,
    pub generated_at_utc: DateTime<Utc>,
    pub filters: SwarmDashboardProjectionFilters,
    pub projection_contract: SwarmDashboardProjectionContractV1,
    pub source_watermark: SwarmDashboardSourceWatermarkV1,
    pub totals: SwarmDashboardTotalsV1,
    pub lanes: Vec<SwarmDashboardLaneRowV1>,
    pub claims: Vec<SwarmDashboardClaimRowV1>,
    pub mailbox_handoffs: Vec<SwarmDashboardHandoffRowV1>,
    pub recovery_checkpoints: Vec<SwarmDashboardCheckpointRowV1>,
    pub recovery_receipts: Vec<SwarmDashboardRecoveryReceiptRowV1>,
    pub indexing_leases: Vec<SwarmDashboardIndexingLeaseRowV1>,
    pub quiet_background_work: Vec<SwarmDashboardQuietWorkRowV1>,
    pub warnings: Vec<SwarmDashboardWarningV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardProjectionFilters {
    pub workspace_id: String,
    pub wp_id: Option<String>,
    pub mt_id: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardProjectionContractV1 {
    pub projection_only: bool,
    pub authority_mutation_allowed: bool,
    pub ui_state_authoritative: bool,
    pub source_component: String,
    pub source_tables: Vec<String>,
    pub source_event_aggregates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardSourceWatermarkV1 {
    pub source_component: String,
    pub event_count: i64,
    pub max_event_created_at_utc: Option<DateTime<Utc>>,
    pub events: Vec<SwarmDashboardEventRefV1>,
    pub aggregate_counts: Vec<SwarmDashboardAggregateCountV1>,
    pub missing_event_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardEventRefV1 {
    pub event_id: String,
    pub source_component: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub created_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardAggregateCountV1 {
    pub aggregate_type: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardTotalsV1 {
    pub claims: i64,
    pub active_claims: i64,
    pub stale_active_claims: i64,
    pub mailbox_handoffs: i64,
    pub recovery_checkpoints: i64,
    pub recovery_receipts: i64,
    pub indexing_leases: i64,
    pub acquired_indexing_leases: i64,
    pub quiet_background_work: i64,
    pub events: i64,
    pub warnings: i64,
    pub claims_by_status: BTreeMap<String, i64>,
    pub handoffs_by_status: BTreeMap<String, i64>,
    pub leases_by_status: BTreeMap<String, i64>,
    pub quiet_work_by_kind: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardLaneRowV1 {
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: String,
    pub attribution_mode: String,
    pub total_rows: i64,
    pub active_claims: i64,
    pub handoffs: i64,
    pub checkpoints: i64,
    pub recovery_receipts: i64,
    pub indexing_leases: i64,
    pub quiet_background_work: i64,
    pub source_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardSourceRefV1 {
    pub table_name: String,
    pub row_id: String,
    pub row_source_ref: String,
    pub event_ledger_event_id: Option<String>,
    pub event_source_ref: Option<String>,
    pub event_aggregate_type: Option<String>,
    pub event_aggregate_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardClaimRowV1 {
    pub claim_id: String,
    pub wp_id: String,
    pub mt_id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: String,
    pub status: String,
    pub reason: String,
    pub claimed_at_utc: DateTime<Utc>,
    pub expires_at_utc: DateTime<Utc>,
    pub released_at_utc: Option<DateTime<Utc>>,
    pub stale: bool,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardHandoffRowV1 {
    pub handoff_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub claim_id: Option<String>,
    pub from_lane_id: String,
    pub from_actor_id: String,
    pub from_lane_kind: String,
    pub to_role: String,
    pub mailbox_thread_id: String,
    pub mailbox_message_id: String,
    pub status: String,
    pub summary: String,
    pub created_at_utc: DateTime<Utc>,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardCheckpointRowV1 {
    pub checkpoint_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub session_id: String,
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: String,
    pub claim_id: Option<String>,
    pub mailbox_handoff_id: Option<String>,
    pub navigation_command_id: Option<String>,
    pub resume_pointer: RecoveryResumePointer,
    pub payload_sha256: String,
    pub compaction_reason: String,
    pub git_head: String,
    pub created_at_utc: DateTime<Utc>,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardRecoveryReceiptRowV1 {
    pub receipt_id: String,
    pub checkpoint_id: String,
    pub prior_session_id: String,
    pub new_session_id: String,
    pub new_lane_id: String,
    pub new_actor_id: String,
    pub new_lane_kind: String,
    pub resume_pointer: RecoveryResumePointer,
    pub recovered_at_utc: DateTime<Utc>,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardIndexingLeaseRowV1 {
    pub lease_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: String,
    pub session_id: String,
    pub index_run_id: String,
    pub status: String,
    pub blocked_by_lease_id: Option<String>,
    pub quiet_policy_ok: bool,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDashboardQuietWorkRowV1 {
    pub receipt_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub work_kind: String,
    pub subject_id: String,
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: String,
    pub session_id: String,
    pub evidence_ref: String,
    pub quiet_policy_ok: bool,
    pub created_at_utc: DateTime<Utc>,
    pub source_refs: Vec<SwarmDashboardSourceRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmDashboardWarningV1 {
    pub code: String,
    pub detail: String,
}

pub fn validate_swarm_dashboard_projection(
    projection: &ParallelSwarmDashboardProjectionV1,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if projection.schema_id != PARALLEL_SWARM_DASHBOARD_SCHEMA_ID {
        errors.push("schema_id must be hsk.parallel_swarm.dashboard_projection@1".to_string());
    }
    let contract = &projection.projection_contract;
    if !contract.projection_only {
        errors.push("projection contract must set projection_only=true".to_string());
    }
    if contract.authority_mutation_allowed {
        errors.push("projection contract must set authority_mutation_allowed=false".to_string());
    }
    if contract.ui_state_authoritative {
        errors.push("projection contract must set ui_state_authoritative=false".to_string());
    }
    if contract.source_component != PARALLEL_SWARM_SOURCE_COMPONENT {
        errors
            .push("projection source_component must be parallel_swarm_state_recovery".to_string());
    }
    for expected in PARALLEL_SWARM_DASHBOARD_SOURCE_TABLES {
        if !contract.source_tables.iter().any(|table| table == expected) {
            errors.push(format!(
                "projection contract missing source table {expected}"
            ));
        }
    }
    for expected in PARALLEL_SWARM_DASHBOARD_EVENT_AGGREGATES {
        if !contract
            .source_event_aggregates
            .iter()
            .any(|aggregate| aggregate == expected)
        {
            errors.push(format!(
                "projection contract missing source event aggregate {expected}"
            ));
        }
    }
    if projection.totals.warnings != projection.warnings.len() as i64 {
        errors.push("totals.warnings must match warnings length".to_string());
    }
    if projection.source_watermark.event_count != projection.source_watermark.events.len() as i64 {
        errors.push(
            "source_watermark.event_count must match source_watermark.events length".to_string(),
        );
    }
    if !projection.source_watermark.missing_event_refs.is_empty() {
        errors.push(
            "source_watermark.missing_event_refs must be empty for a valid projection".to_string(),
        );
    }
    let mut watermark_events = BTreeMap::<String, &SwarmDashboardEventRefV1>::new();
    let mut aggregate_counts = BTreeMap::<String, i64>::new();
    for event in &projection.source_watermark.events {
        if event.source_component != PARALLEL_SWARM_SOURCE_COMPONENT {
            errors.push(format!(
                "watermark event {} has invalid source_component",
                event.event_id
            ));
        }
        if watermark_events
            .insert(event.event_id.clone(), event)
            .is_some()
        {
            errors.push(format!(
                "source_watermark.events contains duplicate event {}",
                event.event_id
            ));
        }
        *aggregate_counts
            .entry(event.aggregate_type.clone())
            .or_insert(0) += 1;
    }
    let declared_aggregate_counts = projection
        .source_watermark
        .aggregate_counts
        .iter()
        .map(|row| (row.aggregate_type.clone(), row.count))
        .collect::<BTreeMap<_, _>>();
    if aggregate_counts != declared_aggregate_counts {
        errors.push(
            "source_watermark.aggregate_counts must match source_watermark.events".to_string(),
        );
    }

    validate_source_refs(
        &mut errors,
        "claim",
        "knowledge_agent_worktree_claims",
        &["parallel_swarm_claim", "parallel_swarm_claim_reclaim"],
        &watermark_events,
        projection
            .claims
            .iter()
            .map(|row| (row.claim_id.as_str(), row.source_refs.as_slice())),
    );
    validate_source_refs(
        &mut errors,
        "mailbox_handoff",
        "knowledge_agent_role_mailbox_handoffs",
        &["parallel_swarm_handoff"],
        &watermark_events,
        projection
            .mailbox_handoffs
            .iter()
            .map(|row| (row.handoff_id.as_str(), row.source_refs.as_slice())),
    );
    validate_source_refs(
        &mut errors,
        "checkpoint",
        "knowledge_agent_state_recovery_checkpoints",
        &["parallel_swarm_checkpoint"],
        &watermark_events,
        projection
            .recovery_checkpoints
            .iter()
            .map(|row| (row.checkpoint_id.as_str(), row.source_refs.as_slice())),
    );
    validate_source_refs(
        &mut errors,
        "recovery_receipt",
        "knowledge_agent_recovery_receipts",
        &["parallel_swarm_recovery"],
        &watermark_events,
        projection
            .recovery_receipts
            .iter()
            .map(|row| (row.receipt_id.as_str(), row.source_refs.as_slice())),
    );
    validate_source_refs(
        &mut errors,
        "indexing_lease",
        "knowledge_parallel_indexing_lease_queue",
        &["parallel_indexing_lease"],
        &watermark_events,
        projection
            .indexing_leases
            .iter()
            .map(|row| (row.lease_id.as_str(), row.source_refs.as_slice())),
    );
    validate_source_refs(
        &mut errors,
        "quiet_background_work",
        "knowledge_agent_quiet_background_work",
        &["parallel_swarm_quiet_background_work"],
        &watermark_events,
        projection
            .quiet_background_work
            .iter()
            .map(|row| (row.receipt_id.as_str(), row.source_refs.as_slice())),
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_source_refs<'a>(
    errors: &mut Vec<String>,
    row_kind: &str,
    expected_table: &str,
    allowed_event_aggregate_types: &[&str],
    watermark_events: &BTreeMap<String, &SwarmDashboardEventRefV1>,
    rows: impl Iterator<Item = (&'a str, &'a [SwarmDashboardSourceRefV1])>,
) {
    for (row_id, refs) in rows {
        if refs.is_empty() {
            errors.push(format!("{row_kind} {row_id} has no source_refs"));
            continue;
        }
        if !refs
            .iter()
            .any(|source_ref| source_ref.event_ledger_event_id.is_some())
        {
            errors.push(format!("{row_kind} {row_id} has no EventLedger source ref"));
        }
        for source_ref in refs {
            if source_ref.table_name.trim().is_empty() {
                errors.push(format!("{row_kind} {row_id} has empty source table"));
            }
            if source_ref.table_name != expected_table {
                errors.push(format!(
                    "{row_kind} {row_id} source table must be {expected_table}"
                ));
            }
            if source_ref.row_id != row_id {
                errors.push(format!("{row_kind} {row_id} has mismatched source row_id"));
            }
            if source_ref.row_source_ref != format!("surreal://{}/{}", expected_table, row_id) {
                errors.push(format!("{row_kind} {row_id} has mismatched row source ref"));
            }
            if source_ref.row_source_ref.trim().is_empty()
                || !source_ref.row_source_ref.starts_with("surreal://")
            {
                errors.push(format!("{row_kind} {row_id} has invalid row source ref"));
            }
            match (
                source_ref.event_ledger_event_id.as_deref(),
                source_ref.event_source_ref.as_deref(),
            ) {
                (Some(event_id), Some(event_ref))
                    if event_ref == format!("event-ledger://{event_id}") => {}
                (Some(_), _) => {
                    errors.push(format!("{row_kind} {row_id} has invalid event source ref"))
                }
                (None, Some(_)) => {
                    errors.push(format!("{row_kind} {row_id} has dangling event source ref"))
                }
                (None, None) => {}
            }
            match (
                source_ref.event_ledger_event_id.as_deref(),
                source_ref.event_aggregate_type.as_deref(),
                source_ref.event_aggregate_id.as_deref(),
            ) {
                (Some(event_id), Some(aggregate_type), Some(aggregate_id)) => {
                    if !allowed_event_aggregate_types.contains(&aggregate_type) {
                        errors.push(format!(
                            "{row_kind} {row_id} has invalid event aggregate_type"
                        ));
                    }
                    if aggregate_id != row_id {
                        errors.push(format!(
                            "{row_kind} {row_id} has mismatched event aggregate_id"
                        ));
                    }
                    match watermark_events.get(event_id) {
                        Some(event)
                            if event.aggregate_type == aggregate_type
                                && event.aggregate_id == aggregate_id
                                && event.source_component == PARALLEL_SWARM_SOURCE_COMPONENT => {}
                        Some(_) => errors.push(format!(
                            "{row_kind} {row_id} EventLedger watermark aggregate mismatch"
                        )),
                        None => errors.push(format!(
                            "{row_kind} {row_id} EventLedger ref missing from watermark"
                        )),
                    }
                }
                (Some(_), _, _) => errors.push(format!(
                    "{row_kind} {row_id} missing EventLedger aggregate identity"
                )),
                (None, Some(_), _) | (None, _, Some(_)) => errors.push(format!(
                    "{row_kind} {row_id} has aggregate identity without EventLedger ref"
                )),
                (None, None, None) => {}
            }
        }
    }
}
