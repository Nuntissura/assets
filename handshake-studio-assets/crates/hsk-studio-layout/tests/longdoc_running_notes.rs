//! SMB-LAYOUT-LONGDOC-2 item 3: host text after tables, running headers/footers with section
//! markers and text variables, endnotes and sidenotes. Hand-computed oracles. Toy font: every glyph
//! advances 6 pt, line height 12, ascent 10. Frames 60 x 60 without insets, first baseline 10.
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
fn text_frame(page: &str, y: f64, prev: Option<String>, next: Option<String>) -> StudioTextFrame {
    StudioTextFrame {
        page_id: page.into(),
        bounds: Bounds { x: len(10.0), y: len(y), width: len(60.0), height: len(60.0) },
        inset_spacing: edges(0.0),
        text_column_count: 1,
        text_column_gutter: len(0.0),
        first_baseline_offset: FirstBaseline::Fixed,
        minimum_first_baseline_offset: len(10.0),
        vertical_justification: VerticalJustification::Top,
        auto_sizing_type: AutoSizing::Off,
        previous_text_frame: prev,
        next_text_frame: next,
    }
}
fn text_layer(n: u32, spread: u32, story: u32, frame: StudioTextFrame) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", spread) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", story) },
        visible: true,
        appearance: None,
        text_frame: Some(frame),
    }
}
fn base(spreads: Vec<StudioPageSpread>, layers: Vec<StudioLayer>) -> StudioDocument {
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 6,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![],
        page_spreads: spreads,
        layers,
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn spread(n: u32, pages: &[&str]) -> StudioPageSpread {
    StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: id("SPGS", n),
        name: format!("s{n}"),
        parent: Parent::Root(()),
        bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0), height: len(100.0) },
        pages: pages.iter().map(|p| page(p)).collect(),
    }
}
/// Story 1 threads frames 1, 2 (page-1) and 3 (page-2).
fn thread_doc() -> StudioDocument {
    let l = |n: u32| id("SLYR", n);
    base(
        vec![spread(1, &["page-1", "page-2"])],
        vec![
            text_layer(1, 1, 1, text_frame("page-1", 0.0, None, Some(l(2)))),
            text_layer(2, 1, 1, text_frame("page-1", 40.0, Some(l(1)), Some(l(3)))),
            text_layer(3, 1, 1, text_frame("page-2", 0.0, Some(l(2)), None)),
        ],
    )
}
fn story(n: u32, text: &str, paras: &[(u64, u64, Option<String>)]) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras.iter().map(|(s, e, st)| StoryParagraph { start: *s, end: *e, style_ref: st.clone(), paragraph_direction: None }).collect(),
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: None }],
    }
}
fn shaped(text: &str, paras: &[(u64, u64, Option<String>)]) -> Vec<ShapedParagraph> {
    paras.iter().map(|(s, e, _)| ShapedParagraph::from_fixed_advance(text, SourceSpan { start: *s, end: *e }, 6.0, 12.0, 10.0).unwrap()).collect()
}
fn request<'a>(doc: &'a StudioDocument, st: &'a StudioTextStory, paras: &'a [ShapedParagraph]) -> ReflowRequest<'a> {
    ReflowRequest { document: doc, story: st, resolved_text: None, paragraphs: paras, obstacles: &[], composer: Composer::FirstFit, space: SpaceParams::default(), expected_revision: doc.revision, limits: Limits::default() }
}

