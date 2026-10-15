//! SMB-LAYOUT-LONGDOC slice 3: sections, page numbering and tables of contents. Hand-computed
//! oracles.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;

fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn page(k: &str) -> StudioPage {
    let e = LayoutEdges { top: len(0.0), right: len(0.0), bottom: len(0.0), left: len(0.0) };
    StudioPage { page_id: k.into(), width: len(100.0), height: len(100.0), side: PageSide::SingleSided, margins: e, bleed: None }
}
fn spread(n: u32, pages: &[&str]) -> StudioPageSpread {
    StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: format!("SPGS-019abcde-0000-7000-8000-{n:012x}"),
        name: format!("s{n}"),
        parent: Parent::Root(()),
        bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0), height: len(100.0) },
        pages: pages.iter().map(|k| page(k)).collect(),
    }
}
/// Pages p1 | p2 p3 | p4 p5 on three spreads.
fn document() -> StudioDocument {
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: "SDOC-019abcde-0000-7000-8000-000000000001".into(),
        revision: 2,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![],
        page_spreads: vec![spread(1, &["p1"]), spread(2, &["p2", "p3"]), spread(3, &["p4", "p5"])],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn section(id: &str, start: &str, style: StudioNumberStyle) -> StudioSection {
    StudioSection {
        section_id: id.into(),
        name: format!("section {id}"),
        page_start: start.into(),
        continue_numbering: true,
        page_number_start: None,
        page_number_style: style,
        include_section_prefix: false,
        section_prefix: None,
        marker: None,
    }
}
fn sections() -> Vec<StudioSection> {
    let mut b = section("b", "p3", StudioNumberStyle::Arabic);
    b.continue_numbering = false;
    b.page_number_start = Some(5);
    b.include_section_prefix = true;
    b.section_prefix = Some("A-".into());
    vec![section("a", "p1", StudioNumberStyle::LowerRoman), b, section("c", "p5", StudioNumberStyle::UpperLetters)]
}

/// Labels: p1 i, p2 ii (section a continues), p3 A-5 (restart at 5 with prefix), p4 A-6, p5 G
/// (section c continues: 7 = G). Formats: roman 1994 MCMXCIV; letters 27 aa, 53 aaa; leading zeros
/// 7 -> 07 / 007 / 0007; full-width 12; kanji 2026 二千二十六, 10000 一万, 15 十五, 0 〇; symbols
/// 7 = ** (second round); asterisks 3.
#[test]
fn sections_number_pages_and_formats() {
    let doc = document();
    let pages = number_pages(&doc, &sections()).unwrap();
    let labels: Vec<&str> = pages.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["i", "ii", "A-5", "A-6", "G"]);
    assert_eq!((pages[3].spread, pages[3].page, pages[3].ordinal, pages[3].section), (2, 0, 4, Some(1)));
    assert_eq!(last_page_label(&pages, NumberScope::Document, 0), Some("G"));
    assert_eq!(last_page_label(&pages, NumberScope::Section, 2), Some("A-6"));
    let plain = number_pages(&doc, &[]).unwrap();
    assert_eq!(plain.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["1", "2", "3", "4", "5"]);
    use StudioNumberStyle::*;
    let f = |n, s| format_number(n, s).unwrap();
    assert_eq!(f(1994, UpperRoman), "MCMXCIV");
    assert_eq!((f(27, LowerLetters).as_str(), f(53, LowerLetters).as_str(), f(2, UpperLetters).as_str()), ("aa", "aaa", "B"));
    assert_eq!((f(7, SingleLeadingZeros), f(7, DoubleLeadingZeros), f(7, TripleLeadingZeros)), ("07".into(), "007".into(), "0007".into()));
    assert_eq!(f(12, FullWidthArabic), "\u{ff11}\u{ff12}");
    assert_eq!((f(2026, Kanji).as_str(), f(10_000, Kanji).as_str(), f(15, Kanji).as_str(), f(0, Kanji).as_str()), ("二千二十六", "一万", "十五", "〇"));
    assert_eq!((f(7, Symbols).as_str(), f(3, Asterisks).as_str(), f(9, FormatNone).as_str()), ("**", "***", ""));
    assert_eq!(format_number(4000, UpperRoman), Err(NumberingError::OutOfRange));
    assert_eq!(format_number(0, LowerLetters), Err(NumberingError::OutOfRange));
    assert_eq!(format_number(26 * 64 + 1, LowerLetters), Err(NumberingError::OutOfRange));
    assert_eq!(format_number(3, Current), Err(NumberingError::NotLegalHere));
    assert_eq!(format_number(3, HebrewBiblical), Err(NumberingError::StyleUnsupported(HebrewBiblical)));
}

