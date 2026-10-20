#![allow(dead_code)]
//! Hand-computed oracles. Toy font: every glyph (letters and spaces) advances 6 pt, line height 12 pt,
//! ascent 10 pt (descent 2 pt), first baseline 10 pt below the frame top, no insets. A 60 pt line
//! therefore holds 10 glyphs. Pages, frames, threads and the story are Folio types.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline,
    GeometryUnit, GraphSchema, LayerSchema, Length, LayoutEdges, NodeKind, NodeRef, PageSide,
    PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection,
    StoryParagraph, StoryRange, StoryTextKind, StudioDocument,
    StudioLayer, StudioLayerGraph, StudioPage, StudioPageSpread, StudioTextFrame, StudioTextStory,
    TextStorySchema, VerticalJustification,
};
use hsk_studio_layout::*;
use hsk_studio_folio::{StudioLetterSpacing, StudioLineHeight, StudioTextAlign, StudioTypeStyle, StudioTypeStyleRecords, TypeStyleSchema, TypeStyleScope};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length {
        value: v,
        unit: GeometryUnit::Pt,
    }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges {
        top: len(v),
        right: len(v),
        bottom: len(v),
        left: len(v),
    }
}
fn page(key: &str) -> StudioPage {
    StudioPage {
        page_id: key.into(),
        width: len(100.0),
        height: len(100.0),
        side: PageSide::RightHand,
        margins: edges(5.0),
        bleed: None,
    }
}
fn spread() -> StudioPageSpread {
    StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: id("SPGS", 1),
        name: "spread".into(),
        parent: Parent::Root(()),
        bounds: Bounds {
            x: len(0.0),
            y: len(0.0),
            width: len(200.0),
            height: len(100.0),
        },
        pages: vec![page("page-1"), page("page-2")],
    }
}

/// Frame geometry on page `page` (0 or 1) of the one spread; obstacles in frame coordinates.
#[derive(Clone)]
struct FrameSpec {
    page: usize,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    obstacles: Vec<Rect>,
}
fn frame(page: usize, x: f64, y: f64, w: f64, h: f64, obstacles: Vec<Rect>) -> FrameSpec {
    FrameSpec {
        page,
        x,
        y,
        w,
        h,
        obstacles,
    }
}
/// Layer `n` carrying `spec` as a text frame of story `story`, linked to the layers numbered
/// `previous` / `next`.
fn frame_layer(
    n: u32,
    story: u32,
    spec: &FrameSpec,
    previous: Option<u32>,
    next: Option<u32>,
) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef {
            node_kind: NodeKind::PageSpread,
            node_id: id("SPGS", 1),
        }),
        kind: "text".into(),
        payload: Payload::Text {
            story_id: id("STXT", story),
        },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: format!("page-{}", spec.page + 1),
            bounds: Bounds {
                x: len(spec.x),
                y: len(spec.y),
                width: len(spec.w),
                height: len(spec.h),
            },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: previous.map(|p| id("SLYR", p)),
            next_text_frame: next.map(|q| id("SLYR", q)),
        }),
    }
}
/// Frames 1..=n of story 1 linked into one chain in order.
fn document(specs: &[FrameSpec], revision: u64) -> StudioDocument {
    let n = specs.len() as u32;
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![spread()],
        layers: specs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let k = i as u32 + 1;
                frame_layer(
                    k,
                    1,
                    s,
                    (k > 1).then_some(k - 1),
                    (k < n).then_some(k + 1),
                )
            })
            .collect(),
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}
fn story_of(story: u32, text: &str) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", story),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline {
            text_utf8: text.into(),
        },
        paragraphs: vec![StoryParagraph {
            start: 0,
            end,
            style_ref: None,
            paragraph_direction: None,
        }],
        character_ranges: vec![StoryRange {
            start: 0,
            end,
            style_ref: None,
            applied_language: Some("en-US".into()),
        }],
    }
}
fn para(text: &str) -> ShapedParagraph {
    let source = SourceSpan {
        start: 0,
        end: text.len() as u64,
    };
    ShapedParagraph::from_fixed_advance(text, source, 6.0, 12.0, 10.0).unwrap()
}
struct Case {
    story: StudioTextStory,
    doc: StudioDocument,
    paragraphs: Vec<ShapedParagraph>,
    obstacles: Vec<FrameObstacles>,
    composer: Composer,
    limits: Limits,
    expected: u64,
}
impl Case {
    fn new(text: &str, frames: Vec<FrameSpec>, composer: Composer) -> Self {
        Self {
            story: story_of(1, text),
            doc: document(&frames, 3),
            paragraphs: vec![para(text)],
            obstacles: frames
                .iter()
                .enumerate()
                .map(|(i, f)| FrameObstacles {
                    frame_id: id("SLYR", i as u32 + 1),
                    rects: f.obstacles.clone(),
                })
                .collect(),
            composer,
            limits: Limits::default(),
            expected: 3,
        }
    }
    fn text(&self) -> &str {
        match &self.story.content {
            StoryContent::Inline { text_utf8 } => text_utf8,
            StoryContent::Stored { .. } => panic!("stored content"),
        }
    }
    /// The frame (0-based) as a mutable Folio text frame.
    fn frame_mut(&mut self, index: usize) -> &mut StudioTextFrame {
        self.doc.layers[index].text_frame.as_mut().unwrap()
    }
    fn run(&self, cancel: &CancellationToken) -> Result<LayoutProposal, LayoutError> {
        reflow(
            &ReflowRequest {
                document: &self.doc,
                story: &self.story,
                resolved_text: None,
                paragraphs: &self.paragraphs,
                obstacles: &self.obstacles,
                composer: self.composer,
                space: SpaceParams::default(),
                expected_revision: self.expected,
                limits: self.limits,
            },
            cancel,
        )
    }
    fn run_t(&self, opts: &TypesetOptions<'_>) -> Result<LayoutProposal, LayoutError> {
        reflow_typeset(
            &ReflowRequest {
                document: &self.doc,
                story: &self.story,
                resolved_text: None,
                paragraphs: &self.paragraphs,
                obstacles: &self.obstacles,
                composer: self.composer,
                space: SpaceParams::default(),
                expected_revision: self.expected,
                limits: self.limits,
            },
            opts,
            &CancellationToken::default(),
        )
    }
    fn ok(&self) -> LayoutProposal {
        self.run(&CancellationToken::default()).unwrap()
    }
    fn err(&self) -> LayoutError {
        self.run(&CancellationToken::default()).unwrap_err()
    }
}
fn one_frame(w: f64, h: f64) -> Vec<FrameSpec> {
    vec![frame(0, 0.0, 0.0, w, h, vec![])]
}
fn span(start: u64, end: u64) -> SourceSpan {
    SourceSpan { start, end }
}
/// (glyph range, source span, width, hang) of a line.
fn shape(l: &Line) -> (std::ops::Range<usize>, SourceSpan, f64, f64) {
    (l.glyph_range.clone(), l.source, l.width_pt, l.hang_pt)
}


