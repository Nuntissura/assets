//! Hand-computed oracles. Toy font: every glyph (letters and spaces) advances 6 pt, line height 12 pt,
//! ascent 10 pt (descent 2 pt), first baseline 10 pt below the frame top, no insets. A 60 pt line
//! therefore holds 10 glyphs.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
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

fn page(id: &str) -> Page {
    Page {
        id: id.into(),
        width_pt: 100.0,
        height_pt: 100.0,
        margins: Margins {
            top_pt: 5.0,
            right_pt: 5.0,
            bottom_pt: 5.0,
            left_pt: 5.0,
        },
    }
}
fn spreads() -> Vec<Spread> {
    vec![Spread {
        id: "spread-1".into(),
        pages: vec![page("page-1"), page("page-2")],
    }]
}
fn frame(id: &str, page: usize, x: f64, y: f64, w: f64, h: f64, obstacles: Vec<Rect>) -> TextFrame {
    TextFrame {
        id: id.into(),
        spread: 0,
        page,
        x_pt: x,
        y_pt: y,
        width_pt: w,
        height_pt: h,
        inset_pt: 0.0,
        first_baseline_pt: 10.0,
        obstacles,
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
    text: String,
    paragraphs: Vec<ShapedParagraph>,
    spreads: Vec<Spread>,
    frames: Vec<TextFrame>,
    thread: Vec<usize>,
    composer: Composer,
    limits: Limits,
    expected: u64,
    current: u64,
}
impl Case {
    fn new(text: &str, frames: Vec<TextFrame>, composer: Composer) -> Self {
        Self {
            text: text.into(),
            paragraphs: vec![para(text)],
            spreads: spreads(),
            thread: (0..frames.len()).collect(),
            frames,
            composer,
            limits: Limits::default(),
            expected: 3,
            current: 3,
        }
    }
    fn run(&self, cancel: &CancellationToken) -> Result<LayoutProposal, LayoutError> {
        reflow(
            &ReflowRequest {
                text: &self.text,
                paragraphs: &self.paragraphs,
                spreads: &self.spreads,
                frames: &self.frames,
                thread: &self.thread,
                composer: self.composer,
                space: SpaceParams::default(),
                expected_revision: self.expected,
                current_revision: self.current,
                limits: self.limits,
            },
            cancel,
        )
    }
    fn ok(&self) -> LayoutProposal {
        self.run(&CancellationToken::default()).unwrap()
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
            vec![frame("f1", 0, 0.0, 0.0, 60.0, 24.0, vec![])],
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
            frame("f1", 0, 0.0, 0.0, 60.0, 12.0, vec![]),
            frame("f2", 1, 10.0, 5.0, 60.0, 12.0, vec![]),
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
            vec![frame("f", 0, 0.0, 0.0, 60.0, 36.0, vec![block(30.0, 60.0)])],
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
            vec![frame("f", 0, 0.0, 0.0, 60.0, 36.0, vec![block(0.0, 30.0)])],
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
            vec![frame("f", 0, 0.0, 0.0, 60.0, 36.0, vec![])],
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
        let frames = vec![frame("f", 0, 0.0, 0.0, 60.0, 48.0, vec![])];
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
            vec![frame("f", 0, 0.0, 0.0, 60.0, 36.0, vec![])],
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
            frame("f1", 0, 0.0, 0.0, 60.0, 24.0, vec![]),
            frame("f2", 1, 0.0, 0.0, 60.0, 24.0, vec![]),
        ];
        let base = Case::new("aaaa bbbb cccc dddd", frames, Composer::FirstFit);
        let err = |c: &Case| c.run(&CancellationToken::default()).unwrap_err();
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.thread = vec![0, 1, 0];
        assert_eq!(err(&c), LayoutError::CyclicThread);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.thread = vec![0, 7];
        assert_eq!(err(&c), LayoutError::InvalidInput);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.paragraphs[0].font_missing = true;
        assert_eq!(err(&c), LayoutError::MissingFont);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.limits.max_lines = 1;
        assert_eq!(err(&c), LayoutError::BudgetExceeded);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.limits.max_glyphs = 5;
        assert_eq!(err(&c), LayoutError::BudgetExceeded);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.expected = 3;
        c.current = 4;
        assert_eq!(
            err(&c),
            LayoutError::StaleRevision {
                expected: 3,
                current: 4
            }
        );
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.frames[0].width_pt = f64::NAN;
        assert_eq!(err(&c), LayoutError::InvalidGeometry);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.frames[1].first_baseline_pt = -1.0;
        assert_eq!(err(&c), LayoutError::InvalidGeometry);
        // Right-to-left and non-Latin paragraphs are refused, never mis-laid.
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        c.paragraphs[0].right_to_left = true;
        assert_eq!(err(&c), LayoutError::UnsupportedComposition);
        let mut c = Case::new(&base.text, base.frames.clone(), Composer::KnuthPlass(KnuthPlassParams::default()));
        c.paragraphs[0].latin_only = false;
        assert_eq!(err(&c), LayoutError::UnsupportedComposition);
        // Pre-cancelled token: no proposal, and the story input is unchanged by every refusal.
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(base.run(&token).unwrap_err(), LayoutError::Canceled);
        let untouched = Case::new(&base.text, base.frames.clone(), Composer::FirstFit);
        assert_eq!(base.paragraphs, untouched.paragraphs);
        assert_eq!(base.text, untouched.text);
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
            vec![frame("f", 0, 0.0, 0.0, 60.0, 36.0, vec![])],
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
