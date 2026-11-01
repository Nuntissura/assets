//! Lane SMB-ID-TOC-INDEX-1 item 2: the index contract (STU-LAY-045, STU-LAY-149; MT-174, MT-30778,
//! MT-30779). Hand-built placements; oracles are hand-computed from the Studio rules in
//! `src/index_contract.rs` (InDesign parity UNVERIFIED, D5).
//!
//! Pages page-1..page-5 (labels 1..5), sections a (page-1..3) and b (page-4..5). Story STXT 1, one
//! line per paragraph: P0 "Apples are red." H p1 | P1 "Bananas." H p2 | P2 "Cherries." B p3 |
//! P3 "Dates." B p4 on the HIDDEN frame layer | P4 "Elder." H p5 | P5 "Figs." B overset.
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use support_layout::{document, frame_layer, id, page, spec};

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
        Some(CommandId::parse("layout.index.generate").unwrap()),
        revision,
        CorrelationId::new(1),
        None,
    )
}

/// Paragraph text, paragraph style number, (frame layer index, page index) or overset.
type Part = (&'static str, u32, Option<(usize, usize)>);
const PARTS: [Part; 6] = [
    ("Apples are red.\n", 1, Some((0, 0))),
    ("Bananas.\n", 1, Some((0, 1))),
    ("Cherries.\n", 2, Some((0, 2))),
    ("Dates.\n", 2, Some((1, 3))),
    ("Elder.\n", 1, Some((0, 4))),
    ("Figs.\n", 2, None),
];

struct Fixture {
    doc: StudioDocument,
    pages: Vec<PageNumber>,
    story: StudioTextStory,
    text: String,
    placement: LayoutProposal,
    /// Paragraph start bytes.
    starts: Vec<u64>,
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

fn section(key: &str, start: &str) -> StudioSection {
    StudioSection {
        section_id: key.into(),
        name: format!("section {key}"),
        page_start: start.into(),
        continue_numbering: true,
        page_number_start: None,
        page_number_style: StudioNumberStyle::Arabic,
        include_section_prefix: false,
        section_prefix: None,
        marker: None,
    }
}

fn fixture() -> Fixture {
    let mut hidden = frame_layer(2, 1, spec(1, 10.0, 100.0, 180.0, 80.0), None, None);
    hidden.visible = false;
    let mut doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 180.0), None, None), hidden], [PageSide::SingleSided; 2]);
    for k in 3..=5 {
        doc.page_spreads[0].pages.push(page(&format!("page-{k}"), PageSide::SingleSided));
    }
    let pages = number_pages(&doc, &[section("a", "page-1"), section("b", "page-4")]).unwrap();
    let mut text = String::new();
    let (mut paragraphs, mut frames, mut starts) = (Vec::new(), Vec::new(), Vec::new());
    for (t, style, at) in PARTS {
        let start = text.len() as u64;
        text.push_str(t);
        let end = text.len() as u64;
        starts.push(start);
        paragraphs.push(StoryParagraph { start, end, style_ref: Some(stys(style)), paragraph_direction: None });
        if let Some((layer, page)) = at {
            frames.push(FramePlacement {
                frame_id: doc.layers[layer].layer_id.clone(),
                frame_index: layer,
                spread: 0,
                page,
                origin_x_pt: 0.0,
                origin_y_pt: 0.0,
                lines: vec![line(start, end)],
            });
        }
    }
    let story = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.clone() },
        paragraphs,
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: None }],
    };
    let placement = LayoutProposal { base_revision: 2, frames, overset: Some(SourceSpan { start: starts[5], end: text.len() as u64 }) };
    Fixture { doc, pages, story, text, placement, starts }
}

fn mark(n: &str, at: u64, topics: &[&str]) -> StudioIndexMark {
    StudioIndexMark { mark_id: n.into(), story_id: id("STXT", 1), at, topics: topics.iter().map(|t| t.to_string()).collect(), sort_by: vec![], xref: None, page_range_scope: None }
}
fn xref(n: &str, at: u64, topic: &str, kind: IndexXrefKind, target: &[&str]) -> StudioIndexMark {
    let mut m = mark(n, at, &[topic]);
    m.xref = Some(StudioIndexXref { kind, target: target.iter().map(|t| t.to_string()).collect(), custom_text: None });
    m
}

