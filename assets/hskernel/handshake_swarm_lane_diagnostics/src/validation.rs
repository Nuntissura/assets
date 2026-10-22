use std::collections::{BTreeMap, BTreeSet};

use crate::model::{SwarmLaneDiagnosticsProjection, PROJECTION_SCHEMA_ID, SURFACE_CONTRACT_ID};

fn token(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub fn scoped_author_id(pane_id: &str, logical_author_id: &str) -> String {
    format!("{logical_author_id}.pane.{}", token(pane_id))
}

fn register_author_id(
    author_ids: &mut BTreeMap<String, String>,
    scope: &str,
    raw_id: &str,
    author_id: String,
) -> Result<(), String> {
    let raw_id = raw_id.trim();
    if raw_id.is_empty() {
        return Err(format!("{scope} author_id missing"));
    }
    let rendered_token = token(raw_id);
    if rendered_token.trim_matches('-').is_empty() {
        return Err(format!("{scope} author_id unusable for {raw_id}"));
    }
    let source = format!("{scope}:{raw_id}");
    if let Some(existing) = author_ids.insert(author_id.clone(), source.clone()) {
        return Err(format!(
            "duplicate AccessKit author_id {author_id} for {existing} and {source}"
        ));
    }
    Ok(())
}

pub fn run_author_id(run_id: &str) -> String {
    format!("swarm-lane-diagnostics.run.{}", token(run_id))
}

pub fn routing_execution_author_id(execution_id: &str) -> String {
    format!(
        "swarm-lane-diagnostics.routing.execution.{}",
        token(execution_id)
    )
}

pub fn routing_stage_author_id(execution_id: &str, stage_id: &str, attempt: u32) -> String {
    format!(
        "swarm-lane-diagnostics.routing.execution.{}.stage.{}.attempt.{attempt}",
        token(execution_id),
        token(stage_id)
    )
}

pub fn routing_outbox_author_id(command_id: &str) -> String {
    format!(
        "swarm-lane-diagnostics.routing.outbox.{}",
        token(command_id)
    )
}

pub fn lane_author_id(lane_id: &str) -> String {
    format!("swarm-lane-diagnostics.lane.{}", token(lane_id))
}

pub fn lane_model_identity_author_id(lane_id: &str) -> String {
    format!("swarm-lane-diagnostics.lane.{}.model-id", token(lane_id))
}

pub fn lane_model_label_author_id(lane_id: &str) -> String {
    format!("swarm-lane-diagnostics.lane.{}.model-label", token(lane_id))
}

pub fn message_author_id(message_id: &str) -> String {
    format!("swarm-lane-diagnostics.message.{}", token(message_id))
}

pub fn message_payload_author_id(message_id: &str) -> String {
    format!(
        "swarm-lane-diagnostics.message.{}.payload",
        token(message_id)
    )
}

pub fn message_promotion_author_id(message_id: &str) -> String {
    format!(
        "swarm-lane-diagnostics.message.{}.promotion",
        token(message_id)
    )
}

pub fn selected_message_author_id(message_id: &str) -> String {
    format!(
        "swarm-lane-diagnostics.message.{}.selected",
        token(message_id)
    )
}

pub fn tier_author_id(tier: &str) -> String {
    format!("swarm-lane-diagnostics.tier.{}", token(tier))
}

pub fn mt_status_author_id(mt_id: &str) -> String {
    format!("swarm-lane-diagnostics.mt-status.{}", token(mt_id))
}

pub fn validate_projection_for_native_surface(
    projection: &SwarmLaneDiagnosticsProjection,
) -> Result<(), String> {
    let mut author_ids = BTreeMap::new();
    if projection.schema_id != PROJECTION_SCHEMA_ID {
        return Err(format!("schema_id mismatch: {}", projection.schema_id));
    }
    if projection.surface_contract_id != SURFACE_CONTRACT_ID {
        return Err(format!(
            "surface_contract_id mismatch: {}",
            projection.surface_contract_id
        ));
    }
    if projection.run.run_id.trim().is_empty() {
        return Err("run_id missing".to_owned());
    }
    let scope = projection.resource_scope.as_ref().ok_or_else(|| {
        "diagnostics resource_scope attribution is required before rendering".to_owned()
    })?;
    if [
        scope.owner_account_fingerprint.as_str(),
        scope.actor_principal_fingerprint.as_str(),
        scope.authenticated_session_fingerprint.as_str(),
        scope.access_space_fingerprint.as_str(),
        scope.workspace_fingerprint.as_str(),
    ]
    .iter()
    .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err("diagnostics resource_scope attribution fingerprint is invalid".to_owned());
    }
    if scope.visibility != "private_exact_scope_only"
        || scope.denial_posture != "foreign_scope_is_absent_restricted_metadata_withheld"
    {
        return Err("diagnostics resource_scope privacy posture is invalid".to_owned());
    }
    register_author_id(
        &mut author_ids,
        "run",
        &projection.run.run_id,
        run_author_id(&projection.run.run_id),
    )?;
    if projection.run.coordinator_session_id.trim().is_empty()
        || projection.run.routing_policy.trim().is_empty()
        || projection.run.artifact_namespace.trim().is_empty()
        || projection.run.owner_session.trim().is_empty()
        || projection.run.candidate_model_ids.is_empty()
        || projection
            .run
            .candidate_model_ids
            .iter()
            .any(|model_id| model_id.trim().is_empty())
        || projection.run.budget_summary_ref.trim().is_empty()
        || projection.run.determinism_mode.trim().is_empty()
    {
        return Err("run coordinator/routing/owner/model/recovery refs missing".to_owned());
    }
    if projection
        .run
        .work_packet_id
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
        || projection
            .run
            .micro_task_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        || projection
            .run
            .task_board_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        || projection
            .run
            .recovery_hint_ref
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return Err("run taskboard or recovery refs missing".to_owned());
    }
    if projection.run.event_ledger_event_id.trim().is_empty()
        || projection
            .run
            .flight_recorder_correlation_id
            .trim()
            .is_empty()
    {
        return Err("run EventLedger or FlightRecorder ref missing".to_owned());
    }
    let mut execution_ids = BTreeSet::new();
    for execution in &projection.routing_executions {
        register_author_id(
            &mut author_ids,
            "routing execution",
            &execution.execution_id,
            routing_execution_author_id(&execution.execution_id),
        )?;
        if !execution_ids.insert(execution.execution_id.clone()) {
            return Err(format!(
                "duplicate routing execution {}",
                execution.execution_id
            ));
        }
        if execution.run_id != projection.run.run_id
            || execution.trace_id != projection.run.trace_id
            || execution.run_span_id != projection.run.run_span_id
            || execution.coordinator_session_id != projection.run.coordinator_session_id
            || Some(execution.work_packet_id.as_str()) != projection.run.work_packet_id.as_deref()
            || execution.micro_task_id.as_deref() != projection.run.micro_task_id.as_deref()
            || Some(execution.task_board_id.as_str()) != projection.run.task_board_id.as_deref()
            || execution.owner_session != projection.run.owner_session
        {
            return Err(format!(
                "routing execution {} run/trace/coordinator lineage mismatch",
                execution.execution_id
            ));
        }
        if execution.selecting_decision_id.trim().is_empty()
            || execution.selecting_decision_event_id.trim().is_empty()
            || execution.selecting_decision_event_seq <= 0
            || execution.event_ledger_event_id.trim().is_empty()
            || execution.event_ledger_seq <= 0
            || execution.canonical_graph_sha256.trim().is_empty()
            || execution.canonical_launch_plan_sha256.trim().is_empty()
            || execution.locus_ref.trim().is_empty()
            || execution.work_packet_id.trim().is_empty()
            || execution.task_board_id.trim().is_empty()
            || execution.owner_session.trim().is_empty()
        {
            return Err(format!(
                "routing execution {} authority/EventLedger/task lineage missing",
                execution.execution_id
            ));
        }
        match execution.status.as_str() {
            "failed"
                if execution
                    .failure_reason
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty() =>
            {
                return Err(format!(
                    "routing execution {} failed without failure_reason",
                    execution.execution_id
                ));
            }
            "cancelled"
                if execution
                    .cancel_reason
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty() =>
            {
                return Err(format!(
                    "routing execution {} cancelled without cancel_reason",
                    execution.execution_id
                ));
            }
            "running" | "awaiting_authority" | "succeeded" | "failed" | "cancelled" => {}
            unsupported => {
                return Err(format!(
                    "routing execution {} status unsupported: {unsupported}",
                    execution.execution_id
                ));
            }
        }
        let stage_ids = execution
            .stages
            .iter()
            .map(|stage| stage.stage_id.clone())
            .collect::<BTreeSet<_>>();
        if stage_ids.len() != execution.stages.len() {
            return Err(format!(
                "routing execution {} has duplicate current stage lineage",
                execution.execution_id
            ));
        }
        for stage in &execution.stages {
            register_author_id(
                &mut author_ids,
                "routing stage",
                &format!(
                    "{}:{}:{}",
                    execution.execution_id, stage.stage_id, stage.attempt
                ),
                routing_stage_author_id(&execution.execution_id, &stage.stage_id, stage.attempt),
            )?;
            register_author_id(
                &mut author_ids,
                "routing outbox",
                &stage.outbox.command_id,
                routing_outbox_author_id(&stage.outbox.command_id),
            )?;
            if stage.execution_id != execution.execution_id
                || stage.expected_run_id != execution.run_id
                || stage.event_ledger_event_id.trim().is_empty()
                || stage.event_ledger_seq <= 0
                || (stage.state != "scheduled" && stage.input_refs.is_empty())
                || stage
                    .input_refs
                    .iter()
                    .any(|reference| reference.trim().is_empty())
            {
                return Err(format!(
                    "routing stage {}/{} execution/run/input/EventLedger lineage mismatch",
                    execution.execution_id, stage.stage_id
                ));
            }
            if stage
                .dependency_stage_ids
                .iter()
                .any(|dependency| dependency == &stage.stage_id || !stage_ids.contains(dependency))
            {
                return Err(format!(
                    "routing stage {}/{} dependency lineage missing or tampered",
                    execution.execution_id, stage.stage_id
                ));
            }
            let expected_command_id = format!(
                "routing-command:{}:{}:{}",
                execution.execution_id, stage.stage_id, stage.attempt
            );
            if stage.outbox.command_id != expected_command_id
                || stage.outbox.event_ledger_event_id.trim().is_empty()
                || stage.outbox.event_ledger_seq <= 0
            {
                return Err(format!(
                    "routing stage {}/{} durable outbox lineage mismatch",
                    execution.execution_id, stage.stage_id
                ));
            }
            let expected_outbox_status = match stage.state.as_str() {
                "scheduled" => "pending",
                "claimed" | "in_flight" | "awaiting_authority" => "claimed",
                "cancelled" => "cancelled",
                "compensated" => "compensated",
                "succeeded" | "failed" | "joined" => "acked",
                unsupported => {
                    return Err(format!(
                        "routing stage {}/{} state unsupported: {unsupported}",
                        execution.execution_id, stage.stage_id
                    ));
                }
            };
            if stage.outbox.status != expected_outbox_status {
                return Err(format!(
                    "routing stage {}/{} outbox state {} does not match lifecycle {}",
                    execution.execution_id, stage.stage_id, stage.outbox.status, stage.state
                ));
            }
            let active = matches!(
                stage.state.as_str(),
                "claimed" | "in_flight" | "awaiting_authority"
            );
            if active
                && (stage
                    .lease_owner
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                    || stage
                        .fencing_token
                        .as_deref()
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    || stage.lease_expires_at_unix_ms.is_none()
                    || stage.outbox.lease_owner != stage.lease_owner
                    || stage.outbox.fencing_token != stage.fencing_token
                    || stage.outbox.lease_expires_at_unix_ms != stage.lease_expires_at_unix_ms)
            {
                return Err(format!(
                    "routing stage {}/{} active lease fencing lineage missing",
                    execution.execution_id, stage.stage_id
                ));
            }
            if !active
                && (stage.lease_owner.is_some()
                    || stage.fencing_token.is_some()
                    || stage.lease_expires_at_unix_ms.is_some()
                    || stage.outbox.lease_owner.is_some()
                    || stage.outbox.fencing_token.is_some()
                    || stage.outbox.lease_expires_at_unix_ms.is_some())
            {
                return Err(format!(
                    "routing stage {}/{} terminal lifecycle retained active lease",
                    execution.execution_id, stage.stage_id
                ));
            }
            if stage.state == "awaiting_authority"
                && (stage
                    .authority_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                    || stage
                        .authority_request_message_ref
                        .as_deref()
                        .unwrap_or_default()
                        .trim()
                        .is_empty())
            {
                return Err(format!(
                    "routing stage {}/{} awaiting authority without causal authority lineage",
                    execution.execution_id, stage.stage_id
                ));
            }
            let output_lineage_count = usize::from(stage.output_ref.is_some())
                + usize::from(stage.output_message_ref.is_some())
                + usize::from(stage.output_sha256.is_some());
            if output_lineage_count != 0 && output_lineage_count != 3 {
                return Err(format!(
                    "routing stage {}/{} output lineage incomplete",
                    execution.execution_id, stage.stage_id
                ));
            }
        }
    }
    let lane_count: usize = projection.lanes.iter().map(|lane| lane.message_count).sum();
    if lane_count != projection.messages.len() {
        return Err(format!(
            "lane message_count mismatch: lanes sum {lane_count}, messages {}",
            projection.messages.len()
        ));
    }
    let mut lane_message_counts = BTreeMap::<String, usize>::new();
    let mut lane_ids = BTreeSet::new();
    for lane in &projection.lanes {
        register_author_id(
            &mut author_ids,
            "lane",
            &lane.lane_id,
            lane_author_id(&lane.lane_id),
        )?;
        register_author_id(
            &mut author_ids,
            "lane model identity",
            &lane.lane_id,
            lane_model_identity_author_id(&lane.lane_id),
        )?;
        register_author_id(
            &mut author_ids,
            "lane model label",
            &lane.lane_id,
            lane_model_label_author_id(&lane.lane_id),
        )?;
        lane_ids.insert(lane.lane_id.clone());
    }
    for message in &projection.messages {
        if !lane_ids.contains(&message.from_lane_id) {
            return Err(format!(
                "message {} references unknown lane {}",
                message.message_id, message.from_lane_id
            ));
        }
        *lane_message_counts
            .entry(message.from_lane_id.clone())
            .or_insert(0) += 1;
        if let Some(target_lane_id) = message.to_lane.strip_prefix("lane:") {
            if target_lane_id.trim().is_empty() || !lane_ids.contains(target_lane_id) {
                return Err(format!(
                    "message {} references unknown to_lane {}",
                    message.message_id, message.to_lane
                ));
            }
        } else if message.to_lane != "coordinator" && message.to_lane != "broadcast" {
            return Err(format!(
                "message {} routing target unsupported: {}",
                message.message_id, message.to_lane
            ));
        }
    }
    for lane in &projection.lanes {
        let actual_message_count = lane_message_counts
            .get(&lane.lane_id)
            .copied()
            .unwrap_or_default();
        if actual_message_count != lane.message_count {
            return Err(format!(
                "lane {} message_count mismatch: lane says {}, messages contain {}",
                lane.lane_id, lane.message_count, actual_message_count
            ));
        }
        if lane.role.trim().is_empty()
            || lane.backend.trim().is_empty()
            || lane.model_display_name.trim().is_empty()
            || lane.session_id.trim().is_empty()
            || lane.model_session_id.trim().is_empty()
            || lane.adapter_id.trim().is_empty()
            || lane.launch_authority.trim().is_empty()
            || lane.owner_session.trim().is_empty()
        {
            return Err(format!(
                "lane {} session/model/runtime authority refs missing",
                lane.lane_id
            ));
        }
        if lane.capability_token_ids.is_empty()
            || lane
                .capability_token_ids
                .iter()
                .any(|token_id| token_id.trim().is_empty())
            || lane.tool_gate_decision_refs.is_empty()
            || lane
                .tool_gate_decision_refs
                .iter()
                .any(|decision_ref| decision_ref.trim().is_empty())
        {
            return Err(format!(
                "lane {} capability/ToolGate authority refs missing",
                lane.lane_id
            ));
        }
        if lane
            .locus_ref
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            || lane
                .work_packet_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || lane
                .micro_task_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || lane
                .task_board_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(format!(
                "lane {} Locus/taskboard refs missing",
                lane.lane_id
            ));
        }
        if lane
            .last_runtime_status_ref
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            || lane
                .recovery_hint_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(format!(
                "lane {} runtime recovery refs missing",
                lane.lane_id
            ));
        }
        if lane.flight_recorder_correlation_id.trim().is_empty() {
            return Err(format!("lane {} FlightRecorder ref missing", lane.lane_id));
        }
    }
    for message in &projection.messages {
        register_author_id(
            &mut author_ids,
            "message",
            &message.message_id,
            message_author_id(&message.message_id),
        )?;
        register_author_id(
            &mut author_ids,
            "message payload",
            &message.message_id,
            message_payload_author_id(&message.message_id),
        )?;
        register_author_id(
            &mut author_ids,
            "message promotion",
            &message.message_id,
            message_promotion_author_id(&message.message_id),
        )?;
        register_author_id(
            &mut author_ids,
            "selected message",
            &message.message_id,
            selected_message_author_id(&message.message_id),
        )?;
        if message.payload_ref.trim().is_empty() || message.event_ledger_event_id.trim().is_empty()
        {
            return Err(format!(
                "message {} payload/EventLedger ref missing",
                message.message_id
            ));
        }
        if message.flight_recorder_correlation_id.trim().is_empty() {
            return Err(format!(
                "message {} FlightRecorder ref missing",
                message.message_id
            ));
        }
        if message.to_lane.trim().is_empty()
            || message.coordinator_session_id.trim().is_empty()
            || message.owner_session.trim().is_empty()
        {
            return Err(format!(
                "message {} routing/session refs missing",
                message.message_id
            ));
        }
        if message
            .routing_target_role
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            || message
                .routing_target_session
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || message
                .routing_correlation_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(format!(
                "message {} routing target refs missing",
                message.message_id
            ));
        }
        if message
            .work_packet_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            || message
                .micro_task_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || message
                .task_board_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || message
                .locus_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(format!(
                "message {} Locus/taskboard refs missing",
                message.message_id
            ));
        }
        if message.authority == "promotion_candidate"
            && message.promotion_state == "decision_recorded"
            && (message
                .promotion_decision_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
                || message
                    .promotion_gate_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                || message
                    .promotion_receipt_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty())
        {
            return Err(format!(
                "message {} promotion refs missing",
                message.message_id
            ));
        }
        let targets_crdt = message.crdt_update_ref.is_some()
            || message.crdt_base_snapshot_ref.is_some()
            || message.crdt_state_vector.is_some()
            || message.crdt_proposal_ref.is_some()
            || message.crdt_stale_base_ref.is_some();
        if targets_crdt
            && (message
                .proposal_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
                || message
                    .crdt_update_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                || message
                    .crdt_base_snapshot_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                || message
                    .crdt_state_vector
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                || message
                    .crdt_proposal_ref
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty())
        {
            return Err(format!("message {} CRDT refs missing", message.message_id));
        }
    }
    for expected in ["flight_recorder", "internal_diagnostics", "palmistry"] {
        if !projection
            .diagnostic_tiers
            .iter()
            .any(|tier| tier.tier == expected)
        {
            return Err(format!("diagnostic tier {expected} missing"));
        }
    }
    for tier in &projection.diagnostic_tiers {
        register_author_id(
            &mut author_ids,
            "diagnostic tier",
            &tier.tier,
            tier_author_id(&tier.tier),
        )?;
        if tier.reason.trim().is_empty() {
            return Err(format!("diagnostic tier {} reason missing", tier.tier));
        }
        if tier.evidence_ref.trim().is_empty() {
            return Err(format!("diagnostic tier {} evidence missing", tier.tier));
        }
        if tier.state.trim().is_empty() || tier.state == "missing" {
            return Err(format!(
                "diagnostic tier {} state cannot be missing",
                tier.tier
            ));
        }
        if tier.state != "wired"
            && tier.state != "not_applicable_with_reason"
            && tier.state != "deferred_with_reason"
        {
            return Err(format!(
                "diagnostic tier {} state unsupported: {}",
                tier.tier, tier.state
            ));
        }
        if tier.state == "deferred_with_reason" {
            if tier
                .follow_up_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(format!(
                    "diagnostic tier {} deferred follow_up_ref missing",
                    tier.tier
                ));
            }
        }
    }
    if projection.mt_runtime_statuses.is_empty() {
        return Err("MT runtime status rows missing".to_owned());
    }
    let expected_micro_task_id = projection.run.micro_task_id.as_deref().unwrap_or_default();
    let mut covers_run_micro_task = false;
    for status in &projection.mt_runtime_statuses {
        register_author_id(
            &mut author_ids,
            "MT runtime status",
            &status.micro_task_id,
            mt_status_author_id(&status.micro_task_id),
        )?;
        if status.micro_task_id == expected_micro_task_id {
            covers_run_micro_task = true;
        }
        if status.status.trim().is_empty()
            || status.event_ledger_event_id.trim().is_empty()
            || status.event_ledger_seq <= 0
            || status
                .proof_status_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            || status
                .hbr_status_ref
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(format!(
                "MT runtime status {} status/proof/HBR/EventLedger ref missing",
                status.micro_task_id
            ));
        }
    }
    if !covers_run_micro_task {
        return Err(format!(
            "MT runtime status for run micro_task_id {expected_micro_task_id} missing"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_projection_for_native_surface;
    use crate::model::test_projection;

    #[test]
    fn valid_unit_projection_is_accepted() {
        assert_eq!(validate_projection_for_native_surface(&test_projection()), Ok(()));
    }

    #[test]
    fn missing_resource_scope_is_rejected() {
        let mut projection = test_projection();
        projection.resource_scope = None;
        assert_eq!(
            validate_projection_for_native_surface(&projection),
            Err("diagnostics resource_scope attribution is required before rendering".to_owned()),
        );
    }

    #[test]
    fn sanitized_mt_author_id_collision_is_rejected() {
        let mut projection = test_projection();
        projection.run.micro_task_id = Some("unit/mt".to_owned());
        projection.mt_runtime_statuses[0].micro_task_id = "unit/mt".to_owned();
        let mut colliding = projection.mt_runtime_statuses[0].clone();
        colliding.micro_task_id = "unit?mt".to_owned();
        projection.mt_runtime_statuses.push(colliding);
        assert_eq!(
            validate_projection_for_native_surface(&projection),
            Err(concat!(
                "duplicate AccessKit author_id swarm-lane-diagnostics.mt-status.unit-mt ",
                "for MT runtime status:unit/mt and MT runtime status:unit?mt",
            ).to_owned()),
        );
    }
}
