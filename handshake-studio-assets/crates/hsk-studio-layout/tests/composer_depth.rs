//! Lane SMB-LAYOUT-COMPOSER-DEPTH-1 (finding F-R56-1): document data never aborts a composer. On the
//! base sha the Adobe paragraph composer ends these shapes in STATUS_STACK_OVERFLOW, so they have a
//! test binary of their own: an abort there counts as that test failing and takes nothing else
//! down. Every composition runs on a thread of a stated stack size (`on_thread`). Toy font: 6 pt a
//! glyph unless stated, 180 pt measure (30 glyphs), so "word " * 6 fills a line to 174 pt.
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const LS: char = '\u{2028}';
/// The stack F-R56-1 measured the abort on.
const FINDING_STACK: usize = 2 * 1024 * 1024;
/// A stack the composer's own frames fit many times, too small for one frame per line of a long
/// paragraph on the base sha.
const SMALL_STACK: usize = 512 * 1024;

fn on_thread<T: Send + 'static>(stack: usize, work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(stack).spawn(work).unwrap().join().unwrap()
}
/// One glyph per char but U+2028 (Type sets a forced line break outside shaping under composition
/// token 3), `advance` points each.
fn shaped_one(text: &str, advance: f64) -> Vec<ShapedParagraph> {
    let source = SourceSpan { start: 0, end: text.len() as u64 };
    let mut p = ShapedParagraph::from_fixed_advance(text, source, advance, 12.0, 10.0).unwrap();
    p.glyphs.retain(|g| !text[g.cluster.start as usize..].starts_with(LS));
    vec![p]
}
#[derive(Clone, Copy)]
struct Shape {
    composer: Composer,
    alignment: Alignment,
    advance: f64,
    measure: f64,
    limits: Limits,
}
impl Shape {
    fn every_line(alignment: Alignment) -> Self {
        Self { composer: every_line(), alignment, advance: 6.0, measure: 180.0, limits: Limits::default() }
    }
}
fn every_line() -> Composer {
    Composer::AdobeParagraph(AdobeParams::default())
}
fn compose(text: &str, s: Shape) -> Result<LayoutProposal, LayoutError> {
    let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, s.measure, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
    let (st, _) = story(1, text, &[(0, text.len() as u64)]);
    let paragraphs = shaped_one(text, s.advance);
    reflow_typeset(
        &ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer: s.composer,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: s.limits,
        },
        &TypesetOptions { alignment: s.alignment, justify: Some(JustifyParams::default()), ..Default::default() },
        &CancellationToken::default(),
    )
}
fn ranges(p: &LayoutProposal) -> Vec<std::ops::Range<usize>> {
    p.frames.iter().flat_map(|f| f.lines.iter().map(|l| l.glyph_range.clone())).collect()
}
/// `text` at `stack`: every line, against the same text set by first-fit (which breaks forced lines
/// and the uniform "word " text the same way and has no depth).
fn assert_like_first_fit(stack: usize, text: String, lines: usize) {
    let answer = on_thread(stack, move || {
        let shape = Shape::every_line(Alignment::Left);
        let composed = compose(&text, shape);
        let control = compose(&text, Shape { composer: Composer::FirstFit, ..shape });
        (composed, control)
    });
    let (composed, control) = (answer.0.unwrap(), answer.1.unwrap());
    assert_eq!(ranges(&composed).len(), lines);
    assert_eq!(ranges(&composed), ranges(&control));
    assert!(composed.overset.is_none());
}
fn plain(words: usize) -> String {
    "word ".repeat(words)
}
fn forced(n: usize) -> String {
    format!("H{}I", LS.to_string().repeat(n))
}
fn alternating(n: usize) -> String {
    format!("a{LS}").repeat(n)
}

