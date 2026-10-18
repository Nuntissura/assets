use handshake_canonical_json::sha256_hex;
use serde_json::json;

use crate::{
    validate_handoff_compression_template, HandoffCompressionSourceRefV1,
    HandoffCompressionTemplateV1, RecoveryResumePointer,
    PARALLEL_SWARM_HANDOFF_COMPRESSION_SCHEMA_ID,
};

fn template() -> HandoffCompressionTemplateV1 {
    HandoffCompressionTemplateV1 {
        schema_id: PARALLEL_SWARM_HANDOFF_COMPRESSION_SCHEMA_ID.into(),
        checkpoint_id: "PSR-CHKPT-test".into(),
        workspace_id: "workspace".into(),
        wp_id: "WP-1".into(),
        mt_id: "MT-007".into(),
        source_session_id: "session".into(),
        source_lane_id: "lane".into(),
        source_actor_id: "actor".into(),
        source_lane_kind: "local".into(),
        resume_pointer: RecoveryResumePointer::MicroTask { mt_id: "MT-007".into() },
        git_head: "git-head".into(),
        payload_sha256: "a".repeat(64),
        body_sha256: "c4c98d55ae454206b19274562490ac8ad67d4d1a30cb4c6dd98398ab22a4cb57".into(),
        body: "PSR-CHKPT-test MT-007 payload_sha256".into(),
        omitted_inputs: vec![
            "raw_checkpoint_payload".into(),
            "provider_chat_transcript".into(),
            "full_conversation_history".into(),
        ],
        source_refs: vec![HandoffCompressionSourceRefV1 {
            source_kind: "checkpoint".into(),
            source_id: "PSR-CHKPT-test".into(),
            event_ledger_event_id: Some("KE-test".into()),
        }],
        warnings: Vec::new(),
        generated_at_utc: "2026-10-18T12:34:56.123456789Z".parse().unwrap(),
    }
}

fn set_body(template: &mut HandoffCompressionTemplateV1, body: String) {
    template.body_sha256 = sha256_hex(body.as_bytes());
    template.body = body;
}

#[test]
fn existing_template_fields_validate_and_roundtrip_without_loss() {
    let template = template();
    assert_eq!(validate_handoff_compression_template(&template), Ok(()));
    let wire = serde_json::to_value(&template).unwrap();
    assert_eq!(wire["schema_id"], "hsk.parallel_swarm.handoff_compression@1");
    assert_eq!(wire["generated_at_utc"], "2026-10-18T12:34:56.123456789Z");
    assert_eq!(wire.as_object().unwrap().len(), 18);
    assert_eq!(serde_json::from_value::<HandoffCompressionTemplateV1>(wire).unwrap(), template);
}

#[test]
fn validation_accumulates_original_errors_in_original_order() {
    let mut template = template();
    template.schema_id.clear();
    template.checkpoint_id = "bad".into();
    template.workspace_id.clear();
    template.wp_id.clear();
    template.mt_id.clear();
    template.source_session_id.clear();
    template.source_lane_id.clear();
    template.source_actor_id.clear();
    template.source_lane_kind.clear();
    template.git_head.clear();
    template.payload_sha256.clear();
    template.body_sha256.clear();
    template.body.clear();
    template.omitted_inputs.clear();
    template.source_refs.clear();

    assert_eq!(validate_handoff_compression_template(&template).unwrap_err(), vec![
        "schema_id must be hsk.parallel_swarm.handoff_compression@1",
        "checkpoint_id must be a PSR-CHKPT id",
        "workspace_id must be present",
        "wp_id must be present",
        "mt_id must be present",
        "source_session_id must be present",
        "source_lane_id must be present",
        "source_actor_id must be present",
        "source_lane_kind must be present",
        "git_head must be present",
        "payload_sha256 must be lowercase sha256 hex",
        "body_sha256 must be lowercase sha256 hex",
        "body_sha256 must match body bytes",
        "body must be non-empty and bounded to 20000 bytes",
        "body must include checkpoint, MT, and payload hash restart anchors",
        "omitted_inputs must declare raw_checkpoint_payload",
        "omitted_inputs must declare provider_chat_transcript",
        "omitted_inputs must declare full_conversation_history",
        "source_refs must include checkpoint table/EventLedger authority",
    ]);
}

#[test]
fn payload_hash_requires_exact_lowercase_hex_without_inventing_payload_verification() {
    for invalid in ["a".repeat(63), "a".repeat(65), "A".repeat(64), "g".repeat(64)] {
        let mut template = template();
        template.payload_sha256 = invalid;
        assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
            vec!["payload_sha256 must be lowercase sha256 hex"]);
    }
    let mut template = template();
    template.payload_sha256 = "0".repeat(64);
    assert_eq!(validate_handoff_compression_template(&template), Ok(()));
}

