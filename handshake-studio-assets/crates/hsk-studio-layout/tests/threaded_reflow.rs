//! Hand-computed oracles. Toy font: every glyph (letters and spaces) advances 6 pt, line height 12 pt,
//! ascent 10 pt (descent 2 pt), first baseline 10 pt below the frame top, no insets. A 60 pt line
//! therefore holds 10 glyphs. Pages, frames, threads and the story are Folio types.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{
    AutoSizing, Bounds, Budget as FolioBudget, ContentDigest, DocumentSchema, FirstBaseline,
    GeometryUnit, GraphSchema, LayerSchema, Length, LayoutEdges, NodeKind, NodeRef, PageSide,
    PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection,
    StoryParagraph, StoryParagraphDirection, StoryRange, StoryTextKind, StudioDocument,
    StudioLayer, StudioLayerGraph, StudioPage, StudioPageSpread, StudioTextFrame, StudioTextStory,
    TextStorySchema, Unresolved, VerticalJustification, validate_document,
};
use hsk_studio_layout::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};

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
    fn ok(&self) -> LayoutProposal {
        self.run(&CancellationToken::default()).unwrap()
    }
    fn err(&self) -> LayoutError {
        self.run(&CancellationToken::default()).unwrap_err()
    }
}
fn span(start: u64, end: u64) -> SourceSpan {
    SourceSpan { start, end }
}
/// (glyph range, source span, width, hang) of a line.
fn shape(l: &Line) -> (std::ops::Range<usize>, SourceSpan, f64, f64) {
    (l.glyph_range.clone(), l.source, l.width_pt, l.hang_pt)
}

#[test]
fn two_frame_flow_overset() {
    bounded_case("two_frame_flow_overset", || {
        // One 60x24 frame holds two lines: "aaaa bbbb " (hanging space) and "cccc dddd".
        let text = "aaaa bbbb cccc dddd";
        let one = Case::new(
            text,
            vec![frame(0, 0.0, 0.0, 60.0, 24.0, vec![])],
            Composer::FirstFit,
        )
        .ok();
        assert_eq!((one.base_revision, one.overset), (3, None));
        let lines = &one.frames[0].lines;
        assert_eq!(lines.len(), 2);
        assert_eq!(shape(&lines[0]), (0..10, span(0, 10), 54.0, 6.0));
        assert_eq!(shape(&lines[1]), (10..19, span(10, 19), 54.0, 0.0));
        assert_eq!((lines[0].baseline_y_pt, lines[1].baseline_y_pt), (10.0, 22.0));
        assert_eq!((lines[0].x_start_pt, lines[0].available_pt), (0.0, 60.0));
        assert!(lines.iter().all(|l| !l.overfull && l.ratio.is_none()));

        // Two 60x12 frames (one line each), the second on the right-hand page: line 2 flows to it.
        let two_frames = vec![
            frame(0, 0.0, 0.0, 60.0, 12.0, vec![]),
            frame(1, 10.0, 5.0, 60.0, 12.0, vec![]),
        ];
        let flow = Case::new(text, two_frames.clone(), Composer::FirstFit).ok();
        assert_eq!(flow.overset, None);
        assert_eq!(flow.frames[0].lines.len(), 1);
        assert_eq!(shape(&flow.frames[0].lines[0]), (0..10, span(0, 10), 54.0, 6.0));
        assert_eq!(shape(&flow.frames[1].lines[0]), (10..19, span(10, 19), 54.0, 0.0));
        assert_eq!(flow.frames[1].lines[0].baseline_y_pt, 10.0);
        // Page 2 starts at x = 100 in spread coordinates.
        assert_eq!(
            (flow.frames[1].origin_x_pt, flow.frames[1].origin_y_pt),
            (110.0, 5.0)
        );
        // Frames are reported by their Folio layer id, in flow order, with their spread and page.
        assert_eq!(flow.frames[0].frame_id, id("SLYR", 1));
        assert_eq!(flow.frames[1].frame_id, id("SLYR", 2));
        assert_eq!(
            (flow.frames[1].spread, flow.frames[1].page, flow.frames[1].frame_index),
            (0, 1, 1)
        );

        // A third line does not fit: overset is exactly the unplaced tail, never truncated.
        let long = "aaaa bbbb cccc dddd eeee";
        let over = Case::new(long, two_frames, Composer::FirstFit).ok();
        assert_eq!(over.overset, Some(span(20, 24)));
        assert_eq!(shape(&over.frames[0].lines[0]), (0..10, span(0, 10), 54.0, 6.0));
        assert_eq!(shape(&over.frames[1].lines[0]), (10..20, span(10, 20), 54.0, 6.0));
        let placed: usize = over.frames.iter().map(|f| f.lines.len()).sum();
        assert_eq!(placed, 2);
    });
}

