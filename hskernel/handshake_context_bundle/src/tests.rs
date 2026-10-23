use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::{canonical_json_bytes, sha256_hex, ContextBundle, ContextBundleError};

fn timestamp() -> DateTime<Utc> {
    "2026-10-18T12:34:56.123456789Z".parse().unwrap()
}

#[test]
fn context_bundle_contract_records_exact_allowed_context_with_stable_hash() {
    let context = json!({
        "visible_messages": [
            {"role": "system", "content_artifact_id": "artifact-system"},
            {"role": "user", "content_artifact_id": "artifact-user"}
        ],
        "tool_grants": ["read_trace"],
        "redactions": []
    });
    let bundle = ContextBundle::new_at("KTR-EXAMPLE", "SR-EXAMPLE", context.clone(), timestamp())
        .expect("valid context bundle");
    let reordered: Value = serde_json::from_str(
        r#"{"redactions":[],"tool_grants":["read_trace"],"visible_messages":[{"content_artifact_id":"artifact-system","role":"system"},{"content_artifact_id":"artifact-user","role":"user"}]}"#,
    ).unwrap();
    let same_bundle = ContextBundle::new_at("KTR-EXAMPLE", "SR-EXAMPLE", reordered, timestamp())
        .expect("same context with reordered object fields");

    assert_eq!(bundle.allowed_context, context);
    assert_eq!(bundle, same_bundle);
    assert_eq!(bundle.created_at, timestamp());
}

#[test]
fn empty_object_has_existing_sha256_and_context_id() {
    let bundle = ContextBundle::new_at("task", "session", json!({}), timestamp()).unwrap();

    assert_eq!(bundle.context_hash,
        "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
    assert_eq!(bundle.context_bundle_id, "CTX-44136fa355b3678a1");
}

#[test]
fn context_hash_uses_nested_sorted_keys_and_original_array_order() {
    let context: Value = serde_json::from_str(
        r#"{"z":[{"b":2,"a":1},null,true,false,1.5],"a":"first"}"#,
    ).unwrap();
    let bundle = ContextBundle::new_at("task", "session", context.clone(), timestamp()).unwrap();
    let expected = br#"{"a":"first","z":[{"a":1,"b":2},null,true,false,1.5]}"#;

    assert_eq!(canonical_json_bytes(&context), expected);
    assert_eq!(bundle.context_hash, sha256_hex(expected));

    let reversed = json!({"a": "first", "z": [1.5, false, true, null, {"a": 1, "b": 2}]});
    let reversed_bundle = ContextBundle::new_at("task", "session", reversed, timestamp()).unwrap();
    assert_ne!(bundle.context_hash, reversed_bundle.context_hash);
    assert_ne!(bundle.context_bundle_id, reversed_bundle.context_bundle_id);
}

#[test]
fn context_hash_preserves_existing_escape_bytes_in_keys_and_values() {
    let context = json!({"k\n\"\\": "\"\\\n\r\t\u{0000}\u{0008}\u{000c}\u{001f}é"});
    let expected = "{\"k\\n\\\"\\\\\":\"\\\"\\\\\\n\\r\\t\u{0000}\u{0008}\u{000c}\u{001f}é\"}".as_bytes();
    let bundle = ContextBundle::new_at("task", "session", context.clone(), timestamp()).unwrap();

    assert_eq!(canonical_json_bytes(&context), expected);
    assert_eq!(bundle.context_hash, sha256_hex(expected));
    assert_eq!(bundle.allowed_context, context);
}

#[test]
fn ids_and_timestamp_do_not_change_context_identity() {
    let first = ContextBundle::new_at("task-a", "session-a", json!({}), timestamp()).unwrap();
    let later = "2027-01-01T00:00:00Z".parse().unwrap();
    let second = ContextBundle::new_at("task-b", "session-b", json!({}), later).unwrap();

    assert_eq!(first.context_hash, second.context_hash);
    assert_eq!(first.context_bundle_id, second.context_bundle_id);
    assert_ne!(first.kernel_task_run_id, second.kernel_task_run_id);
    assert_ne!(first.session_run_id, second.session_run_id);
    assert_eq!(second.created_at, later);
}

#[test]
fn constructor_rejects_blank_ids_with_original_error_messages() {
    for blank in ["", " ", "\t\r\n", "\u{2003}"] {
        assert!(matches!(
            ContextBundle::new_at(blank, "session", Value::Null, timestamp()),
            Err(ContextBundleError::InvalidEvent("kernel_task_run_id is required"))
        ));
        assert!(matches!(
            ContextBundle::new_at("task", blank, Value::Null, timestamp()),
            Err(ContextBundleError::InvalidEvent("session_run_id is required"))
        ));
    }
    let error = ContextBundle::new_at("", "", Value::Null, timestamp()).unwrap_err();
    assert_eq!(error.to_string(), "invalid kernel event: kernel_task_run_id is required");
}

#[test]
fn constructor_preserves_whitespace_around_nonblank_ids() {
    let bundle = ContextBundle::new_at(" task \t", "\n session ", Value::Null, timestamp()).unwrap();

    assert_eq!(bundle.kernel_task_run_id, " task \t");
    assert_eq!(bundle.session_run_id, "\n session ");
}

#[test]
fn serialization_preserves_existing_fields_and_exact_supplied_timestamp() {
    let bundle = ContextBundle::new_at("task", "session", json!({}), timestamp()).unwrap();
    let wire = serde_json::to_value(&bundle).unwrap();

    assert_eq!(wire, json!({
        "context_bundle_id": "CTX-44136fa355b3678a1",
        "kernel_task_run_id": "task",
        "session_run_id": "session",
        "allowed_context": {},
        "context_hash": "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
        "created_at": "2026-10-18T12:34:56.123456789Z"
    }));
    assert_eq!(serde_json::from_value::<ContextBundle>(wire).unwrap(), bundle);
}

#[test]
fn deserialization_preserves_stored_values_without_constructor_validation() {
    let wire = json!({
        "context_bundle_id": "stored-id",
        "kernel_task_run_id": "",
        "session_run_id": "",
        "allowed_context": [null, true, 7],
        "context_hash": "stored-hash",
        "created_at": "2026-10-18T12:34:56.123456789Z"
    });
    let bundle: ContextBundle = serde_json::from_value(wire.clone()).unwrap();

    assert_eq!(serde_json::to_value(bundle).unwrap(), wire);
}
