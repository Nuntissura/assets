//! Lane SMB-LAYOUT-UI slice 2: anchored objects, cross-references, conditional text and story
//! edits. Hand-computed oracles over the toy font of `support_layout` (6 pt glyphs, 12 pt lines,
//! ascent 10, first baseline 10).
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const SIDES: [PageSide; 2] = [PageSide::RightHand, PageSide::LeftHand];

fn rect(p: &PlacedAnchor) -> (f64, f64, f64, f64) {
    (p.rect.x0_pt, p.rect.y0_pt, p.rect.x1_pt, p.rect.y1_pt)
}
fn anchored(layer: u32, byte: u64, w: f64, h: f64, s: StudioAnchoredSettings) -> StudioAnchoredObject {
    StudioAnchoredObject { layer_id: id("SLYR", layer), story_id: id("STXT", 1), anchor_byte: byte, width: len(w), height: len(h), settings: s }
}

#[test]
fn anchored_inline_above_line_and_custom() {
    // Frame 1 on page 1 at (20, 30), 120 x 100. "ab\u{FFFC}cd efgh\nijkl": P0 0..12, P1 13..17.
    let text = "ab\u{FFFC}cd efgh\nijkl";
    let layers = vec![
        frame_layer(1, 1, spec(0, 20.0, 30.0, 120.0, 100.0), None, None),
        item_layer(5, on_spread()),
        item_layer(6, on_spread()),
        item_layer(7, on_spread()),
    ];
    let doc = document(layers.clone(), SIDES);
    let (st, paras) = story(1, text, &[(0, 12), (13, 17)]);
    let mut flow = records();
    // Inline at the placeholder (byte 2): 18 x 8, x offset 2 -> glyph advance 20; 1 pt above baseline.
    let inline = StudioAnchoredSettings { anchor_x_offset: len(2.0), anchor_y_offset: len(1.0), ..Default::default() };
    flow.anchored_objects.push(anchored(5, 2, 18.0, 8.0, inline.clone()));
    // Above line at P1 start: 30 x 10, space above 4, space after (y offset) 2, centred.
    let mut above = StudioAnchoredSettings { anchored_position: StudioAnchorPosition::AboveLine, ..Default::default() };
    above.anchor_space_above = len(4.0);
    above.anchor_y_offset = len(2.0);
    above.horizontal_alignment = Some(StudioAnchorHAlign::Center);
    flow.anchored_objects.push(anchored(6, 13, 30.0, 10.0, above));
    // Custom: top-right of the item at the page edge's top-right, offset (-5, 7).
    let mut custom = StudioAnchoredSettings { anchored_position: StudioAnchorPosition::Custom, ..Default::default() };
    custom.horizontal_reference_point = Some(StudioAnchorHRef::PageEdge);
    custom.horizontal_alignment = Some(StudioAnchorHAlign::Right);
    custom.vertical_reference_point = Some(StudioAnchorVRef::PageEdge);
    custom.vertical_alignment = Some(StudioAnchorVAlign::Top);
    custom.anchor_point = StudioAnchorPoint::TopRight;
    custom.anchor_x_offset = len(-5.0);
    custom.anchor_y_offset = len(7.0);
    flow.anchored_objects.push(anchored(7, 0, 20.0, 10.0, custom.clone()));
    validate_layout_flow_records(&flow, &doc, &CancellationToken::default()).unwrap();

    let p = run(&doc, &st, &paras, &flow).unwrap();
    let lines = &p.frames[0].lines;
    // Line 0: 9 glyphs x 6 + the 20 pt anchor = 74; P1 is pushed down by 4 + 10 + 2 = 16.
    assert_eq!((lines[0].width_pt, lines[0].baseline_y_pt), (74.0, 10.0));
    assert_eq!((lines[1].source.start, lines[1].baseline_y_pt), (13, 38.0));
    let a = place_anchored_objects(&doc, &flow, &st, &paras, &p).unwrap();
    assert!(a.unplaced.is_empty());
    // Inline: anchor glyph x = 20 + 12 = 32 -> box x 34..52, bottom 40 - 1 = 39.
    assert_eq!(rect(&a.placed[0]), (34.0, 31.0, 52.0, 39.0));
    // Above line: top = 30 + 38 - 10 - 2 - 10 = 46 (= line 0 bottom 42 + space above 4); centred
    // in the column 20..140.
    assert_eq!(rect(&a.placed[1]), (65.0, 46.0, 95.0, 56.0));
    // Custom: x 200 - 5 - 20 = 175, y 0 + 7.
    assert_eq!(rect(&a.placed[2]), (175.0, 7.0, 195.0, 17.0));

    // Spine-relative on a left-hand page mirrors alignment, offset and anchor point.
    let left = document(layers.clone(), [PageSide::LeftHand, PageSide::RightHand]);
    let mut mirrored = flow.clone();
    mirrored.anchored_objects[2].settings.spine_relative = true;
    let pm = run(&left, &st, &paras, &mirrored).unwrap();
    let m = place_anchored_objects(&left, &mirrored, &st, &paras, &pm).unwrap();
    assert_eq!(rect(&m.placed[2]), (5.0, 7.0, 25.0, 17.0));
    // Pinned: bottom of the text frame + 10 would leave the frame; pinned to 130 - 10.
    let mut pinned = flow.clone();
    let s = &mut pinned.anchored_objects[2].settings;
    s.horizontal_reference_point = Some(StudioAnchorHRef::ColumnEdge);
    s.horizontal_alignment = Some(StudioAnchorHAlign::Left);
    s.vertical_reference_point = Some(StudioAnchorVRef::TextFrame);
    s.vertical_alignment = Some(StudioAnchorVAlign::Bottom);
    s.anchor_point = StudioAnchorPoint::TopLeft;
    s.anchor_x_offset = len(0.0);
    s.anchor_y_offset = len(10.0);
    s.pin_position = true;
    let pp = place_anchored_objects(&doc, &pinned, &st, &paras, &p).unwrap();
    assert_eq!(rect(&pp.placed[2]), (20.0, 120.0, 40.0, 130.0));

    // An anchored graphic frame moves with its anchor, content transform included.
    let g = PlacedGraphic {
        layer_id: id("SLYR", 5),
        spread: 0,
        page: 0,
        frame: Rect { x0_pt: 200.0, y0_pt: 150.0, x1_pt: 220.0, y1_pt: 160.0 },
        shape: StudioFrameShape::Rectangle,
        transform: [1.0, 0.0, 0.0, 1.0, 200.0, 150.0],
        content_box: Rect { x0_pt: 200.0, y0_pt: 150.0, x1_pt: 220.0, y1_pt: 160.0 },
        content_size: [20.0, 10.0],
    };
    let moved = anchored_graphic(&g, &a.placed[0]);
    assert_eq!((moved.frame.x0_pt, moved.frame.y0_pt, moved.transform[4], moved.transform[5]), (34.0, 31.0, 34.0, 31.0));

    // Overset anchors are unplaced; hostile anchors are typed refusals.
    let short = document(vec![frame_layer(1, 1, spec(0, 20.0, 30.0, 120.0, 12.0), None, None), item_layer(5, on_spread()), item_layer(6, on_spread()), item_layer(7, on_spread())], SIDES);
    let ps = run(&short, &st, &paras, &flow).unwrap();
    let u = place_anchored_objects(&short, &flow, &st, &paras, &ps).unwrap();
    assert_eq!(u.unplaced, vec![id("SLYR", 6)]);
    let mut high = flow.clone();
    high.anchored_objects[0].settings.anchor_y_offset = len(40.0);
    assert_eq!(place_anchored_objects(&doc, &high, &st, &paras, &p).unwrap_err(), AnchorError::InlineBeyondContainer(id("SLYR", 5)));
    let mut grouped_layers = layers.clone();
    grouped_layers[1].parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: id("SLYR", 7) });
    let grouped = document(grouped_layers, SIDES);
    assert_eq!(place_anchored_objects(&grouped, &flow, &st, &paras, &p).unwrap_err().code(), "anchor_group_member");
    assert_eq!(validate_layout_flow_records(&flow, &grouped, &CancellationToken::default()).unwrap_err().code, Code::IllegalParent);
    let mut mid = flow.clone();
    mid.anchored_objects[1].anchor_byte = 14;
    assert_eq!(run(&doc, &st, &paras, &mid), Err(LayoutError::UnsupportedComposition));
    let mut inside = flow.clone();
    inside.anchored_objects[0].anchor_byte = 3;
    assert_eq!(run(&doc, &st, &paras, &inside), Err(LayoutError::InvalidInput));
    let mut bad = flow.clone();
    bad.anchored_objects[0].settings.horizontal_alignment = Some(StudioAnchorHAlign::Left);
    assert_eq!(validate_layout_flow_records(&bad, &doc, &CancellationToken::default()).unwrap_err().code, Code::InvalidField);

    // insert / release: new anchors inherit the document defaults.
    let mut r = records();
    let d = StudioAnchoredSettings { anchor_x_offset: len(3.0), horizontal_alignment: Some(StudioAnchorHAlign::Right), ..Default::default() };
    r.anchored_object_defaults = Some(d);
    r.insert_anchored_object(&id("SLYR", 5), &id("STXT", 1), 2, StudioAnchorPosition::Inline, [len(4.0), len(4.0)]).unwrap();
    let got = &r.anchored_objects[0].settings;
    assert_eq!((got.anchor_x_offset.value, got.horizontal_alignment), (3.0, None));
    assert!(r.insert_anchored_object(&id("SLYR", 5), &id("STXT", 1), 2, StudioAnchorPosition::Inline, [len(4.0), len(4.0)]).is_err());
    assert_eq!(r.release_anchored_object(&id("SLYR", 5)).map(|a| a.anchor_byte), Some(2));
    assert!(r.anchored_objects.is_empty());
}