#[test]
fn obstacle_wrap_and_overfull_word() {
    bounded_case("obstacle_wrap_and_overfull_word", || {
        let text = "aaaa bbbb cccc";
        let block = |x0, x1| Rect {
            x0_pt: x0,
            y0_pt: 0.0,
            x1_pt: x1,
            y1_pt: 12.0,
        };
        // Right obstacle (x 30..60) covers only line 1's box [0, 12]: its slot is 30 pt = 5 glyphs.
        let right = Case::new(
            text,
            vec![frame(0, 0.0, 0.0, 60.0, 36.0, vec![block(30.0, 60.0)])],
            Composer::FirstFit,
        )
        .ok();
        let l = &right.frames[0].lines;
        assert_eq!(shape(&l[0]), (0..5, span(0, 5), 24.0, 6.0));
        assert_eq!((l[0].x_start_pt, l[0].available_pt), (0.0, 30.0));
        assert_eq!(shape(&l[1]), (5..14, span(5, 14), 54.0, 0.0));
        assert_eq!((l[1].x_start_pt, l[1].available_pt), (0.0, 60.0));
        assert_eq!(right.overset, None);
        // Left obstacle (x 0..30) moves line 1's start to x = 30.
        let left = Case::new(
            text,
            vec![frame(0, 0.0, 0.0, 60.0, 36.0, vec![block(0.0, 30.0)])],
            Composer::FirstFit,
        )
        .ok();
        assert_eq!(
            (left.frames[0].lines[0].x_start_pt, left.frames[0].lines[0].available_pt),
            (30.0, 30.0)
        );
        // A word wider than the line (12 glyphs = 72 pt > 60 pt) sits alone and is flagged.
        let wide = Case::new(
            "aaaaaaaaaaaa bb",
            vec![frame(0, 0.0, 0.0, 60.0, 36.0, vec![])],
            Composer::FirstFit,
        )
        .ok();
        let l = &wide.frames[0].lines;
        assert_eq!(shape(&l[0]), (0..13, span(0, 13), 72.0, 6.0));
        assert!(l[0].overfull && !l[1].overfull);
        assert_eq!(shape(&l[1]), (13..15, span(13, 15), 12.0, 0.0));
    });
}