/// STU-LAY-108 section rules, each a typed refusal with Folio's code.
#[test]
fn section_rules_refused() {
    let doc = document();
    let refuse = |edit: &dyn Fn(&mut Vec<StudioSection>)| {
        let mut s = sections();
        edit(&mut s);
        number_pages(&doc, &s).unwrap_err().code()
    };
    assert_eq!(refuse(&|s| s[1].section_prefix = Some("ABCDEF".into())), "section_prefix_too_long");
    assert_eq!(refuse(&|s| s[1].section_prefix = Some("A,".into())), "section_prefix_character");
    assert_eq!(refuse(&|s| s[1].section_prefix = Some("A+".into())), "section_prefix_character");
    assert_eq!(
        refuse(&|s| {
            s[0].section_prefix = Some("X".into());
            s[2].section_prefix = Some("X".into());
        }),
        "section_prefix_duplicate"
    );
    assert_eq!(refuse(&|s| s[2].page_start = "p3".into()), "sections_share_start_page");
    assert_eq!(refuse(&|s| s[2].page_number_start = Some(3)), "page_number_start_while_continuing");
    assert_eq!(refuse(&|s| s[1].page_number_start = Some(0)), "page_number_start_range");
    assert_eq!(refuse(&|s| s[1].page_number_start = Some(1_000_000)), "page_number_start_range");
    assert_eq!(refuse(&|s| s[1].name = "  ".into()), "section_name_empty");
    assert_eq!(refuse(&|s| { s.remove(0); }), "first_page_without_section");
    assert_eq!(refuse(&|s| s[2].page_number_style = StudioNumberStyle::Symbols), "number_style_not_legal_for_pages");
    assert_eq!(refuse(&|s| s[2].page_start = "p9".into()), "section_page_missing");
    let mut big = sections();
    big[1].page_number_start = Some(999_999);
    big[2].page_number_style = StudioNumberStyle::Arabic;
    let p = number_pages(&doc, &big).unwrap();
    assert_eq!(p[4].number, 1_000_001, "continuing past the start bound is counted, not wrapped");
}