fn hy(advance: f64) -> HyphenationParams {
    HyphenationParams {
        hyphen_advance_pt: advance,
        ..Default::default()
    }
}

#[test]
fn justify_widens_spaces_to_the_limit_and_leaves_the_last_line() {
    bounded_case("justify", || {
        // 60 pt slot (10 glyphs). First-fit: "aa bb cc " then "dd"; line 1 natural 48 (8 glyphs,
        // 2 inner spaces of 6 pt = 12 pt). Max word spacing 3.0: need +12 pt = 12 * (2.0 - 1.0).
        let case = Case::new("aa bb cc dd", one_frame(60.0, 24.0), Composer::FirstFit);
        let jp = JustifyParams {
            word_spacing: Spacing { min: 0.8, desired: 1.0, max: 3.0 },
            ..Default::default()
        };
        let o = TypesetOptions {
            justify: Some(jp),
            alignment: Alignment::Justified,
            ..Default::default()
        };
        let p = case.run_t(&o).unwrap();
        let lines = &p.frames[0].lines;
        assert_eq!(lines.len(), 2);
        let j = lines[0].justification.unwrap();
        assert_eq!((j.word_scale, j.letter_gap_pt, j.residual_pt), (2.0, 0.0, 0.0));
        assert_eq!(lines[0].width_pt, 48.0);
        assert!(lines[1].justification.is_none(), "last line stays ragged");
        // Default limits (max 1.33): the spaces reach 1.33 and 12 * 0.33 = 3.96 is all they give.
        let d = TypesetOptions {
            justify: Some(JustifyParams::default()),
            alignment: Alignment::Justified,
            ..Default::default()
        };
        let j = case.run_t(&d).unwrap().frames[0].lines[0].justification.unwrap();
        assert_eq!(j.word_scale, 1.33);
        assert!((j.residual_pt - (12.0 - 3.96)).abs() < 1e-9, "{}", j.residual_pt);
        // Plain reflow carries no justification.
        assert!(case.ok().frames[0].lines.iter().all(|l| l.justification.is_none()));
    });
}

#[test]
fn justify_metrics_shrink_letters_and_desired() {
    // Shrink: natural 54, slot 52, spaces 6 pt, 8 gaps; min 0.8 gives 1.2 pt, 0.8 pt remain.
    let base = JustifyParams::default();
    let j = justify_metrics(54.0, 52.0, 6.0, 8, &base);
    assert_eq!((j.word_scale, j.letter_gap_pt), (0.8, 0.0));
    assert!((j.residual_pt + 0.8).abs() < 1e-9);
    // Letter spacing in [-0.1, 0] x 6 pt takes the rest: 0.8 pt / 8 gaps = 0.1 pt per gap.
    let jp = JustifyParams {
        letter_spacing: Spacing { min: -0.1, desired: 0.0, max: 0.1 },
        reference_space_pt: 6.0,
        ..base
    };
    let j = justify_metrics(54.0, 52.0, 6.0, 8, &jp);
    assert_eq!(j.word_scale, 0.8);
    assert!((j.letter_gap_pt + 0.1).abs() < 1e-9 && j.residual_pt.abs() < 1e-9);
    // Desired 1.1 on a line that already fits exactly: spaces go back to 1.0.
    let jp = JustifyParams {
        word_spacing: Spacing { min: 0.8, desired: 1.1, max: 1.33 },
        ..base
    };
    let j = justify_metrics(48.0, 48.0, 12.0, 7, &jp);
    assert!((j.word_scale - 1.0).abs() < 1e-12 && j.residual_pt == 0.0);
    // No spaces and no letter range: everything is residual.
    let j = justify_metrics(24.0, 60.0, 0.0, 3, &base);
    assert_eq!((j.word_scale, j.residual_pt), (1.0, 36.0));
}

