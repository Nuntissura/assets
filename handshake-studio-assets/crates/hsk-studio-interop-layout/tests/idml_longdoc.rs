//! SMB-INTEROP-IDML: IDML long-document features (tables, footnotes, endnotes, sections, TOC
//! styles, placed graphics with fitting, text variables) imported into Folio long-document
//! records with typed losses, exported back, and stable over import -> export -> import. The
//! fixture IDML is generated here (no InDesign-written file exists on this machine; InDesign parity
//! is UNVERIFIED and routed to the D5 oracle cases ID-05 / ID-06). INDD stays a typed refusal:
//! tests/layout_edit_roundtrip.rs `indd_actual_native_editability_or_blocker`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::*;
use serde_json::Value;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const CSR: &str = "<CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/$ID/[No character style]\">";

fn wrap(element: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<idPkg:{element} xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\">\n{body}\n</idPkg:{element}>\n")
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let pts: String = [(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
        .iter()
        .map(|(x, y)| format!("<PathPointType Anchor=\"{x} {y}\" LeftDirection=\"{x} {y}\" RightDirection=\"{x} {y}\"/>"))
        .collect();
    format!("<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{pts}</PathPointArray></GeometryPathType></PathGeometry></Properties>")
}
fn run(inner: &str) -> String {
    format!("{CSR}{inner}</CharacterStyleRange>")
}
fn para(style: &str, inner: &str) -> String {
    format!("<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/{style}\">{inner}</ParagraphStyleRange>")
}

/// A one-page IDML package (300 x 400 pt) with every long-document feature plus hostile and
/// unmappable members. Host story text (proposal rule) is "Hi\nNote end": table at byte 2,
/// footnote at 7, endnote at 11.
fn fixture() -> Vec<u8> {
    let designmap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Document xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\" Self=\"d\">\n\
<idPkg:Graphic src=\"Resources/Graphic.xml\"/>\n<idPkg:Styles src=\"Resources/Styles.xml\"/>\n<idPkg:Preferences src=\"Resources/Preferences.xml\"/>\n\
<Section Self=\"sec1\" Name=\"Front\" PageStart=\"up1\" ContinueNumbering=\"false\" PageNumberStart=\"5\" IncludeSectionPrefix=\"true\" SectionPrefix=\"A\" Marker=\"Intro\"><Properties><PageNumberStyle type=\"enumeration\">LowerRoman</PageNumberStyle></Properties></Section>\n\
<TextVariable Self=\"dTV1\" Name=\"Chapter\" VariableType=\"ChapterNumberType\"><ChapterNumberVariablePreference TextBefore=\"Ch \" TextAfter=\"\" Format=\"UpperRoman\"/></TextVariable>\n\
<TextVariable Self=\"dTV2\" Name=\"Brand\" VariableType=\"CustomTextType\"><CustomTextVariablePreference Contents=\"Studio\"/></TextVariable>\n\
<TextVariable Self=\"dTV3\" Name=\"Fancy\" VariableType=\"FancyType\"/>\n\
<idPkg:Spread src=\"Spreads/Spread_us1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua2.xml\"/>\n</Document>\n"
    );
    let prefs = wrap(
        "Preferences",
        "<DocumentPreference PageWidth=\"300\" PageHeight=\"400\" FacingPages=\"false\"/>\n<MarginPreference Top=\"0\" Bottom=\"0\" Left=\"0\" Right=\"0\"/>\n\
<ChapterNumberPreference ChapterNumber=\"3\"/>\n\
<FootnoteOption FootnoteNumberingStyle=\"LowerLetters\" StartAt=\"2\" RestartNumbering=\"PageRestart\" Prefix=\"[\" Suffix=\"]\" SpaceBetween=\"4\" Spacer=\"6\" RuleOn=\"true\" RuleLineWeight=\"0.5\" RuleColor=\"Color/Black\" RuleTint=\"100\" RuleLeftIndent=\"0\" RuleWidth=\"72\" RuleOffset=\"2\" NoSplitting=\"true\"/>\n\
<EndnoteOption EndnoteTitle=\"Notes\" EndnoteTitleStyle=\"ParagraphStyle/Notes Title\" EndnoteNumberingStyle=\"Arabic\" StartEndnoteNumberAt=\"1\" RestartEndnoteNumbering=\"EndnoteContinuous\" ScopeValue=\"EndnoteScopeDocument\"/>",
    );
    let styles = wrap(
        "Styles",
        "<RootTOCStyleGroup Self=\"u9\"><TOCStyle Self=\"TOCStyle/$ID/DefaultTOCStyleName\" Name=\"[Default]\" Title=\"Contents\"/>\
<TOCStyle Self=\"TOCStyle/Contents\" Name=\"Contents\" Title=\"Contents\" TitleStyle=\"ParagraphStyle/TOC Title\"><TOCStyleEntry Name=\"Heading 1\" FormatStyle=\"ParagraphStyle/TOC 1\" Level=\"1\" PageNumberPosition=\"AfterEntry\" Separator=\"^t\"/></TOCStyle></RootTOCStyleGroup>",
    );
    let graphic = wrap(
        "Graphic",
        "<Color Self=\"Color/Red\" Model=\"Process\" Space=\"RGB\" ColorValue=\"255 0 0\" Name=\"Red\"/>\n<Color Self=\"Color/Black\" Model=\"Process\" Space=\"CMYK\" ColorValue=\"0 0 0 100\" Name=\"Black\"/>",
    );
    let spread = wrap(
        "Spread",
        &format!(
            "<Spread Self=\"us1\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\">\
<Page Self=\"up1\" Name=\"1\" GeometricBounds=\"0 0 400 300\" ItemTransform=\"1 0 0 1 0 0\"/>\
<TextFrame Self=\"uf1\" ParentStory=\"ua1\" PreviousTextFrame=\"n\" NextTextFrame=\"n\" ItemTransform=\"1 0 0 1 0 0\">{}</TextFrame>\
<Rectangle Self=\"ur1\" Name=\"$ID/\" ItemTransform=\"1 0 0 1 0 0\">{}<FrameFittingOption FittingOnEmptyFrame=\"Proportionally\" FittingAlignment=\"CenterAnchor\"/>\
<Image Self=\"ui1\" ItemTransform=\"0.5 0 0 0.5 70 150\"><Properties><GraphicBounds Left=\"0\" Top=\"0\" Right=\"200\" Bottom=\"200\"/></Properties></Image></Rectangle>\
<Oval Self=\"uo1\" ItemTransform=\"1 0 0 1 0 0\">{}<EPS Self=\"ue1\"/></Oval></Spread>",
            rect_path(20.0, 20.0, 220.0, 120.0),
            rect_path(20.0, 150.0, 220.0, 250.0),
            rect_path(20.0, 300.0, 120.0, 380.0),
        ),
    );
    let cell = |name: &str, extra: &str, body: &str| format!("<Cell Name=\"{name}\"{extra}>{body}</Cell>");
    let table = format!(
        "<Table Self=\"ut1\" HeaderRowCount=\"1\" BodyRowCount=\"1\" FooterRowCount=\"1\" ColumnCount=\"2\">\
<Row Self=\"ut1r0\" Name=\"0\"/><Row Self=\"ut1r1\" Name=\"1\" AutoGrow=\"false\" SingleRowHeight=\"20\"/><Row Self=\"ut1r2\" Name=\"2\" AutoGrow=\"true\" MinimumHeight=\"10\" MaximumHeight=\"40\"/>\
<Column Name=\"0\" SingleColumnWidth=\"100\"/><Column Name=\"1\" SingleColumnWidth=\"50\"/>{}{}{}{}{}{}</Table>",
        cell(
            "0:0",
            " ColumnSpan=\"2\" FillColor=\"Color/Red\" FillTint=\"50\" TopEdgeStrokeWeight=\"1\" TopEdgeStrokeColor=\"Color/Black\" VerticalJustification=\"CenterAlign\" TopInset=\"2\"",
            &para("Cell", &run("<Content>Head</Content>"))
        ),
        cell("1:0", "", ""),
        cell("0:1", "", &para("Cell", &run("<Content>A</Content><Br/><Content>B</Content>"))),
        cell("1:1", "", ""),
        cell("0:2", " ColumnSpan=\"2\"", &para("Cell", &run("<Content>F</Content><Footnote><ParagraphStyleRange/></Footnote>"))),
        cell("1:2", "", ""),
    );
    let big = format!("<Table Self=\"big\" ColumnCount=\"1\">{}<Column Name=\"0\"/></Table>", "<Row/>".repeat(10_001));
    let overflow = "<Table Self=\"ovf\" ColumnCount=\"1\"><Row Name=\"0\"/><Column Name=\"0\"/><Cell Name=\"0:0\" RowSpan=\"4294967295\"/></Table>";
    let footnote = format!("<Footnote Self=\"fn1\">{}</Footnote>", para("Footnote", &run("<Content><?ACE 4?>\tSee page 2</Content>")));
    let host = wrap(
        "Story",
        &format!(
            "<Story Self=\"ua1\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{}{}</Story>",
            para("Body", &[run("<Content>Hi</Content>"), run(&table), run("<Br/>")].concat()),
            para(
                "Body",
                &[
                    run("<Content>Note</Content>"),
                    run(&footnote),
                    run("<Content> end</Content>"),
                    run("<Endnote Self=\"en1\"/>"),
                    run("<TextVariableInstance Self=\"tvi1\" ResultText=\"Studio\" AssociatedTextVariable=\"dTV2\"/>"),
                    run(&big),
                    run(overflow),
                ]
                .concat()
            ),
        ),
    );
    let range = |id: &str, src: &str, text: &str| {
        format!("<EndnoteRange Self=\"{id}\" SourceEndnote=\"{src}\">{}</EndnoteRange>", para("Endnote", &run(&format!("<Content><?ACE 4?>\t{text}</Content>"))))
    };
    let endnotes = wrap(
        "Story",
        &format!(
            "<Story Self=\"ua2\" IsEndnoteStory=\"true\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{}</Story>",
            para("Endnote", &[run(&range("er1", "en1", "Later.")), run("<Br/>"), run(&range("er9", "en9", "Orphan."))].concat())
        ),
    );
    let parts = [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("META-INF/container.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><container/>".to_owned()),
        ("designmap.xml", designmap),
        ("Resources/Preferences.xml", prefs),
        ("Resources/Styles.xml", styles),
        ("Resources/Graphic.xml", graphic),
        ("Spreads/Spread_us1.xml", spread),
        ("Stories/Story_ua1.xml", host),
        ("Stories/Story_ua2.xml", endnotes),
    ];
    write_idml(&IdmlPackage { parts: parts.into_iter().map(|(n, b)| IdmlPart::new(n.into(), b.into_bytes())).collect() }).expect("write fixture")
}