fn stys(n: u32) -> String {
    format!("STYS-019abcde-0000-7000-8000-{n:012x}")
}
/// Body story: "Beta\nalpha\nGamma\nbody\nDelta\n" with H1 (STYS-1) on Beta, Gamma, Delta and H2
/// (STYS-2) on alpha. Placement (hand-built): Beta and alpha on p2, Gamma on p3, body on p4, Delta
/// is overset. Labels from `sections()`: p2 ii, p3 A-5.
#[test]
fn toc_from_paragraph_styles_with_page_labels_and_loops() {
    let doc = document();
    let pages = number_pages(&doc, &sections()).unwrap();
    let text = "Beta\nalpha\nGamma\nbody\nDelta\n";
    let paras = [(0u64, 5u64, Some(1)), (5, 11, Some(2)), (11, 17, Some(1)), (17, 22, None), (22, 28, Some(1))];
    let story = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: "STXT-019abcde-0000-7000-8000-000000000001".into(),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras.iter().map(|&(start, end, s)| StoryParagraph { start, end, style_ref: s.map(stys), paragraph_direction: None }).collect(),
        character_ranges: vec![StoryRange { start: 0, end: 28, style_ref: None, applied_language: None }],
    };
    let line = |start: u64, end: u64| Line {
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
    };
    let frame = |spread: usize, page: usize, lines: Vec<Line>| FramePlacement {
        frame_id: "f".into(),
        frame_index: 0,
        spread,
        page,
        origin_x_pt: 0.0,
        origin_y_pt: 0.0,
        lines,
    };
    let placement = LayoutProposal {
        base_revision: 2,
        frames: vec![frame(1, 0, vec![line(0, 5), line(5, 11)]), frame(1, 1, vec![line(11, 17)]), frame(2, 0, vec![line(17, 22)])],
        overset: Some(SourceSpan { start: 22, end: 28 }),
    };
    let entry = |style: u32, level: u32, fmt: u32| StudioTocEntry {
        paragraph_style: stys(style),
        format_style: Some(stys(fmt)),
        level,
        page_number_position: TocPageNumberPosition::After,
        separator: " ".into(),
        sort_alphabet: true,
    };
    let toc = StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: "Contents".into(),
        title_style: Some(stys(9)),
        include_overset: false,
        entries: vec![entry(1, 1, 11), entry(2, 2, 12)],
    };
    let source = [TocSource { story: &story, text, placement: Some(&placement) }];
    let cancel = CancellationToken::default();
    let target = "STXT-019abcde-0000-7000-8000-000000000009";
    let g = generate_toc(&doc, &toc, &source, &pages, target, TocLimits::default(), &cancel).unwrap();
    // Level 1 siblings sorted: Beta (ii) with its child alpha (ii), Gamma (A-5); Delta is overset.
    assert_eq!(g.text, "Contents\nBeta ii\nalpha ii\nGamma A-5");
    let styles: Vec<Option<String>> = g.paragraphs.iter().map(|p| p.style_ref.clone()).collect();
    assert_eq!(styles, vec![Some(stys(9)), Some(stys(11)), Some(stys(12)), Some(stys(11))]);
    assert_eq!((g.paragraphs[1].start, g.paragraphs[1].end), (9, 17));
    assert!(validate_text_story(&g.to_story(target), Budget::default(), &cancel, &Unresolved).is_ok());
    let mut with_overset = toc.clone();
    with_overset.include_overset = true;
    with_overset.entries[0].page_number_position = TocPageNumberPosition::Before;
    let g = generate_toc(&doc, &with_overset, &source, &pages, target, TocLimits::default(), &cancel).unwrap();
    assert_eq!(g.text, "Contents\nii Beta\nalpha ii\nDelta\nA-5 Gamma");
    assert_eq!(g.entries[2].page_label, None);
    // Loops, double mapping, no entries, budget.
    let run = |t: &StudioTocStyle, target: &str, limits: TocLimits| generate_toc(&doc, t, &source, &pages, target, limits, &cancel).map(|_| ()).map_err(TocError::code);
    assert_eq!(run(&toc, &story.story_id, TocLimits::default()), Err("toc_loop"));
    let mut feeds = toc.clone();
    feeds.entries[1].format_style = Some(stys(1));
    assert_eq!(run(&feeds, target, TocLimits::default()), Err("toc_loop"));
    let mut twice = toc.clone();
    twice.entries[1].paragraph_style = stys(1);
    assert_eq!(run(&twice, target, TocLimits::default()), Err("toc_style_mapped_twice"));
    let mut none = toc.clone();
    none.entries.clear();
    assert_eq!(run(&none, target, TocLimits::default()), Err("toc_no_entries"));
    assert_eq!(run(&toc, target, TocLimits { max_entries: 2, ..TocLimits::default() }), Err("budget_exceeded"));
    assert_eq!(run(&toc, target, TocLimits { max_text_bytes: 12, ..TocLimits::default() }), Err("budget_exceeded"));
}