#[test]
fn body_hash_checks_exact_bytes_and_accumulates_format_failure_first() {
    let mut template = template();
    template.body.push(' ');
    assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
        vec!["body_sha256 must match body bytes"]);
    template.body_sha256 = "A".repeat(64);
    assert_eq!(validate_handoff_compression_template(&template).unwrap_err(), vec![
        "body_sha256 must be lowercase sha256 hex",
        "body_sha256 must match body bytes",
    ]);
}

#[test]
fn body_limit_counts_utf8_bytes_and_includes_exactly_twenty_thousand() {
    let mut template = template();
    let body = format!("{}{}", template.body, "x".repeat(20_000 - template.body.len()));
    set_body(&mut template, body);
    assert_eq!(validate_handoff_compression_template(&template), Ok(()));
    let body = format!("{}é", template.body);
    set_body(&mut template, body);
    assert_eq!(template.body.len(), 20_002);
    assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
        vec!["body must be non-empty and bounded to 20000 bytes"]);
}

#[test]
fn body_requires_checkpoint_mt_and_literal_payload_hash_anchor() {
    for body in [
        "MT-007 payload_sha256",
        "PSR-CHKPT-test payload_sha256",
        "PSR-CHKPT-test MT-007",
    ] {
        let mut template = template();
        set_body(&mut template, body.into());
        assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
            vec!["body must include checkpoint, MT, and payload hash restart anchors"]);
    }
}

#[test]
fn checkpoint_reference_requires_matching_source_kind_id_and_event_prefix() {
    for source in [
        HandoffCompressionSourceRefV1 { source_kind: "other".into(), source_id: "PSR-CHKPT-test".into(), event_ledger_event_id: Some("KE-test".into()) },
        HandoffCompressionSourceRefV1 { source_kind: "checkpoint".into(), source_id: "PSR-CHKPT-other".into(), event_ledger_event_id: Some("KE-test".into()) },
        HandoffCompressionSourceRefV1 { source_kind: "checkpoint".into(), source_id: "PSR-CHKPT-test".into(), event_ledger_event_id: None },
        HandoffCompressionSourceRefV1 { source_kind: "checkpoint".into(), source_id: "PSR-CHKPT-test".into(), event_ledger_event_id: Some("other".into()) },
    ] {
        let mut template = template();
        template.source_refs = vec![source];
        assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
            vec!["source_refs must include checkpoint table/EventLedger authority"]);
    }
}

#[test]
fn raw_secrets_are_rejected_after_other_body_checks_pass() {
    for secret in [
        "sk-test-secret",
        "-----BEGIN PRIVATE KEY-----",
        "AKIA1234567890123456",
        "https://user:password@example.test",
        "Bearer credential",
        "password=credential",
        "password: credential",
        "secret_token=credential",
        "api_key=credential",
        "api-key=credential",
        "next_step_context=aBcD1234EfGh5678IjKl9012MnOp3456",
    ] {
        let mut template = template();
        let body = format!("{}\n{secret}", template.body);
        set_body(&mut template, body);
        assert_eq!(validate_handoff_compression_template(&template).unwrap_err(),
            vec!["body must not contain obvious raw secret tokens"], "{secret}");
    }
}

#[test]
fn existing_redacted_assignment_and_url_labels_are_accepted() {
    for redacted in [
        "password=[REDACTED:secret]",
        "Bearer [REDACTED:token]",
        "next_step_context=api_key=[REDACTED:secret]",
        "https://[REDACTED:URL_CRED]@example.test",
    ] {
        let mut template = template();
        let body = format!("{}\n{redacted}", template.body);
        set_body(&mut template, body);
        assert_eq!(validate_handoff_compression_template(&template), Ok(()), "{redacted}");
    }
}

#[test]
fn resume_pointer_wire_variants_preserve_existing_tag_and_fields() {
    for (pointer, wire) in [
        (RecoveryResumePointer::MicroTask { mt_id: "MT-007".into() }, json!({"pointer": "micro_task", "mt_id": "MT-007"})),
        (RecoveryResumePointer::Claim { claim_id: "claim".into() }, json!({"pointer": "claim", "claim_id": "claim"})),
        (RecoveryResumePointer::Navigation { command_id: "command".into() }, json!({"pointer": "navigation", "command_id": "command"})),
        (RecoveryResumePointer::IndexRunPosition { index_run_id: "index".into(), position: "position".into() }, json!({"pointer": "index_run_position", "index_run_id": "index", "position": "position"})),
    ] {
        assert_eq!(serde_json::to_value(&pointer).unwrap(), wire);
        assert_eq!(serde_json::from_value::<RecoveryResumePointer>(wire).unwrap(), pointer);
    }
}
