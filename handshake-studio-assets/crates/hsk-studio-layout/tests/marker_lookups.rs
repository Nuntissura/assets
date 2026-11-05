//! Lane SMB-LAYOUT-MARKER-LOOKUPS-1 (V4-06 sweep of R54 F-R54-6): every layout lookup that finds
//! the line (frame, page) of a story byte from line extents places a byte that has no glyph and sits
//! where a line breaks by the one rule of `place_markers`: such a byte belongs to the next line when
//! the UAX #14 break taken in the gap (the first mandatory one there, else the first allowed one) is
//! at or before it, else to the line before; a byte before a paragraph's first glyph belongs to that
//! paragraph's first line. Before this lane footnote and endnote references, TOC source paragraphs,
//! index marks and cross-reference destinations at such a byte were taken for overset text.
//!
//! Hand-computed oracles. Toy shaping as Type does it: one 6 pt glyph per character, except the
//! glyph-less marker sentinels (SD-29), the forced line break U+2028 (outside shaping, R54
//! F-R54-3) and the paragraph terminator; line height 12 pt, ascent 10 pt, fixed first baseline
//! 10 pt, no insets, 180 pt wide frames (30 glyphs a line). Studio reading; InDesign placement
//! UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const IDX: char = '\u{FDD7}';
const FN: char = '\u{FDD4}';
const LS: char = '\u{2028}';

/// Shaped paragraphs over `spans` (each the whole paragraph, terminator included, as Type).
fn shaped(text: &str, spans: &[(u64, u64)]) -> Vec<ShapedParagraph> {
    spans
        .iter()
        .map(|&(s, e)| {
            let mut p = ShapedParagraph::from_fixed_advance(text, SourceSpan { start: s, end: e }, 6.0, 12.0, 10.0).unwrap();
            p.glyphs.retain(|g| {
                let c = text[g.cluster.start as usize..].chars().next().unwrap();
                !matches!(c, IDX | FN | LS | '\n')
            });
            p
        })
        .collect()
}
fn styled_story(n: u32, text: &str, spans: &[(u64, u64)], styles: &[Option<u32>]) -> StudioTextStory {
    let (mut st, _) = story(n, text, spans);
    for (p, s) in st.paragraphs.iter_mut().zip(styles) {
        p.style_ref = s.map(|k| id("STYS", k));
    }
    st
}

// ---- The shared composition: two threaded frames, page-1 (one line) and page-2 (three lines) ----
//
// P0 0..37  "a"x20 | IDX 20 | U+2028 23 | IDX 26 | "bbbb" 29..33 | IDX 33 | "\n" 36
// P1 37..48 IDX 37 | "Chapter" 40..47 | "\n" 47
// P2 48..78 "c"x20 | IDX 68 | U+2028 71 | "dddd" 74..78
// Lines: L1 0..20 (frame 1, page-1, baseline 10; the forced break after U+2028 at byte 26 ends it),
// L2 29..33, L3 40..47, L4 48..68 (frame 2, page-2, baselines 10, 22, 34); "dddd" is overset (74..78).
// Bytes in the class and their line: 20 (glued to "a"x20 before the forced break) L1 page-1; 26
// (after the break, glued to "bbbb") L2 page-2; 33 (before its paragraph terminator) L2; 37 (opens
// P1) L3; 68 (glued to "c"x20 before the forced break, the next line overset) L4 page-2. Byte 75 is
// in the overset text.
const A: u64 = 20;
const B: u64 = 26;
const C: u64 = 33;
const D: u64 = 37;
const E: u64 = 68;
const F: u64 = 75;

struct Fixture {
    doc: StudioDocument,
    pages: Vec<PageNumber>,
    story: StudioTextStory,
    text: String,
    paragraphs: Vec<ShapedParagraph>,
    placement: LayoutProposal,
}
fn fixture() -> Fixture {
    let text = format!("{}{IDX}{LS}{IDX}bbbb{IDX}\n{IDX}Chapter\n{}{IDX}{LS}dddd", "a".repeat(20), "c".repeat(20));
    assert_eq!(text.len(), 78);
    let spans = [(0, 37), (37, 48), (48, 78)];
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)),
            frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 36.0), Some(1), None),
        ],
        [PageSide::LeftHand, PageSide::RightHand],
    );
    let pages = number_pages(&doc, &[]).unwrap();
    let story = styled_story(1, &text, &spans, &[Some(1), Some(1), None]);
    let paragraphs = shaped(&text, &spans);
    let placement = run(&doc, &story, &paragraphs, &records()).unwrap();
    Fixture { doc, pages, story, text, paragraphs, placement }
}