#[test]
fn hyphenation_breaks_a_word_with_the_hyphen_width() {
    bounded_case("hyphenation", || {
        // 60 pt = 10 glyphs. "aaaa hy-" is 7 glyphs + 6 pt hyphen = 48; "aaaa hyphen-" is 72.
        let text = "aaaa hyphenation";
        for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default())] {
            let case = Case::new(text, one_frame(60.0, 24.0), composer);
            let o = TypesetOptions { hyphenation: Some(hy(6.0)), ..Default::default() };
            let p = case.run_t(&o).unwrap();
            let lines = &p.frames[0].lines;
            assert_eq!(lines.len(), 2, "{composer:?}");
            assert_eq!((lines[0].glyph_range.clone(), lines[0].width_pt), (0..7, 48.0));
            assert!(lines[0].hyphenated && !lines[0].overfull);
            assert_eq!((lines[1].glyph_range.clone(), lines[1].width_pt), (7..16, 54.0));
            assert!(!lines[1].hyphenated);
            assert_eq!(p.overset, None);
            // Without hyphenation the 11-letter word (66 pt) cannot fit the slot.
            let plain = case.ok();
            assert!(plain.frames[0].lines.iter().any(|l| l.overfull && !l.hyphenated));
        }
    });
}

#[test]
fn consecutive_hyphen_limit() {
    bounded_case("ladder", || {
        let h = Hyphenator::from_pattern_text("[exceptions]\nab-ab-ab-ab\n").unwrap();
        // 18 pt slot = "ab" + hyphen. "abababab" needs three hyphens.
        let frames = || vec![frame(0, 0.0, 0.0, 18.0, 60.0, vec![])];
        let go = |composer, limit| {
            let case = Case::new("abababab", frames(), composer);
            let o = TypesetOptions {
                hyphenation: Some(HyphenationParams { consecutive_limit: limit, ..hy(6.0) }),
                hyphenator: Some(&h),
                ..Default::default()
            };
            case.run_t(&o).unwrap().frames[0].lines.clone()
        };
        for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default())] {
            let lines = go(composer, 3);
            let shape: Vec<_> = lines.iter().map(|l| (l.hyphenated, l.overfull)).collect();
            assert_eq!(shape, vec![(true, false), (true, false), (true, false), (false, false)]);
            // Limit 2: a third hyphenated line is not allowed, so the third line cannot be
            // broken inside the word and goes overfull.
            let lines = go(composer, 2);
            let run = lines.iter().take_while(|l| l.hyphenated && !l.overfull).count();
            assert!(run <= 2, "{composer:?} {lines:?}");
            assert!(lines.iter().any(|l| l.overfull), "{composer:?}");
        }
    });
}

#[test]
fn hostile_integer_options_at_type_max() {
    bounded_case("integer-limits", || {
        // R28 finding: TypesetOptions::validate checked floats only. Every integer option of the
        // typeset options (all live in HyphenationParams; JustifyParams is floats only) at its
        // type's maximum: usize limits refuse InvalidInput before any composition, in both composers.
        let d = hy(6.0);
        for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default())] {
            let case = Case::new("aaaa hyphenation", one_frame(60.0, 24.0), composer);
            for (name, p) in [
                ("min_word_len", HyphenationParams { min_word_len: usize::MAX, ..d }),
                ("min_before", HyphenationParams { min_before: usize::MAX, ..d }),
                ("min_after", HyphenationParams { min_after: usize::MAX, ..d }),
                ("min_before=1+min_after=MAX", HyphenationParams { min_before: 1, min_after: usize::MAX, ..d }),
            ] {
                let o = TypesetOptions { hyphenation: Some(p), ..Default::default() };
                assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput, "{name} {composer:?}");
            }
        }
        // consecutive_limit (u8) at u8::MAX is accepted and behaves as unlimited (limit 0) on the
        // three-hyphen ladder.
        let h = Hyphenator::from_pattern_text("[exceptions]\nab-ab-ab-ab\n").unwrap();
        for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default())] {
            let case = Case::new("abababab", vec![frame(0, 0.0, 0.0, 18.0, 60.0, vec![])], composer);
            let run = |limit| {
                let o = TypesetOptions {
                    hyphenation: Some(HyphenationParams { consecutive_limit: limit, ..d }),
                    hyphenator: Some(&h),
                    ..Default::default()
                };
                case.run_t(&o).unwrap().frames[0].lines.clone()
            };
            let top = run(u8::MAX);
            assert_eq!(top, run(0), "{composer:?}");
            assert_eq!(top.iter().filter(|l| l.hyphenated).count(), 3, "{composer:?}");
        }
    });
}

