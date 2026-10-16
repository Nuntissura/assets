//! Shared fixture of the SMB-LAYOUT-UI-2 tests (same document as `viewer_layout_ui.rs`): one spread
//! of two 200 x 200 pages, frame 1 (story 1, TEXT) on page 1 at (10, 10), 130 x 32; styles Body
//! (paragraph, STYS 1) and Num (character, STYS 2). Preview shaping: 12 pt paragraphs advance 6 pt
//! per glyph, 15 pt lines, first baseline 10 pt.
#![allow(dead_code)]
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutDoc, LayoutEditor};

pub fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
pub fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
pub fn page(key: &str) -> StudioPage {
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::RightHand, margins: edges(10.0), bleed: None }
}
pub const TEXT: &str = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj";
pub fn style(n: u32, scope: TypeStyleScope, name: &str) -> StudioTypeStyle {
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
pub fn doc() -> LayoutDoc {
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
/// Rect shapes painted in one UI frame whose stroke (or fill) has this colour.
pub fn rects_of(shapes: &[egui::epaint::ClippedShape], colour: egui::Color32, fill: bool) -> usize {
    fn walk(s: &egui::Shape, colour: egui::Color32, fill: bool, n: &mut usize) {
        match s {
            egui::Shape::Rect(r) if (if fill { r.fill } else { r.stroke.color }) == colour => *n += 1,
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, colour, fill, n)),
            _ => {}
        }
    }
    let mut n = 0;
    shapes.iter().for_each(|c| walk(&c.shape, colour, fill, &mut n));
    n
}
/// The headless visual surface: three UI frames one second apart on a 1600 x 2400 screen (egui lays
/// a new window out invisibly on its first frame and fades it in over `animation_time`, which
/// scales every colour); the widget ids and epaint shapes of the last frame, at full opacity.
pub fn ui_frames(ed: &mut LayoutEditor) -> (Vec<&'static str>, Vec<egui::epaint::ClippedShape>) {
    let ctx = egui::Context::default();
    let input = |t: f64| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 2400.0))),
        time: Some(t),
        ..egui::RawInput::default()
    };
    for t in [0.0, 1.0] {
        let _ = ctx.run(input(t), |ctx| {
            ed.ui(ctx);
        });
    }
    let mut drawn = Vec::new();
    let out = ctx.run(input(2.0), |ctx| drawn = ed.ui(ctx));
    (drawn, out.shapes)
}

/// Fill and stroke colours of every rect in the shapes (assertion diagnostics).
pub fn rect_colours(shapes: &[egui::epaint::ClippedShape]) -> Vec<(egui::Color32, egui::Color32)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(egui::Color32, egui::Color32)>) {
        match s {
            egui::Shape::Rect(r) => out.push((r.fill, r.stroke.color)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}
