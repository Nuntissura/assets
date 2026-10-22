//! Lane SMB-LAYOUT-UI slice 4: the layout editing surface, headless. Preview shaping: 12 pt
//! paragraphs advance 6 pt per glyph, line height 15, ascent 9 (descent 6); frames use a fixed
//! first baseline of 10 pt, so a 32 pt frame holds baselines 10 and 25.
#![cfg(feature = "layout-ui")]
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutDoc, LayoutEdit, LayoutEditor, LayoutUiError, MAX_HISTORY, Tool, widget};

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
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::RightHand, margins: edges(10.0), bleed: None }
}
const TEXT: &str = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj";
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
        grep_styles: vec![],
    }
}
/// One spread of two 200 x 200 pages; frame 1 (story 1, TEXT) on page 1 at (10, 10), 130 x 32.
fn doc() -> LayoutDoc {
    let unassigned = || ProfileBinding::Unassigned(());
    let frame = StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", 1),
        name: "frame".into(),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: "page-1".into(),
            bounds: Bounds { x: len(10.0), y: len(10.0), width: len(130.0), height: len(32.0) },
            inset_spacing: edges(0.0),
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
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
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
            page_spread_id: id("SPGS", 1),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(400.0), height: len(200.0) },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers: vec![frame],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    };
    let n = TEXT.len() as u64;
    let story = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: TEXT.into() },
        paragraphs: vec![StoryParagraph { start: 0, end: n, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end: n, style_ref: None, applied_language: None }],
    };
    let mut styles = StudioTypeStyleRecords::new(id("SDOC", 1));
    styles.styles = vec![style(1, TypeStyleScope::Paragraph, "Body"), style(2, TypeStyleScope::Character, "Num")];
    LayoutDoc { document, stories: vec![story], flow: StudioLayoutFlowRecords::new(id("SDOC", 1)), styles }
}
/// (frame, column, baseline, x_start, source start, end) of every line of story 1.
fn lines(ed: &LayoutEditor) -> Vec<(usize, usize, f64, f64, u64, u64)> {
    let p = ed.compose(&id("STXT", 1)).unwrap();
    p.frames
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| f.lines.iter().map(move |l| (fi, l.column, l.baseline_y_pt, l.x_start_pt, l.source.start, l.source.end)))
        .collect()
}