fn import_all(bytes: &[u8]) -> (IdmlImport, LongDocImport) {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let mut mint = SequentialIdMint::new(7);
    let i = import_idml(bytes, &limits, &mut mint, &cancel).expect("import");
    let l = import_longdoc(&i, &limits, &mut mint, &cancel).expect("longdoc import");
    (i, l)
}

fn export_all(i: &IdmlImport, l: &LongDocImport) -> FolioExport {
    let mut names = i.export_names();
    l.extend_names(&mut names);
    let stories: Vec<StoryInput> = i
        .proposal
        .stories
        .iter()
        .filter(|p| !l.container_stories.contains(&p.story.story_id))
        .map(|p| StoryInput { story: &p.story, resolved_text: None })
        .collect();
    let notes: Vec<StoryInput> = l.stories.iter().map(|s| StoryInput { story: s, resolved_text: None }).collect();
    let input = FolioExportInput { document: l.snapshot.document(), stories: &stories, names: &names };
    export_folio_longdoc(&input, &LongDocExportInput { records: &l.records, stories: &notes }, &Limits::default(), &CancellationToken::default()).expect("export")
}

fn text_of<'a>(l: &'a LongDocImport, id: &str) -> &'a str {
    match &l.stories.iter().find(|s| s.story_id == id).expect("story").content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        _ => panic!("inline"),
    }
}
fn style_name(i: &IdmlImport, l: &LongDocImport, id: &str) -> String {
    i.proposal.style_refs.iter().chain(&l.style_refs).find(|s| s.folio == id).map(|s| s.idml_name.clone()).expect("style")
}
fn has_loss(l: &LongDocImport, element: &str, code: &str) -> bool {
    l.loss.iter().any(|e| e.element == element && e.code == code)
}
fn black() -> StudioColor {
    StudioColor { r: 0.0, g: 0.0, b: 0.0, colour_profile_id: ProfileBinding::Unassigned(()) }
}