fn bold_styles() -> StudioTypeStyleRecords {
    let mut t = StudioTypeStyleRecords::new(id("SDOC", 1));
    t.styles.push(StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: id("STYS", 1),
        scope: TypeStyleScope::Character,
        name: "Bold".into(),
        based_on: None,
        next_style: None,
        font_family: None,
        font_style: Some("Bold".into()),
        font_size: None,
        fill: None,
        line_height: None,
        letter_spacing: None,
        align: None,
        grep_styles: Vec::new(),
    });
    t
}

#[test]
fn cross_references_resolve_write_and_refuse_hostile_formats() {
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 20.0, 30.0, 160.0, 40.0), None, None),
            frame_layer(2, 2, spec(1, 10.0, 10.0, 150.0, 60.0), None, None),
        ],
        SIDES,
    );
    let (src, _) = story(1, "Read X now", &[(0, 10)]);
    let dest_text = "Intro. More text\nSecond";
    let (dest, dest_paras) = story(2, dest_text, &[(0, 16), (17, 23)]);
    let placement = run(&doc, &dest, &dest_paras, &records()).unwrap();
    let pages = number_pages(&doc, &[]).unwrap();
    let styles = bold_styles();
    let mut r = records();
    r.text_anchors.push(StudioTextAnchor { anchor_id: "a1".into(), name: "Second".into(), story_id: id("STXT", 2), byte: 19 });
    let format = |fid: &str, def: &str| StudioXrefFormat { format_id: fid.into(), name: fid.into(), definition: def.into(), character_style: None };
    r.xref_formats.push(format("f1", r#"see <fullPara delim="." includeDelim="true"/> on <cs name="Bold">p. <pageNum/></cs>"#));
    r.xref_formats.push(format("f2", "<txtAnchrName/>: <paraText/> (<fileName/>)"));
    let xref = |xid: &str, s: u64, e: u64, destination: StudioXrefDestination, f: &str| StudioCrossReference {
        xref_id: xid.into(),
        source_story_id: id("STXT", 1),
        source_start: s,
        source_end: e,
        destination,
        format_id: f.into(),
        resolved_text: None,
    };
    r.cross_references.push(xref("x1", 5, 6, StudioXrefDestination::Paragraph { story_id: id("STXT", 2), paragraph_start: 0 }, "f1"));
    r.cross_references.push(xref("x2", 10, 10, StudioXrefDestination::TextAnchor { anchor_id: "a1".into() }, "f2"));
    r.cross_references.push(xref("x3", 0, 0, StudioXrefDestination::Paragraph { story_id: id("STXT", 2), paragraph_start: 3 }, "f2"));
    validate_layout_flow_records(&r, &doc, &CancellationToken::default()).unwrap();
    let ctx = XrefContext { file_name: "ch1", chapter_number: None, pages: &pages, type_styles: Some(&styles) };
    fn both<'a>(s: &'a StudioTextStory, dest: &'a StudioTextStory, dest_text: &'a str, placement: Option<&'a LayoutProposal>) -> Vec<XrefStory<'a>> {
        vec![
            XrefStory { story: s, text: "", placement: None },
            XrefStory { story: dest, text: dest_text, placement },
        ]
    }
    let stories = |s, with_placement: bool| both(s, &dest, dest_text, with_placement.then_some(&placement));
    let got = resolve_cross_references(&r, &stories(&src, true), &ctx).unwrap();
    // Destination on page 2 (frame 2 sits on the second page; implicit arabic section).
    assert_eq!(got[0].text.as_deref(), Some("see Intro. on p. 2"));
    assert_eq!(
        got[0].runs,
        vec![
            XrefRun { text: "see Intro. on ".into(), character_style: None },
            XrefRun { text: "p. 2".into(), character_style: Some(id("STYS", 1)) },
        ]
    );
    assert_eq!(got[0].status, XrefStatus::Stale);
    assert_eq!(got[1].text.as_deref(), Some("Second: Second (ch1)"));
    assert_eq!(got[2].status, XrefStatus::Broken);
    // Not composed -> no page -> unplaced (x2 has no page block and still resolves).
    let un = resolve_cross_references(&r, &stories(&src, false), &ctx).unwrap();
    assert_eq!((un[0].status, un[1].status), (XrefStatus::Unplaced, XrefStatus::Stale));

    // Writing x1 replaces the source range and shifts x2's range by 17 bytes.
    let written = write_cross_reference(&mut r, &src, None, "x1", "see Intro. on p. 2").unwrap();
    assert_eq!(text_of(&written), "Read see Intro. on p. 2 now");
    assert_eq!((r.cross_references[0].source_end, r.cross_references[1].source_start), (23, 27));
    let again = resolve_cross_references(&r, &stories(&written, true), &ctx).unwrap();
    assert_eq!(again[0].status, XrefStatus::Current);

    // Hostile building-block strings: tokenized, never substring-matched.
    let parse = |d: &str| parse_xref_format(d);
    assert_eq!(parse("&lt;pageNum/&gt; &amp; x").unwrap(), vec![XrefBlock::Text("<pageNum/> & x".into())]);
    assert_eq!(parse("<pageNum \t />").unwrap(), vec![XrefBlock::PageNum]);
    assert_eq!(parse(r#"<fullPara delim="&amp;"/>"#).unwrap(), vec![XrefBlock::PartialPara { delimiter: '&', include: false }]);
    assert!(matches!(parse("<PageNum/>"), Err(XrefError::InvalidSyntax(0))));
    assert!(matches!(parse("<!-- <pageNum/> -->"), Err(XrefError::InvalidSyntax(_))));
    assert!(matches!(parse("a & b"), Err(XrefError::InvalidSyntax(2))));
    assert!(matches!(parse("a > b"), Err(XrefError::InvalidSyntax(2))));
    assert_eq!(parse(r#"<fullPara delim="ab"/>"#), Err(XrefError::DelimiterNotSingleChar));
    assert_eq!(parse(r#"<fullPara delim="." includeDelim="TRUE"/>"#), Err(XrefError::DelimiterInclusionNotBool));
    assert!(matches!(parse(r#"<fullPara includeDelim="true"/>"#), Err(XrefError::InvalidSyntax(_))));
    assert!(matches!(parse(r#"<cs name="Bold">x"#), Err(XrefError::InvalidSyntax(_))));
    assert!(matches!(parse(r#"<cs name="A"><cs name="B">x</cs></cs>"#), Err(XrefError::InvalidSyntax(_))));
    assert!(matches!(parse("x</cs>"), Err(XrefError::InvalidSyntax(1))));
    assert_eq!(parse(&"x".repeat(4097)), Err(XrefError::BudgetExceeded));
    assert_eq!(parse(&"<pageNum/>".repeat(400)).map(|v| v.len()), Ok(400));
    let mut unknown = r.clone();
    unknown.xref_formats[1].definition = r#"<cs name="Italic"><paraText/></cs>"#.into();
    assert_eq!(resolve_cross_references(&unknown, &stories(&src, true), &ctx), Err(XrefError::CharacterStyleMissing("Italic".into())));
    let mut num = r.clone();
    num.xref_formats[1].definition = "<paraNum/>".into();
    assert_eq!(resolve_cross_references(&num, &stories(&src, true), &ctx), Err(XrefError::BlockUnsupported("paraNum")));
}

fn condition(cid: &str, visible: bool) -> StudioCondition {
    StudioCondition {
        condition_id: cid.into(),
        name: cid.into(),
        visible,
        indicator_colour: StudioRgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0, colour_profile_id: ProfileBinding::Unassigned(()) },
        indicator_method: StudioIndicatorMethod::Underline,
        underline_indicator_appearance: StudioUnderlineAppearance::Wavy,
    }
}

#[test]
fn conditional_text_resolves_before_composition_and_story_edits_remap() {
    // "Keep this. " 0..11, "Hide me. " 11..20, "Keep end." 20..29, "\n", "Tail" 30..34.
    let text = "Keep this. Hide me. Keep end.\nTail";
    let doc = document(vec![frame_layer(1, 1, spec(0, 20.0, 30.0, 120.0, 100.0), None, None)], SIDES);
    let (st, _) = story(1, text, &[(0, 29), (30, 34)]);
    let mut r = records();
    r.conditions = vec![condition("print", false), condition("web", true), condition("draft", false)];
    let range = |s: u64, e: u64, c: &[&str]| StudioConditionalRange { story_id: id("STXT", 1), start: s, end: e, conditions: c.iter().map(|x| x.to_string()).collect() };
    r.conditional_ranges = vec![range(11, 20, &["print"]), range(20, 25, &["draft", "web"]), range(30, 34, &["draft"])];
    r.condition_sets.push(StudioConditionSet {
        set_id: "print-set".into(),
        name: "Print".into(),
        visibility: vec![StudioConditionVisibility { condition_id: "print".into(), visible: true }, StudioConditionVisibility { condition_id: "draft".into(), visible: true }],
    });
    r.condition_sets.push(StudioConditionSet { set_id: "web-set".into(), name: "Web".into(), visibility: vec![] });
    r.conditional_text_preferences = Some(StudioConditionalTextPreferences { indicator_mode: StudioIndicatorMode::Hide, active_set: None });
    validate_layout_flow_records(&r, &doc, &CancellationToken::default()).unwrap();

    let v = conditional_view(&st, None, &r).unwrap();
    // "Keep end." stays: one of its conditions (web) is visible.
    assert_eq!(text_of(&v.story), "Keep this. Keep end.\n");
    assert_eq!(v.hidden, vec![(11, 20), (30, 34)]);
    assert_eq!(v.story.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 21)]);
    assert_eq!((v.to_view(20), v.to_view(15), v.to_original(11)), (Some(11), None, Some(20)));
    // Composition sees only visible text: 20 glyphs fill the 120 pt line exactly.
    let shaped = shape(text_of(&v.story), &[(0, 20)]);
    let p = run(&doc, &v.story, &shaped, &records()).unwrap();
    assert_eq!(p.frames[0].lines.len(), 1);
    assert_eq!((p.frames[0].lines[0].source.end, p.frames[0].lines[0].width_pt), (20, 120.0));

    // Sets: apply shows everything; redefine captures the current visibility.
    let mut s = r.clone();
    s.apply_condition_set("print-set").unwrap();
    assert_eq!(text_of(&conditional_view(&st, None, &s).unwrap().story), text);
    assert_eq!(s.conditional_text_preferences.as_ref().unwrap().active_set.as_deref(), Some("print-set"));
    s.redefine_condition_set("web-set").unwrap();
    assert_eq!(s.condition_sets[1].visibility.iter().filter(|v| v.visible).count(), 3);
    // Delete re-tags with the replacing condition; a missing or self replacement is refused.
    let mut d = r.clone();
    assert_eq!(d.delete_condition("draft", Some("ghost")).unwrap_err().code, Code::DanglingReference);
    assert_eq!(d.delete_condition("draft", Some("draft")).unwrap_err().code, Code::DanglingReference);
    d.delete_condition("draft", Some("web")).unwrap();
    assert_eq!(d.conditional_ranges[1].conditions, vec!["web".to_string()]);
    assert_eq!(d.conditional_ranges[2].conditions, vec!["web".to_string()]);
    assert!(d.condition_sets[0].visibility.iter().all(|v| v.condition_id != "draft"));
    // Hostile records: ranges off char boundaries / past the end, unknown conditions.
    let mut off = r.clone();
    off.conditional_ranges[0].end = 40;
    assert_eq!(conditional_view(&st, None, &off), Err(LayoutError::InvalidInput));
    let mut dup = r.clone();
    dup.conditions.push(condition("print", true));
    assert_eq!(validate_layout_flow_records(&dup, &doc, &CancellationToken::default()).unwrap_err().code, Code::DuplicateIdentity);
    let mut active = r.clone();
    active.conditional_text_preferences.as_mut().unwrap().active_set = Some("nope".into());
    assert_eq!(validate_layout_flow_records(&active, &doc, &CancellationToken::default()).unwrap_err().code, Code::DanglingReference);

    // Story edit: replacing "Hide me. " by "X\nY " splits P0 at the new newline; later bytes shift.
    let e = replace_story_text(&st, None, 11, 20, "X\nY ").unwrap();
    assert_eq!(text_of(&e), "Keep this. X\nY Keep end.\nTail");
    assert_eq!(e.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 13), (13, 25), (25, 29)]);
    let mut shifted = r.clone();
    shift_flow_records(&mut shifted, &id("STXT", 1), 11, 20, 4).unwrap();
    assert_eq!(shifted.conditional_ranges.iter().map(|g| (g.start, g.end)).collect::<Vec<_>>(), vec![(11, 15), (15, 20), (25, 29)]);
    assert_eq!(replace_story_text(&st, None, 20, 11, ""), Err(LayoutError::InvalidInput));
    let (uni, _) = story(1, "a\u{e9}b", &[(0, 4)]);
    assert_eq!(replace_story_text(&uni, None, 2, 2, "x"), Err(LayoutError::InvalidInput));
}
