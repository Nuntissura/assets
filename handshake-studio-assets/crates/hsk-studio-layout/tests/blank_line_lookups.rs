//! Lane SMB-LAYOUT-BLANK-LINE-LOOKUPS-1 (STUDIO-R56 F-R56-5, F-R56-9). A paragraph with no glyph has one
//! placed line whose empty source span sits at the paragraph's end. An index mark or a cross-reference
//! destination in it answers the page the composer placed it on; a mapped heading in it is left out for
//! one typed reason and counted.
//!
//! Table of mark and destination kinds against paragraph shapes (empty, marker only, spaces only, U+2028
//! only, with glyphs) and states (placed, hidden layer, overset). Hand-computed oracles. Toy shaping as
//! Type does it: one 6 pt glyph per character except the glyph-less sentinels, the forced line break and
//! the terminator; line height 12 pt, fixed first baseline 10 pt, 180 pt wide frames. Two threaded frames:
//! page-1 holds one line, page-2 five. Studio readings; InDesign behaviour UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const IDX: char = '\u{FDD7}';
const LS: char = '\u{2028}';

fn glyphless(c: char) -> bool {
    ('\u{FDD0}'..='\u{FDEF}').contains(&c) || c == LS || ends_paragraph(c)
}
fn shaped(text: &str) -> Vec<ShapedParagraph> {
    paragraph_partition(text)
        .iter()
        .map(|s| {
            let mut p = ShapedParagraph::from_fixed_advance(text, *s, 6.0, 12.0, 10.0).unwrap();
            p.glyphs.retain(|g| !glyphless(text[g.cluster.start as usize..].chars().next().unwrap()));
            p
        })
        .collect()
}

struct Fx {
    doc: StudioDocument,
    pages: Vec<PageNumber>,
    story: StudioTextStory,
    text: String,
    placement: LayoutProposal,
    /// The first byte of the paragraph under test.
    at: u64,
}
fn fx_on(doc: StudioDocument, text: &str, styled: &[usize], paragraph: usize) -> Fx {
    let pages = number_pages(&doc, &[]).unwrap();
    let spans: Vec<(u64, u64)> = paragraph_partition(text).iter().map(|s| (s.start, s.end)).collect();
    let (mut st, _) = story(1, text, &spans);
    for k in styled {
        st.paragraphs[*k].style_ref = Some(id("STYS", 1));
    }
    let at = st.paragraphs[paragraph].start;
    let placement = run(&doc, &st, &shaped(text), &records()).unwrap();
    Fx { doc, pages, story: st, text: text.to_owned(), placement, at }
}
fn two_pages() -> StudioDocument {
    document(
        vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)), frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 60.0), Some(1), None)],
        [PageSide::LeftHand, PageSide::RightHand],
    )
}
fn fx(text: &str, paragraph: usize) -> Fx {
    fx_on(two_pages(), text, &[paragraph], paragraph)
}
/// `heading` as the second paragraph, on page-2.
fn placed(heading: &str) -> Fx {
    fx(&format!("Intro\n{heading}\nBody\n"), 1)
}
/// The same with page-2's frame hidden.
fn hidden(heading: &str) -> Fx {
    let mut f = placed(heading);
    f.doc.layers[1].visible = false;
    f
}
/// `heading` as the seventh paragraph: six lines fit, so it is overset.
fn overset(heading: &str) -> Fx {
    let f = fx(&format!("a\nb\nc\nd\ne\nf\n{heading}\n"), 6);
    assert!(f.placement.overset.is_some_and(|o| o.start <= f.at), "the fixture overflows at or before the heading");
    f
}
/// The same with page-2's frame hidden: the heading is on no frame, so the layer does not remove it.
fn overset_hidden(heading: &str) -> Fx {
    let mut f = overset(heading);
    f.doc.layers[1].visible = false;
    f
}

