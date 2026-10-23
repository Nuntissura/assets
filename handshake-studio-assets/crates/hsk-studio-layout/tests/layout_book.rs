//! Lane SMB-LAYOUT-UI slice 5: books. Chapter A (2 pages, no sections), B (3 pages, a section that
//! continues numbering), C (1 page, continuing). Hand-computed page labels and a combined TOC.
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::{id, len, page};

/// Chapter document `n`: spread SPGS n with `pages` pages p1..; one text frame (layer 10n) of story
/// n on its first page at (10, 10), 180 x 40.
fn chapter(n: u32, pages: usize) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    let frame = StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", 10 * n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", n) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", n) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: "p1".into(),
            bounds: Bounds { x: len(10.0), y: len(10.0), width: len(180.0), height: len(40.0) },
            inset_spacing: support_layout::edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: None,
            next_text_frame: None,
        }),
    };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", n),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", n),
            name: format!("chapter {n}"),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0 * pages as f64), height: len(200.0) },
            pages: (1..=pages).map(|i| page(&format!("p{i}"), PageSide::RightHand)).collect(),
        }],
        layers: vec![frame],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn section(id_: &str, style: StudioNumberStyle, continue_numbering: bool) -> StudioSection {
    StudioSection {
        section_id: id_.into(),
        name: id_.into(),
        page_start: "p1".into(),
        continue_numbering,
        page_number_start: (!continue_numbering).then_some(1),
        page_number_style: style,
        include_section_prefix: false,
        section_prefix: None,
        marker: None,
    }
}
fn labels(c: &CombinedBook) -> Vec<String> {
    c.pages.iter().map(|p| p.label.clone()).collect()
}

#[test]
fn book_numbering_continues_across_chapters_with_parity_rules() {
    let (a, b, c) = (chapter(1, 2), chapter(2, 3), chapter(3, 1));
    let bs = [section("b", StudioNumberStyle::Arabic, true)];
    let cs = [section("c", StudioNumberStyle::Arabic, true)];
    let chapters = [BookChapter { document: &a, sections: &[] }, BookChapter { document: &b, sections: &bs }, BookChapter { document: &c, sections: &cs }];
    let mut book = StudioBook::new("book", "Book");
    for d in [&a, &b, &c] {
        book.add_chapter(&d.document_id).unwrap();
    }
    validate_book(&book, Some(&[&a, &b, &c]), &CancellationToken::default()).unwrap();
    let combine = |bk: &StudioBook| combine_book(bk, &chapters, &id("SDOC", 99)).unwrap();
    // Continue: 1 2 | 3 4 5 | 6.
    let plain = combine(&book);
    assert_eq!(labels(&plain), ["1", "2", "3", "4", "5", "6"]);
    assert_eq!(plain.pages[2].page_id, "c1-p1");
    assert_eq!(plain.document.layers[1].text_frame.as_ref().unwrap().page_id, "c1-p1");
    assert_eq!(chapter_page_ranges(&plain)[1], (id("SDOC", 2), "3-5".to_string()));
    // Next odd page: C would start on 6 -> blank page 6 inserted, C on 7.
    book.repagination_option = StudioRepagination::ContinueOnNextOdd;
    book.insert_blank_page = true;
    let odd = combine(&book);
    assert_eq!(labels(&odd), ["1", "2", "3", "4", "5", "6", "7"]);
    assert_eq!(odd.blank_pages, vec!["c2-blank".to_string()]);
    assert_eq!(odd.pages[5].page_id, "c2-blank");
    // Without the blank page the number skips.
    book.insert_blank_page = false;
    assert_eq!(labels(&combine(&book)), ["1", "2", "3", "4", "5", "7"]);
    // Next even page: B 3 -> 4, C 7 -> 8.
    book.repagination_option = StudioRepagination::ContinueOnNextEven;
    assert_eq!(labels(&combine(&book)), ["1", "2", "4", "5", "6", "8"]);
    // Automatic pagination off: each chapter keeps its own numbering.
    book.automatic_pagination = false;
    assert_eq!(labels(&combine(&book)), ["1", "2", "1", "2", "3", "1"]);
    // Reorder (C first) and a restarting roman section survive.
    let mut re = StudioBook::new("book", "Book");
    for d in [&c, &a, &b] {
        re.add_chapter(&d.document_id).unwrap();
    }
    re.move_chapter(&b.document_id, 1).unwrap();
    let roman = [section("b", StudioNumberStyle::LowerRoman, false)];
    let chapters2 = [BookChapter { document: &a, sections: &[] }, BookChapter { document: &b, sections: &roman }, BookChapter { document: &c, sections: &cs }];
    let r = combine_book(&re, &chapters2, &id("SDOC", 99)).unwrap();
    assert_eq!(labels(&r), ["1", "i", "ii", "iii", "4", "5"]);
}

