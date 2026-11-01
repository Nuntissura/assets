//! Lane SMB-ID-TOC-INDEX-1 item 1: the table-of-contents contract (STU-LAY-044, STU-LAY-148;
//! MT-173, MT-30776, MT-30777). Hand-built placements; oracles are hand-computed from the Studio
//! rules in `src/toc_contract.rs` (InDesign parity UNVERIFIED, D5).
//!
//! Source story (STXT 1): "Alpha\n" H1 p1 | "beta<U+2028>two\n" H2 p1 | "Gamma\n" H1 p2 |
//! "Secret\n" H1 p2 on the HIDDEN frame layer | "body\n" p2 | "Delta\n" H1 overset.
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use support_layout::{document, frame_layer, id, spec};

fn stys(n: u32) -> String {
    id("STYS", n)
}
fn para(n: u32) -> ParagraphStyleRef {
    ParagraphStyleRef::parse(&stys(n)).unwrap()
}
fn chr(n: u32) -> CharacterStyleRef {
    CharacterStyleRef::parse(&stys(n)).unwrap()
}
fn ctx(revision: u64) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(
        actor,
        ClientSessionId::parse("window-1").unwrap(),
        Some(CommandId::parse("layout.toc.generate").unwrap()),
        revision,
        CorrelationId::new(1),
        None,
    )
}

/// Paragraph texts, styles and where each sits: (frame layer index, page index) or overset.
/// Paragraph text, paragraph style number, (frame layer index, page index) or overset.
type Part = (&'static str, Option<u32>, Option<(usize, usize)>);
const PARTS: [Part; 6] = [
    ("Alpha\n", Some(1), Some((0, 0))),
    ("beta\u{2028}two\n", Some(2), Some((0, 0))),
    ("Gamma\n", Some(1), Some((0, 1))),
    ("Secret\n", Some(1), Some((1, 1))),
    ("body\n", None, Some((0, 1))),
    ("Delta\n", Some(1), None),
];

struct Fixture {
    doc: StudioDocument,
    pages: Vec<PageNumber>,
    story: StudioTextStory,
    text: String,
    placement: LayoutProposal,
}

fn line(start: u64, end: u64) -> Line {
    Line {
        paragraph: 0,
        baseline_y_pt: 10.0,
        x_start_pt: 0.0,
        width_pt: 10.0,
        available_pt: 50.0,
        hang_pt: 0.0,
        source: SourceSpan { start, end },
        glyph_range: 0..1,
        overfull: false,
        ratio: None,
        hyphenated: false,
        justification: None,
        column: 0,
        start_part: None,
        end_part: None,
    }
}

fn fixture() -> Fixture {
    let mut hidden = frame_layer(2, 1, spec(1, 10.0, 100.0, 180.0, 80.0), None, None);
    hidden.visible = false;
    let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 180.0), None, None), hidden], [PageSide::SingleSided; 2]);
    let pages = number_pages(&doc, &[]).unwrap();
    let mut text = String::new();
    let mut paragraphs = Vec::new();
    let mut frames = Vec::new();
    for (t, style, at) in PARTS {
        let start = text.len() as u64;
        text.push_str(t);
        let end = text.len() as u64;
        paragraphs.push(StoryParagraph { start, end, style_ref: style.map(stys), paragraph_direction: None });
        if let Some((layer, page)) = at {
            frames.push(FramePlacement {
                frame_id: doc.layers[layer].layer_id.clone(),
                frame_index: layer,
                spread: 0,
                page,
                origin_x_pt: 0.0,
                origin_y_pt: 5.0,
                lines: vec![line(start, end)],
            });
        }
    }
    let overset_start = paragraphs[5].start;
    let story = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.clone() },
        paragraphs,
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: None }],
    };
    let placement = LayoutProposal { base_revision: 2, frames, overset: Some(SourceSpan { start: overset_start, end: text.len() as u64 }) };
    Fixture { doc, pages, story, text, placement }
}