/// F-R56-1 shape 1: `H LS*2000 I`, 2001 lines.
#[test]
fn forced_breaks_2000_on_the_finding_stack() {
    assert_like_first_fit(FINDING_STACK, forced(2000), 2001);
}
/// F-R56-1 shape 2: `(a LS)*2000`, 2000 glyph lines (no line after the paragraph's last break).
#[test]
fn alternating_breaks_2000_on_the_finding_stack() {
    let control = on_thread(FINDING_STACK, || ranges(&compose(&alternating(2000), Shape { composer: Composer::FirstFit, ..Shape::every_line(Alignment::Left) }).unwrap()).len());
    assert_like_first_fit(FINDING_STACK, alternating(2000), control);
    assert!(control >= 2000);
}
/// F-R56-1 shape 3: plain wrapped text, 3000 words, 500 lines of 6 words (the cubed white space is
/// least when every line takes all it can: hand count).
#[test]
fn plain_3000_words_on_the_finding_stack() {
    assert_like_first_fit(FINDING_STACK, plain(3000), 500);
}
/// The depth does not come back at a larger count on a smaller stack.
#[test]
fn forced_breaks_20000_on_a_small_stack() {
    assert_like_first_fit(SMALL_STACK, forced(20_000), 20_001);
}
/// F-R56-1 cost: 1800 plain words exceeded the 120 s test cap on the base sha (debug); the cap of
/// `.config/nextest.toml` is this test's bound.
#[test]
fn plain_1800_words_answer_inside_the_test_cap() {
    assert_like_first_fit(FINDING_STACK, plain(1800), 300);
}
/// Justified text keeps its 7-line window: the same text, answered, on the same stack.
#[test]
fn justified_3000_words_on_the_finding_stack() {
    let lines = on_thread(FINDING_STACK, || ranges(&compose(&plain(3000), Shape::every_line(Alignment::Justified)).unwrap()));
    assert_eq!(lines.len(), 500);
    assert_eq!(lines.last().unwrap().end, 3000 * 5);
}

/// A paragraph whose candidates never leave the line (every word fits, here by a measure far wider
/// than the text): every word starts a state and every state scans to the paragraph end. Past the
/// composer's declared budget it is a typed refusal, not an abort and not an unbounded run.
#[test]
fn wide_measure_past_the_budget_refuses_typed() {
    let shape = Shape { measure: 1e9, ..Shape::every_line(Alignment::Left) };
    let got = on_thread(FINDING_STACK, move || compose(&plain(6000), shape));
    assert_eq!(got.unwrap_err(), LayoutError::BudgetExceeded);
}
/// The same class from font data alone, at an ordinary measure: glyphs of no advance.
#[test]
fn zero_advance_glyphs_past_the_budget_refuse_typed() {
    let shape = Shape { advance: 0.0, ..Shape::every_line(Alignment::Left) };
    let got = on_thread(FINDING_STACK, move || compose(&plain(6000), shape));
    assert_eq!(got.unwrap_err(), LayoutError::BudgetExceeded);
}
/// Justified: the window bounds the depth, not this scan.
#[test]
fn justified_zero_advance_past_the_budget_refuses_typed() {
    let shape = Shape { advance: 0.0, ..Shape::every_line(Alignment::Justified) };
    let got = on_thread(FINDING_STACK, move || compose(&plain(6000), shape));
    assert_eq!(got.unwrap_err(), LayoutError::BudgetExceeded);
}

/// A word no line holds without hyphens, with a ladder limit: after a hyphenated line the limit
/// skips the next hyphen point and no other break fits, so every path dead-ends. On the base sha
/// that is a panic in `compose_flow` (`slots[used - 1]`, used 0; 11 of the generated cases of
/// `composer_depth_diff`). The composer's emergency rule ("nothing fits at all: one overfull line")
/// answers it, in both windows: the rest of the word up to the next break the limit allows, marked
/// overfull, the lines tile the paragraph, and no two consecutive lines end in a hyphen.
fn ladder_dead_end(alignment: Alignment) -> Result<LayoutProposal, LayoutError> {
    let text = "internationalization hyphenation";
    on_thread(FINDING_STACK, move || {
        let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 36.0, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
        let (st, _) = story(1, text, &[(0, text.len() as u64)]);
        let paragraphs = shaped_one(text, 6.0);
        let hyphenation = HyphenationParams { hyphen_advance_pt: 4.0, min_word_len: 5, min_before: 2, min_after: 2, consecutive_limit: 1, ..HyphenationParams::default() };
        reflow_typeset(
            &ReflowRequest {
                document: &doc,
                story: &st,
                resolved_text: None,
                paragraphs: &paragraphs,
                obstacles: &[],
                composer: every_line(),
                space: SpaceParams::default(),
                expected_revision: doc.revision,
                limits: Limits::default(),
            },
            &TypesetOptions { alignment, justify: Some(JustifyParams::default()), hyphenation: Some(hyphenation), ..Default::default() },
            &CancellationToken::default(),
        )
    })
}
#[test]
fn a_ladder_limit_with_no_path_takes_the_emergency_line() {
    for alignment in [Alignment::Left, Alignment::Justified] {
        let p = ladder_dead_end(alignment).unwrap();
        let lines: Vec<&Line> = p.frames.iter().flat_map(|f| f.lines.iter()).collect();
        assert_eq!(lines.first().unwrap().glyph_range.start, 0, "{alignment:?}");
        assert_eq!(lines.last().unwrap().glyph_range.end, "internationalization hyphenation".chars().count(), "{alignment:?}");
        assert!(lines.windows(2).all(|w| w[0].glyph_range.end == w[1].glyph_range.start), "{alignment:?} {lines:?}");
        assert!(lines.iter().any(|l| l.overfull), "{alignment:?} {lines:?}");
        assert!(lines.windows(2).all(|w| !(w[0].hyphenated && w[1].hyphenated)), "{alignment:?} {lines:?}");
        assert_eq!(ladder_dead_end(alignment).unwrap(), p, "deterministic");
    }
}
/// A line cost past the range of a float (the cube of a measure of 1e200 pt) has no comparable
/// total, emergency rule or not: a typed refusal, not the same panic.
#[test]
fn a_measure_past_the_cost_range_refuses_typed() {
    let shape = Shape { measure: 1e200, ..Shape::every_line(Alignment::Left) };
    let got = on_thread(FINDING_STACK, move || compose(&format!("a{LS}b"), shape));
    assert_eq!(got.unwrap_err(), LayoutError::InvalidGeometry);
}

