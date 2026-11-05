//! Lane SMB-LAYOUT-ENTRY-TEXT-1 (R55 F-R55-5, F-R55-9, F-R55-11; Master Spec v02.229 STU-TYP-207:
//! "a marker sentinel carries no text", "composition shows the resolved value without rewriting the
//! stream", "an unresolved marker shows an explicit unresolved state, never a guessed value"; "each
//! break sentinel ends its paragraph exactly as U+2029 does"). Text derived from a story (TOC entries
//! and their generated story, running text, cross-reference text, endnote text) carries no stream
//! sentinel and ends where the paragraph ends. Declared groups: `src/derived_text.rs`.
//!
//! Hand-computed oracles. Toy shaping as Type does it: one 6 pt glyph per character except the
//! glyph-less sentinels, the forced line break and the terminator; line height 12 pt, fixed first
//! baseline 10 pt, 180 pt wide frames (30 glyphs a line). Two threaded frames: page-1 holds one line,
//! page-2 five. Studio readings; InDesign's handling of markers in a TOC entry UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const CUR: char = '\u{FDD0}';
const NEXT: char = '\u{FDD1}';
const PREV: char = '\u{FDD2}';
const SEC: char = '\u{FDD3}';
const FNOTE: char = '\u{FDD4}';
const ENOTE: char = '\u{FDD5}';
const XR: char = '\u{FDD6}';
const IDX: char = '\u{FDD7}';
const PAGE: char = '\u{FDDA}';
const FLUSH: char = '\u{FDDD}';
const RESERVED: char = '\u{FDDE}';
const OBJECT: char = '\u{FFFC}';
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
    sections: Vec<StudioSection>,
    story: StudioTextStory,
    text: String,
    placement: LayoutProposal,
    heading: u64,
}
/// `text` flowed through page-1 (one line) and page-2; paragraph `styled` carries style 1.
fn fx_with(text: &str, styled: usize, sections: Vec<StudioSection>) -> Fx {
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)),
            frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 60.0), Some(1), None),
        ],
        [PageSide::LeftHand, PageSide::RightHand],
    );
    let pages = number_pages(&doc, &sections).unwrap();
    let spans: Vec<(u64, u64)> = paragraph_partition(text).iter().map(|s| (s.start, s.end)).collect();
    let (mut st, _) = story(1, text, &spans);
    st.paragraphs[styled].style_ref = Some(id("STYS", 1));
    let heading = st.paragraphs[styled].start;
    let placement = run(&doc, &st, &shaped(text), &records()).unwrap();
    Fx { doc, pages, sections, story: st, text: text.to_owned(), placement, heading }
}
fn fx(text: &str, styled: usize) -> Fx {
    fx_with(text, styled, Vec::new())
}
/// `heading` as the second paragraph, on page-2.
fn on_page_2(heading: &str) -> Fx {
    fx(&format!("Intro\n{heading}\n"), 1)
}
fn section(key: &str, start: &str, marker: Option<&str>) -> StudioSection {
    StudioSection {
        section_id: key.into(),
        name: key.into(),
        page_start: start.into(),
        continue_numbering: true,
        page_number_start: None,
        page_number_style: StudioNumberStyle::Arabic,
        include_section_prefix: false,
        section_prefix: None,
        marker: marker.map(Into::into),
    }
}