#[test]
fn hostile_options_are_refused() {
    let case = Case::new("aa bb", one_frame(60.0, 24.0), Composer::FirstFit);
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let o = TypesetOptions { hyphenation: Some(hy(bad)), ..Default::default() };
        assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput);
    }
    let o = TypesetOptions {
        hyphenation: Some(HyphenationParams { hyphen_penalty: f64::NAN, ..hy(6.0) }),
        ..Default::default()
    };
    assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput);
    for (min, max) in [(f64::NAN, 1.3), (1.5, 1.3)] {
        let o = TypesetOptions {
            justify: Some(JustifyParams {
                word_spacing: Spacing { min, desired: 1.0, max },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput);
    }
    // A slot narrower than the hyphen never loops or panics.
    let tiny = Case::new(
        "abababab",
        vec![frame(0, 0.0, 0.0, 4.0, 60.0, vec![])],
        Composer::KnuthPlass(KnuthPlassParams::default()),
    );
    let o = TypesetOptions {
        hyphenation: Some(hy(100.0)),
        alignment: Alignment::Justified,
        ..Default::default()
    };
    let _ = tiny.run_t(&o);
    // A tripped token cancels.
    let token = CancellationToken::default();
    token.cancel();
    let r = reflow_typeset(
        &ReflowRequest {
            document: &case.doc,
            story: &case.story,
            resolved_text: None,
            paragraphs: &case.paragraphs,
            obstacles: &case.obstacles,
            composer: case.composer,
            space: SpaceParams::default(),
            expected_revision: 3,
            limits: Limits::default(),
        },
        &TypesetOptions::default(),
        &token,
    );
    assert_eq!(r.unwrap_err(), LayoutError::Canceled);
}

fn style(n: u32, scope: TypeStyleScope, based_on: Option<u32>) -> StudioTypeStyle {
    StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: id("STYS", n),
        scope,
        name: format!("style/{n}"),
        based_on: based_on.map(|b| id("STYS", b)),
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
fn records(styles: Vec<StudioTypeStyle>) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(id("SDOC", 1));
    r.styles = styles;
    r
}
fn resolve(set: &StudioTypeStyleRecords, n: u32, scope: TypeStyleScope) -> Result<ResolvedStyle, LayoutError> {
    resolve_style(set, &id("STYS", n), scope)
}

#[test]
fn style_inheritance_child_overrides_parent_fields() {
    let mut base = style(1, TypeStyleScope::Paragraph, None);
    base.font_family = Some("Serif".into());
    base.font_size = Some(len(10.0));
    base.line_height = Some(StudioLineHeight::Percent { value: 150.0 });
    base.align = Some(StudioTextAlign::Left);
    let mut mid = style(2, TypeStyleScope::Paragraph, Some(1));
    mid.font_size = Some(Length { value: 1.0, unit: GeometryUnit::In });
    let mut leaf = style(3, TypeStyleScope::Paragraph, Some(2));
    leaf.align = Some(StudioTextAlign::Justified);
    let set = records(vec![base, mid, leaf]);
    let r = resolve(&set, 3, TypeStyleScope::Paragraph).unwrap();
    assert_eq!(r.font_family.as_deref(), Some("Serif"));
    assert_eq!(r.font_size_pt, Some(72.0)); // mid overrides base, leaf inherits mid
    assert_eq!(r.line_height, Some(StudioLineHeight::Percent { value: 150.0 }));
    assert_eq!(r.align, Some(StudioTextAlign::Justified));
    assert_eq!(resolve(&set, 1, TypeStyleScope::Paragraph).unwrap().font_size_pt, Some(10.0));
}

#[test]
fn style_inheritance_hostile_sets_are_refused() {
    use TypeStyleScope::{Character, Paragraph};
    let bad = Err(LayoutError::InvalidInput);
    let cyc = records(vec![style(1, Paragraph, Some(2)), style(2, Paragraph, Some(1))]);
    assert_eq!(resolve(&cyc, 1, Paragraph), bad);
    assert_eq!(resolve(&records(vec![style(1, Paragraph, Some(1))]), 1, Paragraph), bad);
    assert_eq!(resolve(&records(vec![style(1, Paragraph, Some(9))]), 1, Paragraph), bad);
    let scope = records(vec![style(1, Character, None), style(2, Paragraph, Some(1))]);
    assert_eq!(resolve(&scope, 2, Paragraph), bad);
    assert_eq!(resolve(&scope, 1, Paragraph), bad);
    let mut nan = style(1, Paragraph, None);
    nan.font_size = Some(len(f64::NAN));
    assert_eq!(resolve(&records(vec![nan]), 1, Paragraph), Err(LayoutError::InvalidGeometry));
    // A 5000-deep chain terminates and a self-loop elsewhere in the set does not leak in.
    let mut deep: Vec<StudioTypeStyle> =
        (1..=5000).map(|n| style(n, Paragraph, (n < 5000).then_some(n + 1))).collect();
    deep.push(style(9999, Paragraph, Some(9999)));
    let set = records(deep);
    assert!(resolve(&set, 1, Paragraph).is_ok());
    assert!(resolve(&set, 9999, Paragraph).is_err());
}

#[test]
fn styles_drive_spacing_line_height_and_alignment() {
    bounded_case("styles", || {
        // Paragraph style: 10 pt, letter spacing +2 pt (glyphs 8 pt), line height 150% = 15 pt,
        // right-aligned. Slot 60 pt: "aaaa " (32 + hanging) then "bbbb" (32).
        let mut p = style(1, TypeStyleScope::Paragraph, None);
        p.font_size = Some(len(10.0));
        p.letter_spacing = Some(StudioLetterSpacing::Length { length: len(2.0) });
        p.line_height = Some(StudioLineHeight::Percent { value: 150.0 });
        let mut q = style(2, TypeStyleScope::Paragraph, Some(1));
        q.align = Some(StudioTextAlign::Right);
        let mut c = style(3, TypeStyleScope::Character, None);
        c.letter_spacing = Some(StudioLetterSpacing::Percent { value: 0.0 });
        let set = records(vec![p, q, c]);
        let mut case = Case::new("aaaa bbbb", one_frame(60.0, 60.0), Composer::FirstFit);
        case.story.paragraphs[0].style_ref = Some(id("STYS", 2));
        let src = StyleSource { records: &set, default_font_size_pt: 12.0 };
        let o = TypesetOptions { styles: Some(src), ..Default::default() };
        let lines = case.run_t(&o).unwrap().frames[0].lines.clone();
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].width_pt, lines[1].width_pt), (32.0, 32.0));
        assert_eq!((lines[0].baseline_y_pt, lines[1].baseline_y_pt), (10.0, 25.0));
        assert_eq!((lines[0].x_start_pt, lines[1].x_start_pt), (28.0, 28.0));
        // A character range over "bbbb" with letter spacing 0% cancels the paragraph's +2 pt.
        case.story.character_ranges = vec![
            StoryRange { start: 0, end: 5, style_ref: None, applied_language: None },
            StoryRange { start: 5, end: 9, style_ref: Some(id("STYS", 3)), applied_language: None },
        ];
        let lines = case.run_t(&o).unwrap().frames[0].lines.clone();
        assert_eq!((lines[0].width_pt, lines[1].width_pt), (32.0, 24.0));
        assert_eq!(lines[1].x_start_pt, 36.0);
        // A style the set lacks is refused, not ignored.
        case.story.paragraphs[0].style_ref = Some(id("STYS", 77));
        assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput);
        // Zero default size is refused.
        let zero = StyleSource { records: &set, default_font_size_pt: 0.0 };
        let bad = TypesetOptions { styles: Some(zero), ..Default::default() };
        assert_eq!(case.run_t(&bad).unwrap_err(), LayoutError::InvalidGeometry);
    });
}