fn marks(s: &[u64]) -> Vec<StudioIndexMark> {
    vec![
        mark("m1", s[0], &["Apple"]),
        mark("m2", s[1], &["Apple"]),
        mark("m3", s[2], &["Apple"]),
        mark("m4", s[4], &["Apple", "Green"]),
        mark("mb1", s[0], &["Banana"]),
        mark("mb2", s[2], &["Banana"]),
        xref("mb3", s[5], "Banana", IndexXrefKind::See, &["Cherry"]),
        mark("m6", s[2], &["Cherry"]),
        mark("m7", s[0], &["Date"]),
        mark("m8", s[1], &["Elder"]),
        mark("m9", s[0], &["Fig"]),
        mark("m10", s[4], &["Grape"]),
        mark("m11", s[1], &["Honey"]),
        mark("m12", s[0], &["Kiwi"]),
        mark("m13", s[3], &["Hidden"]),
        mark("m14", s[5], &["Lime"]),
        xref("m15", s[5], "Mango", IndexXrefKind::See, &["Apple"]),
        xref("m16", s[5], "Nut", IndexXrefKind::SeeAlso, &["Apple", "Green"]),
    ]
}

fn scopes() -> Vec<IndexMarkScope> {
    let s = |m: &str, scope: PageRangeScope| IndexMarkScope { mark_id: m.into(), scope };
    vec![
        s("m6", PageRangeScope::ToNextStyleChange),
        s("m7", PageRangeScope::ToNextUseOfStyle(para(2))),
        s("m8", PageRangeScope::ForNextParagraphs(3)),
        s("m9", PageRangeScope::ToEndOfStory),
        s("m10", PageRangeScope::ToEndOfSection),
        s("m11", PageRangeScope::ToEndOfSection),
        s("m12", PageRangeScope::ToEndOfDocument),
        s("m1", PageRangeScope::CurrentPage),
    ]
}

fn options() -> IndexOptions {
    IndexOptions {
        title: "Index".into(),
        title_style: Some(para(30)),
        replace_existing_index: false,
        include_book_documents: false,
        include_hidden_entries: false,
        index_format: IndexFormat::Nested,
        include_section_headings: false,
        include_empty_index_sections: false,
        level_1_style: Some(para(21)),
        level_2_style: Some(para(22)),
        level_3_style: Some(para(23)),
        level_4_style: Some(para(24)),
        section_heading_style: Some(para(25)),
        page_number_style: Some(chr(31)),
        cross_reference_style: Some(chr(32)),
        cross_reference_topic_style: Some(chr(33)),
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "\u{2013}".into(),
        between_page_numbers_separator: " / ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: ".".into(),
    }
}

fn request<'a>(
    f: &'a Fixture,
    o: &'a IndexOptions,
    marks: &'a [StudioIndexMark],
    scopes: &'a [IndexMarkScope],
    sources: &'a [TocSource<'a>],
) -> IndexGenerateRequest<'a> {
    IndexGenerateRequest {
        document: &f.doc,
        options: o,
        styles: None,
        marks,
        scopes,
        sources,
        pages: &f.pages,
        header_set: None,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        locked_stories: &[],
        limits: IndexLimits::default(),
    }
}

fn text_of(s: &StudioTextStory) -> &str {
    match &s.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => panic!("stored"),
    }
}
fn style_at(s: &StudioTextStory, byte: usize) -> Option<String> {
    s.character_ranges.iter().find(|r| r.start as usize <= byte && byte < r.end as usize).and_then(|r| r.style_ref.clone())
}

const NESTED: &str = "Index\nApple, 1\u{2013}3.\nGreen, 5.\nBanana, 1 / 3. See Cherry.\nCherry, 3\u{2013}4.\nDate, 1\u{2013}2.\nElder, 2\u{2013}4.\nFig, 1\u{2013}5.\nGrape, 5.\nHoney, 2\u{2013}3.\nKiwi, 1\u{2013}5.\nMango, See Apple.\nNut, See also Apple: Green.";