/// "aaaa bbbb\n" (one line, bottom 12 in frame 1) + table (two 20 pt rows at 12..52) + "zzzz": its
/// first baseline would be 52 + 10 = 62, whose line ends at 64 > 60, so it resumes in frame 2 at
/// baseline 10. With 10 pt rows the table ends at 32 and "zzzz" sits at baseline 42 in frame 1. A
/// table at the story end follows the last line; an anchor inside a paragraph is refused.
#[test]
fn host_text_resumes_after_tables() {
    let doc = thread_doc();
    let text = "aaaa bbbb\nzzzz";
    let paras = [(0, 10, None), (10, 14, None)];
    let st = story(1, text, &paras);
    let sp = shaped(text, &paras);
    let row = |h: f64| StudioTableRow { row_type: TableRowType::Body, minimum_height: len(h), maximum_height: len(h), keep_with_next_row: false, start_row: TableStartRow::Anywhere, allow_split: false };
    let cell = |r: u32, c: u32| StudioTableCell {
        row: r,
        column: c,
        row_span: 1,
        column_span: 1,
        content: None,
        insets: edges(0.0),
        fill: None,
        fill_tint: None,
        strokes: StudioCellEdges::default(),
        stroke_priorities: StudioEdgePriorities::default(),
        vertical_justification: VerticalJustification::Top,
    };
    let table = |anchor: u64, h: f64| StudioTable {
        table_id: "t".into(),
        story_id: id("STXT", 1),
        anchor,
        cell_story_id: None,
        column_widths: vec![len(30.0), len(30.0)],
        rows: vec![row(h), row(h)],
        cells: vec![cell(0, 0), cell(0, 1), cell(1, 0), cell(1, 1)],
        break_headers: TableHeaderBreak::OncePerTextFrame,
        break_footers: TableHeaderBreak::OncePerTextFrame,
        skip_first_header: false,
        skip_last_footer: false,
        stroke_order: TableStrokeOrder::RowOnTop,
    };
    let run = |t: &StudioTable| {
        reflow_with_tables(
            &request(&doc, &st, &sp),
            &[AnchoredTable { table: t, cell_story: None, cell_resolved_text: None, cell_paragraphs: &[] }],
            TableLimits::default(),
            &TypesetOptions::default(),
            &CancellationToken::default(),
        )
    };
    let lines = |p: &StoryWithTables| -> Vec<Vec<(usize, f64)>> { p.text.frames.iter().map(|f| f.lines.iter().map(|l| (l.paragraph, l.baseline_y_pt)).collect()).collect() };
    let p = run(&table(10, 20.0)).unwrap();
    assert_eq!(lines(&p), vec![vec![(0, 10.0)], vec![(1, 10.0)], vec![]]);
    assert_eq!(p.tables[0].parts[0].rows.iter().map(|r| r.y_pt).collect::<Vec<_>>(), [12.0, 32.0]);
    assert_eq!(p.text.overset, None);
    let p = run(&table(10, 10.0)).unwrap();
    assert_eq!(lines(&p), vec![vec![(0, 10.0), (1, 42.0)], vec![], vec![]]);
    let p = run(&table(14, 10.0)).unwrap();
    assert_eq!(p.tables[0].parts[0].rows.iter().map(|r| r.y_pt).collect::<Vec<_>>(), [24.0, 34.0]);
    assert_eq!(run(&table(5, 10.0)).unwrap_err(), TableError::Anchor);
}