fn pitem(id: &str, x: f64) -> ParentItem {
    ParentItem {
        id: id.into(),
        kind: ParentItemKind::Frame,
        rect: Rect { x0_pt: x, y0_pt: 0.0, x1_pt: x + 10.0, y1_pt: 10.0 },
    }
}
fn ppage(id: &str, based_on: Option<&str>, items: Vec<ParentItem>) -> ParentPage {
    ParentPage { id: id.into(), name: id.into(), based_on: based_on.map(Into::into), items }
}
fn bind(page: &str, parent: Option<&str>) -> PageBinding {
    PageBinding {
        page_id: page.into(),
        parent_id: parent.map(Into::into),
        overrides: vec![],
        local_items: vec![],
    }
}
fn set_of(parents: Vec<ParentPage>, bindings: Vec<PageBinding>) -> ParentSet {
    ParentSet { parents, bindings }
}

#[test]
fn parent_pages_inherit_override_and_stack() {
    let mut set = set_of(
        vec![
            ppage("A", None, vec![pitem("logo", 0.0), pitem("rule", 20.0)]),
            ppage("B", Some("A"), vec![pitem("rule", 40.0), pitem("folio", 60.0)]),
        ],
        vec![bind("page-1", Some("B")), bind("page-2", Some("A")), bind("page-3", None)],
    );
    let r = set.resolve("page-1").unwrap();
    let ids: Vec<_> = r.iter().map(|i| (i.item.id.as_str(), i.item.rect.x0_pt)).collect();
    // B's "rule" replaces A's at A's z position; B's own "folio" stacks on top.
    assert_eq!(ids, vec![("logo", 0.0), ("rule", 40.0), ("folio", 60.0)]);
    assert_eq!(r[1].source, ItemSource::Parent { parent_id: "B".into() });
    assert_eq!(r[0].source, ItemSource::Parent { parent_id: "A".into() });
    assert_eq!(set.resolve("page-2").unwrap().len(), 2);
    assert!(set.resolve("page-3").unwrap().is_empty());
    assert!(set.resolve("unbound").unwrap().is_empty());
    // Overrides and local items on page-1.
    set.bindings[0].overrides = vec![
        ItemOverride::Replace { parent_item_id: "logo".into(), replacement: pitem("logo-moved", 5.0) },
        ItemOverride::Suppress { parent_item_id: "folio".into() },
    ];
    set.bindings[0].local_items = vec![pitem("note", 1.0)];
    let r = set.resolve("page-1").unwrap();
    let got: Vec<_> = r.iter().map(|i| (i.item.id.as_str(), i.source.clone())).collect();
    assert_eq!(
        got,
        vec![
            ("logo-moved", ItemSource::Override { parent_id: "A".into() }),
            ("rule", ItemSource::Parent { parent_id: "B".into() }),
            ("note", ItemSource::Local),
        ]
    );
    // The parent itself is untouched by a page override.
    assert_eq!(set.resolve("page-2").unwrap()[0].item.id, "logo");
}

