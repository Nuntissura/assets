//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review F9: the layout records validate every local character
//! override against its story (a stored story, inside its bytes, on character boundaries), so a hostile or
//! stale override is refused when the records are read, not later by an export.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Code, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioCharacterAttributes, StudioCharacterOverride,
    StudioDocument, StudioTextStory, TextStorySchema,
};
use hsk_studio_session::LayoutRecords;
use serde_json::json;

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
/// One story `ab\u{e9}` (4 bytes: the acute e is bytes 2..4) and one override.
fn records(text_id: String, start: u64, end: u64) -> LayoutRecords {
    let mut r = LayoutRecords::new(&identity("SDOC", 1));
    let text = "ab\u{e9}";
    r.stories.push(StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: identity("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: vec![StoryParagraph { start: 0, end: 4, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end: 4, style_ref: None, applied_language: None }],
    });
    r.styles.character_overrides.push(StudioCharacterOverride { text_id, start, end, attributes: StudioCharacterAttributes { tracking: Some(10.0), ..StudioCharacterAttributes::default() } });
    r
}
fn validate(r: &LayoutRecords) -> Result<(), Code> {
    r.validate(&document(), Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}

#[test]
fn an_override_on_its_story_validates() {
    assert_eq!(validate(&records(identity("STXT", 1), 0, 4)), Ok(()));
    assert_eq!(validate(&records(identity("STXT", 1), 2, 4)), Ok(()));
}

#[test]
fn an_override_of_a_story_the_records_do_not_hold_is_refused() {
    assert_eq!(validate(&records(identity("STXT", 2), 0, 1)), Err(Code::DanglingReference));
}

#[test]
fn an_override_past_its_story_or_inside_a_character_is_refused() {
    assert_eq!(validate(&records(identity("STXT", 1), 1, 5)), Err(Code::InvalidField), "past the end");
    assert_eq!(validate(&records(identity("STXT", 1), 0, 3)), Err(Code::InvalidField), "ends inside the acute e");
    assert_eq!(validate(&records(identity("STXT", 1), 3, 4)), Err(Code::InvalidField), "starts inside the acute e");
    assert_eq!(validate(&records(identity("STXT", 1), u64::MAX - 1, u64::MAX)), Err(Code::InvalidField), "hostile bounds");
}