#[test]
fn knuth_plass_hand_computed_and_deterministic() {
    bounded_case("knuth_plass_hand_computed_and_deterministic", || {
        // "aa bb cc dd ee fffff" in 10-glyph (60 pt) lines; a space is 6 pt with stretch 3, shrink 2.
        let text = "aa bb cc dd ee fffff";
        let frames = vec![frame(0, 0.0, 0.0, 60.0, 48.0, vec![])];
        // First fit: "aa bb cc" | "dd ee" | "fffff"; line 2 is 30 pt wide in a 60 pt slot (r = 10).
        let greedy = Case::new(text, frames.clone(), Composer::FirstFit).ok();
        let g = &greedy.frames[0].lines;
        assert_eq!(g.len(), 3);
        assert_eq!(shape(&g[0]), (0..9, span(0, 9), 48.0, 6.0));
        assert_eq!(shape(&g[1]), (9..15, span(9, 15), 30.0, 6.0));
        assert_eq!(shape(&g[2]), (15..20, span(15, 20), 30.0, 0.0));
        // Knuth-Plass pass 1 (tolerance 200) has exactly one feasible breaking:
        //   line 1 "aa bb cc dd": 66 pt, shrink 3 x 2 = 6 -> r = -1, badness 100, demerits (10+100)^2 = 12100;
        //   line 2 "ee fffff" is the last line (infinite stretch) -> r = 0, demerits 10^2 = 100.
        // "aa bb cc" alone would need r = (60-48)/(2 x 3) = 2 (badness 800 > 200), so it is rejected.
        let case = Case::new(text, frames, Composer::KnuthPlass(KnuthPlassParams::default()));
        let kp = case.ok();
        let k = &kp.frames[0].lines;
        assert_eq!(k.len(), 2);
        assert_eq!(shape(&k[0]), (0..12, span(0, 12), 66.0, 6.0));
        assert_eq!(shape(&k[1]), (12..20, span(12, 20), 48.0, 0.0));
        assert!((k[0].ratio.unwrap() + 1.0).abs() < 1e-9);
        assert_eq!(k[1].ratio, Some(0.0));
        // Shrunk within its limit is not overfull although 66 pt > 60 pt available.
        assert!(!k[0].overfull && k[0].width_pt > k[0].available_pt);
        // Deterministic: a second run is byte-identical.
        let again = case.ok();
        assert_eq!(kp, again);
        assert_eq!(format!("{kp:?}"), format!("{again:?}"));
        // An unbreakable overfull word is still placed by Knuth-Plass (third pass), flagged overfull.
        let wide = Case::new(
            "aaaaaaaaaaaa bb",
            vec![frame(0, 0.0, 0.0, 60.0, 36.0, vec![])],
            Composer::KnuthPlass(KnuthPlassParams::default()),
        )
        .ok();
        let w = &wide.frames[0].lines;
        assert_eq!((w.len(), w[0].overfull, w[0].glyph_range.clone()), (2, true, 0..13));
    });
}