/// Label and (frame id, position) of a note reference.
type RefSummary<'a> = (&'a str, Option<(String, [f64; 2])>);
fn stys(n: u32) -> String {
    id("STYS", n)
}
/// Pages p1 | p2 p3 | p4 p5; header frames 31..35 (one per page) and 36 (p1). Sections: a at p1
/// (lower roman, marker "Front"), b at p3 (arabic from 5, prefix "S", marker "Body"): labels i, ii,
/// S5, S6, S7. Body story: Alpha. (H1) and Beta (H1) on p1, Gamma on p2, Delta (H1) on p3. The
/// header "marker | first H1 (end punctuation deleted, upper case) | page of last page in section
/// (current style)" reads: p1 Front | ALPHA | i of ii; p2 has no H1, so the last earlier one: Front
/// | BETA | ii of ii; p3 Body | DELTA | S5 of 7; p4/p5 carry DELTA. Frame 36: "Ch. III report Draft
/// 2026-10-15".
#[test]
fn running_headers_section_markers_and_variables() {
    let frame = |n: u32, spread: u32, page: &str| text_layer(n, spread, 90, text_frame(page, 0.0, None, None));
    let doc = base(
        vec![spread(1, &["p1"]), spread(2, &["p2", "p3"]), spread(3, &["p4", "p5"])],
        vec![frame(31, 1, "p1"), frame(32, 2, "p2"), frame(33, 2, "p3"), frame(34, 3, "p4"), frame(35, 3, "p5"), frame(36, 1, "p1")],
    );
    let section = |sid: &str, start: &str, style, marker: &str| StudioSection {
        section_id: sid.into(),
        name: sid.into(),
        page_start: start.into(),
        continue_numbering: true,
        page_number_start: None,
        page_number_style: style,
        include_section_prefix: false,
        section_prefix: None,
        marker: Some(marker.into()),
    };
    let mut b = section("b", "p3", StudioNumberStyle::Arabic, "Body");
    b.continue_numbering = false;
    b.page_number_start = Some(5);
    b.include_section_prefix = true;
    b.section_prefix = Some("S".into());
    let sections = vec![section("a", "p1", StudioNumberStyle::LowerRoman, "Front"), b];
    let pages = number_pages(&doc, &sections).unwrap();
    let text = "Alpha.\nBeta\nGamma\nDelta\n";
    let st = story(1, text, &[(0, 7, Some(stys(1))), (7, 12, Some(stys(1))), (12, 18, None), (18, 24, Some(stys(1)))]);
    let line = |s: u64, e: u64| Line {
        paragraph: 0,
        baseline_y_pt: 10.0,
        x_start_pt: 0.0,
        width_pt: 1.0,
        available_pt: 60.0,
        hang_pt: 0.0,
        source: SourceSpan { start: s, end: e },
        glyph_range: 0..1,
        overfull: false,
        ratio: None,
        hyphenated: false,
        justification: None,
    };
    let fp = |spread: usize, page: usize, lines: Vec<Line>| FramePlacement { frame_id: "f".into(), frame_index: 0, spread, page, origin_x_pt: 0.0, origin_y_pt: 0.0, lines };
    let placement = LayoutProposal { base_revision: 6, frames: vec![fp(0, 0, vec![line(0, 7), line(7, 12)]), fp(1, 0, vec![line(12, 18)]), fp(1, 1, vec![line(18, 24)])], overset: None };
    let var = |vid: &str, kind: StudioVariableKind| StudioTextVariable { variable_id: vid.into(), kind };
    let vars = vec![
        var("h1", StudioVariableKind::MatchParagraphStyle { style: stys(1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: true, change_case: ChangeCase::Uppercase }),
        var("last", StudioVariableKind::LastPageNumber { scope: VariableScope::Section, style: StudioNumberStyle::Current }),
        var("chapter", StudioVariableKind::ChapterNumber { number: 3, before: "Ch. ".into(), after: String::new(), style: StudioNumberStyle::UpperRoman }),
        var("file", StudioVariableKind::FileName { include_path: false, include_extension: false }),
        var("draft", StudioVariableKind::CustomText { text: "Draft".into() }),
        var("date", StudioVariableKind::OutputDate { format: "yyyy-MM-dd".into() }),
        var("xref", StudioVariableKind::XrefPageNumber),
    ];
    let t = |s: &str| StudioRunningPart::Text { text: s.into() };
    let v = |s: &str| StudioRunningPart::Variable { variable_id: s.into() };
    let header = StudioRunningText {
        running_id: "hdr".into(),
        frame_ids: (31..=35).map(|n| id("SLYR", n)).collect(),
        parts: vec![StudioRunningPart::SectionMarker, t(" | "), v("h1"), t(" | "), StudioRunningPart::PageNumber, t(" of "), v("last")],
    };
    let meta = StudioRunningText { running_id: "meta".into(), frame_ids: vec![id("SLYR", 36)], parts: vec![v("chapter"), t(" "), v("file"), t(" "), v("draft"), t(" "), v("date")] };
    let sources = [TocSource { story: &st, text, placement: Some(&placement) }];
    let ctx = VariableContext { file_path: Some("books/report.indd"), output_date: Some("2026-10-15"), ..VariableContext::default() };
    let got = resolve_running_texts(&doc, &vars, &[header.clone(), meta.clone()], &sections, &pages, &sources, &ctx).unwrap();
    let texts: Vec<&str> = got.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["Front | ALPHA | i of ii", "Front | BETA | ii of ii", "Body | DELTA | S5 of 7", "Body | DELTA | S6 of 7", "Body | DELTA | S7 of 7", "Ch. III report Draft 2026-10-15"]);
    assert_eq!((got[2].frame_id.as_str(), got[2].page_id.as_str()), (id("SLYR", 33).as_str(), "p3"));
    // Refusals.
    let mut bad = meta.clone();
    bad.parts.push(v("xref"));
    assert_eq!(resolve_running_texts(&doc, &vars, &[bad], &sections, &pages, &sources, &ctx).unwrap_err().code(), "variable_unsupported");
    assert_eq!(resolve_running_texts(&doc, &vars, std::slice::from_ref(&meta), &sections, &pages, &sources, &VariableContext::default()).unwrap_err().code(), "file_name_unavailable");
    let mut missing = meta.clone();
    missing.parts.push(v("nope"));
    assert_eq!(resolve_running_texts(&doc, &vars, &[missing], &sections, &pages, &sources, &ctx).unwrap_err().code(), "running_variable_missing");
    let mut shared = meta;
    shared.frame_ids.push(id("SLYR", 31));
    assert_eq!(resolve_running_texts(&doc, &vars, &[header, shared], &sections, &pages, &sources, &ctx).unwrap_err().code(), "running_frame_shared");
    // The resolved text composed into its header frame (frame geometry only).
    let h = &got[0].text;
    let hs = story(90, h, &[(0, h.len() as u64, None)]);
    let o = compose_overlay(&doc, &id("SLYR", 31), &hs, None, &shaped(h, &[(0, h.len() as u64, None)]), Composer::FirstFit, &TypesetOptions::default(), Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((o.frames[0].frame_id.as_str(), o.frames[0].lines[0].baseline_y_pt), (id("SLYR", 31).as_str(), 10.0));
}

/// Story 1 = eight 4-letter words (four lines in frame 1). Endnotes e1 at 5 (line 0) and e2 at 15
/// (line 1): "Notes / 1. first / 2. second", references after "aaaa " and "cccc " (x 30). Story
/// scope with a story-2 note (no placement) and story restart: a second group "Notes / 1. third".
/// Sidenotes s1 at 5 and s2 at 7 (both on line 0, top 0) on page-1 (right-hand): outside = right of
/// the 60 pt frame at x 65, width 30; s1 0..12, s2 stacks at 12 + 2 = 14..26; inside = left, x -35.
#[test]
fn endnotes_and_sidenotes() {
    let doc = thread_doc();
    let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh";
    let st = story(1, text, &[(0, text.len() as u64, None)]);
    let sp = shaped(text, &[(0, text.len() as u64, None)]);
    let placement = reflow(&request(&doc, &st, &sp), &CancellationToken::default()).unwrap();
    let note_story = |n: u32, t: &str| story(n, t, &[(0, t.len() as u64, None)]);
    let bodies = [note_story(11, "first"), note_story(12, "second"), note_story(13, "third")];
    let shapes: Vec<Vec<ShapedParagraph>> = bodies.iter().map(|b| shaped(b.inline_text().unwrap(), &[(0, b.byte_len(), None)])).collect();
    let note = |n: &str, host: u32, anchor: u64, body: u32| StudioEndnote { note_id: n.into(), story_id: id("STXT", host), anchor, note_story_id: id("STXT", body) };
    let records = [note("e1", 1, 5, 11), note("e2", 1, 15, 12), note("e3", 2, 0, 13)];
    let inputs: Vec<NoteBody<'_>> = records.iter().zip(&bodies).zip(&shapes).map(|((r, b), s)| NoteBody { note: r, story: b, resolved_text: None, paragraphs: s }).collect();
    let other = story(2, "x", &[(0, 1, None)]);
    let hosts = [NoteHost { story: &st, placement: Some(&placement), paragraphs: &sp }, NoteHost { story: &other, placement: None, paragraphs: &[] }];
    let mut o = StudioEndnoteOptions {
        title: "Notes".into(),
        title_style: None,
        text_style: Some(stys(5)),
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: EndnoteRestart::Continuous,
        scope: EndnoteScope::Document,
        prefix: String::new(),
        suffix: ".".into(),
        separator_text: " ".into(),
    };
    let g = generate_endnotes(&hosts, &inputs[..2], &o, 1 << 20).unwrap();
    assert_eq!(g.groups.len(), 1);
    assert_eq!(g.groups[0].story.text, "Notes\n1. first\n2. second");
    assert_eq!(g.groups[0].story.paragraphs[1].style_ref, Some(stys(5)));
    let refs: Vec<RefSummary<'_>> = g.references.iter().map(|r| (r.label.as_str(), r.position.clone())).collect();
    assert_eq!(refs, vec![("1.", Some((id("SLYR", 1), [30.0, 10.0]))), ("2.", Some((id("SLYR", 1), [30.0, 22.0])))]);
    o.scope = EndnoteScope::Story;
    o.restart = EndnoteRestart::StoryRestart;
    let g = generate_endnotes(&hosts, &inputs, &o, 1 << 20).unwrap();
    assert_eq!(g.groups.iter().map(|x| (x.story_id.clone(), x.story.text.as_str())).collect::<Vec<_>>(), vec![(Some(id("STXT", 1)), "Notes\n1. first\n2. second"), (Some(id("STXT", 2)), "Notes\n1. third")]);
    assert_eq!(g.references[2].position, None);
    let mut nested = records.clone();
    nested[1].note_story_id = id("STXT", 1);
    let bad: Vec<NoteBody<'_>> = nested.iter().zip(&bodies).zip(&shapes).map(|((r, b), s)| NoteBody { note: r, story: b, resolved_text: None, paragraphs: s }).collect();
    assert_eq!(generate_endnotes(&hosts, &bad, &o, 1 << 20).unwrap_err().code(), "footnote_in_footnote");
    o.start_at = 0;
    assert_eq!(generate_endnotes(&hosts, &inputs, &o, 1 << 20).unwrap_err().code(), "endnote_start_at_range");

    // Sidenotes.
    let side_records = [note("s1", 1, 5, 11), note("s2", 1, 7, 12)];
    let side_inputs: Vec<NoteBody<'_>> = side_records.iter().zip(&bodies).zip(&shapes).map(|((r, b), s)| NoteBody { note: r, story: b, resolved_text: None, paragraphs: s }).collect();
    let mut so = StudioSidenoteOptions {
        width: len(30.0),
        gap: len(5.0),
        space_between: len(2.0),
        side: SidenoteSide::Outside,
        anchor: SidenoteAnchor::ReferenceLine,
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: FootnoteRestart::Page,
    };
    let place = |so: &StudioSidenoteOptions| place_sidenotes(&doc, &hosts[0], &side_inputs, so, &[], Composer::FirstFit, &TypesetOptions::default(), Limits::default(), &CancellationToken::default());
    let p = place(&so).unwrap();
    let rects: Vec<(&str, [f64; 4])> = p.notes.iter().map(|n| (n.reference.label.as_str(), [n.rect.x0_pt, n.rect.y0_pt, n.rect.x1_pt, n.rect.y1_pt])).collect();
    assert_eq!(rects, vec![("1", [65.0, 0.0, 95.0, 12.0]), ("2", [65.0, 14.0, 95.0, 26.0])]);
    assert_eq!(p.text[1].1.frames[0].lines.iter().map(|l| (l.x_start_pt, l.baseline_y_pt)).collect::<Vec<_>>(), [(65.0, 24.0)]);
    so.side = SidenoteSide::Inside;
    assert_eq!(place(&so).unwrap().notes[0].rect.x0_pt, -35.0);
    so.width = len(0.0);
    assert_eq!(place(&so).unwrap_err().code(), "sidenote_geometry");
}