#[test]
fn combined_toc_hostile_books_and_typed_book_errors() {
    let (a, b) = (chapter(1, 2), chapter(2, 3));
    let mut book = StudioBook::new("book", "Book");
    book.add_chapter(&a.document_id).unwrap();
    book.add_chapter(&b.document_id).unwrap();
    let chapters = [BookChapter { document: &a, sections: &[] }, BookChapter { document: &b, sections: &[] }];
    let combined = combine_book(&book, &chapters, &id("SDOC", 99)).unwrap();
    // Each chapter's heading paragraph carries style Head; reflow every story in the combined doc.
    let head = id("STYS", 1);
    let texts = ["Alpha\nbody", "Gamma\nmore"];
    let mut stories = Vec::new();
    for (k, t) in texts.iter().enumerate() {
        let n = k as u32 + 1;
        let (mut st, paras) = support_layout::story(n, t, &[(0, 5), (6, t.len() as u64)]);
        st.paragraphs[0].style_ref = Some(head.clone());
        let placement = reflow_flow(
            &ReflowRequest {
                document: &combined.document,
                story: &st,
                resolved_text: None,
                paragraphs: &paras,
                obstacles: &[],
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: 1,
                limits: Limits::default(),
            },
            &TypesetOptions::default(),
            &StudioLayoutFlowRecords::new(id("SDOC", 99)),
            &CancellationToken::default(),
        )
        .unwrap();
        stories.push((st, placement));
    }
    let toc = StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: "Contents".into(),
        title_style: None,
        include_overset: false,
        entries: vec![StudioTocEntry { paragraph_style: head.clone(), format_style: None, level: 1, page_number_position: TocPageNumberPosition::After, separator: " ".into(), sort_alphabet: false }],
    };
    let sources: Vec<TocSource<'_>> = stories
        .iter()
        .zip(texts)
        .map(|((st, p), t)| TocSource { story: st, text: t, placement: Some(p) })
        .collect();
    let g = generate_toc(&combined.document, &toc, &sources, &combined.pages, &id("STXT", 50), TocLimits::default(), &CancellationToken::default()).unwrap();
    // Chapter A's heading on book page 1, chapter B's on book page 3.
    assert_eq!(g.text, "Contents\nAlpha 1\nGamma 3");

    // Typed book errors (STU-LAY-147) and hostile books.
    assert_eq!(book_export_subset(&book, &[&b.document_id, &a.document_id], "pdf").unwrap(), vec![0, 1]);
    assert_eq!(book_export_subset(&book, &[&id("SDOC", 7)], "pdf").unwrap_err().code(), "book_document_not_in_book");
    assert_eq!(book_export_subset(&book, &[&a.document_id], "epub").unwrap_err().code(), "book_export_format_unsupported");
    assert_eq!(combine_book(&book, &chapters[..1], &id("SDOC", 99)).unwrap_err(), BookError::ChapterMissing(b.document_id.clone()));
    let mut twin = chapter(2, 1);
    twin.document_id = id("SDOC", 3);
    let mut clash = book.clone();
    clash.add_chapter(&twin.document_id).unwrap();
    let three = [chapters[0], chapters[1], BookChapter { document: &twin, sections: &[] }];
    assert_eq!(combine_book(&clash, &three, &id("SDOC", 99)).unwrap_err().code(), "book_identity_collision");
    // A huge book is refused before the combined document is built.
    let mut big = chapter(4, 1);
    big.page_spreads[0].pages = (0..MAX_BOOK_PAGES + 1).map(|i| page(&format!("q{i}"), PageSide::RightHand)).collect();
    let mut huge = StudioBook::new("huge", "Huge");
    huge.add_chapter(&big.document_id).unwrap();
    assert_eq!(combine_book(&huge, &[BookChapter { document: &big, sections: &[] }], &id("SDOC", 99)).unwrap_err(), BookError::BudgetExceeded);
    // Folio: duplicates, dangling style source, too many chapters.
    assert_eq!(book.add_chapter(&a.document_id).unwrap_err().code, Code::DuplicateIdentity);
    let mut src = book.clone();
    src.style_source_document = Some(id("SDOC", 8));
    assert_eq!(validate_book(&src, None, &CancellationToken::default()).unwrap_err().code, Code::DanglingReference);
    let mut many = StudioBook::new("m", "M");
    for i in 0..MAX_BOOK_CHAPTERS {
        many.add_chapter(&id("SDOC", 1000 + i as u32)).unwrap();
    }
    assert_eq!(many.add_chapter(&id("SDOC", 5000)).unwrap_err().code, Code::BudgetExceeded);
    many.remove_chapter(&id("SDOC", 1000)).unwrap();
    assert_eq!(many.chapters.len(), MAX_BOOK_CHAPTERS - 1);
}