#[test]
fn cyclic_missing_font_budget_cancel_stale_unsupported() {
    bounded_case("cyclic_missing_font_budget_cancel_stale_unsupported", || {
        let frames = vec![
            frame(0, 0.0, 0.0, 60.0, 24.0, vec![]),
            frame(1, 0.0, 0.0, 60.0, 24.0, vec![]),
        ];
        let base = Case::new("aaaa bbbb cccc dddd", frames.clone(), Composer::FirstFit);
        let fresh = || Case::new(base.text(), frames.clone(), Composer::FirstFit);
        let set_links = |c: &mut Case, i: usize, prev: Option<u32>, next: Option<u32>| {
            let f = c.frame_mut(i);
            f.previous_text_frame = prev.map(|n| id("SLYR", n));
            f.next_text_frame = next.map(|n| id("SLYR", n));
        };
        // The thread loops back on itself: with and without a first frame.
        let mut c = fresh();
        set_links(&mut c, 0, Some(2), Some(2));
        set_links(&mut c, 1, Some(1), Some(1));
        assert_eq!(c.err(), LayoutError::CyclicThread);
        let mut c = fresh();
        set_links(&mut c, 0, None, Some(2));
        set_links(&mut c, 1, Some(1), Some(1));
        assert_eq!(c.err(), LayoutError::CyclicThread);
        // A link to a frame that does not exist, and a second chain of the same story.
        let mut c = fresh();
        set_links(&mut c, 1, Some(1), Some(7));
        assert_eq!(c.err(), LayoutError::InvalidInput);
        let mut c = fresh();
        set_links(&mut c, 0, None, None);
        set_links(&mut c, 1, None, None);
        assert_eq!(c.err(), LayoutError::InvalidInput);
        let mut c = fresh();
        c.paragraphs[0].font_missing = true;
        assert_eq!(c.err(), LayoutError::MissingFont);
        let mut c = fresh();
        c.limits.max_lines = 1;
        assert_eq!(c.err(), LayoutError::BudgetExceeded);
        let mut c = fresh();
        c.limits.max_glyphs = 5;
        assert_eq!(c.err(), LayoutError::BudgetExceeded);
        let mut c = fresh();
        c.expected = 3;
        c.doc.revision = 4;
        assert_eq!(
            c.err(),
            LayoutError::StaleRevision {
                expected: 3,
                current: 4
            }
        );
        let mut c = fresh();
        c.frame_mut(0).bounds.width.value = f64::NAN;
        assert_eq!(c.err(), LayoutError::InvalidGeometry);
        let mut c = fresh();
        c.frame_mut(1).minimum_first_baseline_offset.value = -1.0;
        assert_eq!(c.err(), LayoutError::InvalidGeometry);
        // Right-to-left and non-Latin paragraphs are refused, never mis-laid.
        let mut c = fresh();
        c.paragraphs[0].right_to_left = true;
        assert_eq!(c.err(), LayoutError::UnsupportedComposition);
        let mut c = Case::new(
            base.text(),
            frames.clone(),
            Composer::KnuthPlass(KnuthPlassParams::default()),
        );
        c.paragraphs[0].latin_only = false;
        assert_eq!(c.err(), LayoutError::UnsupportedComposition);
        // Pre-cancelled token: no proposal, and the story input is unchanged by every refusal.
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(base.run(&token).unwrap_err(), LayoutError::Canceled);
        let untouched = fresh();
        assert_eq!(base.paragraphs, untouched.paragraphs);
        assert_eq!(base.story, untouched.story);
        assert_eq!(base.doc, untouched.doc);
        assert!(base.ok().overset.is_none());
    });
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn observe() -> Observe {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(1, 3, resource, actor, Budget::new(0, 0).unwrap())
}

#[test]
fn break_opportunities_forced_breaks_and_report() {
    bounded_case("break_opportunities_forced_breaks_and_report", || {
        // "a-b c\nd": bytes a0 -1 b2 ' '3 c4 \n5 d6 -> after the hyphen, after the space, after the
        // newline (mandatory) and the mandatory end of text.
        let b = |byte, kind| Break { byte, kind };
        assert_eq!(
            break_opportunities("a-b c\nd"),
            vec![
                b(2, BreakKind::Allowed),
                b(4, BreakKind::Allowed),
                b(6, BreakKind::Mandatory),
                b(7, BreakKind::Mandatory)
            ]
        );
        // A newline inside a paragraph forces a line end: "aa\nbb" -> "aa\n" | "bb" even though both fit.
        let case = Case::new(
            "aa\nbb",
            vec![frame(0, 0.0, 0.0, 60.0, 36.0, vec![])],
            Composer::FirstFit,
        );
        let proposal = case.ok();
        let l = &proposal.frames[0].lines;
        assert_eq!(l.len(), 2);
        assert_eq!(shape(&l[0]), (0..3, span(0, 3), 12.0, 6.0));
        assert_eq!(shape(&l[1]), (3..5, span(3, 5), 12.0, 0.0));
        assert_eq!(proposal.overset, None);
        // Observe: success and overset are Success; refusals map to their failure classes.
        let token = CancellationToken::default();
        let ok: Result<LayoutProposal, LayoutError> = Ok(proposal);
        let receipt = report(&ok, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Success);
        let missing: Result<LayoutProposal, LayoutError> = Err(LayoutError::MissingFont);
        let receipt =
            report(&missing, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Unavailable));
        assert_eq!(
            LayoutError::UnsupportedComposition.failure_code(),
            Some(FailureCode::Unsupported)
        );
        assert_eq!(LayoutError::CyclicThread.failure_code(), Some(FailureCode::Validation));
        assert_eq!(LayoutError::Canceled.failure_code(), None);
    });
}

