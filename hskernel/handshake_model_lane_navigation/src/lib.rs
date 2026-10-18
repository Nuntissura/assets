//! Pure ModelLane navigation lookup and postvalidated scalar projections.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelLaneNavigationError {
    InvalidInput(String),
    AmbiguousLookup(String),
    NotFound(String),
}

pub fn one_navigation_run_id(
    value: &str,
    run_ids: BTreeSet<String>,
) -> Result<Option<String>, ModelLaneNavigationError> {
    if run_ids.len() > 1 {
        return Err(ModelLaneNavigationError::AmbiguousLookup(format!(
            "navigation value {value} resolves to multiple exact-scope runs"
        )));
    }
    Ok(run_ids.into_iter().next())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLaneNavigationProjection {
    pub schema_id: String,
    pub surface_contract_id: String,
    pub route_id: String,
    pub lookup_kind: String,
    pub lookup_ref: String,
    pub input_schema_ref: String,
    pub output_schema_ref: String,
    pub manual_refs: Vec<String>,
    pub run: Option<ModelLaneNavigationRun>,
    pub lanes: Vec<ModelLaneNavigationLane>,
    pub messages: Vec<ModelLaneNavigationMessage>,
    pub artifacts: Vec<ModelLaneNavigationArtifact>,
    pub context_handoffs: Vec<ModelLaneNavigationScalar>,
    pub recovery_checkpoints: Vec<ModelLaneNavigationScalar>,
    pub recovery_events: Vec<ModelLaneNavigationScalar>,
    pub leases: Vec<ModelLaneNavigationScalar>,
    pub diagnostic_tiers: Vec<ModelLaneNavigationScalar>,
    pub mt_runtime_statuses: Vec<ModelLaneNavigationMtRuntimeStatus>,
    pub event_ledger_refs: Vec<String>,
    pub flight_recorder_refs: Vec<String>,
    pub error_codes: Vec<String>,
    pub recovery_routes: Vec<String>,
}

/// Source identifiers from permission-reduced rows. This DTO is not a canonical
/// record and contains neither a rewritten hash nor hidden payload/reference data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationScalar {
    pub aggregate_id: String,
    pub run_id: String,
    pub event_ledger_event_id: String,
    pub event_ledger_seq: i64,
    pub ordering_seq: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationRun {
    pub run_id: String,
    pub source: ModelLaneNavigationScalar,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_pack: Option<ModelLaneNavigationMemoryPack>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationLane { pub lane_id: String, pub source: ModelLaneNavigationScalar }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationMessage { pub message_id: String, pub source: ModelLaneNavigationScalar }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationMemoryPack {
    pub reference: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationArtifactAliases {
    pub artifact_ref: String,
    pub artifact_binding_id: String,
    pub artifact_manifest_ref: String,
    pub artifact_payload_ref: String,
    pub artifact_sha256: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationArtifact {
    #[serde(flatten)]
    pub source: ModelLaneNavigationScalar,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<ModelLaneNavigationArtifactAliases>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationMtRuntimeStatus {
    #[serde(flatten)]
    pub source: ModelLaneNavigationScalar,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub micro_task_id: Option<String>,
}

/// Optional witnesses are supplied by the host after canonical authorization and
/// semantic validation; this pure assembler neither acquires nor authorizes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLaneNavigationSource {
    pub record_kind: String,
    pub source: ModelLaneNavigationScalar,
    pub error_code: Option<String>,
    pub memory_pack: Option<ModelLaneNavigationMemoryPack>,
    pub artifact_aliases: Option<ModelLaneNavigationArtifactAliases>,
    pub micro_task_id: Option<String>,
}

impl From<(String, ModelLaneNavigationScalar, Option<String>)> for ModelLaneNavigationSource {
    fn from((record_kind, source, error_code): (String, ModelLaneNavigationScalar, Option<String>)) -> Self {
        Self { record_kind, source, error_code, memory_pack: None, artifact_aliases: None, micro_task_id: None }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLaneNavigationLookup {
    pub lookup_kind: Option<String>,
    pub lookup_ref: Option<String>,
    pub run_id: Option<String>,
    pub lane_id: Option<String>,
    pub message_id: Option<String>,
    pub model_session_id: Option<String>,
    pub session_id: Option<String>,
    pub wp_id: Option<String>,
    pub work_packet_id: Option<String>,
    pub mt_id: Option<String>,
    pub micro_task_id: Option<String>,
    pub task_board_id: Option<String>,
    pub artifact_ref: Option<String>,
    pub context_bundle_id: Option<String>,
    pub locus_ref: Option<String>,
    pub locus_binding_ref: Option<String>,
    pub loom_ref: Option<String>,
    pub loom_block_id: Option<String>,
    pub fems_ref: Option<String>,
    pub memory_pack_ref: Option<String>,
    pub memory_pack_hash: Option<String>,
    pub event_ledger_event_id: Option<String>,
    pub event_ledger_seq: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub error_code: Option<String>,
}

impl ModelLaneNavigationLookup {
    pub fn requested(&self) -> Result<(String, String), ModelLaneNavigationError> {
        let mut requested = Vec::new();
        if let (Some(kind), Some(value)) = (
            nonempty_lookup_value(self.lookup_kind.as_deref()),
            nonempty_lookup_value(self.lookup_ref.as_deref()),
        ) {
            requested.push((kind, value));
        }
        for (kind, value) in [
            ("run_id", self.run_id.as_deref()),
            ("lane_id", self.lane_id.as_deref()),
            ("message_id", self.message_id.as_deref()),
            ("model_session_id", self.model_session_id.as_deref()),
            ("session_id", self.session_id.as_deref()),
            ("wp_id", self.wp_id.as_deref()),
            ("work_packet_id", self.work_packet_id.as_deref()),
            ("mt_id", self.mt_id.as_deref()),
            ("micro_task_id", self.micro_task_id.as_deref()),
            ("task_board_id", self.task_board_id.as_deref()),
            ("artifact_ref", self.artifact_ref.as_deref()),
            ("context_bundle_id", self.context_bundle_id.as_deref()),
            ("locus_ref", self.locus_ref.as_deref()),
            ("locus_binding_ref", self.locus_binding_ref.as_deref()),
            ("loom_ref", self.loom_ref.as_deref()),
            ("loom_block_id", self.loom_block_id.as_deref()),
            ("fems_ref", self.fems_ref.as_deref()),
            ("memory_pack_ref", self.memory_pack_ref.as_deref()),
            ("memory_pack_hash", self.memory_pack_hash.as_deref()),
            (
                "event_ledger_event_id",
                self.event_ledger_event_id.as_deref(),
            ),
            ("event_ledger_seq", self.event_ledger_seq.as_deref()),
            ("trace_id", self.trace_id.as_deref()),
            ("span_id", self.span_id.as_deref()),
            ("error_code", self.error_code.as_deref()),
        ] {
            if let Some(value) = nonempty_lookup_value(value) {
                requested.push((kind.to_string(), value));
            }
        }
        match requested.len() {
            1 => Ok(requested.remove(0)),
            0 => Err(ModelLaneNavigationError::InvalidInput(
                "ModelLane navigation lookup requires exactly one selector".into(),
            )),
            _ => Err(ModelLaneNavigationError::InvalidInput(
                "ModelLane navigation lookup accepts exactly one selector".into(),
            )),
        }
    }
}

impl ModelLaneNavigationProjection {
    fn empty(route_id: &str, lookup_kind: &str, lookup_ref: &str) -> Self {
        Self { schema_id: "hsk.model_lane_navigation@2".into(), surface_contract_id: "native_swarm_lane_diagnostics".into(),
            route_id: route_id.into(), lookup_kind: lookup_kind.into(), lookup_ref: lookup_ref.into(),
            input_schema_ref: "hsk.model_lane_navigation_request@1".into(), output_schema_ref: "hsk.model_lane_navigation@2".into(),
            manual_refs: vec!["usermanual://model-lane-navigation".into(), "usermanual://model-lane-diagnostics".into(),
                "usermanual://model-lane-recovery".into(), "usermanual://model-lane-validation-harness".into()],
            run: None, lanes: Vec::new(), messages: Vec::new(), artifacts: Vec::new(), context_handoffs: Vec::new(),
            recovery_checkpoints: Vec::new(), recovery_events: Vec::new(), leases: Vec::new(), diagnostic_tiers: Vec::new(),
            mt_runtime_statuses: Vec::new(), event_ledger_refs: Vec::new(), flight_recorder_refs: Vec::new(), error_codes: Vec::new(),
            recovery_routes: vec!["GET /swarm/model-lanes/navigation/recovery/{run_id}".into(),
                "GET /swarm/model-lanes/diagnostics/{run_id}".into()] }
    }
    fn rebuild_navigation_evidence(&mut self) {
        let mut refs = BTreeSet::new();
        let sources = self.run.iter().map(|row| &row.source)
            .chain(self.lanes.iter().map(|row| &row.source))
            .chain(self.messages.iter().map(|row| &row.source))
            .chain(self.artifacts.iter().map(|row| &row.source)).chain(self.context_handoffs.iter())
            .chain(self.recovery_checkpoints.iter()).chain(self.recovery_events.iter())
            .chain(self.leases.iter()).chain(self.diagnostic_tiers.iter()).chain(self.mt_runtime_statuses.iter().map(|row| &row.source));
        for source in sources {
            push_event_ref(&mut refs, &source.event_ledger_event_id);
            push_event_seq_ref(&mut refs, source.event_ledger_seq);
        }
        self.event_ledger_refs = refs.into_iter().collect();
        // Opaque reference-derived diagnostics are intentionally absent from @2.
        self.flight_recorder_refs.clear(); self.error_codes.clear();
    }
}

/// Assemble a projection from host-validated, selected-run scalar rows and error codes.
pub fn assemble_navigation_projection<I, S>(
    route_id: &str,
    lookup_kind: &str,
    lookup_ref: &str,
    span_id: Option<&str>,
    run_id: &str,
    sources: I,
) -> Result<ModelLaneNavigationProjection, ModelLaneNavigationError>
where
    I: IntoIterator<Item = S>,
    S: Into<ModelLaneNavigationSource>,
{
    let mut projection = ModelLaneNavigationProjection::empty(route_id, lookup_kind, lookup_ref);
    let mut error_codes = BTreeSet::new();
    for input in sources {
        let ModelLaneNavigationSource { record_kind, source: scalar, error_code: code,
            memory_pack, artifact_aliases, micro_task_id } = input.into();
        if (memory_pack.is_some() && record_kind != "run")
            || (artifact_aliases.is_some() && record_kind != "context_artifact")
            || (micro_task_id.is_some() && record_kind != "mt_runtime_status") {
            return Err(ModelLaneNavigationError::InvalidInput(
                "navigation witness differs from its owning record kind".into(),
            ));
        }
        if let Some(code) = code {
            error_codes.insert(code);
        }
        match record_kind.as_str() {
            "run" => projection.run = Some(ModelLaneNavigationRun { run_id: scalar.run_id.clone(), source: scalar, memory_pack }),
            "lane" => projection.lanes.push(ModelLaneNavigationLane { lane_id: scalar.aggregate_id.clone(), source: scalar }),
            "message" if lookup_kind != "message_id" || scalar.aggregate_id == lookup_ref => projection.messages.push(ModelLaneNavigationMessage { message_id: scalar.aggregate_id.clone(), source: scalar }),
            "context_artifact" => projection.artifacts.push(ModelLaneNavigationArtifact { source: scalar, aliases: artifact_aliases }), "context_handoff" => projection.context_handoffs.push(scalar),
            "recovery_checkpoint" => projection.recovery_checkpoints.push(scalar), "recovery_event" => projection.recovery_events.push(scalar),
            "lease" => projection.leases.push(scalar), "diagnostic_tier" => projection.diagnostic_tiers.push(scalar),
            "mt_runtime_status" => projection.mt_runtime_statuses.push(ModelLaneNavigationMtRuntimeStatus { source: scalar, micro_task_id }), _ => {},
        }
    }
    if projection.run.as_ref().is_some_and(|run| run.run_id != run_id)
        || (projection.run.is_none() && !(lookup_kind == "trace_id" && span_id.is_some()))
        || (lookup_kind == "message_id" && projection.messages.is_empty())
        || (lookup_kind == "lane_id" && !projection.lanes.iter().any(|lane| lane.lane_id == lookup_ref))
        || (matches!(lookup_kind, "context_bundle_id" | "artifact_ref")
            && projection.context_handoffs.is_empty() && projection.artifacts.is_empty()) {
        return Err(ModelLaneNavigationError::NotFound("navigation target".into()));
    }
    projection.rebuild_navigation_evidence();
    projection.error_codes = error_codes.into_iter().collect();
    Ok(projection)
}

fn push_event_ref(refs: &mut BTreeSet<String>, event_id: &str) {
    if !event_id.is_empty() {
        refs.insert(format!("eventledger://kernel/{event_id}"));
    }
}

fn push_event_seq_ref(refs: &mut BTreeSet<String>, event_seq: i64) {
    if event_seq > 0 {
        refs.insert(format!("eventledger://kernel/seq/{event_seq}"));
    }
}

fn nonempty_lookup_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