/// One bounded count per composer, past the line budget (`Limits::max_lines` 100 000): typed, no abort.
fn past_the_line_budget(composer: Composer) {
    let shape = Shape { composer, ..Shape::every_line(Alignment::Left) };
    let got = on_thread(FINDING_STACK, move || compose(&forced(100_500), shape));
    assert_eq!(got.unwrap_err(), LayoutError::BudgetExceeded);
}
#[test]
fn first_fit_past_the_line_budget() {
    past_the_line_budget(Composer::FirstFit);
}
#[test]
fn knuth_plass_past_the_line_budget() {
    past_the_line_budget(Composer::KnuthPlass(KnuthPlassParams::default()));
}
#[test]
fn single_line_past_the_line_budget() {
    past_the_line_budget(Composer::AdobeSingleLine(AdobeParams::default()));
}
#[test]
fn every_line_past_the_line_budget() {
    past_the_line_budget(every_line());
}

fn cjk_style_records(scope: StudioCompositionScope) -> StudioCjkCompositionRecords {
    let mut r = StudioCjkCompositionRecords::new(id("SDOC", 1));
    r.composers.push(StudioNamedComposer {
        composer_name: "Japanese".into(),
        composition_scope: scope,
        composition_engine: StudioCompositionEngine::LatinCjk,
        kinsoku_table: Some("hard_kinsoku".into()),
        mojikumi_table: Some("line_end_all_one_half_em".into()),
    });
    r.bindings.push(StudioCjkParagraphBinding {
        style_id: id("STYS", 1),
        composer_name: "Japanese".into(),
        kinsoku_type: StudioKinsokuType::PushInFirst,
        kinsoku_hang_type: StudioKinsokuHangType::None,
        bunri_kinshi: false,
        rensuuji: true,
        burasagari_type: StudioBurasagariType::None,
        kinsoku_order: StudioKinsokuOrder::PushIn,
    });
    r
}
fn cjk_past_the_line_budget(scope: StudioCompositionScope) -> Result<usize, LayoutError> {
    cjk_lines(scope, 105, 100)
}
fn cjk_lines(scope: StudioCompositionScope, lines: usize, max_lines: usize) -> Result<usize, LayoutError> {
    on_thread(FINDING_STACK, move || {
        let text = "漢".repeat(lines * 18);
        let text = text.as_str();
        let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
        let (mut st, _) = story(1, text, &[(0, text.len() as u64)]);
        st.paragraphs[0].style_ref = Some(id("STYS", 1));
        let mut p = ShapedParagraph::from_fixed_advance(text, SourceSpan { start: 0, end: text.len() as u64 }, 10.0, 12.0, 10.0).unwrap();
        p.latin_only = false;
        let paragraphs = vec![p];
        let req = ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: Limits { max_lines, ..Limits::default() },
        };
        let flow = StudioLayoutFlowRecords::new(id("SDOC", 1));
        reflow_cjk(&req, &TypesetOptions::default(), &flow, &cjk_style_records(scope), &CancellationToken::default())
            .map(|r| r.proposal.frames.iter().map(|f| f.lines.len()).sum())
    })
}
/// CJK, both scopes: 105 lines of 18 glyphs, past a `Limits::max_lines` of 100.
#[test]
fn cjk_single_line_past_the_line_budget() {
    assert_eq!(cjk_past_the_line_budget(StudioCompositionScope::SingleLine), Err(LayoutError::BudgetExceeded));
}
#[test]
fn cjk_every_line_past_the_line_budget() {
    assert_eq!(cjk_past_the_line_budget(StudioCompositionScope::EveryLine), Err(LayoutError::BudgetExceeded));
}