#[test]
fn folio_inputs_select_the_story_thread_and_unsupported_properties_are_refused() {
    bounded_case(
        "folio_inputs_select_the_story_thread_and_unsupported_properties_are_refused",
        || {
            let text = "aaaa bbbb cccc dddd";
            let specs = vec![
                frame(0, 0.0, 0.0, 60.0, 12.0, vec![]),
                frame(1, 10.0, 5.0, 60.0, 12.0, vec![]),
            ];
            let case = Case::new(text, specs.clone(), Composer::FirstFit);
            // The fixture is a valid Folio document (the layer ids, pages and links are Folio's).
            validate_document(
                case.doc.clone(),
                FolioBudget::default(),
                &CancellationToken::default(),
                &Unresolved,
            )
            .expect("fixture is a valid Folio document");

            // Another story's frame comes first in the document: it is ignored for story 1 and its
            // layer index shifts the reported frame_index; reflowing story 2 uses only that frame.
            let mut doc = case.doc.clone();
            let other = frame(0, 0.0, 50.0, 60.0, 12.0, vec![]);
            doc.layers.insert(0, frame_layer(9, 2, &other, None, None));
            let request = |doc: &StudioDocument, story: &StudioTextStory, paragraphs: &[ShapedParagraph]| {
                reflow(
                    &ReflowRequest {
                        document: doc,
                        story,
                        resolved_text: None,
                        paragraphs,
                        obstacles: &[],
                        composer: Composer::FirstFit,
                        space: SpaceParams::default(),
                        expected_revision: 3,
                        limits: Limits::default(),
                    },
                    &CancellationToken::default(),
                )
            };
            let one = request(&doc, &case.story, &case.paragraphs).unwrap();
            assert_eq!(one, {
                let mut expected = case.ok();
                expected.frames[0].frame_index = 1;
                expected.frames[1].frame_index = 2;
                expected
            });
            let story2 = story_of(2, "aaaa bbbb");
            let two = request(&doc, &story2, &[para("aaaa bbbb")]).unwrap();
            assert_eq!(two.frames.len(), 1);
            assert_eq!(two.frames[0].frame_id, id("SLYR", 9));
            assert_eq!((two.frames[0].frame_index, two.frames[0].origin_y_pt), (0, 50.0));
            assert_eq!(shape(&two.frames[0].lines[0]), (0..9, span(0, 9), 54.0, 0.0));
            let unknown = request(&doc, &story_of(3, "aa"), &[para("aa")]).unwrap_err();
            assert_eq!(unknown, LayoutError::InvalidInput);

            // Four-sided insets bound the slot: left 6, right 12 leave 42 pt starting at x = 6.
            let mut c = Case::new(text, vec![frame(0, 0.0, 0.0, 60.0, 24.0, vec![])], Composer::FirstFit);
            c.frame_mut(0).inset_spacing.left = len(6.0);
            c.frame_mut(0).inset_spacing.right = len(12.0);
            let l = &c.ok().frames[0].lines[0];
            assert_eq!((l.x_start_pt, l.available_pt), (6.0, 42.0));

            // Stored content: the caller resolves the bytes, which must have exactly byte_len bytes.
            let mut stored = Case::new(text, specs.clone(), Composer::FirstFit);
            stored.story.content = StoryContent::Stored {
                artifact_manifest_id: "manifest-a".into(),
                content_digest: ContentDigest {
                    algorithm: "sha256".into(),
                    digest: "ab12".into(),
                },
                byte_len: text.len() as u64,
            };
            let with = |resolved: Option<&str>| {
                reflow(
                    &ReflowRequest {
                        document: &stored.doc,
                        story: &stored.story,
                        resolved_text: resolved,
                        paragraphs: &stored.paragraphs,
                        obstacles: &stored.obstacles,
                        composer: Composer::FirstFit,
                        space: SpaceParams::default(),
                        expected_revision: 3,
                        limits: Limits::default(),
                    },
                    &CancellationToken::default(),
                )
            };
            assert_eq!(with(Some(text)).unwrap(), case.ok());
            assert_eq!(with(None).unwrap_err(), LayoutError::InvalidInput);
            assert_eq!(with(Some("short")).unwrap_err(), LayoutError::InvalidInput);
            // A shaped paragraph that reaches past the end of the story is refused.
            let mut past_end = Case::new(text, specs.clone(), Composer::FirstFit);
            past_end.paragraphs[0].source.end = text.len() as u64 + 1;
            assert_eq!(past_end.err(), LayoutError::InvalidInput);

            // Everything Folio can say that this slice does not compose is a typed refusal.
            let unsupported = |edit: &dyn Fn(&mut Case)| {
                let mut c = Case::new(text, specs.clone(), Composer::FirstFit);
                edit(&mut c);
                c.err()
            };
            let refused = LayoutError::UnsupportedComposition;
            assert_eq!(unsupported(&|c| c.story.story_direction = StoryDirection::RightToLeft), refused);
            assert_eq!(
                unsupported(&|c| c.story.paragraphs[0].paragraph_direction =
                    Some(StoryParagraphDirection::RightToLeft)),
                refused
            );
            assert_eq!(unsupported(&|c| c.story.text_kind = StoryTextKind::FrameGrid), refused);
            // Columns compose now (SMB-LAYOUT-UI); columns without positive width are refused.
            assert_eq!(
                unsupported(&|c| {
                    c.frame_mut(1).text_column_count = 2;
                    c.frame_mut(1).text_column_gutter = len(1000.0);
                }),
                LayoutError::InvalidGeometry
            );
            // Vertical justification composes now (SMB-ID-PREPRESS slice 1). Hand-checked: one 12 pt
            // line box (baseline 10 - ascent 10 to baseline 10 + descent 2) fills the 12 pt frame 0,
            // so Center leaves it at baseline 10; in frame 1 grown to 24 pt the box has 12 pt free,
            // Center puts 6 pt above it: baseline 16.
            let mut centered = Case::new(text, specs.clone(), Composer::FirstFit);
            centered.frame_mut(0).vertical_justification = VerticalJustification::Center;
            centered.frame_mut(1).vertical_justification = VerticalJustification::Center;
            centered.frame_mut(1).bounds.height = len(24.0);
            let centered = centered.ok();
            assert_eq!(centered.overset, None);
            assert_eq!(shape(&centered.frames[0].lines[0]), (0..10, span(0, 10), 54.0, 6.0));
            assert_eq!(shape(&centered.frames[1].lines[0]), (10..19, span(10, 19), 54.0, 0.0));
            assert_eq!(
                (centered.frames[0].lines[0].baseline_y_pt, centered.frames[1].lines[0].baseline_y_pt),
                (10.0, 16.0)
            );
            assert_eq!(unsupported(&|c| c.frame_mut(0).auto_sizing_type = AutoSizing::HeightOnly), refused);
            // First baselines other than fixed compose since SMB-ID-FRAME-AUTOSIZE-1 (tests/
            // frame_autosize.rs): inset top 0 + max(ascent 10, minimum 10) = baseline 10.
            let mut ascent = Case::new(text, specs.clone(), Composer::FirstFit);
            ascent.frame_mut(0).first_baseline_offset = FirstBaseline::Ascent;
            assert_eq!(ascent.ok().frames[0].lines[0].baseline_y_pt, 10.0);
            assert_eq!(
                unsupported(&|c| c.story.text_kind = StoryTextKind::Point),
                LayoutError::InvalidInput
            );
            // Geometry that is not finite points, or not a resolvable page, is refused.
            let invalid = LayoutError::InvalidGeometry;
            assert_eq!(unsupported(&|c| c.frame_mut(0).bounds.width.unit = GeometryUnit::Mm), invalid);
            assert_eq!(unsupported(&|c| c.doc.page_spreads[0].pages[1].width.value = 0.0), invalid);
            assert_eq!(unsupported(&|c| c.frame_mut(0).inset_spacing = edges(40.0)), invalid);
            assert_eq!(
                unsupported(&|c| c.obstacles[0].rects.push(Rect {
                    x0_pt: 5.0,
                    y0_pt: 0.0,
                    x1_pt: 1.0,
                    y1_pt: 12.0
                })),
                invalid
            );
            assert_eq!(
                unsupported(&|c| c.frame_mut(1).page_id = "no-such-page".into()),
                LayoutError::InvalidInput
            );
        },
    );
}
