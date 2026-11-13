//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, lead decision (a) on the empty-story form (ledger
//! 2026-11-07T17:33:46Z): an empty stream carries none, or exactly one empty paragraph `[0, 0)` (the
//! paragraph that keeps an empty frame's paragraph style) and at most one empty range; anything else
//! on an empty stream is still refused, and a non-empty stream still refuses an empty entry.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}

fn paragraph(start: u64, end: u64, style: Option<String>) -> Value {
    json!({"start": start, "end": end, "style_ref": style, "paragraph_direction": null})
}
fn range(start: u64, end: u64) -> Value {
    json!({"start": start, "end": end, "style_ref": null, "applied_language": null})
}
fn story(text: &str, paragraphs: Vec<Value>, ranges: Vec<Value>) -> StudioTextStory {
    serde_json::from_value(json!({"schema_id": "hsk.studio.text_story@1", "story_id": identity("STXT", 1), "text_kind": "area",
        "story_direction": "left_to_right", "content": {"content_kind": "inline", "text_utf8": text},
        "paragraphs": paragraphs, "character_ranges": ranges}))
    .expect("story shape")
}
fn validate(s: &StudioTextStory) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    validate_text_story(s, Budget::default(), &CancellationToken::default(), &Caller)
}

/// Red on the code before the lane: the engine's empty-frame form was refused by Folio.
#[test]
fn an_empty_stream_keeps_one_empty_paragraph_with_its_style() {
    let s = story("", vec![paragraph(0, 0, Some(identity("STYS", 1)))], vec![]);
    assert_eq!(validate(&s).map(|i| i.len()), Ok(0));
    let with_range = story("", vec![paragraph(0, 0, Some(identity("STYS", 1)))], vec![range(0, 0)]);
    assert_eq!(validate(&with_range).map(|i| i.len()), Ok(0));
    // The style binding is still a checked reference.
    let bad = story("", vec![paragraph(0, 0, Some("STYS-nope".into()))], vec![]);
    assert!(validate(&bad).is_err());
}

#[test]
fn an_empty_stream_refuses_a_second_empty_paragraph_or_range() {
    let two = story("", vec![paragraph(0, 0, None), paragraph(0, 0, None)], vec![]);
    let d = validate(&two).expect_err("two empty paragraphs");
    assert_eq!((d.code, d.target.ends_with("paragraphs/1")), (Code::InvalidField, true), "{}", d.target);
    let ranges = story("", vec![paragraph(0, 0, None)], vec![range(0, 0), range(0, 0)]);
    assert_eq!(validate(&ranges).expect_err("two empty ranges").code, Code::InvalidField);
}

#[test]
fn an_empty_stream_refuses_a_range_with_bytes() {
    let s = story("", vec![paragraph(0, 0, None)], vec![range(0, 3)]);
    assert_eq!(validate(&s).expect_err("a range past the stream").code, Code::InvalidField);
    let p = story("", vec![paragraph(0, 2, None)], vec![]);
    assert_eq!(validate(&p).expect_err("a paragraph past the stream").code, Code::InvalidField);
}

/// Older files store an empty story without a paragraph or range: they still validate.
#[test]
fn an_empty_stream_without_a_paragraph_still_validates() {
    assert_eq!(validate(&story("", vec![], vec![])).map(|i| i.len()), Ok(0));
}

#[test]
fn a_non_empty_stream_still_refuses_an_empty_entry() {
    let s = story("abc", vec![paragraph(0, 0, None), paragraph(0, 3, None)], vec![range(0, 3)]);
    assert_eq!(validate(&s).expect_err("an empty paragraph in text").code, Code::InvalidField);
    let r = story("abc", vec![paragraph(0, 3, None)], vec![range(0, 0), range(0, 3)]);
    assert_eq!(validate(&r).expect_err("an empty range in text").code, Code::InvalidField);
}
