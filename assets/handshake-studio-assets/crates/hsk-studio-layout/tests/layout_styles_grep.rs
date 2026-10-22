//! Lane SMB-LAYOUT-UI slice 3: GREP (pattern) styles on paragraph styles and object styles.
//! Hand-computed oracles over the toy font of `support_layout` (6 pt glyphs).
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const SIDES: [PageSide; 2] = [PageSide::RightHand, PageSide::RightHand];

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => std::panic::resume_unwind(p),
        Err(_) => panic!("case {name} exceeded its 30-second deadline"),
    }
}

fn style(n: u32, scope: TypeStyleScope, name: &str) -> StudioTypeStyle {
    StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: id("STYS", n),
        scope,
        name: name.into(),
        based_on: None,
        next_style: None,
        font_family: None,
        font_style: None,
        font_size: None,
        fill: None,
        line_height: None,
        letter_spacing: None,
        align: None,
        grep_styles: Vec::new(),
    }
}
fn grep(pattern: &str, cs: u32) -> StudioGrepStyle {
    StudioGrepStyle { pattern: pattern.into(), character_style: id("STYS", cs) }
}
/// Body (1, paragraph, rule `\d+` -> Num), Num (2, char, +2 pt letter spacing), Em (3, char,
/// +1 pt), Child (4, paragraph, based on Body, rule `now` -> Em), Next (5, paragraph).
fn styles() -> StudioTypeStyleRecords {
    let mut t = StudioTypeStyleRecords::new(id("SDOC", 1));
    let mut body = style(1, TypeStyleScope::Paragraph, "Body");
    body.grep_styles.push(grep(r"\d+", 2));
    body.next_style = Some(id("STYS", 5));
    let mut num = style(2, TypeStyleScope::Character, "Num");
    num.letter_spacing = Some(StudioLetterSpacing::Length { length: len(2.0) });
    let mut em = style(3, TypeStyleScope::Character, "Em");
    em.letter_spacing = Some(StudioLetterSpacing::Length { length: len(1.0) });
    let mut child = style(4, TypeStyleScope::Paragraph, "Child");
    child.based_on = Some(id("STYS", 1));
    child.grep_styles.push(grep("now", 3));
    t.styles = vec![body, num, em, child, style(5, TypeStyleScope::Paragraph, "Next")];
    t
}