fn toc_style(include_overset: bool) -> StudioTocStyle {
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
fn toc_try(f: &Fx, include_overset: bool) -> Result<GeneratedStory, TocError> {
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let target = id("STXT", 9);
    generate_toc(&f.doc, &toc_style(include_overset), &source, &f.pages, &target, TocLimits::default(), &CancellationToken::default())
}
fn ctx() -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse("layout.toc.generate").unwrap()), 2, CorrelationId::new(1), None)
}
fn contract_style(bookmarks: bool) -> TocStyle {
    TocStyle {
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_book_documents: Some(false),
        include_hidden: Some(true),
        run_in: Some(false),
        create_bookmarks: Some(bookmarks),
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
fn contract_try(f: &Fx, bookmarks: bool, include_overset: bool) -> Result<TocGenerated, TocCommandError> {
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let style = contract_style(bookmarks);
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
    generate_table_of_contents(&ctx(), &req, &CancellationToken::default())
}
/// One running text per frame id in `frames`, matching the first style-1 paragraph on its page.
fn running_try(f: &Fx, frames: &[u32]) -> Result<Vec<RunningInstance>, RunningError> {
    let vars = [StudioTextVariable {
        variable_id: "h1".into(),
        kind: StudioVariableKind::MatchParagraphStyle { style: id("STYS", 1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: false, change_case: ChangeCase::None },
    }];
    let running: Vec<StudioRunningText> = frames
        .iter()
        .enumerate()
        .map(|(k, n)| StudioRunningText { running_id: format!("r{k}"), frame_ids: vec![id("SLYR", *n)], parts: vec![StudioRunningPart::Variable { variable_id: "h1".into() }] })
        .collect();
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    resolve_running_texts(&f.doc, &vars, &running, &f.sections, &f.pages, &source, &VariableContext::default())
}
fn xref_try(f: &Fx, definition: &str) -> Result<XrefResolution, XrefError> {
    let mut r = records();
    r.xref_formats.push(StudioXrefFormat { format_id: "fmt".into(), name: "fmt".into(), definition: definition.into(), character_style: None });
    r.cross_references.push(StudioCrossReference {
        xref_id: "x".into(),
        source_story_id: id("STXT", 2),
        source_start: 0,
        source_end: 0,
        destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: f.heading },
        format_id: "fmt".into(),
        resolved_text: None,
    });
    let stories = [XrefStory { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let c = XrefContext { file_name: "doc", chapter_number: None, pages: &f.pages, type_styles: None };
    resolve_cross_references(&r, &stories, &c).map(|mut v| v.remove(0))
}
fn gap(story_byte: u64, kind: MarkerKind, u: UnresolvedMarker) -> DerivedMarkerGap {
    DerivedMarkerGap { story_byte, kind: Some(kind), reason: DerivedGapReason::Marker(u) }
}
fn spans(p: &[StoryParagraph]) -> Vec<(u64, u64)> {
    p.iter().map(|p| (p.start, p.end)).collect()
}
fn partition(t: &str) -> Vec<(u64, u64)> {
    paragraph_partition(t).iter().map(|p| (p.start, p.end)).collect()
}
fn no_sentinel(t: &str) {
    assert!(!t.chars().any(|c| ('\u{FDD0}'..='\u{FDEF}').contains(&c) || c == OBJECT), "{}", t.escape_unicode());
}

/// Every site with `running` frame `frame`: entry text `want`, generated story `want` + " " + `label`
/// whose paragraphs are its partition, and the gaps; `toc_gaps` for the sites without sections.
fn every_site(f: &Fx, frame: u32, want: &str, label: &str, gaps: &[DerivedMarkerGap]) {
    let story_text = format!("{want} {label}");
    let g = toc_try(f, false).unwrap();
    assert_eq!(g.entries.len(), 1);
    assert_eq!(g.entries[0].text, want);
    assert_eq!(g.entries[0].page_label.as_deref(), Some(label));
    assert_eq!(g.entries[0].marker_gaps, gaps);
    assert_eq!(g.text, story_text);
    assert_eq!(spans(&g.paragraphs), partition(&g.text));
    no_sentinel(&g.text);

    let c = contract_try(f, false, false).unwrap();
    assert_eq!(c.entries[0].text, want);
    assert_eq!(c.entries[0].marker_gaps, gaps);
    assert_eq!(text_of(&c.story), story_text);
    assert_eq!(spans(&c.story.paragraphs), partition(text_of(&c.story)));

    let r = running_try(f, &[frame]).unwrap();
    assert_eq!(r[0].text, want);
    assert_eq!(r[0].marker_gaps, gaps);

    for def in ["<fullPara/>", "<paraText/>"] {
        let x = xref_try(f, def).unwrap();
        assert_eq!(x.text.as_deref(), Some(want), "{def}");
        assert_eq!(x.marker_gaps, gaps, "{def}");
    }
}

// ---- F-R55-5: no value at the source ----

#[test]
fn f_r55_5_index_anchor_and_note_markers_leave_derived_text() {
    for m in [IDX, FNOTE, ENOTE, OBJECT] {
        for heading in [format!("{m}Chapter"), format!("Chap{m}ter"), format!("Chapter{m}")] {
            every_site(&on_page_2(&heading), 2, "Chapter", "2", &[]);
        }
    }
}

// ---- F-R55-5: a value at the source ----

#[test]
fn f_r55_5_a_current_page_marker_shows_the_page_of_the_heading() {
    // The heading is on page-2: its value is "2", not the page the generated text lands on (n7c).
    every_site(&on_page_2(&format!("Chapter {CUR}")), 2, "Chapter 2", "2", &[]);
}

#[test]
fn f_r55_5_next_and_previous_page_markers_read_from_the_source_page() {
    every_site(&fx(&format!("A{NEXT}\nB\n"), 0), 1, "A2", "1", &[]);
    every_site(&on_page_2(&format!("A{PREV}")), 2, "A1", "2", &[]);
}

#[test]
fn f_r55_5_a_marker_takes_the_page_of_its_own_line_not_of_the_paragraph_start() {
    // Line 1 is page-1 ("a"x29 + space), line 2 page-2 with the marker.
    let text = format!("{} bbb{CUR}\n", "a".repeat(29));
    let want = format!("{} bbb2", "a".repeat(29));
    every_site(&fx(&text, 0), 1, &want, "1", &[]);
    // The same paragraph with the marker in the first line.
    let text = format!("{CUR}{} bbb\n", "a".repeat(29));
    let want = format!("1{} bbb", "a".repeat(29));
    every_site(&fx(&text, 0), 1, &want, "1", &[]);
}

#[test]
fn f_r55_5_a_section_marker_resolves_where_sections_are_given_and_is_typed_where_not() {
    let sections = vec![section("s1", "page-1", Some("Intro")), section("s2", "page-2", Some("Body"))];
    let f = fx_with(&format!("Intro\nChapter {SEC}\n"), 1, sections);
    // Running text has the sections.
    let r = running_try(&f, &[2]).unwrap();
    assert_eq!((r[0].text.as_str(), r[0].marker_gaps.as_slice()), ("Chapter Body", &[][..]));
    // TOC and cross-reference calls do not: a typed outcome, no value, no sentinel.
    let at = "Intro\nChapter ".len() as u64;
    let want = [DerivedMarkerGap { story_byte: at, kind: Some(MarkerKind::SectionMarker), reason: DerivedGapReason::SectionsUnavailable }];
    let g = toc_try(&f, false).unwrap();
    assert_eq!((g.entries[0].text.as_str(), g.entries[0].marker_gaps.as_slice()), ("Chapter ", &want[..]));
    no_sentinel(&g.text);
    let c = contract_try(&f, false, false).unwrap();
    assert_eq!((c.entries[0].text.as_str(), c.entries[0].marker_gaps.as_slice()), ("Chapter ", &want[..]));
    let x = xref_try(&f, "<fullPara/>").unwrap();
    assert_eq!((x.text.as_deref(), x.marker_gaps.as_slice()), (Some("Chapter "), &want[..]));
    assert_eq!(want[0].reason.code(), "derived_sections_unavailable");
}

#[test]
fn f_r55_5_a_section_marker_without_a_usable_value_is_typed_as_composition_types_it() {
    let at = "Intro\nChapter ".len() as u64;
    let text = format!("Intro\nChapter {SEC}\n");
    let run_gap = |sections: Vec<StudioSection>| {
        let f = fx_with(&text, 1, sections);
        let r = running_try(&f, &[2]).unwrap();
        assert_eq!(r[0].text, "Chapter ");
        r[0].marker_gaps.clone()
    };
    let g = |u| vec![gap(at, MarkerKind::SectionMarker, u)];
    assert_eq!(run_gap(vec![]), g(UnresolvedMarker::NoSection));
    assert_eq!(run_gap(vec![section("s1", "page-1", None)]), g(UnresolvedMarker::NoSectionMarker));
    let long = "x".repeat(MAX_RESOLVED_MARKER_BYTES + 1);
    assert_eq!(run_gap(vec![section("s1", "page-1", Some(&long))]), g(UnresolvedMarker::TextOverBudget));
    let exact = "x".repeat(MAX_RESOLVED_MARKER_BYTES);
    assert_eq!(run_gap(vec![section("s1", "page-1", Some("A\u{FDD0}B"))]), g(UnresolvedMarker::TextNotInline));
    assert_eq!(run_gap(vec![section("s1", "page-1", Some("A\u{2029}B"))]), g(UnresolvedMarker::TextNotInline));
    let f = fx_with(&text, 1, vec![section("s1", "page-1", Some(&exact))]);
    let r = running_try(&f, &[2]).unwrap();
    assert_eq!((r[0].text.len(), r[0].marker_gaps.len()), ("Chapter ".len() + MAX_RESOLVED_MARKER_BYTES, 0));
}

// ---- F-R55-5: the value is not available: typed, no wrong value, no silent drop ----

#[test]
fn f_r55_5_no_next_or_previous_page_is_typed() {
    // page-2 is the last page, page-1 the first.
    let at = "Intro\nA".len() as u64;
    every_site(&on_page_2(&format!("A{NEXT}")), 2, "A", "2", &[gap(at, MarkerKind::NextPageNumber, UnresolvedMarker::NoNextPage)]);
    every_site(&fx(&format!("A{PREV}\nB\n"), 0), 1, "A", "1", &[gap(1, MarkerKind::PreviousPageNumber, UnresolvedMarker::NoPreviousPage)]);
}

#[test]
fn f_r55_5_a_cross_reference_marker_has_no_record_at_the_call_and_is_typed() {
    let at = "Intro\nA".len() as u64;
    every_site(&on_page_2(&format!("A{XR}")), 2, "A", "2", &[gap(at, MarkerKind::CrossReference, UnresolvedMarker::RecordMissing)]);
}

#[test]
fn f_r55_5_a_page_marker_in_overset_text_is_typed() {
    // Six lines fill the two frames; the seventh paragraph is overset.
    let text = format!("a\nb\nc\nd\ne\nf\nChapter {CUR}\n");
    let f = fx(&text, 6);
    assert!(f.placement.overset.is_some());
    let at = "a\nb\nc\nd\ne\nf\nChapter ".len() as u64;
    let want = [gap(at, MarkerKind::CurrentPageNumber, UnresolvedMarker::NotPlaced)];
    let g = toc_try(&f, true).unwrap();
    assert_eq!((g.entries[0].text.as_str(), g.entries[0].marker_gaps.as_slice(), g.text.as_str()), ("Chapter ", &want[..], "Chapter "));
    let c = contract_try(&f, false, true).unwrap();
    assert_eq!((c.entries[0].text.as_str(), c.entries[0].marker_gaps.as_slice()), ("Chapter ", &want[..]));
    let x = xref_try(&f, "<fullPara/>").unwrap();
    assert_eq!((x.text.as_deref(), x.marker_gaps.as_slice()), (Some("Chapter "), &want[..]));
    // Left out of the TOC as before when overset is not included: no entry, no gap.
    assert!(toc_try(&f, false).unwrap().entries.is_empty());
}

#[test]
fn f_r55_5_a_reserved_code_point_is_typed_and_a_flush_space_becomes_a_space() {
    let at = "Intro\nA".len() as u64;
    let reserved = DerivedMarkerGap { story_byte: at, kind: None, reason: DerivedGapReason::ReservedCodePoint };
    every_site(&on_page_2(&format!("A{RESERVED}B")), 2, "AB", "2", &[reserved]);
    every_site(&on_page_2(&format!("A{FLUSH}B")), 2, "A B", "2", &[]);
}

#[test]
fn f_r55_5_every_marker_of_the_catalog_falls_in_a_declared_group() {
    // The catalog's marker table: nine members, eight sentinels and U+FFFC.
    assert_eq!(MarkerKind::ALL.len(), 8);
    for k in MarkerKind::ALL {
        let c = k.sentinel().unwrap();
        let g = toc_try(&on_page_2(&format!("A{c}")), false).unwrap();
        no_sentinel(&g.entries[0].text);
        let at = "Intro\nA".len() as u64;
        let (text, gaps): (&str, Vec<DerivedMarkerGap>) = match k {
            MarkerKind::Index | MarkerKind::FootnoteReference | MarkerKind::EndnoteReference => ("A", vec![]),
            MarkerKind::CurrentPageNumber => ("A2", vec![]),
            MarkerKind::PreviousPageNumber => ("A1", vec![]),
            MarkerKind::NextPageNumber => ("A", vec![gap(at, k, UnresolvedMarker::NoNextPage)]),
            MarkerKind::SectionMarker => ("A", vec![DerivedMarkerGap { story_byte: at, kind: Some(k), reason: DerivedGapReason::SectionsUnavailable }]),
            MarkerKind::CrossReference => ("A", vec![gap(at, k, UnresolvedMarker::RecordMissing)]),
        };
        assert_eq!((g.entries[0].text.as_str(), g.entries[0].marker_gaps.clone()), (text, gaps), "{k:?}");
    }
}

#[test]
fn f_r55_5_a_bookmark_title_and_a_partial_paragraph_read_the_derived_text() {
    let f = on_page_2(&format!("{IDX}Chap{FNOTE}ter: rest {CUR}"));
    let c = contract_try(&f, true, false).unwrap();
    assert_eq!(c.bookmarks[0].title, "Chapter: rest 2");
    let x = xref_try(&f, r#"<fullPara delim=":" includeDelim="true"/>"#).unwrap();
    assert_eq!(x.text.as_deref(), Some("Chapter:"));
}

#[test]
fn f_r55_5_a_format_without_paragraph_text_reports_no_gap_for_it() {
    let f = on_page_2(&format!("A{NEXT}"));
    let x = xref_try(&f, "<pageNum/>").unwrap();
    assert_eq!((x.text.as_deref(), x.marker_gaps.len()), (Some("2"), 0));
}

#[test]
fn f_r55_5_endnote_text_carries_no_sentinel_and_a_page_marker_in_a_note_is_typed() {
    let host_text = "host\n";
    let (host, host_paragraphs) = story(1, host_text, &[(0, 5)]);
    let body = format!("x{IDX}y{CUR}\nz{PAGE}");
    let body_spans: Vec<(u64, u64)> = partition(&body);
    let (note_story, note_paragraphs) = story(2, &body, &body_spans);
    let note = StudioEndnote { note_id: "e1".into(), story_id: id("STXT", 1), anchor: 0, note_story_id: id("STXT", 2) };
    let notes = [NoteBody { note: &note, story: &note_story, resolved_text: None, paragraphs: &note_paragraphs }];
    let hosts = [NoteHost { story: &host, resolved_text: None, placement: None, paragraphs: &host_paragraphs }];
    let g = generate_endnotes(&hosts, &notes, &endnote_options(), 1 << 20).unwrap();
    // One paragraph per note (module docs): the inner terminator becomes a space, the last is trimmed.
    let s = &g.groups[0].story;
    assert_eq!(s.text, "1. xy z");
    assert_eq!(spans(&s.paragraphs), partition(&s.text));
    // A note body has no placement at this call.
    let at = "x\u{FDD7}y".len() as u64;
    assert_eq!(g.marker_gaps, vec![("e1".to_string(), gap(at, MarkerKind::CurrentPageNumber, UnresolvedMarker::NotPlaced))]);
}
fn endnote_options() -> StudioEndnoteOptions {
    StudioEndnoteOptions {
        title: String::new(),
        title_style: None,
        text_style: None,
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: EndnoteRestart::Continuous,
        scope: EndnoteScope::Document,
        prefix: String::new(),
        suffix: ".".into(),
        separator_text: " ".into(),
    }
}

// ---- V4-01/02: no allocation beyond the marker budget ----

fn markers(c: char, n: usize) -> String {
    format!("A{}B", c.to_string().repeat(n))
}

#[test]
fn v4_02_markers_with_a_value_are_bounded_per_paragraph() {
    let over = fx(&format!("{}\n", markers(CUR, MAX_STORY_MARKERS + 1)), 0);
    assert_eq!(toc_try(&over, false).unwrap_err(), TocError::BudgetExceeded);
    assert_eq!(contract_try(&over, false, false).unwrap_err(), TocCommandError::BudgetExceeded);
    assert_eq!(running_try(&over, &[1]).unwrap_err(), RunningError::Layout(LayoutError::BudgetExceeded));
    assert_eq!(xref_try(&over, "<fullPara/>").unwrap_err(), XrefError::BudgetExceeded);
    // The budget itself is accepted: one value per marker.
    let at = fx(&format!("{}\n", markers(CUR, MAX_STORY_MARKERS)), 0);
    let g = toc_try(&at, false).unwrap();
    assert_eq!(g.entries[0].text.len(), MAX_STORY_MARKERS + 2);
    // A marker with no value allocates nothing and spends no budget.
    let free = fx(&format!("{}\n", markers(IDX, MAX_STORY_MARKERS + 1)), 0);
    assert_eq!(toc_try(&free, false).unwrap().entries[0].text, "AB");
}

#[test]
fn v4_02_the_budget_is_the_aggregate_of_one_call() {
    // Two headings of 40 000 markers each, in one TOC call and in one cross-reference call.
    let one = markers(CUR, 40_000);
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 12.0), None, Some(2)),
            frame_layer(2, 1, spec(1, 10.0, 10.0, 180.0, 60.0), Some(1), None),
        ],
        [PageSide::LeftHand, PageSide::RightHand],
    );
    let pages = number_pages(&doc, &[]).unwrap();
    let text = format!("{one}\n{one}\n");
    let parts = partition(&text);
    let (mut st, _) = story(1, &text, &parts);
    st.paragraphs[0].style_ref = Some(id("STYS", 1));
    st.paragraphs[1].style_ref = Some(id("STYS", 1));
    let placement = run(&doc, &st, &shaped(&text), &records()).unwrap();
    let source = [TocSource { story: &st, text: &text, placement: Some(&placement) }];
    let target = id("STXT", 9);
    assert_eq!(
        generate_toc(&doc, &toc_style(false), &source, &pages, &target, TocLimits::default(), &CancellationToken::default()).unwrap_err(),
        TocError::BudgetExceeded
    );
    let mut r = records();
    r.xref_formats.push(StudioXrefFormat { format_id: "fmt".into(), name: "fmt".into(), definition: "<fullPara/>".into(), character_style: None });
    for (k, p) in parts.iter().enumerate() {
        r.cross_references.push(StudioCrossReference {
            xref_id: format!("x{k}"),
            source_story_id: id("STXT", 2),
            source_start: 0,
            source_end: 0,
            destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: p.0 },
            format_id: "fmt".into(),
            resolved_text: None,
        });
    }
    let stories = [XrefStory { story: &st, text: &text, placement: Some(&placement) }];
    let c = XrefContext { file_name: "doc", chapter_number: None, pages: &pages, type_styles: None };
    assert_eq!(resolve_cross_references(&r, &stories, &c).unwrap_err(), XrefError::BudgetExceeded);
}