/// Zero-advance glyphs and one glyph no line holds: the first Knuth-Plass pass finds no feasible
/// path, the second admits every break of every line count, so the active list holds one node per
/// line count per break (cubic time, quadratic memory, no cancellation inside it on the base sha).
fn past_every_measure(text: &str) -> ShapedParagraph {
    let source = SourceSpan { start: 0, end: text.len() as u64 };
    let mut p = ShapedParagraph::from_fixed_advance(text, source, 0.0, 12.0, 10.0).unwrap();
    p.glyphs.last_mut().unwrap().advance_pt = 1e6;
    p
}
#[test]
fn knuth_plass_active_list_past_the_budget_refuses_typed() {
    let got = on_thread(FINDING_STACK, || {
        let text = format!("{}X", plain(1000));
        let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
        let (st, _) = story(1, &text, &[(0, text.len() as u64)]);
        let paragraphs = vec![past_every_measure(&text)];
        reflow_typeset(
            &ReflowRequest {
                document: &doc,
                story: &st,
                resolved_text: None,
                paragraphs: &paragraphs,
                obstacles: &[],
                composer: Composer::KnuthPlass(KnuthPlassParams::default()),
                space: SpaceParams::default(),
                expected_revision: doc.revision,
                limits: Limits::default(),
            },
            &TypesetOptions::default(),
            &CancellationToken::default(),
        )
        .map(|p| p.frames.iter().map(|f| f.lines.len()).sum::<usize>())
    });
    assert_eq!(got, Err(LayoutError::BudgetExceeded));
}
/// The CJK every-line scope runs the same breaker.
#[test]
fn cjk_every_line_active_list_past_the_budget_refuses_typed() {
    let got = on_thread(FINDING_STACK, || {
        let text = format!("{}X", "漢".repeat(1000));
        let mut para = past_every_measure(&text);
        para.latin_only = false;
        let c = CjkParagraph::new(StudioCompositionScope::EveryLine, KinsokuSet::shipped("hard_kinsoku"), MojikumiRules::base(StudioMojikumiBaseSet::Nothing));
        compose_cjk_lines(&para, &text, &c, 10.0, 200.0, false, &CancellationToken::default()).map(|l| l.len())
    });
    assert_eq!(got, Err(LayoutError::BudgetExceeded));
}

/// One glyph past `Limits::max_glyphs` is refused before any composer runs, for every composer.
#[test]
fn every_composer_past_the_glyph_budget() {
    for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default()), Composer::AdobeSingleLine(AdobeParams::default()), every_line()] {
        let shape = Shape { composer, limits: Limits { max_glyphs: 100, ..Limits::default() }, ..Shape::every_line(Alignment::Left) };
        assert_eq!(compose(&"a".repeat(101), shape).unwrap_err(), LayoutError::BudgetExceeded, "{composer:?}");
        assert!(compose(&"a".repeat(100), shape).is_ok(), "{composer:?}");
    }
}

