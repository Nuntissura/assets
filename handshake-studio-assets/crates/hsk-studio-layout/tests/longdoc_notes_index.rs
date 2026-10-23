//! SMB-LAYOUT-LONGDOC slice 4: footnotes and index. Hand-computed oracles. Toy font: every glyph
//! advances 6 pt, line height 12, ascent 10 (descent 2). Frames 60 x 60, no insets, first baseline
//! 10: a frame holds five lines (baselines 10..58) and a line holds two 4-letter words.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
fn page(key: &str) -> StudioPage {
    StudioPage { page_id: key.into(), width: len(100.0), height: len(100.0), side: PageSide::RightHand, margins: edges(5.0), bleed: None }
}
/// Frames 1, 2 on page-1 and 3 on page-2 thread story 1.
fn document() -> StudioDocument {
    let frame = |n: u32, page: &str, y: f64| StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: page.into(),
            bounds: Bounds { x: len(10.0), y: len(y), width: len(60.0), height: len(60.0) },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: (n > 1).then(|| id("SLYR", n - 1)),
            next_text_frame: (n < 3).then(|| id("SLYR", n + 1)),
        }),
    };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 4,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", 1),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0), height: len(100.0) },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers: vec![frame(1, "page-1", 0.0), frame(2, "page-1", 40.0), frame(3, "page-2", 0.0)],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn story(n: u32, text: &str) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: vec![StoryParagraph { start: 0, end, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: None }],
    }
}
fn shaped(text: &str) -> Vec<ShapedParagraph> {
    vec![ShapedParagraph::from_fixed_advance(text, SourceSpan { start: 0, end: text.len() as u64 }, 6.0, 12.0, 10.0).unwrap()]
}
/// `n` 4-letter words: word k is the letter ('a' + k % 26) four times.
fn words(n: usize) -> String {
    (0..n).map(|k| char::from(b'a' + (k % 26) as u8).to_string().repeat(4)).collect::<Vec<_>>().join(" ")
}
/// note id, number, label, reference frame, reference position, text frames.
type NoteSummary<'a> = (&'a str, u32, &'a str, usize, [f64; 2], Vec<usize>);
fn options() -> StudioFootnoteOptions {
    let black = StudioColor { r: 0.0, g: 0.0, b: 0.0, colour_profile_id: ProfileBinding::Unassigned(()) };
    let rule = |w: f64| StudioFootnoteRule { weight: len(w), colour: black.clone(), tint: 100.0, left_indent: len(0.0), width: len(20.0), offset: len(1.0) };
    StudioFootnoteOptions {
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: FootnoteRestart::Page,
        prefix: String::new(),
        suffix: ".".into(),
        space_between: len(2.0),
        spacer: len(3.0),
        rule: Some(rule(1.0)),
        continuing_rule: Some(rule(0.5)),
        eos_placement: false,
        no_splitting: false,
    }
}
fn note(n: u32, anchor: u64, body: u32) -> StudioFootnote {
    StudioFootnote { note_id: format!("n{n}"), story_id: id("STXT", 1), anchor, note_story_id: id("STXT", body) }
}
fn run(
    host_text: &str,
    notes: &[(StudioFootnote, StudioTextStory, Vec<ShapedParagraph>)],
    o: &StudioFootnoteOptions,
    max_iterations: usize,
) -> Result<FootnoteProposal, FootnoteError> {
    let doc = document();
    let host = story(1, host_text);
    let paragraphs = shaped(host_text);
    let inputs: Vec<NoteInput<'_>> = notes.iter().map(|(n, s, p)| NoteInput { note: n, story: s, resolved_text: None, paragraphs: p }).collect();
    layout_footnotes(
        &FootnoteRequest {
            reflow: ReflowRequest {
                document: &doc,
                story: &host,
                resolved_text: None,
                paragraphs: &paragraphs,
                obstacles: &[],
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: 4,
                limits: Limits::default(),
            },
            notes: &inputs,
            options: o,
            pages: &[],
            max_iterations,
        },
        &TypesetOptions::default(),
        &CancellationToken::default(),
    )
}
fn with_story(n: StudioFootnote, n_body: u32, text: &str) -> (StudioFootnote, StudioTextStory, Vec<ShapedParagraph>) {
    (n, story(n_body, text), shaped(text))
}