fn style() -> TocStyle {
    TocStyle {
        name: "Contents".into(),
        title: "Contents".into(),
        title_style: Some(para(9)),
        include_book_documents: false,
        include_hidden: false,
        run_in: false,
        create_bookmarks: false,
        make_anchor: false,
        remove_forced_line_break: true,
        numbered_paragraphs: None,
        set_story_direction: None,
        default_styled: false,
        entries: vec![
            TocEntryStyle {
                name: para(1),
                format_style: Some(para(11)),
                level: 1,
                page_number_style: Some(chr(21)),
                page_number_position: TocPageNumberPosition::After,
                separator: " .. ".into(),
                separator_style: Some(chr(22)),
                sort_alphabet: false,
            },
            TocEntryStyle {
                name: para(2),
                format_style: Some(para(12)),
                level: 2,
                page_number_style: None,
                page_number_position: TocPageNumberPosition::Before,
                separator: " ".into(),
                separator_style: None,
                sort_alphabet: false,
            },
        ],
    }
}

fn request<'a>(f: &'a Fixture, style: &'a TocStyle, sources: &'a [TocSource<'a>]) -> TocGenerateRequest<'a> {
    TocGenerateRequest {
        document: &f.doc,
        style,
        styles: None,
        sources,
        pages: &f.pages,
        replace_existing: false,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        place_point: None,
        include_overset: false,
        locked_stories: &[],
        existing_anchors: &[],
        limits: TocLimits::default(),
    }
}

fn text_of(s: &StudioTextStory) -> &str {
    match &s.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => panic!("stored"),
    }
}