#[test]
fn grep_styles_split_ranges_and_reach_composition() {
    let text = "Call 555 now 12";
    let doc = document(vec![frame_layer(1, 1, spec(0, 0.0, 0.0, 120.0, 60.0), None, None)], SIDES);
    let (mut st, paras) = story(1, text, &[(0, 15)]);
    st.paragraphs[0].style_ref = Some(id("STYS", 4));
    let t = styles();
    validate_type_style_records(&t, &doc, Budget::default(), &CancellationToken::default(), &Unresolved).unwrap();
    let (styled, matches) = apply_grep_styles(&st, None, &t).unwrap();
    // Parent rules first (\d+ from Body), then the child's own (now).
    assert_eq!(
        matches.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
        vec![(5, 8), (13, 15), (9, 12)]
    );
    let ranges: Vec<(u64, u64, Option<String>)> = styled.character_ranges.iter().map(|r| (r.start, r.end, r.style_ref.clone())).collect();
    assert_eq!(
        ranges,
        vec![
            (0, 5, None),
            (5, 8, Some(id("STYS", 2))),
            (8, 9, None),
            (9, 12, Some(id("STYS", 3))),
            (12, 13, None),
            (13, 15, Some(id("STYS", 2))),
        ]
    );
    // Composition with the GREP-styled ranges: 15 glyphs x 6 + 5 digits x 2 + 3 letters x 1 = 103.
    let opts = TypesetOptions { styles: Some(StyleSource { records: &t, default_font_size_pt: 12.0 }), ..TypesetOptions::default() };
    let p = reflow_flow(
        &ReflowRequest {
            document: &doc,
            story: &styled,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: 2,
            limits: Limits::default(),
        },
        &opts,
        &records(),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(p.frames[0].lines[0].width_pt, 103.0);
    // Folio refuses rules on character styles, oversized patterns and paragraph-style targets.
    let refuse = |edit: &dyn Fn(&mut StudioTypeStyleRecords)| {
        let mut bad = styles();
        edit(&mut bad);
        validate_type_style_records(&bad, &doc, Budget::default(), &CancellationToken::default(), &Unresolved).unwrap_err().code
    };
    assert_eq!(refuse(&|b| b.styles[1].grep_styles.push(grep("x", 3))), Code::InvalidField);
    assert_eq!(refuse(&|b| b.styles[0].grep_styles[0].pattern = "a".repeat(1025)), Code::InvalidField);
    assert_eq!(refuse(&|b| b.styles[0].grep_styles[0].character_style = id("STYS", 5)), Code::PayloadMismatch);
    assert_eq!(refuse(&|b| b.styles[0].grep_styles[0].character_style = id("STYS", 9)), Code::DanglingReference);
}

#[test]
fn grep_hostile_patterns_are_bounded_or_refused() {
    bounded_case("grep_hostile_patterns_are_bounded_or_refused", || {
        let doc_text = |s: String| {
            let n = s.len() as u64;
            let (mut st, _) = story(1, &s, &[(0, n)]);
            st.paragraphs[0].style_ref = Some(id("STYS", 1));
            st
        };
        let with = |pattern: &str| {
            let mut t = styles();
            t.styles[0].grep_styles[0].pattern = pattern.into();
            t
        };
        // Catastrophic for a backtracking engine; linear here: 100 000 'a' then '!' -> no match.
        let long = doc_text(format!("{}!", "a".repeat(100_000)));
        let started = std::time::Instant::now();
        assert!(grep_matches(&long, None, &with("(a+)+$")).unwrap().is_empty());
        assert!(grep_matches(&long, None, &with("(a|aa)+b")).unwrap().is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        let short = doc_text("abc".into());
        let code = |p: &str| grep_matches(&short, None, &with(p)).unwrap_err().code();
        assert_eq!(code("(?=a)b"), "grep_pattern_unsupported");
        assert_eq!(code(r"(a)\1"), "grep_pattern_unsupported");
        assert_eq!(code("a{1000}{1000}"), "grep_pattern_unsupported");
        assert_eq!(code(&format!("{}a{}", "(".repeat(200), ")".repeat(200))), "grep_pattern_unsupported");
        assert_eq!(code("(ab"), "grep_pattern_invalid");
        // Every character a match: 100 001 matches exceed the per-story budget.
        let many = doc_text("x".repeat(100_001));
        assert_eq!(grep_matches(&many, None, &with(".")).unwrap_err(), GrepError::BudgetExceeded);
        // Empty matches are skipped, never styled.
        assert!(grep_matches(&short, None, &with("z*")).unwrap().is_empty());
        let mut missing = styles();
        missing.styles[0].grep_styles[0].character_style = id("STYS", 9);
        assert_eq!(grep_matches(&short, None, &missing).unwrap_err(), GrepError::StyleMissing(id("STYS", 9)));
    });
}

#[test]
fn object_styles_apply_in_one_operation() {
    let mut picture = item_layer(5, on_spread());
    picture.name = "picture".into();
    let doc = document(vec![frame_layer(1, 1, spec(0, 0.0, 0.0, 130.0, 48.0), None, None), picture], SIDES);
    let text = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj\nkk";
    let (st, paras) = story(1, text, &[(0, 49), (50, 52)]);
    let t = styles();
    let mut flow = records();
    let mut base = StudioObjectStyle::new("base", "Base");
    base.categories.enable_text_frame_general_options = true;
    base.categories.enable_text_frame_baseline_options = true;
    base.categories.enable_fill = true;
    base.text_column_count = Some(2);
    base.text_column_gutter = Some(len(10.0));
    base.vertical_balance_columns = Some(true);
    base.minimum_first_baseline_offset = Some(len(14.0));
    base.fill = Some(StudioRgba { r: 1.0, g: 1.0, b: 0.0, a: 1.0, colour_profile_id: ProfileBinding::Unassigned(()) });
    let mut child = StudioObjectStyle::new("child", "Child");
    child.based_on = Some("base".into());
    child.categories.enable_paragraph_style = true;
    child.categories.enable_anchored_object_options = true;
    child.categories.enable_frame_fitting_options = true;
    child.paragraph_style = Some(id("STYS", 1));
    child.apply_next_paragraph_style = true;
    child.anchored_settings = Some(StudioAnchoredSettings { anchored_position: StudioAnchorPosition::Custom, ..Default::default() });
    child.frame_fitting = Some(StudioFit::Proportionally);
    child.fitting_anchor = Some(StudioFitAnchor::Center);
    child.miter_limit = Some(4.0);
    flow.object_styles = vec![base, child];
    flow.insert_anchored_object(&id("SLYR", 5), &id("STXT", 1), 0, StudioAnchorPosition::Inline, [len(6.0), len(6.0)]).unwrap();
    validate_layout_flow_records(&flow, &doc, &CancellationToken::default()).unwrap();
    let mut longdoc = StudioLongDocRecords::new(id("SDOC", 1));
    longdoc.graphic_frames.push(StudioGraphicFrame {
        layer_id: id("SLYR", 5),
        page_id: "page-1".into(),
        bounds: Bounds { x: len(0.0), y: len(100.0), width: len(6.0), height: len(6.0) },
        shape: StudioFrameShape::Rectangle,
        content_width: len(6.0),
        content_height: len(6.0),
        fit: None,
        frame_fitting: None,
        anchor: StudioFitAnchor::TopLeft,
        crop: None,
    });

    // Apply "child" (inherits base's frame options) to the text frame: one operation.
    let a = apply_object_style(&doc, &flow, Some(&longdoc), Some(&st), Some(&t), &id("SLYR", 1), "child").unwrap();
    let f = a.document.layers[0].text_frame.as_ref().unwrap();
    assert_eq!((f.text_column_count, f.text_column_gutter.value, f.minimum_first_baseline_offset.value), (2, 10.0, 14.0));
    assert!(a.flow.frame_options(&id("SLYR", 1)).unwrap().vertical_balance_columns);
    let story = a.story.as_ref().unwrap();
    assert_eq!(
        story.paragraphs.iter().map(|p| p.style_ref.clone()).collect::<Vec<_>>(),
        vec![Some(id("STYS", 1)), Some(id("STYS", 5))]
    );
    assert_eq!(a.stored_only, vec!["fill"]);
    assert_eq!(a.flow.applied_object_styles, vec![StudioAppliedObjectStyle { layer_id: id("SLYR", 1), style_id: "child".into() }]);
    validate_layout_flow_records(&a.flow, &a.document, &CancellationToken::default()).unwrap();
    // The restyled frame composes in two balanced columns from baseline 14: five lines of P0 and
    // P1 -> k = ceil(6 / 2) = 3 per column.
    let p = run(&a.document, story, &paras, &a.flow).unwrap();
    let cols: Vec<(usize, f64)> = p.frames[0].lines.iter().map(|l| (l.column, l.baseline_y_pt)).collect();
    assert_eq!(cols, vec![(0, 14.0), (0, 26.0), (0, 38.0), (1, 14.0), (1, 26.0), (1, 38.0)]);
    // Applied to the anchored picture: anchored settings and frame fitting follow the style.
    let b = apply_object_style(&doc, &flow, Some(&longdoc), None, None, &id("SLYR", 5), "child").unwrap();
    assert_eq!(b.flow.anchored_objects[0].settings.anchored_position, StudioAnchorPosition::Custom);
    let g = &b.longdoc.as_ref().unwrap().graphic_frames[0];
    assert_eq!((g.frame_fitting, g.anchor), (Some(StudioFit::Proportionally), StudioFitAnchor::Center));
    // Typed refusals: unknown style or layer; Folio: bounds, cycles, duplicate names.
    assert_eq!(apply_object_style(&doc, &flow, None, None, None, &id("SLYR", 1), "nope").unwrap_err(), LayoutError::InvalidInput);
    assert_eq!(apply_object_style(&doc, &flow, None, None, None, &id("SLYR", 9), "base").unwrap_err(), LayoutError::InvalidInput);
    let code = |edit: &dyn Fn(&mut StudioLayoutFlowRecords)| {
        let mut bad = flow.clone();
        edit(&mut bad);
        validate_layout_flow_records(&bad, &doc, &CancellationToken::default()).unwrap_err().code
    };
    assert_eq!(code(&|r| r.object_styles[1].miter_limit = Some(0.5)), Code::InvalidField);
    assert_eq!(code(&|r| r.object_styles[0].left_arrow_head_scale = Some(1001.0)), Code::InvalidField);
    assert_eq!(code(&|r| r.object_styles[0].based_on = Some("child".into())), Code::RenderCycle);
    assert_eq!(code(&|r| r.object_styles[1].name = "Base".into()), Code::DuplicateIdentity);
    assert_eq!(code(&|r| r.object_styles[1].based_on = Some("ghost".into())), Code::DanglingReference);
}
