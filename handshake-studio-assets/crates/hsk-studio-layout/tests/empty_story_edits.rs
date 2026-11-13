//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, ledger 2026-11-07T12:31:03Z and 17:33:46Z: text typed into an
//! empty story splits at its paragraph terminators like any other edit (it used to stay one
//! paragraph), the empty story's paragraph style carries to every new paragraph, deleting all text
//! keeps one empty paragraph with that style, and an older story stored without a paragraph splits
//! the same way.
use hsk_studio_folio::{StoryContent, StoryDirection, StoryParagraph, StoryTextKind, StudioTextStory, TextStorySchema};
use hsk_studio_layout::replace_story_text;

fn style() -> Option<String> {
    Some("STYS-019abcde-0000-7000-8000-000000000001".into())
}
fn empty(paragraphs: Vec<StoryParagraph>) -> StudioTextStory {
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: "STXT-019abcde-0000-7000-8000-000000000001".into(),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: String::new() },
        paragraphs,
        character_ranges: vec![],
    }
}
fn spans(s: &StudioTextStory) -> Vec<(u64, u64, Option<String>)> {
    s.paragraphs.iter().map(|p| (p.start, p.end, p.style_ref.clone())).collect()
}

#[test]
fn a_newline_typed_into_an_empty_story_makes_two_paragraphs_with_its_style() {
    let s = empty(vec![StoryParagraph { start: 0, end: 0, style_ref: style(), paragraph_direction: None }]);
    let e = replace_story_text(&s, None, 0, 0, "one\ntwo").unwrap();
    assert_eq!(spans(&e), [(0, 4, style()), (4, 7, style())]);
    assert_eq!(e.character_ranges.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(), [(0, 7)]);
    let three = replace_story_text(&s, None, 0, 0, "a\u{2029}b\nc\n").unwrap();
    assert_eq!(spans(&three).iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(), [(0, 4), (4, 6), (6, 8)], "U+2029 is 3 bytes; a final terminator ends the last paragraph");
}

#[test]
fn deleting_all_text_keeps_one_empty_paragraph_with_its_style() {
    let s = empty(vec![StoryParagraph { start: 0, end: 0, style_ref: style(), paragraph_direction: None }]);
    let typed = replace_story_text(&s, None, 0, 0, "one\ntwo").unwrap();
    let cleared = replace_story_text(&typed, None, 0, 7, "").unwrap();
    assert_eq!(spans(&cleared), [(0, 0, style())]);
    assert!(cleared.character_ranges.is_empty());
    let again = replace_story_text(&cleared, None, 0, 0, "x\ny").unwrap();
    assert_eq!(spans(&again), [(0, 2, style()), (2, 3, style())], "the style survives the empty state");
}

#[test]
fn a_story_stored_without_a_paragraph_splits_too() {
    let e = replace_story_text(&empty(vec![]), None, 0, 0, "one\ntwo").unwrap();
    assert_eq!(spans(&e), [(0, 4, None), (4, 7, None)]);
}
