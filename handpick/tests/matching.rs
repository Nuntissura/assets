#![cfg(feature = "matching")]
use handpick::matching::{match_literal, LiteralMatchError, MAX_CANDIDATE_CHARS, MAX_NEEDLE_CHARS};
use serde_json::Value;

#[test]
fn shared_literal_vectors_preserve_whole_string_distance() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/literal-matching-v1.json")).unwrap();
    assert_eq!(fixture["version"], 1);
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 14);
    for row in rows {
        let max_edits = u8::try_from(row["max_edits"].as_u64().unwrap()).unwrap();
        let result = match_literal(row["needle"].as_str().unwrap(), row["candidate"].as_str().unwrap(), max_edits).unwrap();
        assert_eq!(serde_json::to_value(result).unwrap(), row["match"], "{row}");
    }
}

#[test]
fn scalar_and_threshold_limits_reject_before_distance_work() {
    assert_eq!(match_literal("", "text", 1), Err(LiteralMatchError::EmptyNeedle));
    assert_eq!(match_literal("x", "text", 9), Err(LiteralMatchError::ThresholdLimit));
    let exact = "😀".repeat(MAX_NEEDLE_CHARS);
    assert_eq!(match_literal(&exact, &exact, 0).unwrap().unwrap().edit_distance, 0);
    assert_eq!(match_literal(&(exact + "猫"), "x", 1), Err(LiteralMatchError::NeedleLimit));
    assert_eq!(match_literal("x", &"猫".repeat(MAX_CANDIDATE_CHARS), 8), Ok(None));
    assert_eq!(match_literal("x", &"猫".repeat(MAX_CANDIDATE_CHARS + 1), 8), Err(LiteralMatchError::CandidateLimit));
    assert_eq!(match_literal("a", "abcdefghi", 8).unwrap().unwrap().edit_distance, 8);
    assert_eq!(match_literal("a", "abcdefghij", 8), Ok(None));
}