/// Name, heading text, whether the heading has text.
fn shapes() -> Vec<(&'static str, String, bool)> {
    vec![
        ("empty", String::new(), false),
        ("marker only", IDX.to_string(), false),
        ("spaces only", "  ".to_string(), false),
        ("no-break space only", "\u{A0}".to_string(), false),
        ("U+2028 only", LS.to_string(), false),
        ("marker and U+2028", format!("{IDX}{LS}"), false),
        ("U+2028 and marker", format!("{LS}{IDX}"), false),
        ("space and marker", format!(" {IDX} "), false),
        ("with glyphs", "Head".to_string(), true),
    ]
}

// ---- The readers ----

fn plain_index_options() -> StudioIndexOptions {
    StudioIndexOptions {
        title: "Index".into(),
        title_style: None,
        index_format: IndexFormat::Nested,
        include_section_headings: false,
        include_empty_index_sections: false,
        level_styles: vec![],
        section_heading_style: None,
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "-".into(),
        between_page_numbers_separator: ", ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: String::new(),
        replace_existing_index: None,
        include_book_documents: None,
        include_hidden_entries: Some(false),
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        section_header_set: None,
    }
}
fn contract_index_options() -> IndexOptions {
    IndexOptions {
        title: "Index".into(),
        title_style: None,
        replace_existing_index: Some(false),
        include_book_documents: Some(false),
        include_hidden_entries: Some(false),
        index_format: IndexFormat::Nested,
        include_section_headings: false,
        include_empty_index_sections: false,
        level_1_style: None,
        level_2_style: None,
        level_3_style: None,
        level_4_style: None,
        section_heading_style: None,
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "-".into(),
        between_page_numbers_separator: ", ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: String::new(),
    }
}
fn ctx() -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse("layout.toc.generate").unwrap()), 2, CorrelationId::new(1), None)
}
fn mark(n: usize, at: u64) -> StudioIndexMark {
    StudioIndexMark { mark_id: format!("m{n}"), story_id: id("STXT", 1), at, topics: vec!["t".into()], sort_by: vec![], xref: None, page_range_scope: None }
}
/// Both index generators, which must agree: (text, unplaced, omitted hidden).
fn index_at(f: &Fx, ats: &[u64]) -> (String, usize, usize) {
    let marks: Vec<StudioIndexMark> = ats.iter().enumerate().map(|(n, at)| mark(n, *at)).collect();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let cancel = CancellationToken::default();
    let plain = generate_index(&f.doc, &plain_index_options(), &marks, &source, &f.pages, None, IndexLimits::default(), &cancel).unwrap();
    let options = contract_index_options();
    let req = IndexGenerateRequest {
        document: &f.doc,
        options: &options,
        styles: None,
        marks: &marks,
        scopes: &[],
        sources: &source,
        pages: &f.pages,
        header_set: None,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        locked_stories: &[],
        limits: IndexLimits::default(),
    };
    let contract = generate_index_contract(&ctx(), &req, &cancel).unwrap();
    let got = (plain.story.text.clone(), plain.unplaced, plain.omitted_hidden);
    assert_eq!((text_of(&contract.story).to_owned(), contract.unplaced, contract.omitted_hidden), got, "the two generators");
    got
}
/// `<pageNum/>` through a paragraph destination and a text anchor, which must agree.
fn page_num_at(f: &Fx, at: u64) -> (XrefStatus, Option<String>) {
    let mut r = records();
    r.xref_formats.push(StudioXrefFormat { format_id: "fmt".into(), name: "fmt".into(), definition: "<pageNum/>".into(), character_style: None });
    r.text_anchors.push(StudioTextAnchor { anchor_id: "a".into(), name: "a".into(), story_id: id("STXT", 1), byte: at });
    let xref = |x: &str, destination| StudioCrossReference { xref_id: x.into(), source_story_id: id("STXT", 2), source_start: 0, source_end: 0, destination, format_id: "fmt".into(), resolved_text: None };
    r.cross_references.push(xref("p", StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: at }));
    r.cross_references.push(xref("a", StudioXrefDestination::TextAnchor { anchor_id: "a".into() }));
    let stories = [XrefStory { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let c = XrefContext { file_name: "doc", chapter_number: None, pages: &f.pages, type_styles: None };
    let got = resolve_cross_references(&r, &stories, &c).unwrap();
    assert_eq!((got[0].status, &got[0].text), (got[1].status, &got[1].text), "paragraph and anchor destination");
    (got[0].status, got[0].text.clone())
}

#[derive(Debug, PartialEq, Eq)]
struct Toc {
    entries: Vec<(String, Option<String>)>,
    /// (overset, hidden, no text)
    omitted: (usize, usize, usize),
}
fn plain_toc_style(include_hidden: bool, include_overset: bool) -> StudioTocStyle {
    StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_overset,
        entries: vec![StudioTocEntry {
            paragraph_style: id("STYS", 1),
            format_style: Some(id("STYS", 11)),
            level: 1,
            page_number_position: TocPageNumberPosition::After,
            separator: " ".into(),
            sort_alphabet: false,
            page_number_style: None,
            separator_style: None,
        }],
        include_book_documents: None,
        include_hidden: Some(include_hidden),
        run_in: None,
        create_bookmarks: None,
        make_anchor: None,
        remove_forced_line_break: None,
        numbered_paragraphs: None,
        set_story_direction: None,
        default_styled: false,
    }
}
fn contract_toc_style(include_hidden: bool) -> TocStyle {
    TocStyle {
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_book_documents: Some(false),
        include_hidden: Some(include_hidden),
        run_in: Some(false),
        create_bookmarks: Some(true),
        make_anchor: Some(false),
        remove_forced_line_break: Some(true),
        numbered_paragraphs: None,
        set_story_direction: None,
        default_styled: false,
        entries: vec![TocEntryStyle {
            name: ParagraphStyleRef::parse(&id("STYS", 1)).unwrap(),
            format_style: Some(ParagraphStyleRef::parse(&id("STYS", 11)).unwrap()),
            level: 1,
            page_number_style: None,
            page_number_position: TocPageNumberPosition::After,
            separator: " ".into(),
            separator_style: None,
            sort_alphabet: false,
        }],
    }
}
fn contract_toc(f: &Fx, include_hidden: bool, include_overset: bool) -> TocGenerated {
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let style = contract_toc_style(include_hidden);
    let req = TocGenerateRequest {
        document: &f.doc,
        style: &style,
        styles: None,
        sources: &source,
        pages: &f.pages,
        replace_existing: false,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        place_point: None,
        include_overset,
        locked_stories: &[],
        existing_anchors: &[],
        limits: TocLimits::default(),
    };
    generate_table_of_contents(&ctx(), &req, &CancellationToken::default()).unwrap()
}
fn plain_toc(f: &Fx, include_hidden: bool, include_overset: bool) -> GeneratedStory {
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let style = plain_toc_style(include_hidden, include_overset);
    generate_toc(&f.doc, &style, &source, &f.pages, &id("STXT", 9), TocLimits::default(), &CancellationToken::default()).unwrap()
}
/// Both TOC generators, which must agree on entries and on every count.
fn toc(f: &Fx, include_hidden: bool, include_overset: bool) -> Toc {
    let g = plain_toc(f, include_hidden, include_overset);
    let c = contract_toc(f, include_hidden, include_overset);
    let got = Toc {
        entries: g.entries.iter().map(|e| (e.text.clone(), e.page_label.clone())).collect(),
        omitted: (g.omitted.overset, g.omitted.hidden, g.omitted.no_text),
    };
    let want = Toc { entries: c.entries.iter().map(|e| (e.text.clone(), e.page_label.clone())).collect(), omitted: (c.omitted_overset, c.omitted_hidden, c.omitted_no_text) };
    assert_eq!(got, want, "the two generators");
    assert_eq!(g.omitted_marker_gaps, c.omitted_marker_gaps, "the two generators' gaps");
    // Every bookmark of the contract comes from an entry, so a left-out heading has none.
    assert_eq!(c.bookmarks.len(), c.entries.len());
    got
}
fn running(f: &Fx) -> Vec<(String, String)> {
    let vars = [StudioTextVariable {
        variable_id: "h1".into(),
        kind: StudioVariableKind::MatchParagraphStyle { style: id("STYS", 1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: false, change_case: ChangeCase::None },
    }];
    let shown = [StudioRunningText { running_id: "r".into(), frame_ids: vec![id("SLYR", 1), id("SLYR", 2)], parts: vec![StudioRunningPart::Variable { variable_id: "h1".into() }] }];
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    resolve_running_texts(&f.doc, &vars, &shown, &[], &f.pages, &source, &VariableContext::default()).unwrap().into_iter().map(|r| (r.page_id, r.text)).collect()
}
fn page_2() -> (XrefStatus, Option<String>) {
    // Stale: the reference stores no resolved text, so the fresh text differs from it.
    (XrefStatus::Stale, Some("2".to_owned()))
}
fn unplaced() -> (XrefStatus, Option<String>) {
    (XrefStatus::Unplaced, None)
}

// ---- The table: marks and destinations ----

#[test]
fn table_a_placed_mark_answers_the_page_of_its_line_whatever_the_paragraph_shape() {
    for (name, heading, _) in shapes() {
        let f = placed(&heading);
        assert_eq!(index_at(&f, &[f.at]), ("Index\nt, 2".to_owned(), 0, 0), "{name}");
    }
}

#[test]
fn table_a_placed_destination_answers_the_page_of_its_line_whatever_the_paragraph_shape() {
    for (name, heading, _) in shapes() {
        let f = placed(&heading);
        assert_eq!(page_num_at(&f, f.at), page_2(), "{name}");
    }
}

#[test]
fn table_a_mark_on_a_hidden_layer_counts_hidden_whatever_the_paragraph_shape() {
    for (name, heading, _) in shapes() {
        let f = hidden(&heading);
        assert_eq!(index_at(&f, &[f.at]), ("Index".to_owned(), 0, 1), "{name}");
    }
}

#[test]
fn table_a_destination_on_a_hidden_layer_still_answers_its_page() {
    // A cross-reference has no hidden rule: the page exists whether or not the layer shows.
    for (name, heading, _) in shapes() {
        let f = hidden(&heading);
        assert_eq!(page_num_at(&f, f.at), page_2(), "{name}");
    }
}

#[test]
fn table_an_overset_mark_counts_unplaced_and_an_overset_destination_reads_unplaced() {
    for (name, heading, _) in shapes() {
        let f = overset(&heading);
        assert_eq!(index_at(&f, &[f.at]), ("Index".to_owned(), 1, 0), "{name}");
        assert_eq!(page_num_at(&f, f.at), unplaced(), "{name}");
        let f = overset_hidden(&heading);
        assert_eq!(index_at(&f, &[f.at]), ("Index".to_owned(), 1, 0), "{name} on no frame, hidden layer or not");
    }
}

// ---- The table: headings ----

fn entry(text: &str, label: Option<&str>) -> Vec<(String, Option<String>)> {
    vec![(text.to_owned(), label.map(str::to_owned))]
}

#[test]
fn table_a_placed_heading_with_no_text_gives_no_entry_and_counts_no_text() {
    for (name, heading, has_text) in shapes() {
        let f = placed(&heading);
        for include_overset in [false, true] {
            let got = toc(&f, true, include_overset);
            if has_text {
                assert_eq!(got, Toc { entries: entry("Head", Some("2")), omitted: (0, 0, 0) }, "{name}");
            } else {
                assert_eq!(got, Toc { entries: vec![], omitted: (0, 0, 1) }, "{name} include_overset {include_overset}");
            }
        }
    }
}

#[test]
fn table_a_heading_on_a_hidden_layer_counts_hidden_whatever_it_holds() {
    for (name, heading, _) in shapes() {
        let f = hidden(&heading);
        for include_overset in [false, true] {
            assert_eq!(toc(&f, false, include_overset), Toc { entries: vec![], omitted: (0, 1, 0) }, "{name} include_overset {include_overset}");
        }
    }
}

#[test]
fn table_a_heading_on_a_hidden_layer_that_is_included_follows_the_placed_rows() {
    for (name, heading, has_text) in shapes() {
        let f = hidden(&heading);
        let want = if has_text { Toc { entries: entry("Head", Some("2")), omitted: (0, 0, 0) } } else { Toc { entries: vec![], omitted: (0, 0, 1) } };
        assert_eq!(toc(&f, true, false), want, "{name}");
    }
}

#[test]
fn table_an_overset_heading_counts_overset_unless_overset_is_included() {
    for (name, heading, has_text) in shapes() {
        for f in [overset(&heading), overset_hidden(&heading)] {
            // Left out as overset before its text is read.
            assert_eq!(toc(&f, false, false), Toc { entries: vec![], omitted: (1, 0, 0) }, "{name}");
            let want = if has_text { Toc { entries: entry("Head", None), omitted: (0, 0, 0) } } else { Toc { entries: vec![], omitted: (0, 0, 1) } };
            assert_eq!(toc(&f, false, true), want, "{name} included");
        }
    }
}

#[test]
fn a_heading_with_a_letter_among_white_space_keeps_its_entry_as_written() {
    let f = placed(" x ");
    assert_eq!(toc(&f, true, false), Toc { entries: entry(" x ", Some("2")), omitted: (0, 0, 0) });
}

#[test]
fn a_forced_break_kept_on_request_is_still_white_space() {
    let f = placed(&LS.to_string());
    let mut style = plain_toc_style(true, false);
    style.remove_forced_line_break = Some(false);
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let g = generate_toc(&f.doc, &style, &source, &f.pages, &id("STXT", 9), TocLimits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((g.entries.len(), g.omitted.no_text), (0, 1));
}

#[test]
fn every_heading_is_in_one_entry_or_one_count() {
    // Three real headings, one of each no-text shape, one hidden frame: nothing vanishes.
    let text = format!("Intro\nOne\n\n{IDX}\n  \nTwo\n");
    let mut f = fx_on(two_pages(), &text, &[1, 2, 3, 4, 5], 1);
    let got = toc(&f, true, false);
    assert_eq!(got, Toc { entries: vec![("One".to_owned(), Some("2".to_owned())), ("Two".to_owned(), Some("2".to_owned()))], omitted: (0, 0, 3) });
    f.doc.layers[1].visible = false;
    assert_eq!(toc(&f, false, false), Toc { entries: vec![], omitted: (0, 5, 0) });
}

// ---- Running text: a blank styled paragraph stays no occurrence ----

#[test]
fn a_blank_styled_paragraph_is_no_running_text_occurrence() {
    for heading in [String::new(), IDX.to_string(), LS.to_string(), format!("{IDX}{LS}")] {
        let f = placed(&heading);
        assert_eq!(running(&f), vec![("page-1".to_owned(), String::new()), ("page-2".to_owned(), String::new())], "{}", heading.escape_unicode());
        // It does not take the place of the heading before it either.
        let f = fx_on(two_pages(), &format!("Head\n{heading}\nBody\n"), &[0, 1], 1);
        assert_eq!(running(&f), vec![("page-1".to_owned(), "Head".to_owned()), ("page-2".to_owned(), "Head".to_owned())], "{}", heading.escape_unicode());
    }
}

#[test]
fn a_white_space_paragraph_stays_the_running_text_occurrence_it_was() {
    // It has glyphs and a line. A declared reading: unchanged by this lane.
    let f = fx_on(two_pages(), "Head\n  \nBody\n", &[0, 1], 1);
    assert_eq!(running(&f), vec![("page-1".to_owned(), "Head".to_owned()), ("page-2".to_owned(), "  ".to_owned())]);
}

// ---- A blank line belongs to its own paragraph, not to the next one ----

#[test]
fn a_blank_line_is_not_the_line_of_the_paragraph_after_it() {
    // "\nHead\n": the blank line (1, 1) on page-1, "Head" (1, 5) on page-2, both start at byte 1.
    let f = fx("\nHead\n", 1);
    let lines: Vec<(u64, u64)> = f.placement.frames.iter().flat_map(|fr| fr.lines.iter().map(|l| (l.source.start, l.source.end))).collect();
    assert_eq!(lines, vec![(1, 1), (1, 5)]);
    assert_eq!(index_at(&f, &[f.at]), ("Index\nt, 2".to_owned(), 0, 0));
    assert_eq!(page_num_at(&f, f.at), page_2());
    assert_eq!(toc(&f, true, false), Toc { entries: entry("Head", Some("2")), omitted: (0, 0, 0) });
    assert_eq!(running(&f), vec![("page-1".to_owned(), String::new()), ("page-2".to_owned(), "Head".to_owned())]);
    // Page-2's frame hidden: the heading is on it, the blank line before it is not.
    let mut h = fx("\nHead\n", 1);
    h.doc.layers[1].visible = false;
    assert_eq!(toc(&h, false, false), Toc { entries: vec![], omitted: (0, 1, 0) });
    assert_eq!(index_at(&h, &[h.at]), ("Index".to_owned(), 0, 1));
}

#[test]
fn consecutive_blank_lines_keep_their_own_pages() {
    // "\n\nBody\n": blank (1, 1) on page-1, blank (2, 2) on page-2. The byte of the second paragraph's
    // terminator is the start of the first blank line's span.
    let f = fx("\n\nBody\n", 0);
    assert_eq!(index_at(&f, &[0]), ("Index\nt, 1".to_owned(), 0, 0));
    assert_eq!(index_at(&f, &[1]), ("Index\nt, 2".to_owned(), 0, 0));
    assert_eq!(page_num_at(&f, 1), page_2());
}

#[test]
fn a_paragraph_overset_after_a_placed_blank_line_is_overset() {
    // The blank line is the sixth and last line, (11, 11); "Head" starts at byte 11 and does not fit.
    let f = fx("a\nb\nc\nd\ne\n\nHead\n", 6);
    assert_eq!(f.placement.overset, Some(SourceSpan { start: 11, end: 16 }));
    assert_eq!(index_at(&f, &[f.at]), ("Index".to_owned(), 1, 0));
    assert_eq!(page_num_at(&f, f.at), unplaced());
    assert_eq!(toc(&f, true, false), Toc { entries: vec![], omitted: (1, 0, 0) });
    assert_eq!(toc(&f, true, true), Toc { entries: entry("Head", None), omitted: (0, 0, 0) });
    // The blank paragraph itself is placed.
    assert_eq!(index_at(&f, &[10]), ("Index\nt, 2".to_owned(), 0, 0));
}

#[test]
fn a_blank_paragraph_at_the_story_end_is_placed() {
    // Its span is (6, 7), its blank line (7, 7): a mark at the story's last byte and past it.
    let f = fx("Intro\n\n", 1);
    assert_eq!(index_at(&f, &[6]), ("Index\nt, 2".to_owned(), 0, 0));
    assert_eq!(index_at(&f, &[7]), ("Index\nt, 2".to_owned(), 0, 0));
    // No terminator: a marker alone in the last paragraph.
    let f = fx(&format!("Intro\n{IDX}"), 1);
    assert_eq!(index_at(&f, &[6]), ("Index\nt, 2".to_owned(), 0, 0));
    assert_eq!(page_num_at(&f, 6), page_2());
}

#[test]
fn a_blank_line_made_by_a_forced_break_among_glyphs_is_found_as_before() {
    // F-R56-5's control: the line spans its bytes, (6, 9), then "X" (9, 10).
    let f = placed(&format!("{LS}X"));
    assert_eq!(index_at(&f, &[f.at]), ("Index\nt, 2".to_owned(), 0, 0));
    assert_eq!(page_num_at(&f, f.at), page_2());
    assert_eq!(toc(&f, true, false), Toc { entries: entry(" X", Some("2")), omitted: (0, 0, 0) });
}

// ---- The answer is the composer's ----

fn composed_marker_page(text: &str, at: u64) -> (Fx, Option<String>) {
    let mut f = fx(text, 1);
    let req = SentinelRequest {
        document: &f.doc,
        story: &f.story,
        resolved_text: None,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
        sections: &[],
        markers: &[],
        budget: SentinelBudget::default(),
    };
    let mut shaper = |input: &ShapeInput<'_>| Ok(shaped(input.text)[input.index].clone());
    let out = compose_with_sentinels(&req, &TypesetOptions::default(), &records(), &mut shaper, &CancellationToken::default()).unwrap();
    let page = out.markers.iter().find(|m| m.story_byte == at).and_then(|m| m.page_id.clone());
    f.placement = out.layout;
    (f, page)
}

#[test]
fn a_mark_and_a_destination_answer_the_page_composition_places_the_marker_on() {
    let cases = [
        (format!("Intro\n{IDX}\nBody\n"), 6),
        (format!("Intro\n{IDX}{LS}\nBody\n"), 6),
        (format!("Intro\n{LS}{IDX}\nBody\n"), 9),
        (format!("Intro\n{IDX}{IDX}\nBody\n"), 9),
        (format!("\n{IDX}\nBody\n"), 1),
        (format!("\n{IDX}Head\n"), 1),
    ];
    for (text, at) in cases {
        let (f, page) = composed_marker_page(&text, at);
        assert_eq!(page.as_deref(), Some("page-2"), "{}", text.escape_unicode());
        assert_eq!(index_at(&f, &[at]), ("Index\nt, 2".to_owned(), 0, 0), "{}", text.escape_unicode());
        // A paragraph destination names the paragraph's first byte.
        assert_eq!(page_num_at(&f, f.at), page_2(), "{}", text.escape_unicode());
    }
}

// ---- A page range ending in a blank paragraph ----

#[test]
fn a_scoped_range_ending_in_a_blank_paragraph_ends_on_its_page() {
    // "M" on page-1, the blank paragraph on page-2, "Body" on page-3. Before, the range's end read as
    // overset, which takes the story's last placed page.
    let mut doc = document(
        vec![
            frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)),
            frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 12.0), Some(1), Some(3)),
            frame_layer(3, 1, spec(2, 10.0, 10.0, 180.0, 60.0), Some(2), None),
        ],
        [PageSide::LeftHand, PageSide::RightHand],
    );
    doc.page_spreads[0].pages.push(page("page-3", PageSide::LeftHand));
    let f = fx_on(doc, "M\n\nBody\n", &[], 0);
    let marks = [mark(0, 0)];
    let scopes = [IndexMarkScope { mark_id: "m0".into(), scope: PageRangeScope::ForNextParagraphs(2) }];
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let options = contract_index_options();
    let req = IndexGenerateRequest {
        document: &f.doc,
        options: &options,
        styles: None,
        marks: &marks,
        scopes: &scopes,
        sources: &source,
        pages: &f.pages,
        header_set: None,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        locked_stories: &[],
        limits: IndexLimits::default(),
    };
    let g = generate_index_contract(&ctx(), &req, &CancellationToken::default()).unwrap();
    assert_eq!(text_of(&g.story), "Index\nt, 1-2");
}

