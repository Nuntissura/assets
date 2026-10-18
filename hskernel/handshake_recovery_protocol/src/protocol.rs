use chrono::{DateTime, Utc};
use handshake_canonical_json::sha256_hex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "pointer", rename_all = "snake_case")]
pub enum RecoveryResumePointer {
    MicroTask {
        mt_id: String,
    },
    Claim {
        claim_id: String,
    },
    Navigation {
        command_id: String,
    },
    IndexRunPosition {
        index_run_id: String,
        position: String,
    },
}

pub const PARALLEL_SWARM_HANDOFF_COMPRESSION_SCHEMA_ID: &str =
    "hsk.parallel_swarm.handoff_compression@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffCompressionSourceRefV1 {
    pub source_kind: String,
    pub source_id: String,
    pub event_ledger_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCompressionTemplateV1 {
    pub schema_id: String,
    pub checkpoint_id: String,
    pub workspace_id: String,
    pub wp_id: String,
    pub mt_id: String,
    pub source_session_id: String,
    pub source_lane_id: String,
    pub source_actor_id: String,
    pub source_lane_kind: String,
    pub resume_pointer: RecoveryResumePointer,
    pub git_head: String,
    pub payload_sha256: String,
    pub body_sha256: String,
    pub body: String,
    pub omitted_inputs: Vec<String>,
    pub source_refs: Vec<HandoffCompressionSourceRefV1>,
    pub warnings: Vec<String>,
    pub generated_at_utc: DateTime<Utc>,
}

pub fn validate_handoff_compression_template(
    template: &HandoffCompressionTemplateV1,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if template.schema_id != PARALLEL_SWARM_HANDOFF_COMPRESSION_SCHEMA_ID {
        errors.push("schema_id must be hsk.parallel_swarm.handoff_compression@1".to_string());
    }
    if !template.checkpoint_id.starts_with("PSR-CHKPT-") {
        errors.push("checkpoint_id must be a PSR-CHKPT id".to_string());
    }
    for (field, value) in [
        ("workspace_id", template.workspace_id.as_str()),
        ("wp_id", template.wp_id.as_str()),
        ("mt_id", template.mt_id.as_str()),
        ("source_session_id", template.source_session_id.as_str()),
        ("source_lane_id", template.source_lane_id.as_str()),
        ("source_actor_id", template.source_actor_id.as_str()),
        ("source_lane_kind", template.source_lane_kind.as_str()),
        ("git_head", template.git_head.as_str()),
    ] {
        if value.trim().is_empty() {
            errors.push(format!("{field} must be present"));
        }
    }
    if !is_lower_sha256(&template.payload_sha256) {
        errors.push("payload_sha256 must be lowercase sha256 hex".to_string());
    }
    if !is_lower_sha256(&template.body_sha256) {
        errors.push("body_sha256 must be lowercase sha256 hex".to_string());
    }
    if template.body_sha256 != sha256_hex(template.body.as_bytes()) {
        errors.push("body_sha256 must match body bytes".to_string());
    }
    if template.body.trim().is_empty() || template.body.len() > 20_000 {
        errors.push("body must be non-empty and bounded to 20000 bytes".to_string());
    }
    if !template.body.contains(&template.checkpoint_id)
        || !template.body.contains(&template.mt_id)
        || !template.body.contains("payload_sha256")
    {
        errors
            .push("body must include checkpoint, MT, and payload hash restart anchors".to_string());
    }
    for required in [
        "raw_checkpoint_payload",
        "provider_chat_transcript",
        "full_conversation_history",
    ] {
        if !template
            .omitted_inputs
            .iter()
            .any(|input| input == required)
        {
            errors.push(format!("omitted_inputs must declare {required}"));
        }
    }
    if !template.source_refs.iter().any(|source| {
        source.source_kind == "checkpoint"
            && source.source_id == template.checkpoint_id
            && source
                .event_ledger_event_id
                .as_deref()
                .is_some_and(|event_id| event_id.starts_with("KE-"))
    }) {
        errors.push("source_refs must include checkpoint table/EventLedger authority".to_string());
    }
    if contains_obvious_secret_token(&template.body) {
        errors.push("body must not contain obvious raw secret tokens".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

fn contains_obvious_secret_token(value: &str) -> bool {
    value.lines().any(|line| {
        let dynamic_value = handoff_dynamic_line_value(line);
        if dynamic_value.is_some_and(contains_high_entropy_secret_token) {
            return true;
        }
        let lowered = dynamic_value.unwrap_or(line).to_ascii_lowercase();
        lowered.contains("sk-")
            || lowered.contains("-----begin ")
            || contains_aws_access_key(&lowered)
            || contains_url_credential(&lowered)
            || contains_unredacted_assignment(&lowered, "bearer ")
            || contains_unredacted_assignment(&lowered, "password=")
            || contains_unredacted_assignment(&lowered, "password:")
            || contains_unredacted_assignment(&lowered, "secret_token=")
            || contains_unredacted_assignment(&lowered, "api_key=")
            || contains_unredacted_assignment(&lowered, "api-key=")
    })
}

fn handoff_dynamic_line_value(line: &str) -> Option<&str> {
    if line.starts_with("touched_files[")
        || line.starts_with("tests[")
        || line.starts_with("hbr_rows[")
        || line.starts_with("next_step_context=")
    {
        line.split_once('=').map(|(_, value)| value)
    } else {
        None
    }
}

fn contains_unredacted_assignment(lowered: &str, needle: &str) -> bool {
    let mut offset = 0;
    while let Some(relative) = lowered[offset..].find(needle) {
        let value_start = offset + relative + needle.len();
        let value = lowered[value_start..].trim_start();
        if !value.starts_with("[redacted:") {
            return true;
        }
        offset = value_start;
    }
    false
}

fn contains_aws_access_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(20).any(|window| {
        matches!(&window[..4], b"akia" | b"asia")
            && window[4..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
    })
}

fn contains_url_credential(value: &str) -> bool {
    value.split_whitespace().any(|part| {
        part.find("://")
            .and_then(|scheme| {
                let after_scheme = &part[scheme + 3..];
                after_scheme.find('@').map(|at| {
                    let credential = &after_scheme[..at];
                    !credential.contains("[redacted:url_cred]") && credential.contains(':')
                })
            })
            .unwrap_or(false)
    })
}

fn contains_high_entropy_secret_token(value: &str) -> bool {
    let mut buf = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=') {
            buf.push(c);
        } else if flush_high_entropy_probe(&mut buf) {
            return true;
        }
    }
    flush_high_entropy_probe(&mut buf)
}

fn flush_high_entropy_probe(buf: &mut String) -> bool {
    let found = buf.len() >= 32
        && buf.chars().any(|c| c.is_ascii_lowercase())
        && buf.chars().any(|c| c.is_ascii_uppercase())
        && buf.chars().any(|c| c.is_ascii_digit());
    buf.clear();
    found
}
