use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ModelProviderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLaneKind {
    Local,
    Cloud,
    Operator,
    Validator,
    IntegrationValidator,
    Indexer,
    Editor,
    System,
}

impl AgentLaneKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::Operator => "operator",
            Self::Validator => "validator",
            Self::IntegrationValidator => "integration_validator",
            Self::Indexer => "indexer",
            Self::Editor => "editor",
            Self::System => "system",
        }
    }

    pub fn parse<E>(value: &str, unknown: impl FnOnce(String) -> E) -> Result<Self, E> {
        match value {
            "local" => Ok(Self::Local),
            "cloud" => Ok(Self::Cloud),
            "operator" => Ok(Self::Operator),
            "validator" => Ok(Self::Validator),
            "integration_validator" => Ok(Self::IntegrationValidator),
            "indexer" => Ok(Self::Indexer),
            "editor" => Ok(Self::Editor),
            "system" => Ok(Self::System),
            other => Err(unknown(format!("unknown lane kind: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    ClaimWorktree,
    ClaimWorkspace,
    EditRichDocument,
    InspectEvidence,
    MutateGraph,
    RunQuietBackgroundWork,
    WriteLocalIndex,
    WriteMailbox,
    NavigateBackend,
    RecordCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionMode {
    Local,
    Cloud,
    Operator,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalCloudAttribution {
    pub mode: AttributionMode,
    pub provider: Option<ModelProviderKind>,
    pub runtime: Option<String>,
    pub model_label: String,
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub provider_metadata: Value,
}

impl LocalCloudAttribution {
    pub fn local(runtime: impl Into<String>, model_label: impl Into<String>) -> Self {
        Self {
            mode: AttributionMode::Local,
            provider: Some(ModelProviderKind::LocalRuntime),
            runtime: Some(runtime.into()),
            model_label: model_label.into(),
            credential_ref: None,
            provider_metadata: json!({}),
        }
    }

    pub fn cloud(
        provider: ModelProviderKind,
        model_label: impl Into<String>,
        credential_ref: impl Into<String>,
        provider_metadata: Value,
        scrub: impl FnOnce(Value) -> Value,
    ) -> Self {
        Self {
            mode: AttributionMode::Cloud,
            provider: Some(provider),
            runtime: None,
            model_label: model_label.into(),
            credential_ref: Some(credential_ref.into()),
            provider_metadata: scrub(provider_metadata),
        }
    }

    pub fn scrubbed_for_persistence(&self, scrub: impl FnOnce(Value) -> Value) -> Self {
        let mut scrubbed = self.clone();
        scrubbed.provider_metadata = scrub(scrubbed.provider_metadata);
        scrubbed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLaneIdentity {
    pub lane_id: String,
    pub actor_id: String,
    pub lane_kind: AgentLaneKind,
    pub attribution: LocalCloudAttribution,
}

impl AgentLaneIdentity {
    pub fn new<E>(
        lane_id: impl Into<String>,
        actor_id: impl Into<String>,
        lane_kind: AgentLaneKind,
        attribution: LocalCloudAttribution,
        mut validate: impl FnMut(&'static str, &str) -> Result<(), E>,
    ) -> Result<Self, E> {
        let lane_id = lane_id.into();
        let actor_id = actor_id.into();
        validate("lane_id", &lane_id)?;
        validate("actor_id", &actor_id)?;
        Ok(Self {
            lane_id,
            actor_id,
            lane_kind,
            attribution,
        })
    }

    pub fn capabilities(&self) -> Vec<AgentCapability> {
        use AgentCapability::*;
        match self.lane_kind {
            AgentLaneKind::Local => vec![
                ClaimWorktree,
                ClaimWorkspace,
                EditRichDocument,
                MutateGraph,
                RunQuietBackgroundWork,
                WriteLocalIndex,
                WriteMailbox,
                NavigateBackend,
                RecordCheckpoint,
            ],
            AgentLaneKind::Cloud => {
                vec![
                    ClaimWorkspace,
                    RunQuietBackgroundWork,
                    WriteMailbox,
                    NavigateBackend,
                    RecordCheckpoint,
                ]
            }
            AgentLaneKind::Operator => vec![
                ClaimWorktree,
                ClaimWorkspace,
                EditRichDocument,
                MutateGraph,
                RunQuietBackgroundWork,
                WriteMailbox,
                NavigateBackend,
                RecordCheckpoint,
            ],
            AgentLaneKind::Validator | AgentLaneKind::IntegrationValidator => {
                vec![InspectEvidence, NavigateBackend]
            }
            AgentLaneKind::Indexer => {
                vec![
                    ClaimWorkspace,
                    RunQuietBackgroundWork,
                    WriteLocalIndex,
                    NavigateBackend,
                    RecordCheckpoint,
                ]
            }
            AgentLaneKind::Editor => vec![
                ClaimWorkspace,
                EditRichDocument,
                MutateGraph,
                RunQuietBackgroundWork,
                NavigateBackend,
                RecordCheckpoint,
            ],
            AgentLaneKind::System => vec![
                ClaimWorktree,
                ClaimWorkspace,
                EditRichDocument,
                MutateGraph,
                RunQuietBackgroundWork,
                WriteLocalIndex,
                WriteMailbox,
                NavigateBackend,
                RecordCheckpoint,
            ],
        }
    }

    pub fn scrubbed_for_persistence(&self, scrub: impl FnOnce(Value) -> Value) -> Self {
        Self {
            lane_id: self.lane_id.clone(),
            actor_id: self.actor_id.clone(),
            lane_kind: self.lane_kind,
            attribution: self.attribution.scrubbed_for_persistence(scrub),
        }
    }
}