#[test]
fn the_shared_composition_breaks_where_the_oracle_says() {
    let f = fixture();
    let lines: Vec<(usize, u64, u64, f64)> = f
        .placement
        .frames
        .iter()
        .enumerate()
        .flat_map(|(k, fp)| fp.lines.iter().map(move |l| (k, l.source.start, l.source.end, l.baseline_y_pt)))
        .collect();
    assert_eq!(lines, vec![(0, 0, 20, 10.0), (1, 29, 33, 10.0), (1, 40, 47, 22.0), (1, 48, 68, 34.0)]);
    assert_eq!(f.placement.overset, Some(SourceSpan { start: 74, end: 78 }));
}

// ---- footnotes.rs locate ----

fn footnote_options() -> StudioFootnoteOptions {
    StudioFootnoteOptions {
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: FootnoteRestart::Page,
        prefix: String::new(),
        suffix: ".".into(),
        space_between: len(2.0),
        spacer: len(3.0),
        rule: None,
        continuing_rule: None,
        eos_placement: false,
        no_splitting: false,
    }
}
/// One frame of height `h` on page-1, the host story `text` (one paragraph) with one footnote at
/// `anchor` whose note is "note" (one 12 pt line).
fn footnotes(text: &str, h: f64, anchor: u64) -> FootnoteProposal {
    let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, h), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
    let span = [(0, text.len() as u64)];
    let host = styled_story(1, text, &span, &[None]);
    let paragraphs = shaped(text, &span);
    let (note_story, note_paragraphs) = story(2, "note", &[(0, 4)]);
    let note = StudioFootnote { note_id: "n1".into(), story_id: id("STXT", 1), anchor, note_story_id: id("STXT", 2) };
    let notes = [NoteInput { note: &note, story: &note_story, resolved_text: None, paragraphs: &note_paragraphs }];
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
                expected_revision: 2,
                limits: Limits::default(),
            },
            notes: &notes,
            options: &footnote_options(),
            pages: &[],
            max_iterations: 8,
        },
        &TypesetOptions::default(),
        &CancellationToken::default(),
    )
    .unwrap()
}
/// note id, number, label, reference frame, reference position, text frames.
type NoteSummary = (String, u32, String, usize, [f64; 2], Vec<usize>);
fn summary(p: &FootnoteProposal) -> Vec<NoteSummary> {
    p.notes.iter().map(|n| (n.note_id.clone(), n.number, n.label.clone(), n.reference_frame, n.reference, n.text_frames.clone())).collect()
}
fn body_lines(p: &LayoutProposal) -> Vec<(u64, u64)> {
    p.frames.iter().flat_map(|f| f.lines.iter().map(|l| (l.source.start, l.source.end))).collect()
}