// ---- Hostile input (V4-01/02) ----

#[test]
fn v4_02_a_document_of_only_empty_paragraphs_at_the_limits_stays_bounded() {
    let n = Limits::default().max_paragraphs;
    let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0 * n as f64), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
    let text = "\n".repeat(n);
    let styled: Vec<usize> = (0..n).collect();
    let f = fx_on(doc, &text, &styled, 0);
    assert_eq!(f.placement.frames[0].lines.len(), n);
    assert_eq!(f.placement.overset, None);
    // One lookup first, so a wrong answer fails before the whole document is walked.
    let last = n as u64 - 1;
    assert_eq!(index_at(&f, &[last]), ("Index\nt, 1".to_owned(), 0, 0));
    assert_eq!(page_num_at(&f, last), (XrefStatus::Stale, Some("1".to_owned())));
    // Every paragraph a mark and a heading, each read through the same lookups.
    let every: Vec<u64> = (0..n as u64).collect();
    assert_eq!(index_at(&f, &every), ("Index\nt, 1".to_owned(), 0, 0));
    assert_eq!(toc(&f, true, false), Toc { entries: vec![], omitted: (0, 0, n) });
    // One paragraph over the limit is refused before anything is laid out.
    let over = "\n".repeat(n + 1);
    let (st, _) = story(1, "", &[]);
    let over_story = StudioTextStory { content: StoryContent::Inline { text_utf8: over.clone() }, paragraphs: paragraph_partition(&over).iter().map(|s| StoryParagraph { start: s.start, end: s.end, style_ref: None, paragraph_direction: None }).collect(), ..st };
    assert_eq!(run(&f.doc, &over_story, &shaped(&over), &records()).unwrap_err().code(), LayoutError::BudgetExceeded.code());
}