#[test]
fn v4_02_running_gaps_are_bounded_over_the_instances_of_one_call() {
    // 40 000 gaps per instance, two instances: over MAX_STORY_MARKERS in the result.
    let mut f = on_page_2(&markers(NEXT, 40_000));
    // A second frame on page-2 (Folio: a frame shows one running text), not in the story's thread.
    f.doc.layers.push(frame_layer(3, 3, spec(1, 10.0, 100.0, 180.0, 12.0), None, None));
    assert_eq!(running_try(&f, &[2]).unwrap()[0].marker_gaps.len(), 40_000);
    assert_eq!(running_try(&f, &[3]).unwrap()[0].marker_gaps.len(), 40_000);
    assert_eq!(running_try(&f, &[2, 3]).unwrap_err(), RunningError::Layout(LayoutError::BudgetExceeded));
}

// ---- F-R55-9: one test per terminator, all 12 of hsk_studio_type::ends_paragraph ----

fn ends_where_the_paragraph_ends(t: char) {
    assert!(ends_paragraph(t));
    // The heading on page-1 ended by `t`; a following paragraph so it is not the story's last.
    let f = fx(&format!("Chapter{t}Body\n"), 0);
    assert_eq!(spans(&f.story.paragraphs)[0], (0, 7 + t.len_utf8() as u64));
    every_site(&f, 1, "Chapter", "1", &[]);
    // A last paragraph with no following text, same result.
    let f = fx(&format!("Chapter{t}"), 0);
    every_site(&f, 1, "Chapter", "1", &[]);
}
macro_rules! terminator_tests {
    ($($name:ident $c:expr;)*) => {
        $(#[test] fn $name() { ends_where_the_paragraph_ends($c); })*
        #[test]
        fn f_r55_9_the_tests_cover_every_terminator_of_type() {
            let covered = [$($c),*];
            assert_eq!(covered.len(), 12);
            let all: Vec<char> = (0..=0x10FFFFu32).filter_map(char::from_u32).filter(|c| ends_paragraph(*c)).collect();
            let mut sorted = covered.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, all);
        }
    };
}
terminator_tests! {
    f_r55_9_line_feed '\u{000A}';
    f_r55_9_carriage_return '\u{000D}';
    f_r55_9_file_separator '\u{001C}';
    f_r55_9_group_separator '\u{001D}';
    f_r55_9_record_separator '\u{001E}';
    f_r55_9_next_line '\u{0085}';
    f_r55_9_paragraph_separator '\u{2029}';
    f_r55_9_column_break '\u{FDD8}';
    f_r55_9_frame_break '\u{FDD9}';
    f_r55_9_page_break '\u{FDDA}';
    f_r55_9_odd_page_break '\u{FDDB}';
    f_r55_9_even_page_break '\u{FDDC}';
}