/// The records with every minted id replaced by what it names (style names, story / layer roles),
/// so two imports compare on the modelled fields only.
fn canon(i: &IdmlImport, l: &LongDocImport) -> Value {
    let mut s = serde_json::to_string(&l.records).unwrap();
    for r in i.proposal.style_refs.iter().chain(&l.style_refs) {
        s = s.replace(&r.folio, &format!("style:{}", r.idml_name));
    }
    for p in &i.proposal.stories {
        s = s.replace(&p.story.story_id, "host-story");
    }
    for (n, link) in l.ids.iter().enumerate() {
        s = s.replace(&link.folio, &format!("{}:{}", link.kind, if link.kind == "story" { link.idml.clone() } else { n.to_string() }));
    }
    let mut v: Value = serde_json::from_str(&s).unwrap();
    v["document_id"] = Value::Null;
    v
}

#[test]
fn idml_longdoc_features_import_with_typed_losses() {
    let (i, l) = import_all(&fixture());
    let host = i.proposal.stories[0].story.story_id.clone();
    let r = &l.records;
    assert_eq!(l.container_stories, vec![i.proposal.stories[1].story.story_id.clone()]);

    // Table: typed rows, spans (covered cell elements skipped), fill / stroke / inset, cell text.
    assert_eq!(r.tables.len(), 1, "hostile tables are losses, not records");
    let t = &r.tables[0];
    assert_eq!((t.table_id.as_str(), t.story_id.as_str(), t.anchor), ("ut1", host.as_str(), 2));
    assert_eq!(t.column_widths.iter().map(|w| w.value).collect::<Vec<_>>(), vec![100.0, 50.0]);
    let rows: Vec<_> = t.rows.iter().map(|r| (r.row_type, r.minimum_height.value, r.maximum_height.value)).collect();
    assert_eq!(rows, vec![(TableRowType::Header, 3.0, 1000.0), (TableRowType::Body, 20.0, 20.0), (TableRowType::Footer, 10.0, 40.0)]);
    let cells: Vec<_> = t.cells.iter().map(|c| (c.row, c.column, c.row_span, c.column_span, c.content.map(|r| (r.start, r.end)))).collect();
    assert_eq!(cells, vec![(0, 0, 1, 2, Some((0, 4))), (1, 0, 1, 1, Some((4, 7))), (1, 1, 1, 1, None), (2, 0, 1, 2, Some((7, 8)))]);
    let head = &t.cells[0];
    assert_eq!(head.fill, Some(StudioColor { r: 1.0, g: 0.0, b: 0.0, colour_profile_id: ProfileBinding::Unassigned(()) }));
    assert_eq!(head.fill_tint, Some(50.0));
    assert_eq!(head.strokes.top, Some(StudioCellStroke { weight: Length { value: 1.0, unit: GeometryUnit::Pt }, colour: black(), tint: 100.0 }));
    assert_eq!((head.strokes.left.is_none(), head.insets.top.value, head.vertical_justification), (true, 2.0, VerticalJustification::Center));
    assert_eq!(text_of(&l, t.cell_story_id.as_deref().unwrap()), "HeadA\nBF");

    // Footnote and endnote: anchors in host bytes, note text without marker and separator tab.
    assert_eq!(r.footnotes.len(), 1);
    assert_eq!((r.footnotes[0].note_id.as_str(), r.footnotes[0].anchor), ("fn1", 7));
    assert_eq!(text_of(&l, &r.footnotes[0].note_story_id), "See page 2");
    assert_eq!((r.endnotes[0].note_id.as_str(), r.endnotes[0].story_id.as_str(), r.endnotes[0].anchor), ("en1", host.as_str(), 11));
    assert_eq!(text_of(&l, &r.endnotes[0].note_story_id), "Later.");
    let fo = r.footnote_options.as_ref().unwrap();
    assert_eq!((fo.numbering_style, fo.start_at, fo.restart, fo.prefix.as_str(), fo.suffix.as_str()), (StudioNumberStyle::LowerLetters, 2, FootnoteRestart::Page, "[", "]"));
    let rule = fo.rule.as_ref().unwrap();
    assert_eq!((rule.weight.value, rule.width.value, rule.offset.value, rule.colour.clone(), fo.no_splitting), (0.5, 72.0, 2.0, black(), true));
    let eo = r.endnote_options.as_ref().unwrap();
    assert_eq!((eo.title.as_str(), style_name(&i, &l, eo.title_style.as_deref().unwrap())), ("Notes", "ParagraphStyle/Notes Title".to_owned()));

    // Section (page labels), text variables, TOC style.
    let sec = &r.sections[0];
    assert_eq!(
        (sec.section_id.as_str(), sec.name.as_str(), sec.page_start.as_str(), sec.continue_numbering, sec.page_number_start, sec.page_number_style),
        ("sec1", "Front", "up1", false, Some(5), StudioNumberStyle::LowerRoman)
    );
    assert_eq!((sec.include_section_prefix, sec.section_prefix.as_deref(), sec.marker.as_deref()), (true, Some("A"), Some("Intro")));
    assert_eq!(
        r.text_variables,
        vec![
            StudioTextVariable {
                variable_id: "Chapter".into(),
                kind: StudioVariableKind::ChapterNumber { number: 3, before: "Ch ".into(), after: String::new(), style: StudioNumberStyle::UpperRoman }
            },
            StudioTextVariable { variable_id: "Brand".into(), kind: StudioVariableKind::CustomText { text: "Studio".into() } },
        ]
    );
    let toc = &r.toc_styles[..];
    assert_eq!(toc.len(), 1);
    assert_eq!((toc[0].toc_id.as_str(), toc[0].title.as_str(), toc[0].entries[0].level, toc[0].entries[0].separator.as_str()), ("Contents", "Contents", 1, "\t"));
    assert_eq!(style_name(&i, &l, &toc[0].entries[0].paragraph_style), "ParagraphStyle/Heading 1");

    // Graphic frame: page bounds, content size, and the fit recovered from the image transform:
    // 200 x 200 content proportionally into 200 x 100 = scale 0.5, centred at x 20 + 50 = 70.
    let g = &r.graphic_frames[..];
    assert_eq!(g.len(), 1, "the EPS frame is a loss");
    assert_eq!([g[0].bounds.x.value, g[0].bounds.y.value, g[0].bounds.width.value, g[0].bounds.height.value], [20.0, 150.0, 200.0, 100.0]);
    assert_eq!((g[0].content_width.value, g[0].content_height.value, g[0].page_id.as_str()), (200.0, 200.0, "up1"));
    assert_eq!((g[0].fit, g[0].frame_fitting, g[0].anchor, g[0].crop.is_none()), (Some(StudioFit::Proportionally), Some(StudioFit::Proportionally), StudioFitAnchor::Center, true));
    let layer = l.snapshot.document().layers.iter().find(|x| x.layer_id == g[0].layer_id).unwrap();
    assert!(matches!(&layer.payload, Payload::Raster { tiles } if tiles.is_empty()));

    // Everything unmapped is a typed loss; hostile tables were refused before any grid existed.
    for (element, code) in [
        ("Table", "table_too_large"),
        ("Table", "span_overflow"),
        ("Footnote", "note_in_note_or_cell_not_imported"),
        ("EndnoteRange", "endnote_range_without_reference"),
        ("TextVariable", "variable_type_not_mapped"),
        ("TOCStyle", "toc_no_entries"),
        ("TextVariableInstance", "variable_instance_text_not_in_story"),
        ("EPS", "placed_content_not_raster"),
        ("Image", "linked_pixels_not_imported"),
        ("Color", "cmyk_swatch_converted_without_profile"),
    ] {
        assert!(has_loss(&l, element, code), "missing loss {element}/{code}: {:?}", l.loss);
    }
}