#[test]
fn frame_thread_columns_text_and_styles_each_one_undo_step() {
    let original = doc();
    let mut ed = LayoutEditor::new(original.clone());
    // 21 glyphs fit a 130 pt line: lines 0..20 and 20..40 fill the frame, 40..49 is overset.
    assert_eq!(ed.compose(&id("STXT", 1)).unwrap().overset.map(|o| o.start), Some(40));

    // Text-frame tool: draw a frame on page 2, thread frame 1 into it, two columns in frame 1.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 10.0, y: 10.0, w: 60.0, h: 30.0 }).unwrap();
    let f2 = ed.selected.clone().unwrap();
    assert_eq!((ed.history_len(), ed.doc().stories.len()), ((1, 0), 2));
    ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: f2.clone() }).unwrap();
    assert_eq!(ed.doc().stories.len(), 1, "the empty story of the new frame is merged away");
    ed.apply(LayoutEdit::SetColumns { frame: id("SLYR", 1), count: 2, gutter: 10.0 }).unwrap();
    assert_eq!(ed.history_len(), (3, 0));
    // Two 60 pt columns (gutter 10) of two lines each, then the threaded frame on page 2.
    assert_eq!(
        lines(&ed),
        vec![
            (0, 0, 10.0, 0.0, 0, 10),
            (0, 0, 25.0, 0.0, 10, 20),
            (0, 1, 10.0, 70.0, 20, 30),
            (0, 1, 25.0, 70.0, 30, 40),
            (1, 0, 10.0, 0.0, 40, 49),
        ]
    );
    let p = ed.compose(&id("STXT", 1)).unwrap();
    // Glyph positions: 'e' (byte 20) at frame x 10 + column 70; 'i' (byte 40) at page 2 x 200 + 10.
    assert_eq!((p.frames[0].origin_x_pt + p.frames[0].lines[2].x_start_pt, p.frames[1].origin_x_pt), (80.0, 210.0));

    // Story editor: append a paragraph, style it, character-style a range.
    ed.apply(LayoutEdit::ReplaceText { story: id("STXT", 1), start: 49, end: 49, text: "\nkk".into() }).unwrap();
    ed.apply(LayoutEdit::ApplyParagraphStyle { story: id("STXT", 1), paragraph: 1, style: Some(id("STYS", 1)) }).unwrap();
    ed.apply(LayoutEdit::ApplyCharacterStyle { story: id("STXT", 1), start: 0, end: 4, style: Some(id("STYS", 2)) }).unwrap();
    let rows = ed.story_rows(&id("STXT", 1)).unwrap();
    assert_eq!(
        rows.iter().map(|r| (r.text.as_str(), r.style_name.as_deref(), r.overset)).collect::<Vec<_>>(),
        vec![(TEXT, None, false), ("kk", Some("Body"), true)]
    );
    let ranges: Vec<(u64, u64, Option<String>)> = ed.doc().stories[0].character_ranges.iter().map(|r| (r.start, r.end, r.style_ref.clone())).collect();
    assert_eq!(ranges, vec![(0, 4, Some(id("STYS", 2))), (4, 52, None)]);
    assert_eq!(ed.history_len(), (6, 0));
    let last = ed.doc().clone();

    // Refused edits change neither the state nor the history.
    let refused = [
        (LayoutEdit::Thread { from: f2.clone(), to: id("SLYR", 1) }, "thread_cycle"),
        (LayoutEdit::Thread { from: id("SLYR", 1), to: f2.clone() }, "already_threaded"),
        (LayoutEdit::Thread { from: id("SLYR", 1), to: id("SLYR", 1) }, "thread_cycle"),
        (LayoutEdit::SetColumns { frame: id("SLYR", 1), count: 0, gutter: 0.0 }, "invalid_geometry"),
        (LayoutEdit::SetColumns { frame: id("SLYR", 1), count: 2, gutter: 200.0 }, "invalid_geometry"),
        (LayoutEdit::ApplyParagraphStyle { story: id("STXT", 1), paragraph: 0, style: Some(id("STYS", 2)) }, "wrong_style_scope"),
        (LayoutEdit::ApplyCharacterStyle { story: id("STXT", 1), start: 0, end: 4, style: Some(id("STYS", 9)) }, "unknown_style"),
        (LayoutEdit::ReplaceText { story: id("STXT", 1), start: 60, end: 61, text: String::new() }, "invalid_input"),
        (LayoutEdit::DrawFrame { page_id: "page-9".into(), x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, "unknown_page"),
        (LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 0.0, y: 0.0, w: f64::NAN, h: 1.0 }, "invalid_geometry"),
        (LayoutEdit::Unthread { from: f2.clone() }, "not_threaded"),
    ];
    for (edit, code) in refused {
        assert_eq!(ed.apply(edit).unwrap_err().code(), code);
        assert_eq!((ed.doc(), ed.history_len()), (&last, (6, 0)));
    }

    // Six undos restore the original exactly; six redos the last state.
    for _ in 0..6 {
        assert!(ed.undo());
    }
    assert!(!ed.undo());
    assert_eq!(ed.doc(), &original);
    for _ in 0..6 {
        assert!(ed.redo());
    }
    assert_eq!(ed.doc(), &last);
    // Unthread: frames after the cut get a new empty story; one undo step.
    ed.apply(LayoutEdit::Unthread { from: id("SLYR", 1) }).unwrap();
    assert_eq!(ed.doc().stories.len(), 2);
    assert!(ed.undo());
    assert_eq!(ed.doc(), &last);
}