/// Topic/subtopic hierarchy from index marks, the six independent separators, page-range scopes,
/// cross-reference forms and the typed styles of the generated story (MT-174 AC-001/AC-002/AC-004,
/// MT-30778 AC-001, MT-30779 AC-001/AC-002).
#[test]
fn index_hierarchy_scopes_xrefs_separators_and_styles() {
    let f = fixture();
    let o = options();
    let (m, sc) = (marks(&f.starts), scopes());
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let g = generate_index_contract(&ctx(2), &request(&f, &o, &m, &sc, &src), &cancel).unwrap();
    let t = text_of(&g.story);
    assert_eq!(t, NESTED);
    assert_eq!((g.unplaced, g.omitted_hidden, g.max_levels), (1, 1, 4));
    let pstyles: Vec<Option<String>> = g.story.paragraphs.iter().take(4).map(|p| p.style_ref.clone()).collect();
    assert_eq!(pstyles, vec![Some(stys(30)), Some(stys(21)), Some(stys(22)), Some(stys(21))]);
    let apple = t.find("Apple, 1").unwrap();
    assert_eq!(style_at(&g.story, apple), None, "topic text is unstyled");
    assert_eq!(style_at(&g.story, apple + 7), Some(stys(31)), "page number");
    assert_eq!(style_at(&g.story, apple + 8), None, "range separator");
    assert_eq!(style_at(&g.story, apple + 11), Some(stys(31)), "range end page number");
    let see = t.find("See Cherry").unwrap();
    assert_eq!(style_at(&g.story, see), Some(stys(32)), "cross-reference lead-in");
    assert_eq!(style_at(&g.story, see + 4), Some(stys(33)), "cross-reference topic");
    assert!(validate_text_story(&g.story, Budget::default(), &cancel, &Unresolved).is_ok());
    // Six independent separators, each used as itself.
    let seps = o.separators();
    assert_eq!(seps, [", ", "; ", "\u{2013}", " / ", ". ", "."]);
    // Run-in: level 2 continues the level-1 paragraph after between_entries_separator.
    let run_in = IndexOptions { index_format: IndexFormat::RunIn, ..options() };
    let g = generate_index_contract(&ctx(2), &request(&f, &run_in, &m, &sc, &src), &cancel).unwrap();
    assert!(text_of(&g.story).starts_with("Index\nApple, 1\u{2013}3.; Green, 5.\nBanana, 1 / 3. See Cherry.\n"), "{}", text_of(&g.story));
    assert_eq!(g.story.paragraphs[1].style_ref, Some(stys(21)));
    // Hidden entries included on request.
    let hidden = IndexOptions { include_hidden_entries: true, ..options() };
    let g = generate_index_contract(&ctx(2), &request(&f, &hidden, &m, &sc, &src), &cancel).unwrap();
    assert!(text_of(&g.story).contains("\nHidden, 4.\n"));
    assert_eq!(g.omitted_hidden, 0);
}