#[test]
fn mt_30780_a_footnote_reference_at_a_line_end_before_a_line_break_stays_on_that_line() {
    // "a"x29 " " (line 1, 0..30) | "b"x20 30..50 | reference FN 50 | U+2028 53 | "c"x6 56..62. The
    // reference is glued to "b"x20: the forced break (mandatory, byte 56) comes after it.
    let text = format!("{} {}{FN}{LS}{}", "a".repeat(29), "b".repeat(20), "c".repeat(6));
    // 48 pt frame. Pass 1: three lines, the reference on line 2 (bottom 24), the band may hold
    // 48 - 24 - 3 = 21 >= 12: band 12 + spacer 3 = 15. Pass 2: body lines down to 33, so "c"x6 is
    // overset (from 56) and the reference stays on line 2, before the overset text -> stable.
    let p = footnotes(&text, 48.0, 50);
    assert_eq!((p.iterations, p.bands.clone()), (2, vec![15.0]));
    assert_eq!(body_lines(&p.body), vec![(0, 30), (30, 50)]);
    assert_eq!(p.body.overset, Some(SourceSpan { start: 56, end: 62 }));
    // Reference x after the 20 glyphs of line 2, its baseline 22; the note in the band (top 48 - 12).
    assert_eq!(summary(&p), vec![("n1".into(), 1, "1.".into(), 0, [120.0, 22.0], vec![0])]);
    assert_eq!(p.note_text[0].1.frames[0].lines[0].baseline_y_pt, 46.0);
    assert!(p.overset_notes.is_empty());

    // Glued to the NEXT word: "b"x20 " " | FN 51 | "c"x10 54..64; the break taken is the allowed
    // one after the space (byte 51), at the reference, so it is on line 3 (x 0, baseline 34).
    let text = format!("{} {} {FN}{}", "a".repeat(29), "b".repeat(20), "c".repeat(10));
    let p = footnotes(&text, 60.0, 51);
    assert_eq!(body_lines(&p.body), vec![(0, 30), (30, 51), (54, 64)]);
    assert_eq!(summary(&p), vec![("n1".into(), 1, "1.".into(), 0, [0.0, 34.0], vec![0])]);
    // Control: the same reference when line 3 is overset (24 pt frame) is overset with it.
    let p = footnotes(&text, 24.0, 51);
    assert_eq!(p.body.overset, Some(SourceSpan { start: 54, end: 64 }));
    assert!(p.notes.is_empty());
    assert_eq!(p.overset_notes, vec!["n1".to_string()]);
}

// ---- endnotes.rs (the same footnotes.rs locate) ----

#[test]
fn mt_30781_endnote_references_in_a_line_break_gap_keep_their_position() {
    let f = fixture();
    let host = [NoteHost { story: &f.story, placement: Some(&f.placement), paragraphs: &f.paragraphs }];
    let anchors = [A, B, E, F];
    let bodies: Vec<(StudioTextStory, Vec<ShapedParagraph>)> = (0..4).map(|k| story(11 + k, "note", &[(0, 4)])).collect();
    let records: Vec<StudioEndnote> = anchors
        .iter()
        .enumerate()
        .map(|(k, &a)| StudioEndnote { note_id: format!("e{k}"), story_id: id("STXT", 1), anchor: a, note_story_id: id("STXT", 11 + k as u32) })
        .collect();
    let notes: Vec<NoteBody<'_>> =
        records.iter().zip(&bodies).map(|(r, (s, p))| NoteBody { note: r, story: s, resolved_text: None, paragraphs: p }).collect();
    let o = StudioEndnoteOptions {
        title: String::new(),
        title_style: None,
        text_style: None,
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: EndnoteRestart::Continuous,
        scope: EndnoteScope::Document,
        prefix: String::new(),
        suffix: String::new(),
        separator_text: String::new(),
    };
    let g = generate_endnotes(&host, &notes, &o, 1 << 20).unwrap();
    let positions: Vec<Option<(String, [f64; 2])>> = g.references.iter().map(|r| r.position.clone()).collect();
    assert_eq!(
        positions,
        vec![
            Some((id("SLYR", 1), [120.0, 10.0])),
            Some((id("SLYR", 2), [0.0, 10.0])),
            Some((id("SLYR", 2), [120.0, 34.0])),
            None,
        ]
    );
}

// ---- toc.rs page_of / baseline_of ----

fn toc_style() -> StudioTocStyle {
    StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_overset: false,
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
        include_hidden: None,
        run_in: None,
        create_bookmarks: None,
        make_anchor: None,
        remove_forced_line_break: None,
        numbered_paragraphs: None,
        set_story_direction: None,
        default_styled: false,
    }
}

