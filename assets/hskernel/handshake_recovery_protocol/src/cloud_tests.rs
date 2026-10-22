use serde_json::json;

use crate::{
    validate_cloud_assistance_receipt, CloudAssistanceOutputKind, CloudAssistanceReceiptV1,
    CloudFallbackReason, ModelProviderKind, PARALLEL_SWARM_CLOUD_ASSISTANCE_SCHEMA_ID,
};

fn receipt() -> CloudAssistanceReceiptV1 {
    CloudAssistanceReceiptV1 {
        schema_id: PARALLEL_SWARM_CLOUD_ASSISTANCE_SCHEMA_ID.into(),
        receipt_id: "PSR-CLOUD-test".into(),
        workspace_id: "workspace".into(),
        wp_id: "WP-1".into(),
        mt_id: "MT-007".into(),
        claim_id: "claim".into(),
        handoff_id: "PSR-HANDOFF-test".into(),
        handoff_event_ledger_event_id: "KE-handoff".into(),
        cloud_assistance_event_id: "KE-cloud".into(),
        fallback_basis_event_id: "KE-fallback".into(),
        parent_session_id: "session".into(),
        prompt_sha256: "a".repeat(64),
        lane_id: "lane".into(),
        actor_id: "actor".into(),
        provider: Some(ModelProviderKind::OpenAi),
        model_label: "model".into(),
        fallback_reason: CloudFallbackReason::LocalFailed,
        output_kind: CloudAssistanceOutputKind::Analysis,
        output_sha256: "b".repeat(64),
        body_sha256: "c".repeat(64),
        output_text: "Reviewable analysis".into(),
        target_ref: "target".into(),
        review_state: "pending_review".into(),
        non_authoritative: true,
        requires_promotion: true,
        authority_mutation_allowed: false,
        promotion_event_id: None,
    }
}

#[test]
fn reviewable_cloud_receipt_preserves_existing_wire_fields() {
    let receipt = receipt();
    assert_eq!(validate_cloud_assistance_receipt(&receipt), Ok(()));
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 27);
    assert_eq!(wire["schema_id"], "hsk.parallel_swarm.cloud_assistance@1");
    assert_eq!(wire["provider"], "open_ai");
    assert_eq!(wire["review_state"], "pending_review");
    assert_eq!(wire["non_authoritative"], true);
    assert_eq!(wire["requires_promotion"], true);
    assert_eq!(wire["authority_mutation_allowed"], false);
    assert!(wire["promotion_event_id"].is_null());
    assert_eq!(serde_json::from_value::<CloudAssistanceReceiptV1>(wire).unwrap(), receipt);
}

#[test]
fn cloud_receipt_accumulates_all_original_errors_in_order() {
    let mut receipt = receipt();
    receipt.schema_id.clear();
    receipt.receipt_id.clear();
    receipt.handoff_id.clear();
    receipt.handoff_event_ledger_event_id.clear();
    receipt.cloud_assistance_event_id.clear();
    receipt.fallback_basis_event_id.clear();
    receipt.parent_session_id.clear();
    receipt.prompt_sha256.clear();
    receipt.provider = Some(ModelProviderKind::LocalRuntime);
    receipt.model_label.clear();
    receipt.output_sha256.clear();
    receipt.body_sha256.clear();
    receipt.output_text.clear();
    receipt.review_state.clear();
    receipt.non_authoritative = false;
    receipt.requires_promotion = false;
    receipt.authority_mutation_allowed = true;
    receipt.promotion_event_id = Some("KE-promotion".into());
    assert_eq!(validate_cloud_assistance_receipt(&receipt).unwrap_err(), vec![
        "schema_id must be hsk.parallel_swarm.cloud_assistance@1",
        "receipt_id must be a PSR-CLOUD receipt id",
        "handoff_id must be a PSR-HANDOFF id",
        "handoff_event_ledger_event_id must be an EventLedger id",
        "cloud_assistance_event_id must be an EventLedger id",
        "fallback_basis_event_id must be an EventLedger id",
        "parent_session_id must be present for cloud assistance",
        "prompt_sha256 must be lowercase sha256 hex",
        "cloud assistance provider must not be local_runtime",
        "model_label must be present for cloud assistance",
        "output_sha256 must be lowercase sha256 hex",
        "body_sha256 must be lowercase sha256 hex",
        "output_text must be present for review",
        "cloud assistance review_state must be pending_review",
        "cloud assistance receipt must be non_authoritative=true",
        "cloud assistance receipt must require promotion",
        "cloud assistance must not allow authority mutation",
        "cloud assistance receipt must not carry a promotion_event_id",
    ]);
}

#[test]
fn provider_gate_preserves_optional_and_nonlocal_provider_behavior() {
    for provider in [None, Some(ModelProviderKind::OpenAi), Some(ModelProviderKind::Anthropic),
        Some(ModelProviderKind::OfficialCli), Some(ModelProviderKind::Other)] {
        let mut receipt = receipt();
        receipt.provider = provider;
        assert_eq!(validate_cloud_assistance_receipt(&receipt), Ok(()));
    }
    let mut receipt = receipt();
    receipt.provider = Some(ModelProviderKind::LocalRuntime);
    assert_eq!(validate_cloud_assistance_receipt(&receipt).unwrap_err(),
        vec!["cloud assistance provider must not be local_runtime"]);
}