#[test]
fn threading_into_text_or_a_loop_is_refused_and_history_is_bounded() {
    let mut ed = LayoutEditor::new(doc());
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 0.0, y: 0.0, w: 50.0, h: 50.0 }).unwrap();
    let f2 = ed.selected.clone().unwrap();
    let s2 = ed.story_of_frame(&f2).unwrap().story_id.clone();
    ed.apply(LayoutEdit::ReplaceText { story: s2, start: 0, end: 0, text: "x".into() }).unwrap();
    assert_eq!(ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: f2.clone() }).unwrap_err(), LayoutUiError::TargetNotEmpty(f2.clone()));
    // A chain 1 -> 3 -> 4: threading 4 back into 1 is a loop.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 60.0, y: 0.0, w: 50.0, h: 50.0 }).unwrap();
    let f3 = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 120.0, y: 0.0, w: 50.0, h: 50.0 }).unwrap();
    let f4 = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: f3.clone() }).unwrap();
    ed.apply(LayoutEdit::Thread { from: f3.clone(), to: f4.clone() }).unwrap();
    assert_eq!(ed.apply(LayoutEdit::Thread { from: f4.clone(), to: id("SLYR", 1) }).unwrap_err().code(), "thread_cycle");
    // A hand-built cyclic document reaches layout as a typed refusal in the projection.
    let mut cyc = ed.doc().clone();
    for l in &mut cyc.document.layers {
        if l.layer_id == id("SLYR", 1) {
            l.text_frame.as_mut().unwrap().previous_text_frame = Some(f4.clone());
        }
        if l.layer_id == f4 {
            l.text_frame.as_mut().unwrap().next_text_frame = Some(id("SLYR", 1));
        }
    }
    let cyc_ed = LayoutEditor::new(cyc);
    assert_eq!(cyc_ed.state_json()["stories"][0]["refusal"], "cyclic_thread");
    // The history keeps the newest MAX_HISTORY steps.
    for i in 0..(MAX_HISTORY + 20) {
        ed.apply(LayoutEdit::SetColumns { frame: id("SLYR", 1), count: 1 + (i % 3) as u32, gutter: 4.0 }).unwrap();
    }
    assert_eq!(ed.history_len(), (MAX_HISTORY, 0));
}

#[test]
fn one_ui_frame_draws_the_layout_ids_and_projects_state() {
    let mut ed = LayoutEditor::new(doc());
    ed.selected = Some(id("SLYR", 1));
    ed.tool = Tool::Frame;
    let ctx = egui::Context::default();
    let mut drawn = Vec::new();
    let _ = ctx.run(egui::RawInput::default(), |ctx| drawn = ed.ui(ctx));
    for w in [
        widget::PANEL,
        widget::PAGE_VIEW,
        widget::TOOL_SELECT,
        widget::TOOL_FRAME,
        widget::TOOL_THREAD,
        widget::UNDO,
        widget::REDO,
        widget::COLUMNS,
        widget::GUTTER,
        widget::APPLY_COLUMNS,
        widget::STORY_EDITOR,
        widget::STYLE_COLUMN,
        widget::STORY_TEXT,
        widget::COMMIT_TEXT,
        widget::CHAR_STYLE,
        widget::APPLY_CHAR_STYLE,
    ] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    let s = ed.state_json();
    assert_eq!(s["schema"], "hsk.studio.harness.layout_ui@1");
    assert_eq!(s["tool"], "frame");
    assert_eq!(s["frames"][0]["columns"], 1);
    assert_eq!(s["stories"][0]["lines"], 2);
    assert_eq!(s["stories"][0]["overset"], serde_json::json!([40, 49]));
    assert_eq!(ed.frame_at("page-1", 20.0, 20.0), Some(id("SLYR", 1)));
    assert_eq!(ed.frame_at("page-2", 20.0, 20.0), None);
}