#[test]
fn parent_pages_hostile_sets_are_refused() {
    let bad = Err(LayoutError::InvalidInput);
    let a = |items: Vec<ParentItem>| ppage("A", None, items);
    let cyc = set_of(vec![ppage("A", Some("B"), vec![]), ppage("B", Some("A"), vec![])], vec![]);
    assert_eq!(cyc.validate(), bad);
    assert_eq!(set_of(vec![ppage("A", Some("Z"), vec![])], vec![]).validate(), bad);
    assert_eq!(set_of(vec![a(vec![]), a(vec![])], vec![]).validate(), bad);
    assert_eq!(set_of(vec![a(vec![pitem("x", 0.0), pitem("x", 1.0)])], vec![]).validate(), bad);
    let mut nan = pitem("x", 0.0);
    nan.rect.x1_pt = f64::NAN;
    assert_eq!(set_of(vec![a(vec![nan])], vec![]).validate(), Err(LayoutError::InvalidGeometry));
    let mut inverted = pitem("x", 0.0);
    inverted.rect.x1_pt = -5.0;
    assert_eq!(set_of(vec![a(vec![inverted])], vec![]).validate(), Err(LayoutError::InvalidGeometry));
    assert_eq!(set_of(vec![ppage("", None, vec![])], vec![]).validate(), bad);
    assert_eq!(set_of(vec![a(vec![])], vec![bind("p", Some("A")), bind("p", None)]).validate(), bad);
    assert_eq!(set_of(vec![], vec![bind("p", Some("A"))]).validate(), bad);
    let mut ghost = bind("p", Some("A"));
    ghost.overrides = vec![ItemOverride::Suppress { parent_item_id: "ghost".into() }];
    assert_eq!(set_of(vec![a(vec![pitem("x", 0.0)])], vec![ghost]).validate(), bad);
    let mut twice = bind("p", Some("A"));
    twice.overrides = vec![
        ItemOverride::Suppress { parent_item_id: "x".into() },
        ItemOverride::Replace { parent_item_id: "x".into(), replacement: pitem("y", 0.0) },
    ];
    assert_eq!(set_of(vec![a(vec![pitem("x", 0.0)])], vec![twice]).validate(), bad);
    let mut orphan = bind("p", None);
    orphan.overrides = vec![ItemOverride::Suppress { parent_item_id: "x".into() }];
    assert_eq!(set_of(vec![], vec![orphan]).validate(), bad);
    // A 500-deep chain resolves; a hostile set is refused as a unit; too many parents are refused.
    let long = set_of(
        (0..500)
            .map(|n| {
                let base = (n > 0).then(|| (n - 1).to_string());
                ppage(&n.to_string(), base.as_deref(), vec![pitem(&format!("i{n}"), 0.0)])
            })
            .collect(),
        vec![bind("p", Some("499"))],
    );
    assert_eq!(long.resolve("p").unwrap().len(), 500);
    assert_eq!(cyc.resolve("p").unwrap_err(), LayoutError::InvalidInput);
    let many = set_of((0..1025).map(|n| ppage(&n.to_string(), None, vec![])).collect(), vec![]);
    assert_eq!(many.validate(), Err(LayoutError::BudgetExceeded));
}

// ---------------------------------------------------------------------------------------------
// Adobe-parity composers (lane SMB-ID-COMPOSER-PARITY). The InDesign oracle itself is compared in
// hsk-studio-interop-layout/tests/d5_id_oracle.rs; these pin the rules on the 6 pt toy font.

fn adobe_opts(alignment: Alignment, hyphenation: Option<HyphenationParams>) -> TypesetOptions<'static> {
    TypesetOptions { justify: Some(JustifyParams::default()), hyphenation, alignment, ..Default::default() }
}

#[test]
fn adobe_single_line_compresses_within_minimum_and_honours_the_hyphenation_zone() {
    bounded_case("adobe_single_line", || {
        // 82 pt: "aaaa bbbb cccc" is 84 pt with 12 pt of spaces; 0.8333 >= minimum 0.8, so the
        // Single-line Composer takes the tight break where first-fit stops after "bbbb".
        let mut case = Case::new("aaaa bbbb cccc dddd", one_frame(82.0, 48.0), Composer::AdobeSingleLine(AdobeParams::default()));
        let p = case.run_t(&adobe_opts(Alignment::Justified, None)).unwrap();
        let l = &p.frames[0].lines;
        assert_eq!(l[0].glyph_range, 0..15, "tight break taken");
        let j = l[0].justification.unwrap();
        assert!((j.word_scale - (1.0 - 2.0 / 12.0)).abs() < 1e-12 && j.residual_pt == 0.0, "{j:?}");
        case.composer = Composer::FirstFit;
        assert_eq!(case.run_t(&adobe_opts(Alignment::Justified, None)).unwrap().frames[0].lines[0].glyph_range, 0..10);
        // Ragged + hyphenation: breaking before "typographic" leaves 48 pt. Zone 36 < 48: the word
        // is hyphenated; zone 50 >= 48: it is not.
        let zone = |z: f64| {
            let case = Case::new("aa typographic", one_frame(60.0, 48.0), Composer::AdobeSingleLine(AdobeParams { hyphenation_zone_pt: z }));
            case.run_t(&adobe_opts(Alignment::Left, Some(hy(6.0)))).unwrap().frames[0].lines[0].clone()
        };
        assert!(zone(36.0).hyphenated, "zone 36 hyphenates");
        let whole = zone(50.0);
        assert!(!whole.hyphenated && whole.source.end == 3, "{whole:?}");
        // Hostile: a word wider than the measure is set overfull, never looped on.
        let wide = Case::new("aaaaaaaaaaaaaaaaaaaa b", one_frame(60.0, 48.0), Composer::AdobeSingleLine(AdobeParams::default()));
        let p = wide.run_t(&adobe_opts(Alignment::Justified, None)).unwrap();
        assert!(p.frames[0].lines[0].overfull);
    });
}

#[test]
fn adobe_paragraph_fills_every_justified_line_and_is_deterministic() {
    bounded_case("adobe_paragraph", || {
        let text = "aa bbb c dddd ee f ggg hh iiii j kk lll m nn ooo pppp q rr";
        let case = Case::new(text, one_frame(70.0, 400.0), Composer::AdobeParagraph(AdobeParams::default()));
        let o = adobe_opts(Alignment::Justified, None);
        let p = case.run_t(&o).unwrap();
        let lines = &p.frames[0].lines;
        assert!(lines.len() >= 3);
        for l in &lines[..lines.len() - 1] {
            let j = l.justification.expect("justified");
            let spaces = 6.0 * (case.text()[l.source.start as usize..l.source.end as usize].trim_end().matches(' ').count() as f64);
            // InDesign fills the measure past the word-spacing limits (no residual).
            assert_eq!(j.residual_pt, 0.0, "{l:?}");
            assert!((l.width_pt + spaces * (j.word_scale - 1.0) - l.available_pt).abs() < 1e-9, "{l:?}");
        }
        assert_eq!(case.run_t(&o).unwrap(), p, "deterministic");
        // Ragged paragraph composition never sets a line past the measure.
        let r = case.run_t(&adobe_opts(Alignment::Left, None)).unwrap();
        assert!(r.frames[0].lines.iter().all(|l| l.width_pt <= l.available_pt + 1e-9 && l.justification.is_none()));
        // Hostile: an unbreakable run wider than the frame is one overfull line.
        let wide = Case::new("aaaaaaaaaaaaaaaaaaaa b", one_frame(60.0, 48.0), Composer::AdobeParagraph(AdobeParams::default()));
        assert!(wide.run_t(&o).unwrap().frames[0].lines[0].overfull);
    });
}