#[test]
fn idml_longdoc_export_round_trip_is_stable() {
    let (i0, l0) = import_all(&fixture());
    let e1 = export_all(&i0, &l0);
    let (i1, l1) = import_all(&e1.bytes);
    let e2 = export_all(&i1, &l1);
    let (i2, l2) = import_all(&e2.bytes);
    // Import -> export -> import: equal on every modelled field, ids aside.
    assert_eq!(canon(&i0, &l0), canon(&i1, &l1));
    // And from then on a fixed point: same records (same minted ids), same package bytes.
    assert_eq!(l1.records, l2.records);
    assert_eq!(canon(&i1, &l1), canon(&i2, &l2));
    assert_eq!(e1.bytes, e2.bytes);
    let cell = |l: &LongDocImport| text_of(l, l.records.tables[0].cell_story_id.as_deref().unwrap()).to_owned();
    assert_eq!(cell(&l1), "HeadA\nBF");
    // The written package carries no unmodelled leftovers: only the known approximations remain.
    assert!(!has_loss(&l1, "Table", "table_too_large") && !has_loss(&l1, "EPS", "placed_content_not_raster"));
    assert!(l1.loss.iter().all(|e| e.kind != LossKind::Approximated || e.code == "colour_mode_assumed_rgb"), "{:?}", l1.loss);
}