/// The readings `notes.measurements` of the lane file records: run by hand in debug and release
/// (`--run-ignored only --no-capture`). Prints only; proves nothing by itself.
#[test]
#[ignore = "timing record for the lane notes, not a proof"]
fn timings() {
    for words in [600usize, 1800, 5000, 20_000] {
        for alignment in [Alignment::Left, Alignment::Justified] {
            let text = plain(words);
            let shape = Shape::every_line(alignment);
            let t = std::time::Instant::now();
            let got = on_thread(FINDING_STACK, move || compose(&text, shape).map(|p| ranges(&p).len()));
            println!("TIMING plain words={words} alignment={alignment:?} answer={got:?} elapsed={:?}", t.elapsed());
        }
    }
    // A paragraph at `Limits::max_glyphs` (5 glyphs a word, 1 048 575 glyphs), both windows, and the
    // same with long hyphenated words; then the other breakers on the same text.
    let timed = |label: &str, text: String, shape: Shape, hyphen: Option<u8>| {
        let t = std::time::Instant::now();
        let got = on_thread(FINDING_STACK, move || {
            let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, shape.measure, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
            let (st, _) = story(1, &text, &[(0, text.len() as u64)]);
            let paragraphs = shaped_one(&text, shape.advance);
            let hyphenation = hyphen.map(|consecutive_limit| HyphenationParams { hyphen_advance_pt: 4.0, consecutive_limit, ..HyphenationParams::default() });
            reflow_typeset(
                &ReflowRequest { document: &doc, story: &st, resolved_text: None, paragraphs: &paragraphs, obstacles: &[], composer: shape.composer, space: SpaceParams::default(), expected_revision: doc.revision, limits: shape.limits },
                &TypesetOptions { alignment: shape.alignment, justify: Some(JustifyParams::default()), hyphenation, ..Default::default() },
                &CancellationToken::default(),
            )
            .map(|p| ranges(&p).len())
        });
        println!("TIMING {label} answer={got:?} elapsed={:?}", t.elapsed());
    };
    let long = |n: usize| "typographic ".repeat(n);
    let kp = Composer::KnuthPlass(KnuthPlassParams::default());
    for (alignment, name) in [(Alignment::Left, "ragged"), (Alignment::Justified, "justified")] {
        let shape = Shape::every_line(alignment);
        timed(&format!("max_glyphs plain {name}"), plain(209_715), shape, None);
        for words in [1000usize, 2000] {
            timed(&format!("knuth-plass hyphen limit 3 long words={words} {name}"), long(words), Shape { composer: kp, ..shape }, Some(3));
        }
        timed(&format!("max_glyphs hyphen limit 3 {name}"), long(87_381), shape, Some(3));
        timed(&format!("max_glyphs hyphen no limit {name}"), long(87_381), shape, Some(0));
        timed(&format!("knuth-plass 20000 words {name}"), plain(20_000), Shape { composer: kp, ..shape }, None);
        timed(&format!("knuth-plass max_glyphs {name}"), plain(209_715), Shape { composer: kp, ..shape }, None);
    }
    for lines in [1000usize, 2000] {
        let t = std::time::Instant::now();
        let got = cjk_lines(StudioCompositionScope::EveryLine, lines, 100_000);
        println!("TIMING cjk reflow every-line mojikumi lines={lines} answer={got:?} elapsed={:?}", t.elapsed());
    }
    for glyphs in [1890usize, 18_090, 180_900] {
        let t = std::time::Instant::now();
        let got = on_thread(FINDING_STACK, move || {
            let text = "漢".repeat(glyphs);
            let mut para = ShapedParagraph::from_fixed_advance(&text, SourceSpan { start: 0, end: text.len() as u64 }, 10.0, 12.0, 10.0).unwrap();
            para.latin_only = false;
            let c = CjkParagraph::new(StudioCompositionScope::EveryLine, KinsokuSet::shipped("hard_kinsoku"), MojikumiRules::base(StudioMojikumiBaseSet::Nothing));
            compose_cjk_lines(&para, &text, &c, 10.0, 180.0, false, &CancellationToken::default()).map(|l| l.len())
        });
        println!("TIMING cjk every-line glyphs={glyphs} answer={got:?} elapsed={:?}", t.elapsed());
    }
    for words in [600usize, 1800, 5000, 6000, 20_000, 209_715] {
        for (label, advance, measure, alignment) in [
            ("wide_measure", 6.0, 1e9, Alignment::Left),
            ("zero_advance", 0.0, 180.0, Alignment::Left),
            ("zero_advance", 0.0, 180.0, Alignment::Justified),
        ] {
            let shape = Shape { advance, measure, ..Shape::every_line(alignment) };
            let t = std::time::Instant::now();
            let got = on_thread(FINDING_STACK, move || compose(&plain(words), shape).map(|p| ranges(&p).len()));
            println!("TIMING hostile {label} words={words} alignment={alignment:?} answer={got:?} elapsed={:?}", t.elapsed());
        }
    }
    let kp_hostile = |words: usize| {
        let t = std::time::Instant::now();
        let got = on_thread(FINDING_STACK, move || {
            let text = format!("{}X", plain(words));
            let doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 180.0, 2_000_000.0), None, None)], [PageSide::LeftHand, PageSide::RightHand]);
            let (st, _) = story(1, &text, &[(0, text.len() as u64)]);
            let paragraphs = vec![past_every_measure(&text)];
            let req = ReflowRequest { document: &doc, story: &st, resolved_text: None, paragraphs: &paragraphs, obstacles: &[], composer: Composer::KnuthPlass(KnuthPlassParams::default()), space: SpaceParams::default(), expected_revision: doc.revision, limits: Limits::default() };
            reflow_typeset(&req, &TypesetOptions::default(), &CancellationToken::default()).map(|p| ranges(&p).len())
        });
        println!("TIMING hostile knuth-plass active list words={words} answer={got:?} elapsed={:?}", t.elapsed());
    };
    for words in [1000usize, 4000, 20_000] {
        kp_hostile(words);
    }
}