#[test]
fn justify_all_sets_the_last_line_and_splits_past_the_limits_by_range() {
    bounded_case("justify_all", || {
        // "aa bb" (30 pt, one 6 pt space, 4 glyph gaps) in 60 pt, justify all lines. Ranges: words
        // 0.33 * 6 = 1.98 pt, letters 0.05 * 6 * 4 = 1.2 pt; the 30 pt are shared 1.98 : 1.2.
        let case = Case::new("aa bb", one_frame(60.0, 24.0), Composer::AdobeSingleLine(AdobeParams::default()));
        let mut o = adobe_opts(Alignment::JustifiedAll, None);
        o.justify = Some(JustifyParams {
            letter_spacing: Spacing { min: -0.05, desired: 0.0, max: 0.05 },
            reference_space_pt: 6.0,
            ..JustifyParams::default()
        });
        let l = case.run_t(&o).unwrap().frames[0].lines[0].clone();
        let j = l.justification.expect("last line justified");
        let k = 30.0 / 3.18;
        assert!((j.word_scale - (1.0 + k * 0.33)).abs() < 1e-9 && (j.letter_gap_pt - k * 0.3).abs() < 1e-9, "{j:?}");
        assert!((6.0 * (j.word_scale - 1.0) + 4.0 * j.letter_gap_pt - 30.0).abs() < 1e-9);
        // Justified (not all): the single last line stays as set.
        assert!(case.run_t(&adobe_opts(Alignment::Justified, None)).unwrap().frames[0].lines[0].justification.is_none());
    });
}

// ---------------------------------------------------------------------------------------------
// Type composition v2 line-end data (lane SMB-TYPE-FF-LIGATURE): a hyphenation break inside a
// presentation-form ligature sets each side with its components; a kerned glyph before a hyphen
// takes its hmtx advance. Toy font as above (6 pt glyphs, 6 pt hyphen).

/// "xx affix": glyphs x x ' ' a [ff] i x when `ligature`, else x x ' ' a f f i x with the first f
/// kerned to 5 pt (hmtx 6 pt recorded as its line-end advance).
fn affix(ligature: bool) -> ShapedParagraph {
    let text = "xx affix";
    let g = |s: u64, e: u64, w: f64| LayoutGlyph { cluster: span(s, e), advance_pt: w };
    let mut glyphs = vec![g(0, 1, 6.0), g(1, 2, 6.0), g(2, 3, 6.0), g(3, 4, 6.0)];
    if ligature {
        glyphs.push(g(4, 6, 10.0));
    } else {
        glyphs.extend([g(4, 5, 5.0), g(5, 6, 6.0)]);
    }
    glyphs.extend([g(6, 7, 6.0), g(7, 8, 6.0)]);
    let mut p = ShapedParagraph::latin_ltr(span(0, text.len() as u64), glyphs, 12.0, 10.0);
    if ligature {
        p.ligature_parts = vec![
            LigaturePart { glyph: 4, source: span(4, 5), advance_pt: 6.0 },
            LigaturePart { glyph: 4, source: span(5, 6), advance_pt: 6.0 },
        ];
    } else {
        p.line_end_advances = vec![LineEndAdvance { glyph: 4, advance_pt: 6.0 }];
    }
    p
}