/// Scenario 1: 16 words (8 lines). Note A at byte 5 (line 0) "note" (12 pt); note B at byte 15
/// (line 1) ten words = five lines (60 pt). Pass 1 (no bands): lines 0-4 in frame 1, both
/// references in frame 1, lowest reference line bottom 24, so frame 1's band may hold 60 - 24 - 3 =
/// 33: A (12) + 2 + B's first line (12) = 26; B's lines 2-5 continue in frame 2 (from line top 12:
/// bottoms 12..48 <= 57) -> bands [29, 51, 0]. Pass 2: frame 1 keeps lines with bottoms <= 31 (lines
/// 0, 1), frame 2 none (band top 9), frame 3 lines 2-6, line 7 (byte 70) overset; the references
/// stay -> stable. Band tops: frame 1 60 - 26 = 34 (A baseline 44, B 34 + 14 + 10 = 58), frame 2
/// 60 - 48 = 12 (B baselines 22, 34, 46, 58). Rules at band top - 1: frame 1 first rule (1 pt), frame
/// 2 continuing rule (0.5 pt). References: x after "aaaa " / "cccc " = 30.
/// Scenario 2: 24 words; note A at 5 (line 0) and E at 105 (line 10): pass 1 bands [15, 0, 15],
/// pass 2 frame 1 lines 0-2, frame 2 lines 3-7, frame 3 lines 8-10 (E still there) -> stable; E is on
/// page-2, so page restart numbers both 1, no restart numbers them 1 and 2.
#[test]
fn footnotes_reserve_bands_number_and_continue() {
    let host = words(16);
    let notes = [with_story(note(1, 5, 2), 2, "note"), with_story(note(2, 15, 3), 3, &words(10).replace(|c: char| c.is_alphabetic(), "b"))];
    let p = run(&host, &notes, &options(), 8).unwrap();
    assert_eq!((p.iterations, p.bands.clone()), (2, vec![29.0, 51.0, 0.0]));
    assert_eq!(p.body.frames.iter().map(|f| f.lines.len()).collect::<Vec<_>>(), [2, 0, 5]);
    assert_eq!(p.body.overset.map(|s| s.start), Some(70));
    let summary: Vec<NoteSummary<'_>> =
        p.notes.iter().map(|n| (n.note_id.as_str(), n.number, n.label.as_str(), n.reference_frame, n.reference, n.text_frames.clone())).collect();
    assert_eq!(summary, vec![("n1", 1, "1.", 0, [30.0, 10.0], vec![0]), ("n2", 2, "2.", 0, [30.0, 22.0], vec![0, 1])]);
    let baselines = |k: usize| -> Vec<Vec<f64>> { p.note_text[k].1.frames.iter().map(|f| f.lines.iter().map(|l| l.baseline_y_pt).collect()).collect() };
    assert_eq!(baselines(0), vec![vec![44.0]]);
    assert_eq!(baselines(1), vec![vec![58.0], vec![22.0, 34.0, 46.0, 58.0]]);
    let rules: Vec<(usize, [f64; 2], [f64; 2], f64)> = p.rules.iter().map(|r| (r.thread_index, r.from, r.to, r.weight_pt)).collect();
    assert_eq!(rules, vec![(0, [0.0, 33.0], [20.0, 33.0], 1.0), (1, [0.0, 11.0], [20.0, 11.0], 0.5)]);
    assert!(p.overset_notes.is_empty());

    let host = words(24);
    let notes = [with_story(note(1, 5, 2), 2, "note"), with_story(note(5, 105, 3), 3, "five")];
    let p = run(&host, &notes, &options(), 8).unwrap();
    assert_eq!((p.iterations, p.bands.clone()), (2, vec![15.0, 0.0, 15.0]));
    assert_eq!(p.body.frames.iter().map(|f| f.lines.len()).collect::<Vec<_>>(), [3, 5, 3]);
    assert_eq!(p.notes.iter().map(|n| (n.label.as_str(), n.reference_frame)).collect::<Vec<_>>(), [("1.", 0), ("1.", 2)]);
    let mut o = options();
    o.restart = FootnoteRestart::DontRestart;
    o.numbering_style = StudioNumberStyle::LowerRoman;
    let p = run(&host, &notes, &o, 8).unwrap();
    assert_eq!(p.notes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["i.", "ii."]);
}