/// STU-LAY-044/148 style-driven generation: mapped styles, levels, page-number position, separator,
/// typed paragraph and character styles on the generated story; hidden and overset left out and
/// counted (MT-173 AC-001, MT-30776 AC-001).
#[test]
fn toc_style_driven_levels_positions_separators_and_styles() {
    let f = fixture();
    let st = style();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let g = generate_table_of_contents(&ctx(2), &request(&f, &st, &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta two\nGamma .. 2");
    let pstyles: Vec<Option<String>> = g.story.paragraphs.iter().map(|p| p.style_ref.clone()).collect();
    assert_eq!(pstyles, vec![Some(stys(9)), Some(stys(11)), Some(stys(12)), Some(stys(11))]);
    let ranges: Vec<(u64, u64, Option<String>)> = g.story.character_ranges.iter().map(|r| (r.start, r.end, r.style_ref.clone())).collect();
    assert_eq!(
        ranges,
        vec![(0, 14, None), (14, 18, Some(stys(22))), (18, 19, Some(stys(21))), (19, 36, None), (36, 40, Some(stys(22))), (40, 41, Some(stys(21)))]
    );
    let levels: Vec<u32> = g.entries.iter().map(|e| e.level).collect();
    assert_eq!(levels, [1, 2, 1]);
    assert_eq!((g.omitted_hidden, g.omitted_overset), (1, 1), "hidden Secret and overset Delta are counted, not dropped silently");
    assert_eq!(g.story.story_id, "STXT-019abcde-0000-7000-8000-000000000009");
    assert!(!g.replaced);
    assert!(validate_text_story(&g.story, Budget::default(), &cancel, &Unresolved).is_ok(), "generated story tiles per Folio rules");
}

/// The include-overset flag is a generate argument (STU-LAY-148, MT-30777 AC-001): with it the
/// overset Delta is listed without a page; `include_hidden` lists Secret.
#[test]
fn toc_include_overset_and_include_hidden_flags() {
    let f = fixture();
    let mut st = style();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let r = TocGenerateRequest { include_overset: true, ..request(&f, &st, &src) };
    let g = generate_table_of_contents(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta two\nGamma .. 2\nDelta");
    assert_eq!(g.entries[3].page_label, None);
    assert_eq!(g.omitted_overset, 0);
    st.include_hidden = true;
    let g = generate_table_of_contents(&ctx(2), &request(&f, &st, &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta two\nGamma .. 2\nSecret .. 2");
    assert_eq!(g.omitted_hidden, 0);
}

/// `remove_forced_line_break`, `run_in` and `sort_alphabet` (STU-LAY-148 style fields).
#[test]
fn toc_forced_breaks_run_in_and_sort() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let mut keep = style();
    keep.remove_forced_line_break = false;
    let g = generate_table_of_contents(&ctx(2), &request(&f, &keep, &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta\u{2028}two\nGamma .. 2");
    let mut run_in = style();
    run_in.run_in = true;
    let g = generate_table_of_contents(&ctx(2), &request(&f, &run_in, &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), format!("Contents\nAlpha .. 1{RUN_IN_JOINER}1 beta two\nGamma .. 2"));
    assert_eq!(g.entries.iter().map(|e| e.run_in).collect::<Vec<_>>(), [false, true, false]);
    assert_eq!(g.story.paragraphs.len(), 3);
    let mut sorted = style();
    sorted.entries[0].sort_alphabet = true;
    let r = TocGenerateRequest { include_overset: true, ..request(&f, &sorted, &src) };
    let g = generate_table_of_contents(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta two\nDelta\nGamma .. 2");
}

/// `make_anchor` creates live anchors in the source paragraphs (reusing an existing one) and
/// `create_bookmarks` emits bookmarks bound to them (MT-173 AC-004); a locked source story is
/// reported per entry, never silently skipped (MT-30777 AC-003).
#[test]
fn toc_anchors_bookmarks_and_locked_story_outcome() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let mut st = style();
    st.make_anchor = true;
    st.create_bookmarks = true;
    let existing = [StudioTextAnchor { anchor_id: "toc-anchor-1".into(), name: "TOC anchor toc-anchor-2".into(), story_id: id("STXT", 1), byte: 0 }];
    let r = TocGenerateRequest { existing_anchors: &existing, ..request(&f, &st, &src) };
    let g = generate_table_of_contents(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(g.entries[0].anchor, Some(TocAnchorOutcome::Existing("toc-anchor-1".into())));
    let created: Vec<&StudioTextAnchor> = g.created_anchors().collect();
    assert_eq!(created.len(), 2);
    assert_eq!((created[0].byte, created[1].byte), (6, 17), "beta and Gamma paragraph starts");
    assert!(created.iter().all(|a| a.anchor_id != "toc-anchor-1" && a.name != "TOC anchor toc-anchor-2"), "fresh ids and names");
    assert_ne!(created[0].anchor_id, created[1].anchor_id);
    assert_eq!(g.bookmarks.len(), 3);
    assert_eq!(g.bookmarks[0].anchor_id.as_deref(), Some("toc-anchor-1"));
    assert_eq!(g.bookmarks[1].anchor_id.as_deref(), Some(created[0].anchor_id.as_str()));
    assert_eq!((g.bookmarks[2].level, g.bookmarks[2].title.as_str(), g.bookmarks[2].page_id.as_deref()), (1, "Gamma", Some("page-2")));
    assert_eq!(g.bookmarks[0].baseline_y, Some(15.0));
    // Locked source story: no anchor, a typed outcome on every entry.
    let locked = [id("STXT", 1)];
    let r = TocGenerateRequest { locked_stories: &locked, ..request(&f, &st, &src) };
    let g = generate_table_of_contents(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(g.locked_anchor_count(), 3);
    assert_eq!(g.entries[2].anchor, Some(TocAnchorOutcome::NotCreatedStoryLocked { story_id: id("STXT", 1), source_start: 17 }));
    assert_eq!(g.created_anchors().count(), 0);
    assert_eq!(g.bookmarks[0].anchor_id, None);
}

/// Multiple TOCs with independent settings and update-all refreshing every one, content replaced
/// in place (MT-173 AC-002/AC-003, MT-30777 AC-002).
#[test]
fn toc_multiple_independent_tocs_and_update_all() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let contents = style();
    let mut headings = style();
    headings.name = "Headings only".into();
    headings.title = String::new();
    headings.entries.truncate(1);
    headings.entries[0].page_number_position = TocPageNumberPosition::None;
    headings.make_anchor = true;
    let mut old_a = generate_table_of_contents(&ctx(2), &request(&f, &contents, &src), &cancel).unwrap().story;
    old_a.text_kind = StoryTextKind::Point;
    let mut old_b = old_a.clone();
    old_b.story_id = id("STXT", 10);
    old_b.content = StoryContent::Inline { text_utf8: "stale".into() };
    old_b.paragraphs = vec![StoryParagraph { start: 0, end: 5, style_ref: None, paragraph_direction: None }];
    old_b.character_ranges = vec![StoryRange { start: 0, end: 5, style_ref: None, applied_language: None }];
    let tocs = [TocInstance { style: &contents, story: &old_a }, TocInstance { style: &headings, story: &old_b }];
    let base = request(&f, &contents, &src);
    let all = update_all_tables_of_contents(&ctx(2), &base, &tocs, &cancel).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(text_of(&all[0].story), "Contents\nAlpha .. 1\n1 beta two\nGamma .. 2");
    assert_eq!(text_of(&all[1].story), "Alpha\nGamma");
    assert_eq!((all[1].story.story_id.as_str(), all[1].replaced), (id("STXT", 10).as_str(), true));
    assert_eq!(all[0].story.text_kind, StoryTextKind::Point, "replace keeps the story's other properties");
    assert_eq!(all[1].created_anchors().count(), 2, "only the TOC with make_anchor creates anchors");
    // Shared names or stories are refused with the TOC index; nothing is updated.
    let twin = [TocInstance { style: &contents, story: &old_a }, TocInstance { style: &contents, story: &old_b }];
    assert_eq!(update_all_tables_of_contents(&ctx(2), &base, &twin, &cancel).unwrap_err(), TocBatchError { toc: 1, error: TocCommandError::NameDuplicate });
    let shared = [TocInstance { style: &contents, story: &old_a }, TocInstance { style: &headings, story: &old_a }];
    assert_eq!(update_all_tables_of_contents(&ctx(2), &base, &shared, &cancel).unwrap_err().error.code(), "toc_story_shared");
    // A single update replaces content in place.
    let u = update_table_of_contents(&ctx(2), &request(&f, &headings, &src), &old_b, &cancel).unwrap();
    assert_eq!((u.story.story_id.as_str(), text_of(&u.story)), (id("STXT", 10).as_str(), "Alpha\nGamma"));
}

/// The three STU-LAY-148 typed errors plus the other refusals (MT-30777 AC-003).
#[test]
fn toc_typed_errors() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let run = |s: &TocStyle| generate_table_of_contents(&ctx(2), &request(&f, s, &src), &cancel).map(|_| ()).map_err(|e| e.code());
    let mut none = style();
    none.entries.clear();
    assert_eq!(run(&none), Err("toc_no_entries"));
    none.default_styled = true;
    let g = generate_table_of_contents(&ctx(2), &request(&f, &none, &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents", "the default-styled TOC is the one style allowed without entries");
    let mut twice = style();
    twice.entries[1].name = para(1);
    assert_eq!(run(&twice), Err("toc_style_mapped_twice"));
    let mut level = style();
    level.entries[1].level = 0;
    assert_eq!(run(&level), Err("toc_level"));
    let mut sep = style();
    sep.entries[0].separator = "a\nb".into();
    assert_eq!(run(&sep), Err("toc_separator"));
    let mut feeds = style();
    feeds.entries[1].format_style = Some(para(1));
    assert_eq!(run(&feeds), Err("toc_loop"));
    let mut blank = style();
    blank.name = " ".into();
    assert_eq!(run(&blank), Err("toc_name"));
    assert_eq!(ParagraphStyleRef::parse("SLYR-019abcde-0000-7000-8000-000000000001"), Err(StyleRefError));
    assert_eq!(StyleRefError.code(), "style_ref_invalid");
    let st = style();
    let base = request(&f, &st, &src);
    let code = |r: &TocGenerateRequest<'_>, c: &CommandContext| generate_table_of_contents(c, r, &cancel).map(|_| ()).map_err(|e| e.code());
    // Writing into a locked story fails closed; the target cannot be a source.
    let locked = ["STXT-019abcde-0000-7000-8000-000000000009".to_string()];
    assert_eq!(code(&TocGenerateRequest { locked_stories: &locked, ..base }, &ctx(2)), Err("toc_story_locked"));
    assert_eq!(code(&TocGenerateRequest { new_story_id: &f.story.story_id, ..base }, &ctx(2)), Err("toc_loop"));
    assert_eq!(code(&TocGenerateRequest { new_story_id: "story-9", ..base }, &ctx(2)), Err("toc_story_id"));
    // Attribution: another revision or no command id is refused.
    assert_eq!(code(&base, &ctx(3)), Err("stale_revision"));
    let anonymous = CommandContext::new(ctx(2).actor().clone(), ClientSessionId::parse("w").unwrap(), None, 2, CorrelationId::new(1), None);
    assert_eq!(code(&base, &anonymous), Err("command_id_missing"));
    // Place point must name a page of the document.
    let bad = PlacePoint { page_id: "page-9".into(), x_pt: 0.0, y_pt: 0.0 };
    assert_eq!(code(&TocGenerateRequest { place_point: Some(&bad), ..base }, &ctx(2)), Err("toc_place_point"));
    let nan = PlacePoint { page_id: "page-1".into(), x_pt: f64::NAN, y_pt: 0.0 };
    assert_eq!(code(&TocGenerateRequest { place_point: Some(&nan), ..base }, &ctx(2)), Err("toc_place_point"));
    let good = PlacePoint { page_id: "page-1".into(), x_pt: 20.0, y_pt: 30.0 };
    let g = generate_table_of_contents(&ctx(2), &TocGenerateRequest { place_point: Some(&good), ..base }, &cancel).unwrap();
    assert_eq!(g.place_point, Some(good.clone()));
    // Budgets (hostile sources): entry count and text bytes.
    let small = TocLimits { max_entries: 2, ..TocLimits::default() };
    assert_eq!(code(&TocGenerateRequest { limits: small, ..base }, &ctx(2)), Err("budget_exceeded"));
    let tiny = TocLimits { max_text_bytes: 12, ..TocLimits::default() };
    assert_eq!(code(&TocGenerateRequest { limits: tiny, ..base }, &ctx(2)), Err("budget_exceeded"));
    let mut huge = style();
    huge.entries = (0..300).map(|k| TocEntryStyle { name: para(1000 + k), ..style().entries[0].clone() }).collect();
    assert_eq!(run(&huge), Err("budget_exceeded"));
    // Canceled before any work.
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(generate_table_of_contents(&ctx(2), &base, &stop).unwrap_err().code(), "canceled");
}

/// Style references resolve against the type-style records with their scope.
#[test]
fn toc_style_references_checked_against_style_records() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let make = |n: u32, scope: TypeStyleScope| StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: stys(n),
        scope,
        name: format!("s/{n}"),
        based_on: None,
        next_style: None,
        font_family: None,
        font_style: None,
        font_size: None,
        fill: None,
        line_height: None,
        letter_spacing: None,
        align: None,
        grep_styles: Vec::new(),
    };
    let mut records = StudioTypeStyleRecords::new(id("SDOC", 1));
    for n in [1, 2, 9, 11, 12] {
        records.styles.push(make(n, TypeStyleScope::Paragraph));
    }
    records.styles.push(make(22, TypeStyleScope::Character));
    // stys(21) is the page-number style and is missing.
    let st = style();
    let r = TocGenerateRequest { styles: Some(&records), ..request(&f, &st, &src) };
    assert_eq!(generate_table_of_contents(&ctx(2), &r, &cancel).unwrap_err().code(), "style_missing");
    records.styles.push(make(21, TypeStyleScope::Paragraph));
    let r = TocGenerateRequest { styles: Some(&records), ..request(&f, &st, &src) };
    assert_eq!(generate_table_of_contents(&ctx(2), &r, &cancel).unwrap_err().code(), "style_scope");
    records.styles.last_mut().unwrap().scope = TypeStyleScope::Character;
    let r = TocGenerateRequest { styles: Some(&records), ..request(&f, &st, &src) };
    assert!(generate_table_of_contents(&ctx(2), &r, &cancel).is_ok());
}

/// `include_book_documents`: without a combined book the command returns the typed unavailable
/// outcome; with one, entries come from the combined book; a chapter missing from it is typed.
#[test]
fn toc_book_documents_unavailable_missing_and_combined() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let mut st = style();
    st.include_book_documents = true;
    let e = generate_table_of_contents(&ctx(2), &request(&f, &st, &src), &cancel).unwrap_err();
    assert_eq!((e.code(), e.failure_code()), ("toc_book_unavailable", Some(FailureCode::Unavailable)));
    let mut book = StudioBook::new("book", "Book");
    book.add_chapter(&f.doc.document_id).unwrap();
    let combined = CombinedBook {
        document: f.doc.clone(),
        sections: Vec::new(),
        pages: f.pages.clone(),
        chapter_pages: vec![(f.doc.document_id.clone(), 0, 1)],
        blank_pages: Vec::new(),
    };
    let r = TocGenerateRequest { book: Some(TocBook { book: &book, combined: &combined }), ..request(&f, &st, &src) };
    let g = generate_table_of_contents(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Contents\nAlpha .. 1\n1 beta two\nGamma .. 2");
    book.add_chapter(&id("SDOC", 7)).unwrap();
    let r = TocGenerateRequest { book: Some(TocBook { book: &book, combined: &combined }), ..request(&f, &st, &src) };
    assert_eq!(generate_table_of_contents(&ctx(2), &r, &cancel).unwrap_err(), TocCommandError::DocumentMissingFromBook(id("SDOC", 7)));
}

/// The full style table and entry table are carried (field census), the enumerations round-trip,
/// and a Folio style lifts with generate_toc's behaviour.
#[test]
fn toc_record_fields_enumerations_and_folio_lift() {
    let st = style();
    // Eleven style-table fields.
    let TocStyle {
        name: _,
        title: _,
        title_style: _,
        include_book_documents: _,
        include_hidden: _,
        run_in: _,
        create_bookmarks: _,
        make_anchor: _,
        remove_forced_line_break: _,
        numbered_paragraphs: _,
        set_story_direction: _,
        default_styled: _,
        entries,
    } = st.clone();
    // Eight entry-table fields.
    let TocEntryStyle { name: _, format_style: _, level: _, page_number_style: _, page_number_position: _, separator: _, separator_style: _, sort_alphabet: _ } =
        entries[0].clone();
    assert_eq!(TocStoryDirection::ALL.map(|d| d.code()), ["direction.horizontal", "direction.vertical"]);
    for d in TocStoryDirection::ALL {
        assert_eq!(TocStoryDirection::from_code(d.code()), Some(d));
    }
    assert_eq!(TocStoryDirection::from_code("direction.Vertical"), None);
    assert!(CapturedToken::parse("numbered.include_full_paragraph").is_some());
    assert!(CapturedToken::parse("Has Space").is_none());
    assert!(CapturedToken::parse(&"a".repeat(MAX_CAPTURED_TOKEN_BYTES + 1)).is_none());
    let folio = StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: "Contents".into(),
        title_style: Some(stys(9)),
        include_overset: false,
        entries: vec![StudioTocEntry { paragraph_style: stys(1), format_style: Some(stys(11)), level: 1, page_number_position: TocPageNumberPosition::After, separator: " ".into(), sort_alphabet: false }],
    };
    let lifted = TocStyle::from_folio(&folio).unwrap();
    assert!(lifted.include_hidden && lifted.remove_forced_line_break && !lifted.make_anchor && !lifted.run_in);
    assert_eq!(lifted.entries[0].name, para(1));
    let mut bad = folio.clone();
    bad.entries[0].paragraph_style = "not-a-style".into();
    assert_eq!(TocStyle::from_folio(&bad).unwrap_err(), TocCommandError::StyleRef);
    // Equivalence with generate_toc for the lifted style.
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let old = generate_toc(&f.doc, &folio, &src, &f.pages, "STXT-019abcde-0000-7000-8000-000000000009", TocLimits::default(), &cancel).unwrap();
    let new = generate_table_of_contents(&ctx(2), &request(&f, &lifted, &src), &cancel).unwrap();
    assert_eq!(text_of(&new.story), old.text);
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

/// Every terminal outcome goes through Observe with the command's correlation and revision.
#[test]
fn toc_outcomes_report_through_observe() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let st = style();
    let ok = generate_table_of_contents(&ctx(2), &request(&f, &st, &src), &cancel);
    let mut observe = Observe::new(1, 2, resource.clone(), actor.clone(), hsk_studio_observe::Budget::new(0, 0).unwrap());
    let receipt = report_command(&ok, TocCommandError::failure_code, &ctx(2), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!((receipt.outcome, receipt.correlation_id, receipt.revision), (Outcome::Success, 1, 2));
    let mut book_style = style();
    book_style.include_book_documents = true;
    let refused = generate_table_of_contents(&ctx(2), &request(&f, &book_style, &src), &cancel);
    let mut observe = Observe::new(1, 2, resource, actor, hsk_studio_observe::Budget::new(0, 0).unwrap());
    let receipt = report_command(&refused, TocCommandError::failure_code, &ctx(2), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Unavailable));
}
