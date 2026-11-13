//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1 (V4-06 class: records keyed by story bytes): the Folio local
//! character overrides move with every text edit that goes through `shift_paragraph_records` (the
//! editor's replace, find/change, cross-reference and data-merge updates), the way character
//! ranges move.
use hsk_studio_folio::{
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioCharacterAttributes, StudioCharacterOverride, StudioLayoutFlowRecords,
    StudioTextStory, StudioTypeStyleRecords, TextStorySchema,
};
use hsk_studio_layout::{replace_story_text, shift_character_overrides, shift_paragraph_records};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const ME: &str = "STXT-019abcde-0000-7000-8000-000000000001";
const OTHER: &str = "STXT-019abcde-0000-7000-8000-000000000002";

fn story(text: &str) -> StudioTextStory {
    let n = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: ME.into(),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: vec![StoryParagraph { start: 0, end: n, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end: n, style_ref: None, applied_language: None }],
    }
}
fn bold() -> StudioCharacterAttributes {
    StudioCharacterAttributes { faux_bold: Some(true), ..StudioCharacterAttributes::default() }
}
fn styles(spans: &[(&str, u64, u64)]) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(DOC);
    r.character_overrides = spans.iter().map(|(id, start, end)| StudioCharacterOverride { text_id: (*id).into(), start: *start, end: *end, attributes: bold() }).collect();
    r
}
fn edit(text: &str, spans: &[(&str, u64, u64)], start: u64, end: u64, new: &str) -> Vec<(String, u64, u64)> {
    let original = story(text);
    let edited = replace_story_text(&original, None, start, end, new).unwrap();
    let (mut flow, mut s) = (StudioLayoutFlowRecords::new(DOC), styles(spans));
    shift_paragraph_records(&mut flow, &mut s, &original, &edited, start, end, new.len() as u64).unwrap();
    s.character_overrides.iter().map(|o| (o.text_id.clone(), o.start, o.end)).collect()
}

#[test]
fn overrides_follow_inserts_and_deletes() {
    // "hello world": override on "world" (6..11) and another story's on 0..5.
    let spans = [(ME, 6, 11), (OTHER, 0, 5)];
    assert_eq!(edit("hello world", &spans, 0, 0, ">> "), [(ME.into(), 9, 14), (OTHER.into(), 0, 5)], "an insert before moves it");
    assert_eq!(edit("hello world", &spans, 8, 8, "XX"), [(ME.into(), 6, 13), (OTHER.into(), 0, 5)], "text typed inside joins it");
    assert_eq!(edit("hello world", &spans, 6, 6, "big "), [(ME.into(), 10, 15), (OTHER.into(), 0, 5)], "text typed at its first byte stays outside");
    assert_eq!(edit("hello world", &spans, 4, 8, ""), [(ME.into(), 4, 7), (OTHER.into(), 0, 5)], "a delete over its start trims it");
    assert_eq!(edit("hello world", &spans, 5, 11, ""), [(OTHER.into(), 0, 5)], "a delete of all of it drops it");
}

#[test]
fn a_reversed_edit_is_refused() {
    let mut s = styles(&[(ME, 0, 3)]);
    assert!(shift_character_overrides(&mut s, ME, 4, 2, 0).is_err());
    assert_eq!(s.character_overrides.len(), 1);
}