#[test]
fn hyphen_inside_a_ligature_sets_its_components_and_drops_kerning_at_the_line_end() {
    bounded_case("ligature_split", || {
        let h = Hyphenator::from_pattern_text("[exceptions]\naf-fix\n").unwrap();
        let opts = |h: &'static Hyphenator| TypesetOptions { hyphenation: Some(hy(6.0)), hyphenator: Some(h), ..Default::default() };
        let h: &'static Hyphenator = Box::leak(Box::new(h));
        // Both take the longest fitting line (the Single-line Composer with a zero hyphenation zone).
        for composer in [Composer::FirstFit, Composer::AdobeSingleLine(AdobeParams { hyphenation_zone_pt: 0.0 })] {
            // Ligature: "xx affix" is 46 pt > 40 pt; "xx af-" = 18 + 6 + 6 (left f) + 6 = 36 pt, the
            // next line "fix" = 6 (right f) + 6 + 6 = 18 pt; the ligature glyph starts line 2.
            let mut case = Case::new("xx affix", one_frame(40.0, 48.0), composer);
            case.paragraphs = vec![affix(true)];
            let l = case.run_t(&opts(h)).unwrap().frames[0].lines.clone();
            assert_eq!(l.len(), 2, "{composer:?} {l:?}");
            assert!(l[0].hyphenated, "{composer:?}");
            assert_eq!((l[0].glyph_range.clone(), l[0].source, l[0].width_pt), (0..4, span(0, 5), 36.0), "{composer:?}");
            assert_eq!(l[0].end_part, Some(LinePart { glyph: 4, source: span(4, 5), advance_pt: 6.0 }));
            assert_eq!(l[0].start_part, None);
            assert_eq!((l[1].glyph_range.clone(), l[1].source, l[1].width_pt), (4..7, span(5, 8), 18.0), "{composer:?}");
            assert_eq!(l[1].start_part, Some(LinePart { glyph: 4, source: span(5, 6), advance_pt: 6.0 }));
            // Unbroken (wide slot): the ligature keeps its own 10 pt advance.
            let mut wide = Case::new("xx affix", one_frame(60.0, 48.0), composer);
            wide.paragraphs = vec![affix(true)];
            let w = wide.run_t(&opts(h)).unwrap().frames[0].lines.clone();
            assert_eq!((w.len(), w[0].width_pt, w[0].end_part, w[0].start_part), (1, 46.0, None, None), "{composer:?}");
            // Kerned f before the hyphen: "xx af-" = 24 + 6 (hmtx, not the kerned 5) + 6 = 36 pt;
            // unbroken the kern stays (47 pt).
            let mut kern = Case::new("xx affix", one_frame(40.0, 48.0), composer);
            kern.paragraphs = vec![affix(false)];
            let k = kern.run_t(&opts(h)).unwrap().frames[0].lines.clone();
            assert_eq!((k[0].glyph_range.clone(), k[0].width_pt), (0..5, 36.0), "{composer:?}");
            assert_eq!(k[0].end_part, Some(LinePart { glyph: 4, source: span(4, 5), advance_pt: 6.0 }));
            assert_eq!((k[1].source, k[1].width_pt, k[1].start_part), (span(5, 8), 18.0, None));
            let mut kern_wide = Case::new("xx affix", one_frame(60.0, 48.0), composer);
            kern_wide.paragraphs = vec![affix(false)];
            let kw = kern_wide.run_t(&opts(h)).unwrap().frames[0].lines.clone();
            assert_eq!((kw.len(), kw[0].width_pt, kw[0].end_part), (1, 47.0, None), "{composer:?}");
        }
        // Hostile: component data that does not partition the ligature at the hyphen (one part
        // spanning both bytes) offers no break inside it: the word is never cut there.
        let mut bad = affix(true);
        bad.ligature_parts = vec![LigaturePart { glyph: 4, source: span(4, 6), advance_pt: 12.0 }];
        let mut case = Case::new("xx affix", one_frame(40.0, 48.0), Composer::FirstFit);
        case.paragraphs = vec![bad];
        let l = case.run_t(&opts(h)).unwrap().frames[0].lines.clone();
        assert!(l.iter().all(|l| !l.hyphenated && l.end_part.is_none() && l.start_part.is_none()), "{l:?}");
    });
}

#[test]
fn glyph_scaling_shares_the_rest_with_letters_by_range_and_is_validated() {
    // Natural 100 pt (10 pt of spaces, 20 gaps), slot 104, word spacing fixed. Letter range
    // 20 * 0.05 * 4 = 4 pt, glyph range 100 * 0.1 = 10 pt: the 4 pt are shared 4 : 10 (k = 2/7).
    let jp = JustifyParams {
        word_spacing: Spacing { min: 1.0, desired: 1.0, max: 1.0 },
        letter_spacing: Spacing { min: -0.05, desired: 0.0, max: 0.05 },
        reference_space_pt: 4.0,
        glyph_scaling: Spacing { min: 0.9, desired: 1.0, max: 1.1 },
    };
    let j = justify_metrics(100.0, 104.0, 10.0, 20, &jp);
    let k = 4.0 / 14.0;
    assert!((j.glyph_scale - (1.0 + 0.1 * k)).abs() < 1e-12 && (j.letter_gap_pt - 0.2 * k).abs() < 1e-12, "{j:?}");
    // Spaces scale with the glyphs (word spacing additive): word_scale = 1 + (glyph - 1).
    assert!((j.word_scale - j.glyph_scale).abs() < 1e-12 && j.residual_pt == 0.0);
    assert!((100.0 * j.glyph_scale + 20.0 * j.letter_gap_pt - 104.0).abs() < 1e-9);
    // Shrink past both limits: glyphs and letters at their minimum, the rest is residual.
    let j = justify_metrics(100.0, 80.0, 10.0, 20, &jp);
    assert!(j.glyph_scale == 0.9 && (j.letter_gap_pt + 0.2).abs() < 1e-12, "{j:?}");
    assert!((j.residual_pt + 6.0).abs() < 1e-9, "{j:?}");
    // Off (default 1 / 1 / 1): the v1 arithmetic, bit for bit.
    let base = JustifyParams::default();
    let j = justify_metrics(54.0, 52.0, 6.0, 8, &base);
    assert_eq!((j.word_scale, j.glyph_scale, j.letter_gap_pt), (0.8, 1.0, 0.0));
    // Hostile ranges are refused (spec 50 .. 200 %, min <= desired <= max).
    let case = Case::new("aa bb", one_frame(60.0, 24.0), Composer::FirstFit);
    for (min, desired, max) in [(0.4, 1.0, 1.1), (0.9, 1.0, 2.5), (1.1, 1.0, 1.2), (f64::NAN, 1.0, 1.0)] {
        let o = TypesetOptions {
            justify: Some(JustifyParams { glyph_scaling: Spacing { min, desired, max }, ..Default::default() }),
            ..Default::default()
        };
        assert_eq!(case.run_t(&o).unwrap_err(), LayoutError::InvalidInput, "{min} {desired} {max}");
    }
}