/// Footnotes in footnotes, a note story shared, anchors past the story, option bounds, an
/// unsettled band (one pass allowed) and too many notes are typed refusals.
#[test]
fn footnote_refusals() {
    let host = words(16);
    let o = options();
    let code = |notes: &[(StudioFootnote, StudioTextStory, Vec<ShapedParagraph>)], o: &StudioFootnoteOptions, it: usize| run(&host, notes, o, it).unwrap_err().code();
    assert_eq!(code(&[with_story(note(1, 5, 1), 1, "self")], &o, 8), "footnote_in_footnote");
    let nested = StudioFootnote { story_id: id("STXT", 2), ..note(2, 0, 3) };
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a"), with_story(nested, 3, "b")], &o, 8), "footnote_in_footnote");
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a"), with_story(note(2, 6, 2), 2, "a")], &o, 8), "note_story_shared");
    assert_eq!(code(&[with_story(note(1, 500, 2), 2, "a")], &o, 8), "invalid_input");
    let mut bad = o.clone();
    bad.start_at = 0;
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a")], &bad, 8), "footnote_start_at_range");
    bad.start_at = 401;
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a")], &bad, 8), "footnote_start_at_range");
    let mut bad = o.clone();
    bad.numbering_style = StudioNumberStyle::Current;
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a")], &bad, 8), "number_style_not_legal_for_footnotes");
    assert_eq!(code(&[with_story(note(1, 5, 2), 2, "a")], &o, 1), "footnote_layout_unstable");
    let one = with_story(note(1, 5, 2), 2, "a");
    let many: Vec<_> = (0..=MAX_FOOTNOTES).map(|_| one.clone()).collect();
    assert_eq!(code(&many, &o, 8), "too_many_footnotes");
}