#[test]
fn receipt_and_event_ids_retain_existing_prefix_only_checks() {
    let mut receipt = receipt();
    receipt.receipt_id = "PSR-CLOUD-".into();
    receipt.handoff_id = "PSR-HANDOFF-".into();
    receipt.handoff_event_ledger_event_id = "KE-".into();
    receipt.cloud_assistance_event_id = "KE-".into();
    receipt.fallback_basis_event_id = "KE-".into();
    assert_eq!(validate_cloud_assistance_receipt(&receipt), Ok(()));
}

#[test]
fn all_hash_fields_require_exact_lowercase_hex() {
    for invalid in ["a".repeat(63), "a".repeat(65), "A".repeat(64), "g".repeat(64)] {
        let mut receipt = receipt();
        receipt.prompt_sha256 = invalid.clone();
        receipt.output_sha256 = invalid.clone();
        receipt.body_sha256 = invalid;
        assert_eq!(validate_cloud_assistance_receipt(&receipt).unwrap_err(), vec![
            "prompt_sha256 must be lowercase sha256 hex",
            "output_sha256 must be lowercase sha256 hex",
            "body_sha256 must be lowercase sha256 hex",
        ]);
    }
}

#[test]
fn validator_preserves_hash_syntax_scope_without_inventing_content_verification() {
    let mut receipt = receipt();
    receipt.prompt_sha256 = "0".repeat(64);
    receipt.output_sha256 = "0".repeat(64);
    receipt.body_sha256 = "0".repeat(64);
    receipt.output_text = "Different reviewable text".into();
    assert_eq!(validate_cloud_assistance_receipt(&receipt), Ok(()));
}

#[test]
fn forged_authority_and_any_promotion_id_are_rejected() {
    let mut receipt = receipt();
    receipt.non_authoritative = false;
    receipt.authority_mutation_allowed = true;
    receipt.promotion_event_id = Some(String::new());
    assert_eq!(validate_cloud_assistance_receipt(&receipt).unwrap_err(), vec![
        "cloud assistance receipt must be non_authoritative=true",
        "cloud assistance must not allow authority mutation",
        "cloud assistance receipt must not carry a promotion_event_id",
    ]);
}

#[test]
fn required_text_is_checked_without_trimming_accepted_values() {
    let mut receipt = receipt();
    receipt.parent_session_id = "\t\n".into();
    receipt.model_label = "\u{2003}".into();
    receipt.output_text = " ".into();
    assert_eq!(validate_cloud_assistance_receipt(&receipt).unwrap_err(), vec![
        "parent_session_id must be present for cloud assistance",
        "model_label must be present for cloud assistance",
        "output_text must be present for review",
    ]);
    receipt.parent_session_id = " session ".into();
    receipt.model_label = " model ".into();
    receipt.output_text = " analysis ".into();
    assert_eq!(validate_cloud_assistance_receipt(&receipt), Ok(()));
    assert_eq!(receipt.output_text, " analysis ");
}

#[test]
fn model_provider_wire_names_preserve_existing_variants() {
    for (provider, wire) in [
        (ModelProviderKind::OpenAi, "open_ai"),
        (ModelProviderKind::Anthropic, "anthropic"),
        (ModelProviderKind::LocalRuntime, "local_runtime"),
        (ModelProviderKind::OfficialCli, "official_cli"),
        (ModelProviderKind::Other, "other"),
    ] {
        assert_eq!(serde_json::to_value(provider).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<ModelProviderKind>(json!(wire)).unwrap(), provider);
    }
    assert!(serde_json::from_value::<ModelProviderKind>(json!("OpenAi")).is_err());
}

#[test]
fn fallback_reason_wire_names_preserve_existing_variants() {
    for (reason, wire) in [
        (CloudFallbackReason::LocalFailed, "local_failed"),
        (CloudFallbackReason::LocalLowConfidence, "local_low_confidence"),
        (CloudFallbackReason::LocalOverloaded, "local_overloaded"),
        (CloudFallbackReason::LocalSuppressed, "local_suppressed"),
        (CloudFallbackReason::HardReasoning, "hard_reasoning"),
        (CloudFallbackReason::ForceCloud, "force_cloud"),
        (CloudFallbackReason::NoLocalModel, "no_local_model"),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<CloudFallbackReason>(json!(wire)).unwrap(), reason);
    }
}

#[test]
fn cloud_output_kind_wire_names_preserve_existing_variants() {
    for (kind, wire) in [
        (CloudAssistanceOutputKind::Analysis, "analysis"),
        (CloudAssistanceOutputKind::PatchSuggestion, "patch_suggestion"),
        (CloudAssistanceOutputKind::ValidationSummary, "validation_summary"),
        (CloudAssistanceOutputKind::HandoffSummary, "handoff_summary"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        assert_eq!(serde_json::from_value::<CloudAssistanceOutputKind>(json!(wire)).unwrap(), kind);
    }
}