/// Studio supports exactly the four captured levels and declares the maximum (MT-30779 AC-001).
#[test]
fn index_four_levels_supported_and_declared() {
    assert_eq!(STUDIO_MAX_INDEX_LEVELS, 4);
    let f = fixture();
    let o = options();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let four = [mark("d", f.starts[0], &["A", "B", "C", "D"])];
    let g = generate_index_contract(&ctx(2), &request(&f, &o, &four, &[], &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Index\nA\nB\nC\nD, 1.");
    let styles: Vec<Option<String>> = g.story.paragraphs.iter().map(|p| p.style_ref.clone()).collect();
    assert_eq!(styles, vec![Some(stys(30)), Some(stys(21)), Some(stys(22)), Some(stys(23)), Some(stys(24))]);
    let five = [mark("e", f.starts[0], &["A", "B", "C", "D", "E"])];
    assert_eq!(
        generate_index_contract(&ctx(2), &request(&f, &o, &five, &[], &src), &cancel).unwrap_err(),
        IndexCommandError::InvalidTopicEntry("e".into())
    );
}

/// Pluggable section-header sets: a non-Latin group, a catch-all, empty sections only with headings.
#[test]
fn index_section_headings_from_pluggable_header_sets() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let o = IndexOptions { include_section_headings: true, ..options() };
    let m = vec![mark("a", f.starts[0], &["Apple"]), mark("k", f.starts[1], &["Kiwi"]), mark("r", f.starts[2], &["\u{308a}\u{3093}\u{3054}"]), mark("n", f.starts[4], &["9 lives"])];
    let set = SectionHeaderSet {
        name: "Fruit groups".into(),
        language_id: "ja".into(),
        header_type: Some(HeaderType::JapaneseHiraganaConsonants),
        groups: vec![
            support_group("A-F", "abcdef"),
            support_group("G-M", "ghijklm"),
            support_group("\u{3089}\u{884c}", "\u{3089}\u{308a}\u{308b}\u{308c}\u{308d}"),
            support_group("Other", ""),
            support_group("Z", "z"),
        ],
    };
    let r = IndexGenerateRequest { header_set: Some(&set), ..request(&f, &o, &m, &[], &src) };
    let g = generate_index_contract(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Index\nA-F\nApple, 1.\nG-M\nKiwi, 2.\n\u{3089}\u{884c}\n\u{308a}\u{3093}\u{3054}, 3.\nOther\n9 lives, 5.");
    assert_eq!(g.story.paragraphs[1].style_ref, Some(stys(25)), "heading style");
    let empty = IndexOptions { include_empty_index_sections: true, ..o.clone() };
    let r = IndexGenerateRequest { header_set: Some(&set), ..request(&f, &empty, &m, &[], &src) };
    let t = generate_index_contract(&ctx(2), &r, &cancel).unwrap();
    assert_eq!(text_of(&t.story), "Index\nA-F\nApple, 1.\nG-M\nKiwi, 2.\n\u{3089}\u{884c}\n\u{308a}\u{3093}\u{3054}, 3.\nOther\n9 lives, 5.\nZ");
    // The default set is Latin.
    let g = generate_index_contract(&ctx(2), &request(&f, &o, &m[..2], &[], &src), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Index\nA\nApple, 1.\nK\nKiwi, 2.");
    assert_eq!(SectionHeaderSet::latin().groups.len(), 27);
    // Header-set bounds.
    let check = |edit: &dyn Fn(&mut SectionHeaderSet)| {
        let mut s = set.clone();
        edit(&mut s);
        s.check().map_err(|e| e.code())
    };
    assert_eq!(check(&|s| s.groups[1].initials.push('a')), Err("header_set_initial_duplicate"));
    assert_eq!(check(&|s| s.groups[1].initials.push('Q')), Err("header_set_initial"));
    assert_eq!(check(&|s| s.groups.clear()), Err("header_set_groups"));
    assert_eq!(check(&|s| s.groups = (0..=MAX_HEADER_GROUPS).map(|k| support_group(&format!("g{k}"), "")).collect()), Err("header_set_groups"));
    assert_eq!(check(&|s| s.language_id = "j a".into()), Err("header_set_language_id"));
    assert_eq!(check(&|s| s.groups[0].heading = " ".into()), Err("header_set_heading"));
    assert_eq!(check(&|s| s.name = String::new()), Err("header_set_name"));
    assert_eq!(check(&|s| s.groups[0].initials = vec!['x'; MAX_HEADER_INITIALS + 1]), Err("budget_exceeded"));
}

fn support_group(heading: &str, initials: &str) -> HeaderGroup {
    HeaderGroup { heading: heading.into(), initials: initials.chars().collect() }
}

/// The five captured index typed errors, each with the offending mark or story.
#[test]
fn index_captured_typed_errors() {
    let f = fixture();
    let o = options();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let run = |m: &[StudioIndexMark], sc: &[IndexMarkScope]| generate_index_contract(&ctx(2), &request(&f, &o, m, sc, &src), &cancel).map(|_| ());
    let ok = mark("ok", f.starts[0], &["Apple"]);
    // Invalid page entry: unknown story, byte past the story, bad scopes.
    let mut foreign = mark("p1", 0, &["Apple"]);
    foreign.story_id = id("STXT", 5);
    assert_eq!(run(&[foreign], &[]), Err(IndexCommandError::InvalidPageEntry("p1".into())));
    assert_eq!(run(&[mark("p2", 9999, &["Apple"])], &[]), Err(IndexCommandError::InvalidPageEntry("p2".into())));
    let zero = [IndexMarkScope { mark_id: "ok".into(), scope: PageRangeScope::ForNextParagraphs(0) }];
    assert_eq!(run(std::slice::from_ref(&ok), &zero), Err(IndexCommandError::InvalidPageEntry("ok".into())));
    let stray = [IndexMarkScope { mark_id: "ghost".into(), scope: PageRangeScope::ToEndOfStory }];
    assert_eq!(run(std::slice::from_ref(&ok), &stray), Err(IndexCommandError::InvalidPageEntry("ghost".into())));
    let twice = [IndexMarkScope { mark_id: "ok".into(), scope: PageRangeScope::ToEndOfStory }, IndexMarkScope { mark_id: "ok".into(), scope: PageRangeScope::CurrentPage }];
    assert_eq!(run(std::slice::from_ref(&ok), &twice), Err(IndexCommandError::InvalidPageEntry("ok".into())));
    // Invalid topic entry.
    assert_eq!(run(&[mark("t1", 0, &["  "])], &[]), Err(IndexCommandError::InvalidTopicEntry("t1".into())));
    assert_eq!(run(&[mark("t2", 0, &[])], &[]), Err(IndexCommandError::InvalidTopicEntry("t2".into())));
    let mut sort = mark("t3", 0, &["A", "B"]);
    sort.sort_by = vec!["a".into()];
    assert_eq!(run(&[sort], &[]), Err(IndexCommandError::InvalidTopicEntry("t3".into())));
    assert_eq!(run(&[ok.clone(), ok.clone()], &[]), Err(IndexCommandError::InvalidTopicEntry("ok".into())));
    assert_eq!(run(&[mark("t4", 0, &[&"x".repeat(MAX_TOPIC_BYTES + 1)])], &[]), Err(IndexCommandError::InvalidTopicEntry("t4".into())));
    // Invalid referenced topic entry: a target naming no topic, custom text rules.
    let nowhere = xref("x1", 0, "Apple", IndexXrefKind::See, &["Zucchini"]);
    assert_eq!(run(&[nowhere], &[]), Err(IndexCommandError::InvalidReferencedTopic("x1".into())));
    let custom = xref("x2", 0, "Apple", IndexXrefKind::Custom, &["Apple"]);
    assert_eq!(run(std::slice::from_ref(&custom), &[]), Err(IndexCommandError::InvalidReferencedTopic("x2".into())));
    let mut with_text = custom;
    with_text.xref.as_mut().unwrap().custom_text = Some("Compare".into());
    assert!(run(&[with_text], &[]).is_ok());
    // Index could not be updated because a story is locked.
    let locked = ["STXT-019abcde-0000-7000-8000-000000000009".to_string()];
    let ok_marks = [ok.clone()];
    let r = IndexGenerateRequest { locked_stories: &locked, ..request(&f, &o, &ok_marks, &[], &src) };
    let e = generate_index_contract(&ctx(2), &r, &cancel).unwrap_err();
    assert_eq!((e.code(), e), ("index_story_locked", IndexCommandError::StoryLocked(locked[0].clone())));
    // Document missing from the book; no book at all.
    let book_opts = IndexOptions { include_book_documents: true, ..options() };
    let e = generate_index_contract(&ctx(2), &request(&f, &book_opts, &ok_marks, &[], &src), &cancel).unwrap_err();
    assert_eq!((e.code(), e.failure_code()), ("index_book_unavailable", Some(FailureCode::Unavailable)));
    let mut book = StudioBook::new("book", "Book");
    book.add_chapter(&f.doc.document_id).unwrap();
    let combined = CombinedBook { document: f.doc.clone(), sections: Vec::new(), pages: f.pages.clone(), chapter_pages: vec![(f.doc.document_id.clone(), 0, 4)], blank_pages: Vec::new() };
    let r = IndexGenerateRequest { book: Some(TocBook { book: &book, combined: &combined }), ..request(&f, &book_opts, &ok_marks, &[], &src) };
    assert_eq!(text_of(&generate_index_contract(&ctx(2), &r, &cancel).unwrap().story), "Index\nApple, 1.");
    book.add_chapter(&id("SDOC", 8)).unwrap();
    let r = IndexGenerateRequest { book: Some(TocBook { book: &book, combined: &combined }), ..request(&f, &book_opts, &ok_marks, &[], &src) };
    let e = generate_index_contract(&ctx(2), &r, &cancel).unwrap_err();
    assert_eq!((e.code(), e), ("book_document_missing", IndexCommandError::DocumentMissingFromBook(id("SDOC", 8))));
}

/// Options rules: an invalid separator set, empty sections without headings, attribution, budgets.
#[test]
fn index_option_rules_attribution_and_budgets() {
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let ok_marks = [mark("ok", f.starts[0], &["Apple"])];
    let code = |o: &IndexOptions| generate_index_contract(&ctx(2), &request(&f, o, &ok_marks, &[], &src), &cancel).map(|_| ()).map_err(|e| e.code());
    let with_sep = |k: usize, v: &str| {
        let mut o = options();
        let slot = [
            &mut o.following_topic_separator,
            &mut o.between_entries_separator,
            &mut o.page_range_separator,
            &mut o.between_page_numbers_separator,
            &mut o.before_cross_reference_separator,
            &mut o.entry_end_separator,
        ];
        if let Some(target) = slot.into_iter().nth(k) {
            *target = v.into();
        }
        o
    };
    for k in 0..6 {
        assert_eq!(code(&with_sep(k, "a\nb")), Err("index_separator"), "separator {k}");
        assert_eq!(code(&with_sep(k, &"x".repeat(101))), Err("index_separator"), "separator {k} length");
        assert!(code(&with_sep(k, &"x".repeat(100))).is_ok(), "separator {k}: 100 characters admitted");
    }
    let o = IndexOptions { include_empty_index_sections: true, ..options() };
    assert_eq!(code(&o), Err("index_empty_sections_without_headings"));
    let o = options();
    let base = request(&f, &o, &ok_marks, &[], &src);
    assert_eq!(generate_index_contract(&ctx(3), &base, &cancel).unwrap_err().code(), "stale_revision");
    let anonymous = CommandContext::new(ctx(2).actor().clone(), ClientSessionId::parse("w").unwrap(), None, 2, CorrelationId::new(1), None);
    assert_eq!(generate_index_contract(&anonymous, &base, &cancel).unwrap_err().code(), "command_id_missing");
    let r = IndexGenerateRequest { limits: IndexLimits { max_marks: 0, ..IndexLimits::default() }, ..base };
    assert_eq!(generate_index_contract(&ctx(2), &r, &cancel).unwrap_err().code(), "budget_exceeded");
    let r = IndexGenerateRequest { limits: IndexLimits { max_text_bytes: 6, ..IndexLimits::default() }, ..base };
    assert_eq!(generate_index_contract(&ctx(2), &r, &cancel).unwrap_err().code(), "budget_exceeded");
    let r = IndexGenerateRequest { new_story_id: "nope", ..base };
    assert_eq!(generate_index_contract(&ctx(2), &r, &cancel).unwrap_err().code(), "index_story_id");
    // replace_existing_index replaces content only.
    let mut old = generate_index_contract(&ctx(2), &base, &cancel).unwrap().story;
    old.story_id = id("STXT", 44);
    old.text_kind = StoryTextKind::Point;
    let replace = IndexOptions { replace_existing_index: true, ..options() };
    let r = IndexGenerateRequest { existing: Some(&old), ..request(&f, &replace, &ok_marks, &[], &src) };
    let g = generate_index_contract(&ctx(2), &r, &cancel).unwrap();
    assert_eq!((g.story.story_id.as_str(), g.story.text_kind, g.replaced), (id("STXT", 44).as_str(), StoryTextKind::Point, true));
    let r = IndexGenerateRequest { existing: Some(&old), ..base };
    let g = generate_index_contract(&ctx(2), &r, &cancel).unwrap();
    assert_eq!((g.story.story_id.as_str(), g.replaced), ("STXT-019abcde-0000-7000-8000-000000000009", false), "no replace: a new story");
}

/// The eight cross-reference types and four capitalisation members by code; capitalisation with
/// the whole-index constraint (MT-30779 AC-003).
#[test]
fn index_xref_codes_and_capitalisation() {
    let codes: Vec<&str> = XREF_CODES.iter().map(|(_, c)| *c).collect();
    assert_eq!(
        codes,
        ["xref.see_or_also_bracket", "xref.see", "xref.see_also", "xref.see_herein", "xref.see_also_herein", "xref.custom", "xref.custom_before", "xref.custom_after"]
    );
    for (k, c) in XREF_CODES {
        assert_eq!((xref_code(k), xref_from_code(c)), (c, Some(k)));
    }
    assert_eq!(xref_from_code("xref.SEE"), None);
    assert_eq!(IndexCaps::ALL.map(IndexCaps::code), ["index_caps.selected_entry", "index_caps.include_subentries", "index_caps.all_level_1", "index_caps.all_entries"]);
    for c in IndexCaps::ALL {
        assert_eq!(IndexCaps::from_code(c.code()), Some(c));
    }
    assert_eq!(IndexCaps::ALL.map(IndexCaps::allowed_for_whole_index), [false, false, true, true]);
    let mut target = mark("x", 0, &["banana"]);
    target.xref = Some(StudioIndexXref { kind: IndexXrefKind::See, target: vec!["apple".into()], custom_text: None });
    let m = vec![mark("a", 0, &["apple", "green"]), target];
    let topics = |v: &[StudioIndexMark]| v.iter().map(|m| m.topics.join("/")).collect::<Vec<_>>();
    let lim = IndexLimits::default();
    let all = capitalize_index_topics(&m, IndexCaps::AllEntries, None, true, lim).unwrap();
    assert_eq!(topics(&all), ["Apple/Green", "Banana"]);
    assert_eq!(all[1].xref.as_ref().unwrap().target, ["Apple"], "cross-reference targets follow");
    assert_eq!(topics(&capitalize_index_topics(&m, IndexCaps::AllLevel1, None, true, lim).unwrap()), ["Apple/green", "Banana"]);
    let sel = ["apple".to_string(), "green".to_string()];
    assert_eq!(topics(&capitalize_index_topics(&m, IndexCaps::SelectedEntry, Some(&sel), false, lim).unwrap()), ["apple/Green", "banana"]);
    let sel = ["apple".to_string()];
    assert_eq!(topics(&capitalize_index_topics(&m, IndexCaps::IncludeSubentries, Some(&sel), false, lim).unwrap()), ["Apple/Green", "banana"]);
    for c in [IndexCaps::SelectedEntry, IndexCaps::IncludeSubentries] {
        assert_eq!(capitalize_index_topics(&m, c, Some(&sel), true, lim).unwrap_err().code(), "index_caps_whole_index");
    }
    assert_eq!(capitalize_index_topics(&m, IndexCaps::SelectedEntry, None, false, lim).unwrap_err().code(), "index_caps_selection");
    let missing = ["cherry".to_string()];
    assert_eq!(capitalize_index_topics(&m, IndexCaps::SelectedEntry, Some(&missing), false, lim).unwrap_err().code(), "index_caps_selection");
    assert_eq!(capitalize_index_topics(&m, IndexCaps::AllEntries, None, true, IndexLimits { max_marks: 1, ..lim }).unwrap_err().code(), "budget_exceeded");
}

/// The header-type model: the spec's named members, captured tokens for the unnamed rest.
#[test]
fn index_header_types_named_and_captured() {
    assert_eq!(CAPTURED_HEADER_TYPE_COUNT, 27);
    assert_eq!(HeaderType::NAMED.len(), 13);
    let codes: std::collections::BTreeSet<String> = HeaderType::NAMED.iter().map(|h| h.code().to_owned()).collect();
    assert_eq!(codes.len(), 13, "distinct codes");
    for h in HeaderType::NAMED {
        assert_eq!(HeaderType::from_code(h.code()), Some(h));
    }
    assert!(matches!(HeaderType::from_code("header.captured_member_14"), Some(HeaderType::Captured(_))));
    assert_eq!(HeaderType::from_code("Bad Token"), None);
}

/// Capture: the selected word, and proper names last-name-first (MT-174 AC-003).
#[test]
fn index_capture_selection_and_proper_names() {
    let text = "Ada Lovelace wrote notes.";
    let st = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 3),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: vec![StoryParagraph { start: 0, end: 25, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end: 25, style_ref: None, applied_language: None }],
    };
    let m = capture_index_mark(&st, text, 0, 12, "c1", true).unwrap();
    assert_eq!((m.topics.clone(), m.at, m.story_id.clone()), (vec!["Lovelace, Ada".to_string()], 0, id("STXT", 3)));
    assert_eq!(capture_index_mark(&st, text, 0, 12, "c2", false).unwrap().topics, ["Ada Lovelace"]);
    assert_eq!(capture_index_mark(&st, text, 19, 24, "c3", true).unwrap().topics, ["notes"]);
    assert_eq!(capture_index_mark(&st, text, 3, 4, "c4", false).unwrap_err(), IndexCommandError::InvalidTopicEntry("c4".into()));
    assert_eq!(capture_index_mark(&st, text, 20, 99, "c5", false).unwrap_err(), IndexCommandError::InvalidPageEntry("c5".into()));
    assert_eq!(capture_index_mark(&st, text, 0, 3, "bad id", false).unwrap_err(), IndexCommandError::InvalidTopicEntry("bad id".into()));
}

/// Folio options lift into the full record; the lifted record reproduces `generate_index`.
#[test]
fn index_options_lift_from_folio_matches_generate_index() {
    let folio = StudioIndexOptions {
        title: "Index".into(),
        title_style: Some(stys(30)),
        index_format: IndexFormat::Nested,
        include_section_headings: true,
        include_empty_index_sections: false,
        level_styles: vec![Some(stys(21)), Some(stys(22))],
        section_heading_style: Some(stys(31)),
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "\u{2013}".into(),
        between_page_numbers_separator: ", ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: String::new(),
        replace_existing_index: None,
        include_book_documents: None,
        include_hidden_entries: None,
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        section_header_set: None,
    };
    let lifted = IndexOptions::from_folio(&folio).unwrap();
    assert_eq!((lifted.level_1_style.clone(), lifted.level_3_style.clone()), (Some(para(21)), None));
    assert!(lifted.include_hidden_entries && !lifted.replace_existing_index && !lifted.include_book_documents);
    let f = fixture();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let m: Vec<StudioIndexMark> = marks(&f.starts);
    let old = generate_index(&f.doc, &folio, &m, &src, &f.pages, None, IndexLimits::default(), &cancel).unwrap();
    let new = generate_index_contract(&ctx(2), &request(&f, &lifted, &m, &[], &src), &cancel).unwrap();
    assert_eq!(text_of(&new.story), old.story.text);
    let mut bad = folio;
    bad.title_style = Some("STXT-019abcde-0000-7000-8000-000000000001".into());
    assert_eq!(IndexOptions::from_folio(&bad).unwrap_err().code(), "index_style_ref");
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn index_outcomes_report_through_observe() {
    let f = fixture();
    let o = options();
    let src = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let bad = [mark("t1", 0, &[" "])];
    let refused = generate_index_contract(&ctx(2), &request(&f, &o, &bad, &[], &src), &cancel);
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let mut observe = Observe::new(1, 2, resource, actor, hsk_studio_observe::Budget::new(0, 0).unwrap());
    let receipt = report_command(&refused, IndexCommandError::failure_code, &ctx(2), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
}