fn stys(n: u32) -> String {
    id("STYS", n)
}
/// Five single-page spreads p1..p5 labelled 1..5 (no sections); one hand-built placement frame per
/// page; story 1 bytes 0..9 sit on p1, 10..19 on p2, ... 40..49 on p5; byte 60 is overset.
/// Marks: Apple on p1, p2, p3 (three consecutive pages -> "1–3"), Apple > Green on p5, banana
/// (sorted as "Banana") on p2, Cherry with a see reference to Apple, 123 on p4 (catch-all
/// "Symbols" group), zeta overset (unplaced).
#[test]
fn index_from_marks_nested_run_in_and_refusals() {
    let spread = |n: u32| StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: id("SPGS", n),
        name: format!("s{n}"),
        parent: Parent::Root(()),
        bounds: Bounds { x: len(0.0), y: len(0.0), width: len(100.0), height: len(100.0) },
        pages: vec![page(&format!("p{n}"))],
    };
    let mut doc = document();
    doc.page_spreads = (1..=5).map(spread).collect();
    doc.layers.clear();
    let pages = number_pages(&doc, &[]).unwrap();
    let text = "x".repeat(70);
    let st = story(1, &text);
    let line = |start: u64| Line {
        paragraph: 0,
        baseline_y_pt: 10.0,
        x_start_pt: 0.0,
        width_pt: 10.0,
        available_pt: 60.0,
        hang_pt: 0.0,
        source: SourceSpan { start, end: start + 10 },
        glyph_range: 0..1,
        overfull: false,
        ratio: None,
        hyphenated: false,
        justification: None,
        column: 0,
        start_part: None,
        end_part: None,
    };
    let placement = LayoutProposal {
        base_revision: 4,
        frames: (0..5).map(|k| FramePlacement { frame_id: "f".into(), frame_index: 0, spread: k, page: 0, origin_x_pt: 0.0, origin_y_pt: 0.0, lines: vec![line(k as u64 * 10)] }).collect(),
        overset: Some(SourceSpan { start: 50, end: 70 }),
    };
    let mark = |n: u32, at: u64, topics: &[&str]| StudioIndexMark { mark_id: format!("m{n}"), story_id: id("STXT", 1), at, topics: topics.iter().map(|t| t.to_string()).collect(), sort_by: vec![], xref: None };
    let mut banana = mark(5, 12, &["banana"]);
    banana.sort_by = vec!["Banana".into()];
    let mut cherry = mark(6, 60, &["Cherry"]);
    cherry.xref = Some(StudioIndexXref { kind: IndexXrefKind::See, target: vec!["Apple".into()], custom_text: None });
    let marks = vec![mark(1, 1, &["Apple"]), mark(2, 11, &["Apple"]), mark(3, 21, &["Apple"]), mark(4, 41, &["Apple", "Green"]), banana, cherry, mark(7, 31, &["123"]), mark(8, 65, &["zeta"])];
    let options = StudioIndexOptions {
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
    };
    let sources = [TocSource { story: &st, text: &text, placement: Some(&placement) }];
    let cancel = CancellationToken::default();
    let build = |o: &StudioIndexOptions, marks: &[StudioIndexMark], limits: IndexLimits| generate_index(&doc, o, marks, &sources, &pages, None, limits, &cancel);
    let g = build(&options, &marks, IndexLimits::default()).unwrap();
    assert_eq!(g.story.text, "Index\nSymbols\n123, 4\nA\nApple, 1\u{2013}3\nGreen, 5\nB\nbanana, 2\nC\nCherry, See Apple");
    assert_eq!(g.unplaced, 1);
    let styles: Vec<Option<String>> = g.story.paragraphs.iter().map(|p| p.style_ref.clone()).collect();
    assert_eq!(styles[..6], [Some(stys(30)), Some(stys(31)), Some(stys(21)), Some(stys(31)), Some(stys(21)), Some(stys(22))]);
    let mut run_in = options.clone();
    run_in.index_format = IndexFormat::RunIn;
    run_in.include_section_headings = false;
    assert_eq!(build(&run_in, &marks, IndexLimits::default()).unwrap().story.text, "Index\n123, 4\nApple, 1\u{2013}3; Green, 5\nbanana, 2\nCherry, See Apple");
    let mut empty = options.clone();
    empty.include_empty_index_sections = true;
    let t = build(&empty, &marks, IndexLimits::default()).unwrap().story.text;
    assert!(t.starts_with("Index\nSymbols\n123, 4\nA\n") && t.ends_with("Cherry, See Apple\nD\nE\nF\nG\nH\nI\nJ\nK\nL\nM\nN\nO\nP\nQ\nR\nS\nT\nU\nV\nW\nX\nY\nZ"), "{t}");
    // Refusals.
    let refuse = |marks: &[StudioIndexMark], o: &StudioIndexOptions| build(o, marks, IndexLimits::default()).unwrap_err().code();
    assert_eq!(refuse(&[mark(9, 1, &["a", "b", "c", "d", "e"])], &options), "index_topic_levels");
    assert_eq!(refuse(&[mark(9, 1, &["a", " "])], &options), "index_topic_levels");
    let mut custom = mark(9, 1, &["a"]);
    custom.xref = Some(StudioIndexXref { kind: IndexXrefKind::Custom, target: vec!["b".into()], custom_text: None });
    assert_eq!(refuse(&[custom], &options), "index_xref_custom_text");
    let mut sep = options.clone();
    sep.entry_end_separator = "\n".into();
    assert_eq!(refuse(&marks, &sep), "index_separator");
    let mut orphan = options.clone();
    orphan.include_section_headings = false;
    orphan.include_empty_index_sections = true;
    assert_eq!(refuse(&marks, &orphan), "index_empty_sections_without_headings");
    assert_eq!(build(&options, &marks, IndexLimits { max_marks: 3, ..IndexLimits::default() }).unwrap_err(), IndexError::BudgetExceeded);
    assert_eq!(build(&options, &marks, IndexLimits { max_text_bytes: 20, ..IndexLimits::default() }).unwrap_err(), IndexError::BudgetExceeded);
}