#[test]
fn f_r55_9_a_character_style_range_across_a_terminator_becomes_one_line_of_running_text() {
    let text = format!("One{PAGE}Two\n");
    let mut f = fx(&text, 0);
    f.story.paragraphs[0].style_ref = None;
    f.story.character_ranges = vec![StoryRange { start: 0, end: text.len() as u64, style_ref: Some(id("STYS", 1)), applied_language: None }];
    let vars = [StudioTextVariable {
        variable_id: "h1".into(),
        kind: StudioVariableKind::MatchCharacterStyle { style: id("STYS", 1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: false, change_case: ChangeCase::None },
    }];
    let running = [StudioRunningText { running_id: "r".into(), frame_ids: vec![id("SLYR", 1)], parts: vec![StudioRunningPart::Variable { variable_id: "h1".into() }] }];
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let r = resolve_running_texts(&f.doc, &vars, &running, &[], &f.pages, &source, &VariableContext::default()).unwrap();
    assert_eq!(r[0].text, "One Two");
}

#[test]
fn f_r55_9_a_forced_line_break_kept_on_request_stays_and_is_no_terminator() {
    let f = on_page_2(&format!("A{LS}B"));
    let mut style = toc_style(false);
    style.remove_forced_line_break = Some(false);
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let g = generate_toc(&f.doc, &style, &source, &f.pages, &id("STXT", 9), TocLimits::default(), &CancellationToken::default()).unwrap();
    assert_eq!(g.entries[0].text, format!("A{LS}B"));
    assert_eq!(toc_try(&f, false).unwrap().entries[0].text, "A B");
    assert_eq!(xref_try(&f, "<fullPara/>").unwrap().text.unwrap(), format!("A{LS}B"));
}

// ---- F-R55-11: note hosts with stored content ----

fn stored(st: &StudioTextStory) -> StudioTextStory {
    let mut s = st.clone();
    s.content = StoryContent::Stored {
        artifact_manifest_id: "manifest-a".into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab12".into() },
        byte_len: st.byte_len(),
    };
    s
}
/// page-1: "a"x20 then a glyph-less reference at byte 20, then the forced break; page-2: "bbbb".
fn gap_host() -> (Fx, Vec<ShapedParagraph>, u64) {
    let text = format!("{}{IDX}{LS}bbbb\n", "a".repeat(20));
    let f = fx(&text, 0);
    let p = shaped(&text);
    (f, p, 20)
}
fn endnote_position(host: &NoteHost<'_>, anchor: u64) -> Result<Option<(String, [f64; 2])>, FootnoteError> {
    let (body, body_paragraphs) = story(11, "note", &[(0, 4)]);
    let note = StudioEndnote { note_id: "e".into(), story_id: id("STXT", 1), anchor, note_story_id: id("STXT", 11) };
    let notes = [NoteBody { note: &note, story: &body, resolved_text: None, paragraphs: &body_paragraphs }];
    generate_endnotes(std::slice::from_ref(host), &notes, &endnote_options(), 1 << 20).map(|g| g.references[0].position.clone())
}
#[allow(clippy::result_large_err)]
fn sidenote(doc: &StudioDocument, host: &NoteHost<'_>, anchor: u64) -> Result<SidenoteProposal, FootnoteError> {
    let (body, body_paragraphs) = story(11, "note", &[(0, 4)]);
    let note = StudioEndnote { note_id: "s".into(), story_id: id("STXT", 1), anchor, note_story_id: id("STXT", 11) };
    let notes = [NoteBody { note: &note, story: &body, resolved_text: None, paragraphs: &body_paragraphs }];
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
    place_sidenotes(doc, host, &notes, &o, &[], Composer::FirstFit, &TypesetOptions::default(), Limits::default(), &CancellationToken::default())
}

#[test]
fn f_r55_11_a_stored_endnote_host_places_a_gap_reference_like_an_inline_one() {
    let (f, p, at) = gap_host();
    let inline = NoteHost { story: &f.story, resolved_text: None, placement: Some(&f.placement), paragraphs: &p };
    let want = Some((id("SLYR", 1), [120.0, 10.0]));
    assert_eq!(endnote_position(&inline, at).unwrap(), want);
    let st = stored(&f.story);
    let host = NoteHost { story: &st, resolved_text: Some(&f.text), placement: Some(&f.placement), paragraphs: &p };
    assert_eq!(endnote_position(&host, at).unwrap(), want);
    // A byte with a glyph needs no text either way.
    assert_eq!(endnote_position(&host, 5).unwrap(), Some((id("SLYR", 1), [30.0, 10.0])));
    // Stored content without its text stays typed: no position for the gap byte, no guess.
    let unresolved = NoteHost { story: &st, resolved_text: None, placement: Some(&f.placement), paragraphs: &p };
    assert_eq!(endnote_position(&unresolved, at).unwrap(), None);
}

#[test]
fn f_r55_11_a_stored_sidenote_host_places_a_gap_reference_like_an_inline_one() {
    let (f, p, at) = gap_host();
    let rects = |s: &SidenoteProposal| -> Vec<(String, [f64; 4])> { s.notes.iter().map(|n| (n.frame_id.clone(), [n.rect.x0_pt, n.rect.y0_pt, n.rect.x1_pt, n.rect.y1_pt])).collect() };
    let inline = NoteHost { story: &f.story, resolved_text: None, placement: Some(&f.placement), paragraphs: &p };
    let want = vec![(id("SLYR", 1), [-35.0, 0.0, -5.0, 12.0])];
    assert_eq!(rects(&sidenote(&f.doc, &inline, at).unwrap()), want);
    let st = stored(&f.story);
    let host = NoteHost { story: &st, resolved_text: Some(&f.text), placement: Some(&f.placement), paragraphs: &p };
    assert_eq!(rects(&sidenote(&f.doc, &host, at).unwrap()), want);
    let unresolved = NoteHost { story: &st, resolved_text: None, placement: Some(&f.placement), paragraphs: &p };
    let s = sidenote(&f.doc, &unresolved, at).unwrap();
    assert_eq!((s.notes.len(), s.overset), (0, vec!["s".to_string()]));
}

#[test]
fn f_r55_11_a_note_host_whose_text_does_not_fit_its_story_is_refused() {
    let (f, p, at) = gap_host();
    let st = stored(&f.story);
    for (story, text) in [(&st, "short"), (&f.story, f.text.as_str())] {
        let host = NoteHost { story, resolved_text: Some(text), placement: Some(&f.placement), paragraphs: &p };
        assert!(matches!(endnote_position(&host, at), Err(FootnoteError::Layout(LayoutError::InvalidInput))));
        assert!(matches!(sidenote(&f.doc, &host, at), Err(FootnoteError::Layout(LayoutError::InvalidInput))));
    }
}

#[test]
fn f_r55_11_layout_footnotes_with_stored_content_places_a_gap_reference() {
    // Coverage the lane before left open (footnotes.rs locate with resolved stored text).
    let (f, p, at) = gap_host();
    let st = stored(&f.story);
    let (body, body_paragraphs) = story(11, "note", &[(0, 4)]);
    let note = StudioFootnote { note_id: "n".into(), story_id: id("STXT", 1), anchor: at, note_story_id: id("STXT", 11) };
    let notes = [NoteInput { note: &note, story: &body, resolved_text: None, paragraphs: &body_paragraphs }];
    let o = StudioFootnoteOptions {
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
    };
    // A frame tall enough for host and note on one page.
    let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 60.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
    let got = layout_footnotes(
        &FootnoteRequest {
            reflow: ReflowRequest {
                document: &doc,
                story: &st,
                resolved_text: Some(&f.text),
                paragraphs: &p,
                obstacles: &[],
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: 2,
                limits: Limits::default(),
            },
            notes: &notes,
            options: &o,
            pages: &[],
            max_iterations: 8,
        },
        &TypesetOptions::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(got.notes.len(), 1);
    assert_eq!((got.notes[0].reference_frame, got.notes[0].reference), (0, [120.0, 10.0]));
    assert!(got.overset_notes.is_empty());
}

#[test]
fn f_r55_11_generate_index_counts_a_mark_on_a_hidden_layer_as_hidden() {
    // Coverage the lane before left open (index.rs on_hidden_layer). The mark's line is on frame 2.
    let f = on_page_2("Chapter");
    let mark = |t: &str, at: u64| StudioIndexMark { mark_id: format!("m{t}"), story_id: id("STXT", 1), at, topics: vec![t.into()], sort_by: vec![], xref: None, page_range_scope: None };
    let marks = [mark("a", 0), mark("b", 6)];
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
        include_hidden_entries: Some(false),
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        section_header_set: None,
    };
    let source = [TocSource { story: &f.story, text: &f.text, placement: Some(&f.placement) }];
    let c = CancellationToken::default();
    let shown = generate_index(&f.doc, &options, &marks, &source, &f.pages, None, IndexLimits::default(), &c).unwrap();
    assert_eq!((shown.story.text.as_str(), shown.omitted_hidden, shown.unplaced), ("Index\na, 1\nb, 2", 0, 0));
    let mut hidden = f.doc.clone();
    hidden.layers[1].visible = false;
    let g = generate_index(&hidden, &options, &marks, &source, &f.pages, None, IndexLimits::default(), &c).unwrap();
    assert_eq!((g.story.text.as_str(), g.omitted_hidden, g.unplaced), ("Index\na, 1", 1, 0));
}
