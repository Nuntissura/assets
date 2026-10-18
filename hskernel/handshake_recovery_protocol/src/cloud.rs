use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    OpenAi,
    Anthropic,
    LocalRuntime,
    OfficialCli,
    Other,
}

pub const PARALLEL_SWARM_CLOUD_ASSISTANCE_SCHEMA_ID: &str = "hsk.parallel_swarm.cloud_assistance@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudFallbackReason {
    LocalFailed,
    LocalLowConfidence,
    LocalOverloaded,
    LocalSuppressed,
    HardReasoning,
    ForceCloud,
    NoLocalModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudAssistanceOutputKind {
    Analysis,
    PatchSuggestion,
    ValidationSummary,
    HandoffSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudAssistanceReceiptV1 {
    pub schema_id: String,
    pub receipt_id: String,
    pub workspace_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub claim_id: String,
    pub handoff_id: String,
    pub handoff_event_ledger_event_id: String,
    pub cloud_assistance_event_id: String,
    pub fallback_basis_event_id: String,
    pub parent_session_id: String,
    pub prompt_sha256: String,
    pub lane_id: String,
    pub actor_id: String,
    pub provider: Option<ModelProviderKind>,
    pub model_label: String,
    pub fallback_reason: CloudFallbackReason,
    pub output_kind: CloudAssistanceOutputKind,
    pub output_sha256: String,
    pub body_sha256: String,
    pub output_text: String,
    pub target_ref: String,
    pub review_state: String,
    pub non_authoritative: bool,
    pub requires_promotion: bool,
    pub authority_mutation_allowed: bool,
    pub promotion_event_id: Option<String>,
}

pub fn validate_cloud_assistance_receipt(
    receipt: &CloudAssistanceReceiptV1,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if receipt.schema_id != PARALLEL_SWARM_CLOUD_ASSISTANCE_SCHEMA_ID {
        errors.push("schema_id must be hsk.parallel_swarm.cloud_assistance@1".to_string());
    }
    if receipt.receipt_id.trim().is_empty() || !receipt.receipt_id.starts_with("PSR-CLOUD-") {
        errors.push("receipt_id must be a PSR-CLOUD receipt id".to_string());
    }
    if receipt.handoff_id.trim().is_empty() || !receipt.handoff_id.starts_with("PSR-HANDOFF-") {
        errors.push("handoff_id must be a PSR-HANDOFF id".to_string());
    }
    if !receipt.handoff_event_ledger_event_id.starts_with("KE-") {
        errors.push("handoff_event_ledger_event_id must be an EventLedger id".to_string());
    }
    if !receipt.cloud_assistance_event_id.starts_with("KE-") {
        errors.push("cloud_assistance_event_id must be an EventLedger id".to_string());
    }
    if !receipt.fallback_basis_event_id.starts_with("KE-") {
        errors.push("fallback_basis_event_id must be an EventLedger id".to_string());
    }
    if receipt.parent_session_id.trim().is_empty() {
        errors.push("parent_session_id must be present for cloud assistance".to_string());
    }
    if receipt.prompt_sha256.len() != 64
        || !receipt
            .prompt_sha256
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    {
        errors.push("prompt_sha256 must be lowercase sha256 hex".to_string());
    }
    if receipt.provider == Some(ModelProviderKind::LocalRuntime) {
        errors.push("cloud assistance provider must not be local_runtime".to_string());
    }
    if receipt.model_label.trim().is_empty() {
        errors.push("model_label must be present for cloud assistance".to_string());
    }
    if receipt.output_sha256.len() != 64
        || !receipt
            .output_sha256
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    {
        errors.push("output_sha256 must be lowercase sha256 hex".to_string());
    }
    if receipt.body_sha256.len() != 64
        || !receipt
            .body_sha256
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    {
        errors.push("body_sha256 must be lowercase sha256 hex".to_string());
    }
    if receipt.output_text.trim().is_empty() {
        errors.push("output_text must be present for review".to_string());
    }
    if receipt.review_state != "pending_review" {
        errors.push("cloud assistance review_state must be pending_review".to_string());
    }
    if !receipt.non_authoritative {
        errors.push("cloud assistance receipt must be non_authoritative=true".to_string());
    }
    if !receipt.requires_promotion {
        errors.push("cloud assistance receipt must require promotion".to_string());
    }
    if receipt.authority_mutation_allowed {
        errors.push("cloud assistance must not allow authority mutation".to_string());
    }
    if receipt.promotion_event_id.is_some() {
        errors.push("cloud assistance receipt must not carry a promotion_event_id".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