#[test]
fn mt_30776_a_toc_paragraph_opening_with_a_glyphless_marker_takes_its_first_lines_page() {
    let f = fixture();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let target = id("STXT", 9);
    let g = generate_toc(&f.doc, &toc_style(), &source, &f.pages, &target, TocLimits::default(), &CancellationToken::default()).unwrap();
    // P0 (byte 0 has a glyph) on page-1; P1 opens with a glyph-less marker before "Chapter": the
    // page and baseline (frame 2 at y 10, line 3 baseline 22) of its first line.
    let got: Vec<(u64, Option<String>, Option<String>, Option<f64>)> =
        g.entries.iter().map(|e| (e.source_start, e.page_label.clone(), e.page_id.clone(), e.baseline_y)).collect();
    assert_eq!(
        got,
        vec![
            (0, Some("1".into()), Some("page-1".into()), Some(20.0)),
            (D, Some("2".into()), Some("page-2".into()), Some(32.0)),
        ]
    );
}

// ---- toc_contract.rs frame_of / on_hidden_layer ----

fn ctx() -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse("layout.toc.generate").unwrap()), 2, CorrelationId::new(1), None)
}
fn contract_style() -> TocStyle {
    TocStyle {
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_book_documents: Some(false),
        include_hidden: Some(false),
        run_in: Some(false),
        create_bookmarks: Some(false),
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

fn toc_request<'a>(doc: &'a StudioDocument, style: &'a TocStyle, sources: &'a [TocSource<'a>], pages: &'a [PageNumber]) -> TocGenerateRequest<'a> {
    TocGenerateRequest {
        document: doc,
        style,
        styles: None,
        sources,
        pages,
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

#[test]
fn mt_30777_the_toc_contract_places_a_paragraph_opening_with_a_glyphless_marker() {
    let f = fixture();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let style = contract_style();
    let cancel = CancellationToken::default();
    let entries = |g: &TocGenerated| -> Vec<(u64, Option<String>, Option<f64>)> {
        g.entries.iter().map(|e| (e.source_start, e.page_label.clone(), e.baseline_y)).collect()
    };
    // Placed P1 is listed with the page of its first line, not omitted as overset.
    let g = generate_table_of_contents(&ctx(), &toc_request(&f.doc, &style, &source, &f.pages), &cancel).unwrap();
    assert_eq!(entries(&g), vec![(0, Some("1".into()), Some(20.0)), (D, Some("2".into()), Some(32.0))]);
    assert_eq!((g.omitted_hidden, g.omitted_overset), (0, 0));
    // Frame 2's layer hidden: P1 is on it, so `include_hidden` off omits it as hidden.
    let mut hidden = f.doc.clone();
    hidden.layers[1].visible = false;
    let g = generate_table_of_contents(&ctx(), &toc_request(&hidden, &style, &source, &f.pages), &cancel).unwrap();
    assert_eq!(entries(&g), vec![(0, Some("1".into()), Some(20.0))]);
    assert_eq!((g.omitted_hidden, g.omitted_overset), (1, 0));
}

// ---- toc.rs page_of through index.rs (index marks are glyph-less markers) ----

#[test]
fn mt_30776_index_marks_in_a_line_break_gap_take_the_page_of_their_line() {
    let f = fixture();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let marks: Vec<StudioIndexMark> = [(A, "a"), (B, "b"), (C, "c"), (D, "d"), (E, "e"), (F, "f")]
        .iter()
        .map(|&(at, t)| StudioIndexMark { mark_id: format!("m{t}"), story_id: id("STXT", 1), at, topics: vec![t.into()], sort_by: vec![], xref: None, page_range_scope: None })
        .collect();
    let options = StudioIndexOptions {
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
        include_hidden_entries: None,
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        section_header_set: None,
    };
    let g = generate_index(&f.doc, &options, &marks, &source, &f.pages, None, IndexLimits::default(), &CancellationToken::default()).unwrap();
    // Mark f (byte 75) is in the overset text.
    assert_eq!(g.story.text, "Index\na, 1\nb, 2\nc, 2\nd, 2\ne, 2");
    assert_eq!(g.unplaced, 1);
}

// ---- xref.rs page_of ----

#[test]
fn mt_30785_cross_reference_destinations_in_a_line_break_gap_resolve_their_page() {
    let f = fixture();
    let mut r = records();
    for (name, byte) in [("A", A), ("B", B), ("C", C), ("E", E), ("F", F)] {
        r.text_anchors.push(StudioTextAnchor { anchor_id: format!("t{name}"), name: name.into(), story_id: id("STXT", 1), byte });
    }
    r.xref_formats.push(StudioXrefFormat { format_id: "page".into(), name: "page".into(), definition: "<pageNum/>".into(), character_style: None });
    let xref = |k: usize, destination: StudioXrefDestination| StudioCrossReference {
        xref_id: format!("x{k}"),
        source_story_id: id("STXT", 2),
        source_start: 0,
        source_end: 0,
        destination,
        format_id: "page".into(),
        resolved_text: None,
    };
    let anchor = |n: &str| StudioXrefDestination::TextAnchor { anchor_id: format!("t{n}") };
    r.cross_references = vec![
        xref(1, anchor("A")),
        xref(2, anchor("B")),
        xref(3, anchor("C")),
        xref(4, StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: D }),
        xref(5, anchor("E")),
        xref(6, anchor("F")),
    ];
    let stories = [XrefStory { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let ctx = XrefContext { file_name: "doc", chapter_number: None, pages: &f.pages, type_styles: None };
    let got: Vec<(XrefStatus, Option<String>)> =
        resolve_cross_references(&r, &stories, &ctx).unwrap().into_iter().map(|x| (x.status, x.text)).collect();
    let page = |s: &str| (XrefStatus::Stale, Some(s.to_string()));
    assert_eq!(got, vec![page("1"), page("2"), page("2"), page("2"), page("2"), (XrefStatus::Unplaced, None)]);
}

// ---- endnotes.rs place_sidenotes (the same footnotes.rs locate) ----

#[test]
fn mt_30781_sidenotes_of_references_in_a_line_break_gap_are_placed_beside_their_line() {
    let f = fixture();
    let host = NoteHost { story: &f.story, placement: Some(&f.placement), paragraphs: &f.paragraphs };
    let bodies: Vec<(StudioTextStory, Vec<ShapedParagraph>)> = (0..3).map(|k| story(21 + k, "note", &[(0, 4)])).collect();
    let records: Vec<StudioEndnote> = [A, E, F]
        .iter()
        .enumerate()
        .map(|(k, &a)| StudioEndnote { note_id: format!("s{k}"), story_id: id("STXT", 1), anchor: a, note_story_id: id("STXT", 21 + k as u32) })
        .collect();
    let notes: Vec<NoteBody<'_>> =
        records.iter().zip(&bodies).map(|(r, (s, p))| NoteBody { note: r, story: s, resolved_text: None, paragraphs: p }).collect();
    let o = StudioSidenoteOptions {
        width: len(30.0),
        gap: len(5.0),
        space_between: len(2.0),
        side: SidenoteSide::Outside,
        anchor: SidenoteAnchor::ReferenceLine,
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: FootnoteRestart::DontRestart,
    };
    let p = place_sidenotes(&f.doc, &host, &notes, &o, &[], Composer::FirstFit, &TypesetOptions::default(), Limits::default(), &CancellationToken::default()).unwrap();
    // Page-1 is left-hand (outside = left, x -5 - 30), line 1 top 0; page-2 right-hand (outside =
    // right, x 180 + 5), line 4 top 34 - 10; the third reference is overset.
    let rects: Vec<(String, [f64; 4])> =
        p.notes.iter().map(|n| (n.frame_id.clone(), [n.rect.x0_pt, n.rect.y0_pt, n.rect.x1_pt, n.rect.y1_pt])).collect();
    assert_eq!(rects, vec![(id("SLYR", 1), [-35.0, 0.0, -5.0, 12.0]), (id("SLYR", 2), [185.0, 24.0, 215.0, 36.0])]);
    assert_eq!(p.overset, vec!["s2".to_string()]);
}

// ---- index_contract.rs (toc.rs page_of, toc_contract.rs on_hidden_layer, scope_end) ----

fn index_options() -> IndexOptions {
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
fn index_request<'a>(
    doc: &'a StudioDocument,
    options: &'a IndexOptions,
    marks: &'a [StudioIndexMark],
    scopes: &'a [IndexMarkScope],
    sources: &'a [TocSource<'a>],
    pages: &'a [PageNumber],
) -> IndexGenerateRequest<'a> {
    IndexGenerateRequest {
        document: doc,
        options,
        styles: None,
        marks,
        scopes,
        sources,
        pages,
        header_set: None,
        existing: None,
        new_story_id: "STXT-019abcde-0000-7000-8000-000000000009",
        book: None,
        locked_stories: &[],
        limits: IndexLimits::default(),
    }
}
fn index_mark(at: u64, topic: &str) -> StudioIndexMark {
    StudioIndexMark { mark_id: format!("m{topic}"), story_id: id("STXT", 1), at, topics: vec![topic.into()], sort_by: vec![], xref: None, page_range_scope: None }
}

#[test]
fn mt_30778_the_index_contract_places_marks_in_a_line_break_gap() {
    let f = fixture();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let marks: Vec<StudioIndexMark> = [(A, "a"), (B, "b"), (C, "c"), (D, "d"), (E, "e"), (F, "f")].iter().map(|&(at, t)| index_mark(at, t)).collect();
    let o = index_options();
    let cancel = CancellationToken::default();
    let g = generate_index_contract(&ctx(), &index_request(&f.doc, &o, &marks, &[], &source, &f.pages), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Index\na, 1\nb, 2\nc, 2\nd, 2\ne, 2");
    assert_eq!((g.unplaced, g.omitted_hidden), (1, 0));
    // Frame 2's layer hidden: the four marks on its lines are omitted as hidden, not as overset.
    let mut hidden = f.doc.clone();
    hidden.layers[1].visible = false;
    let g = generate_index_contract(&ctx(), &index_request(&hidden, &o, &marks, &[], &source, &f.pages), &cancel).unwrap();
    assert_eq!(text_of(&g.story), "Index\na, 1");
    assert_eq!((g.unplaced, g.omitted_hidden), (1, 4));
}

#[test]
fn mt_30779_a_page_range_ending_on_a_paragraph_terminator_ends_on_its_lines_page() {
    // "aaaa\n" on page-1, "bbbb" on page-2. A range over the mark's own paragraph ends on its
    // glyph-less terminator (byte 4): page-1, not the story's last placed page.
    let text = "aaaa\nbbbb";
    let spans = [(0, 5), (5, 9)];
    let doc = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)), frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 12.0), Some(1), None)],
        [PageSide::LeftHand, PageSide::RightHand],
    );
    let pages = number_pages(&doc, &[]).unwrap();
    let st = styled_story(1, text, &spans, &[None, None]);
    let placement = run(&doc, &st, &shaped(text, &spans), &records()).unwrap();
    let source = [TocSource { story: &st, text, placement: Some(&placement) }];
    let marks = [index_mark(0, "x")];
    let scopes = [IndexMarkScope { mark_id: "mx".into(), scope: PageRangeScope::ForNextParagraphs(1) }];
    let o = index_options();
    let g = generate_index_contract(&ctx(), &index_request(&doc, &o, &marks, &scopes, &source, &pages), &CancellationToken::default()).unwrap();
    assert_eq!(text_of(&g.story), "Index\nx, 1");
}

// ---- running.rs occurrences (toc.rs page_of) ----

#[test]
fn a_running_header_finds_a_paragraph_opening_with_a_glyphless_marker_on_its_page() {
    let f = fixture();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let vars = [StudioTextVariable {
        variable_id: "h1".into(),
        kind: StudioVariableKind::MatchParagraphStyle { style: id("STYS", 1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: false, change_case: ChangeCase::None },
    }];
    let header = StudioRunningText {
        running_id: "hdr".into(),
        frame_ids: vec![id("SLYR", 2)],
        parts: vec![StudioRunningPart::Variable { variable_id: "h1".into() }],
    };
    let got = resolve_running_texts(&f.doc, &vars, &[header], &[], &f.pages, &source, &VariableContext::default()).unwrap();
    // On page-2 the first paragraph of the style is P1 (byte 37), not P0 from the page before.
    assert!(got[0].text.ends_with("Chapter"), "{:?}", got[0].text);
}