#[test]
fn idml_longdoc_refusals_are_typed() {
    let (i, l) = import_all(&fixture());
    let names = i.export_names();
    let stories: Vec<StoryInput> = vec![StoryInput { story: &i.proposal.stories[0].story, resolved_text: None }];
    let input = FolioExportInput { document: l.snapshot.document(), stories: &stories, names: &names };
    let cancel = CancellationToken::default();
    // Overlapping cells: Folio's grid check refuses the records before anything is written.
    let mut bad = l.records.clone();
    bad.tables[0].cells[1].row = 0;
    let none: Vec<StoryInput> = l.stories.iter().map(|s| StoryInput { story: s, resolved_text: None }).collect();
    let err = export_folio_longdoc(&input, &LongDocExportInput { records: &bad, stories: &none }, &Limits::default(), &cancel).unwrap_err();
    assert_eq!(err, IdmlError::Folio(Code::InvalidField));
    // A note story the caller did not supply.
    let err = export_folio_longdoc(&input, &LongDocExportInput { records: &l.records, stories: &[] }, &Limits::default(), &cancel).unwrap_err();
    assert_eq!(err, IdmlError::DanglingStory);
    // Cancellation: no partial result.
    let canceled = CancellationToken::default();
    canceled.cancel();
    let mut mint = SequentialIdMint::new(7);
    assert_eq!(import_longdoc(&i, &Limits::default(), &mut mint, &canceled).unwrap_err(), IdmlError::Canceled);
}
