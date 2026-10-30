//! Lane SMB-AI-AREA-TYPE-1 slice 1: area type inside any closed container, auto-size (STU-TYP-116),
//! overset reporting, point <-> area conversion (STU-TYP-114) and hostile containers. Expected
//! geometry is closed-form (band intersections with a circle and rectangles). Illustrator parity of
//! the band rule, inset, first baseline and auto-size reference point is UNVERIFIED (D5).
use hsk_studio_type::*;

const EPS: f64 = 1e-9;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<BaselineCommand> {
    vec![
        BaselineCommand::MoveTo([x0, y0]),
        BaselineCommand::LineTo([x1, y0]),
        BaselineCommand::LineTo([x1, y1]),
        BaselineCommand::LineTo([x0, y1]),
        BaselineCommand::Close,
    ]
}
/// Circle of radius `r` about `(cx, cy)` as four cubic arcs (kappa construction).
fn circle(cx: f64, cy: f64, r: f64) -> Vec<BaselineCommand> {
    let k = 0.552_284_749_830_793_4 * r;
    vec![
        BaselineCommand::MoveTo([cx + r, cy]),
        BaselineCommand::CubicTo([cx + r, cy + k], [cx + k, cy + r], [cx, cy + r]),
        BaselineCommand::CubicTo([cx - k, cy + r], [cx - r, cy + k], [cx - r, cy]),
        BaselineCommand::CubicTo([cx - r, cy - k], [cx - k, cy - r], [cx, cy - r]),
        BaselineCommand::CubicTo([cx + k, cy - r], [cx + r, cy - k], [cx + r, cy]),
        BaselineCommand::Close,
    ]
}
fn run(n: usize) -> ComposeRun {
    ComposeRun { glyph_count: n, ascent_pt: 8.0, descent_pt: 2.0, leading_pt: 12.0, cap_height_pt: 7.0, x_height_pt: 5.0 }
}
/// `words` words of `letters` glyphs (5 pt each) separated by one 5 pt space; `paragraphs` ends
/// each paragraph after that many words with a paragraph glyph.
fn story(words: usize, letters: usize, per_paragraph: usize) -> Vec<ComposeGlyph> {
    let mut g = Vec::new();
    for w in 0..words {
        for _ in 0..letters {
            g.push(ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::None });
        }
        let last = w + 1 == words;
        if (w + 1) % per_paragraph == 0 || last {
            g.push(ComposeGlyph { advance_pt: 0.0, brk: GlyphBreak::Paragraph });
        } else {
            g.push(ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::Space });
        }
    }
    g
}
fn lay(c: &[BaselineCommand], g: &[ComposeGlyph], o: &AreaTextOptions) -> AreaTextLayout {
    layout_area_text(c, &[run(g.len())], g, o, &AreaTextLimits::default()).expect("layout")
}

#[test]
fn area_text_wraps_inside_a_circle_container() {
    // Circle r = 100 about (100, 100); words of 5 x 5 pt (25 pt) + 5 pt spaces, one paragraph
    // (sizes chosen so no line is within 4 pt of a word boundary: the Bezier circle error cannot
    // flip a word).
    let (cx, cy, r) = (100.0, 100.0, 100.0);
    let glyphs = story(60, 5, 1000);
    let l = lay(&circle(cx, cy, r), &glyphs, &AreaTextOptions::default());
    // Closed form: baseline_k = 8 + 12 k (first line `ascent` below the top 0, then leading 12);
    // band [b - 8, b + 2] (top clamped to 0); the widest interval inside the whole band has
    // half-width sqrt(r^2 - d^2), d = the band edge farthest from the centre line; a line takes
    // n words with 25 n + 5 (n - 1) <= width; a band too narrow for one word is skipped.
    let mut expected: Vec<(f64, f64, usize)> = Vec::new(); // (baseline, half-width, words)
    let mut placed_words = 0;
    let mut b = 8.0;
    while b + 2.0 <= 200.0 && placed_words < 60 {
        let (t, bot) = ((b - 8.0_f64).max(0.0), b + 2.0);
        let d = (t - cy).abs().max((bot - cy).abs());
        let hw = (r * r - d * d).max(0.0).sqrt();
        let n = (((2.0 * hw + 5.0) / 30.0).floor() as usize).min(60 - placed_words);
        if n > 0 {
            expected.push((b, hw, n));
            placed_words += n;
        }
        b += 12.0;
    }
    assert_eq!(l.lines.len(), expected.len(), "lines {:?}", l.lines.iter().map(|x| (x.baseline_y, x.right - x.left)).collect::<Vec<_>>());
    let mut first = 0;
    for (line, (b, hw, n)) in l.lines.iter().zip(expected.iter().copied()) {
        assert!((line.baseline_y - b).abs() < EPS, "baseline {} != {b}", line.baseline_y);
        // Bezier circle (radial error < 0.03) and flattening (0.01): 0.05 pt.
        assert!((line.left - (cx - hw)).abs() < 0.05 && (line.right - (cx + hw)).abs() < 0.05, "interval {line:?} vs half-width {hw}");
        assert!((line.x - line.left).abs() < EPS, "left aligned");
        assert_eq!(line.first_glyph, first);
        assert_eq!(line.glyph_count, n * 6, "words on the line at {b}");
        assert!((line.width_pt - (n as f64 * 30.0 - 5.0)).abs() < EPS);
        first += n * 6;
    }
    assert_eq!(placed_words, 60);
    assert_eq!((l.lines.len(), l.overset_from), (12, None));
    // Every ink box of every placed glyph lies inside the circle.
    for g in &l.glyphs {
        let a = glyphs[g.index];
        if a.brk != GlyphBreak::None {
            continue;
        }
        let (x, by) = (g.transform[4], g.transform[5]);
        assert_eq!(&g.transform[..4], &[1.0, 0.0, 0.0, -1.0]);
        for (px, py) in [(x, by - 8.0), (x + 5.0, by - 8.0), (x, by + 2.0), (x + 5.0, by + 2.0)] {
            assert!((px - cx).hypot(py - cy) <= r + 0.05, "glyph {} corner ({px}, {py}) outside", g.index);
        }
    }
    // Centre alignment centres each line in its interval; inset shrinks the interval.
    let c = lay(&circle(cx, cy, r), &glyphs, &AreaTextOptions { align: TextAlign::Center, ..Default::default() });
    for line in &c.lines {
        assert!((line.x + line.width_pt / 2.0 - (line.left + line.right) / 2.0).abs() < EPS);
    }
    let inset = lay(&circle(cx, cy, r), &glyphs, &AreaTextOptions { inset_pt: 10.0, ..Default::default() });
    // First baseline 10 + 8; every interval at least 10 inside the circle on the band edges.
    assert!(inset.lines.iter().all(|l| l.baseline_y >= 18.0 - EPS));
    for line in &inset.lines {
        let (t, bot) = (line.baseline_y - 8.0 - 10.0, line.baseline_y + 2.0 + 10.0);
        let d = (t - cy).abs().max((bot - cy).abs());
        let hw = (r * r - d * d).sqrt() - 10.0;
        assert!((line.left - (cx - hw)).abs() < 0.05 && (line.right - (cx + hw)).abs() < 0.05, "inset interval {line:?}");
    }
    // A ring (circle with an oppositely wound, off-centre hole of r 30 about (110, 100)): the band
    // [96, 106] of the line at 104 crosses the hole; the left part [100 - sqrt(r^2 - 6^2), 80] is
    // wider than the right part [140, 100 + sqrt(r^2 - 6^2)] and is used.
    let mut ring = circle(cx, cy, r);
    let (hx, k) = (110.0, 0.552_284_749_830_793_4 * 30.0);
    ring.extend([
        BaselineCommand::MoveTo([hx + 30.0, cy]),
        BaselineCommand::CubicTo([hx + 30.0, cy - k], [hx + k, cy - 30.0], [hx, cy - 30.0]),
        BaselineCommand::CubicTo([hx - k, cy - 30.0], [hx - 30.0, cy - k], [hx - 30.0, cy]),
        BaselineCommand::CubicTo([hx - 30.0, cy + k], [hx - k, cy + 30.0], [hx, cy + 30.0]),
        BaselineCommand::CubicTo([hx + k, cy + 30.0], [hx + 30.0, cy + k], [hx + 30.0, cy]),
        BaselineCommand::Close,
    ]);
    let rl = lay(&ring, &story(120, 5, 1000), &AreaTextOptions::default());
    let across = rl.lines.iter().find(|l| (l.baseline_y - 104.0).abs() < EPS).expect("line at 104");
    assert!((across.left - (cx - (r * r - 36.0_f64).sqrt())).abs() < 0.05 && (across.right - 80.0).abs() < 0.05, "{across:?}");
}

#[test]
fn auto_size_members_resize_rectangular_containers() {
    // Rectangle (10, 20)-(110, 60), inset 2: 96 pt wide, first baseline 20 + 2 + 8 = 30.
    let c = rect(10.0, 20.0, 110.0, 60.0);
    // Paragraphs of 6 words (6 x 25 - 5 = 145 pt on one line), 3 paragraphs.
    let glyphs = story(18, 4, 6);
    let o = AreaTextOptions { inset_pt: 2.0, ..Default::default() };
    // off: fixed geometry; 96 pt holds 4 words (95 pt) per line, so each paragraph takes a 4-word
    // and a 2-word line; lines at 30, 42 (band bottom 44 <= 58), 54 (bottom 56 <= 58); the rest
    // is overset.
    let off = lay(&c, &glyphs, &o);
    assert_eq!((off.container, off.resized, off.scale), ([10.0, 20.0, 110.0, 60.0], false, 1.0));
    assert_eq!(off.lines.iter().map(|l| l.baseline_y).collect::<Vec<_>>(), vec![30.0, 42.0, 54.0]);
    assert_eq!(off.lines.iter().map(|l| l.glyph_count).collect::<Vec<_>>(), vec![20, 10, 20]);
    assert_eq!((off.overset_from, off.overset_count), (Some(50), glyphs.len() - 50));
    // height_only: 6 lines (2 per paragraph) at 30 + 12 k; height = 90 + 2 + 2 - 20 = 74.
    let h = lay(&c, &glyphs, &AreaTextOptions { auto_size: AutoSize::HeightOnly, ..o });
    assert_eq!(h.lines.len(), 6);
    assert_eq!((h.overset_from, h.resized), (None, true));
    assert!((h.container[3] - (90.0 + 2.0 + 2.0)).abs() < EPS && h.container[..3] == [10.0, 20.0, 110.0], "{:?}", h.container);
    // width_only: width = 145 + 4 = 149; one line per paragraph, height fixed (3 lines fit).
    let w = lay(&c, &glyphs, &AreaTextOptions { auto_size: AutoSize::WidthOnly, ..o });
    assert_eq!(w.container, [10.0, 20.0, 159.0, 60.0]);
    assert_eq!((w.lines.len(), w.overset_from), (3, None));
    assert!(w.lines.iter().all(|l| (l.width_pt - 145.0).abs() < EPS && l.glyph_count == 30));
    // height_and_width: width 149, height = 54 + 2 + 2 - 20 = 38.
    let hw = lay(&c, &glyphs, &AreaTextOptions { auto_size: AutoSize::HeightAndWidth, ..o });
    assert!((hw.container[2] - 159.0).abs() < EPS && (hw.container[3] - 58.0).abs() < EPS, "{:?}", hw.container);
    assert_eq!(hw.overset_from, None);
    // Proportional: the smallest common factor at which nothing is overset (bisection): fits at
    // the factor, overset 0.1 % below it, aspect kept.
    let p = lay(&c, &glyphs, &AreaTextOptions { auto_size: AutoSize::HeightAndWidthProportional, ..o });
    assert_eq!(p.overset_from, None);
    assert!(p.scale > 1.0);
    let (pw, ph) = (p.container[2] - p.container[0], p.container[3] - p.container[1]);
    assert!((pw / ph - 2.5).abs() < 1e-9 && (pw - 100.0 * p.scale).abs() < 1e-9);
    let below = 0.999 * p.scale;
    let smaller = lay(&rect(10.0, 20.0, 10.0 + 100.0 * below, 20.0 + 40.0 * below), &glyphs, &o);
    assert!(smaller.overset_from.is_some(), "a smaller factor must overset");
    // A non-rectangular container takes only `off`.
    let tri = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([200.0, 0.0]), BaselineCommand::LineTo([100.0, 200.0]), BaselineCommand::Close];
    for a in [AutoSize::HeightOnly, AutoSize::WidthOnly, AutoSize::HeightAndWidth, AutoSize::HeightAndWidthProportional] {
        let e = layout_area_text(&tri, &[run(glyphs.len())], &glyphs, &AreaTextOptions { auto_size: a, ..o }, &AreaTextLimits::default());
        assert_eq!(e, Err(AreaTextError::AutoSizeNeedsRectangle), "{a:?}");
    }
    assert!(layout_area_text(&tri, &[run(glyphs.len())], &glyphs, &o, &AreaTextLimits::default()).is_ok());
    // A rotated square is not a rectangle for auto-size.
    let diamond = vec![BaselineCommand::MoveTo([50.0, 0.0]), BaselineCommand::LineTo([100.0, 50.0]), BaselineCommand::LineTo([50.0, 100.0]), BaselineCommand::LineTo([0.0, 50.0]), BaselineCommand::Close];
    let e = layout_area_text(&diamond, &[run(glyphs.len())], &glyphs, &AreaTextOptions { auto_size: AutoSize::HeightOnly, ..o }, &AreaTextLimits::default());
    assert_eq!(e, Err(AreaTextError::AutoSizeNeedsRectangle));
}

#[test]
fn overset_is_reported_not_dropped() {
    // 50 x 30 rectangle: 2 words per line (45 pt), lines at 8 and 20 (band bottom 22 <= 30; the
    // third at 32 would end at 34 > 30).
    let glyphs = story(9, 4, 1000);
    let l = lay(&rect(0.0, 0.0, 50.0, 30.0), &glyphs, &AreaTextOptions::default());
    assert_eq!(l.lines.len(), 2);
    assert_eq!((l.overset_from, l.overset_count), (Some(20), glyphs.len() - 20));
    assert_eq!(l.glyphs.len() + l.overset_count, glyphs.len(), "placed + overset = every glyph");
    assert!(l.glyphs.iter().enumerate().all(|(i, g)| g.index == i), "placed glyphs are the leading prefix, in order");
    // The space at a soft wrap hangs at the line end (placed, not counted in the width).
    assert!((l.lines[0].width_pt - 45.0).abs() < EPS && l.lines[0].glyph_count == 10);
    // A word wider than a full-width rectangle breaks between glyphs; a glyph wider than the box
    // stops the layout with the rest overset.
    let long = vec![ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::None }; 25];
    let b = lay(&rect(0.0, 0.0, 50.0, 100.0), &long, &AreaTextOptions::default());
    assert_eq!(b.lines.iter().map(|l| l.glyph_count).collect::<Vec<_>>(), vec![10, 10, 5]);
    let wide = vec![ComposeGlyph { advance_pt: 60.0, brk: GlyphBreak::None }, ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::None }];
    let w = lay(&rect(0.0, 0.0, 50.0, 100.0), &wide, &AreaTextOptions::default());
    assert_eq!((w.glyphs.len(), w.overset_from, w.overset_count), (0, Some(0), 2));
    // A container too short for one line oversets everything.
    let s = lay(&rect(0.0, 0.0, 50.0, 5.0), &glyphs, &AreaTextOptions::default());
    assert_eq!((s.glyphs.len(), s.overset_from, s.overset_count), (0, Some(0), glyphs.len()));
    // An empty story lays out to nothing, with nothing overset.
    let e = lay(&rect(0.0, 0.0, 50.0, 30.0), &[], &AreaTextOptions::default());
    assert_eq!((e.lines.len(), e.overset_from), (0, None));
}

/// A stand-in story value with every attribute kind STU-TYP-114 names.
#[derive(Clone, Debug, PartialEq)]
struct Story {
    text: String,
    character_attributes: Vec<(u64, u64, &'static str)>,
    paragraph_attributes: Vec<(u64, u64, &'static str)>,
    style_bindings: Vec<(u64, u64, &'static str)>,
    overrides: Vec<(u64, u64, &'static str, f64)>,
}

#[test]
fn point_area_conversion_keeps_every_attribute() {
    let s = Story {
        text: "four word line here\nsecond paragraph".into(),
        character_attributes: vec![(0, 4, "bold"), (5, 9, "tracking=40")],
        paragraph_attributes: vec![(0, 20, "space_after=6"), (20, 36, "indent=12")],
        style_bindings: vec![(0, 20, "STYS-p1"), (0, 36, "STYS-c1")],
        overrides: vec![(10, 14, "size", 14.5)],
    };
    // Two paragraphs: 3 words then 2 words; second run with a larger leading.
    let glyphs = story(5, 4, 3);
    let runs = [run(15), ComposeRun { glyph_count: glyphs.len() - 15, leading_pt: 15.0, ..run(0) }];
    for align in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
        let point = PointText { story: s.clone(), origin: [40.0, 70.0], align };
        let pl = layout_point_text(point.origin, &runs, &glyphs, align, &AreaTextLimits::default()).unwrap();
        assert_eq!(pl.lines.iter().map(|l| l.baseline_y).collect::<Vec<_>>(), vec![70.0, 85.0]);
        let area = point_to_area(point.clone(), &pl).unwrap();
        // Every attribute, binding and override moved through untouched.
        assert_eq!(area.story, s);
        assert_eq!((area.options.align, area.options.inset_pt, area.options.auto_size), (align, 0.0, AutoSize::Off));
        // Container = the line boxes: widest line 70 pt, top 70 - 8, bottom 85 + 2.
        let al = layout_area_text(&area.container, &runs, &glyphs, &area.options, &AreaTextLimits::default()).unwrap();
        assert_eq!(al.container[3] - al.container[1], 25.0);
        assert!((al.container[2] - al.container[0] - 70.0).abs() < EPS);
        // The area layout places every glyph where the point layout did.
        assert_eq!(al.overset_from, None);
        assert_eq!(al.glyphs.len(), pl.glyphs.len());
        for (a, p) in al.glyphs.iter().zip(&pl.glyphs) {
            assert_eq!((a.index, a.run, a.line), (p.index, p.run, p.line));
            for (x, y) in a.transform.iter().zip(p.transform) {
                assert!((x - y).abs() < EPS, "{align:?} glyph {}: {:?} vs {:?}", a.index, a.transform, p.transform);
            }
        }
        // Back to point: same origin, same story.
        let back = area_to_point(area, &al);
        assert_eq!(back, point, "{align:?}");
    }
    // Area -> point keeps overset text: the story is not cut.
    let tight = AreaText { story: s.clone(), container: rect(0.0, 0.0, 50.0, 12.0), options: AreaTextOptions::default() };
    let tl = layout_area_text(&tight.container, &runs, &glyphs, &tight.options, &AreaTextLimits::default()).unwrap();
    assert!(tl.overset_count > 0);
    let p = area_to_point(tight, &tl);
    assert_eq!((p.story, p.origin), (s, [0.0, 8.0]));
    // An empty point text has no box to convert.
    let empty = layout_point_text([0.0, 0.0], &[], &[], TextAlign::Left, &AreaTextLimits::default()).unwrap();
    assert_eq!(point_to_area(PointText { story: (), origin: [0.0, 0.0], align: TextAlign::Left }, &empty), Err(AreaTextError::InvalidInput("empty_point_text")));
}

#[test]
fn hostile_containers_are_bounded() {
    let lim = AreaTextLimits::default();
    let o = AreaTextOptions::default();
    let g = story(3, 4, 1000);
    let r1 = [run(g.len())];
    let err = |c: &[BaselineCommand], runs: &[ComposeRun], g: &[ComposeGlyph], o: &AreaTextOptions, lim: &AreaTextLimits| layout_area_text(c, runs, g, o, lim).unwrap_err();
    // Open, empty, degenerate and non-finite containers.
    let open = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([100.0, 0.0]), BaselineCommand::LineTo([100.0, 100.0])];
    assert_eq!(err(&open, &r1, &g, &o, &lim), AreaTextError::OpenContainer);
    assert_eq!(err(&[], &r1, &g, &o, &lim), AreaTextError::DegenerateContainer);
    let line = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([100.0, 0.0]), BaselineCommand::Close];
    assert_eq!(err(&line, &r1, &g, &o, &lim), AreaTextError::DegenerateContainer);
    let nan = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([f64::NAN, 1.0]), BaselineCommand::Close];
    assert_eq!(err(&nan, &r1, &g, &o, &lim), AreaTextError::InvalidInput("container"));
    let after_close = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([1.0, 0.0]), BaselineCommand::LineTo([1.0, 1.0]), BaselineCommand::Close, BaselineCommand::LineTo([3.0, 3.0])];
    assert_eq!(err(&after_close, &r1, &g, &o, &lim), AreaTextError::InvalidInput("container_segment_after_close"));
    assert_eq!(err(&[BaselineCommand::LineTo([1.0, 1.0])], &r1, &g, &o, &lim), AreaTextError::InvalidInput("container_without_moveto"));
    // Caps refuse before allocation: commands, flattened vertices (a huge curve), glyphs, runs.
    let small = AreaTextLimits { max_commands: 3, ..lim };
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &g, &o, &small), AreaTextError::OverLimit("max_commands"));
    let few = AreaTextLimits { max_vertices: 100, ..lim };
    assert_eq!(err(&circle(0.0, 0.0, 1.0e6), &r1, &g, &o, &few), AreaTextError::OverLimit("max_vertices"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &g, &o, &AreaTextLimits { max_glyphs: 3, ..lim }), AreaTextError::OverLimit("max_glyphs"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &[run(0); 3], &[], &o, &AreaTextLimits { max_runs: 2, ..lim }), AreaTextError::OverLimit("max_runs"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &[run(usize::MAX), run(1)], &g, &o, &lim), AreaTextError::OverLimit("max_glyphs"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &[run(1)], &g, &o, &lim), AreaTextError::InvalidInput("run_glyph_counts"));
    // Metrics and options.
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &[ComposeRun { leading_pt: 0.0, ..run(g.len()) }], &g, &o, &lim), AreaTextError::InvalidInput("run_metrics"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &[ComposeRun { ascent_pt: f64::INFINITY, ..run(g.len()) }], &g, &o, &lim), AreaTextError::InvalidInput("run_metrics"));
    let mut neg = g.clone();
    neg[0].advance_pt = -1.0;
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &neg, &o, &lim), AreaTextError::InvalidInput("advance"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &g, &AreaTextOptions { inset_pt: -1.0, ..o }, &lim), AreaTextError::InvalidInput("inset_pt"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &g, &AreaTextOptions { flatness_pt: 0.0, ..o }, &lim), AreaTextError::InvalidInput("flatness_pt"));
    assert_eq!(err(&rect(0.0, 0.0, 9.0, 9.0), &r1, &g, &AreaTextOptions { first_baseline_min_pt: f64::NAN, ..o }, &lim), AreaTextError::InvalidInput("first_baseline_min_pt"));
    // Band budget: a tall container with a hair leading and nothing that fits is bounded.
    let hair = [ComposeRun { leading_pt: 1e-3, ascent_pt: 0.0, descent_pt: 0.0, ..run(1) }];
    let fat = [ComposeGlyph { advance_pt: 1.0e9, brk: GlyphBreak::None }];
    let tall = vec![BaselineCommand::MoveTo([5.0, 0.0]), BaselineCommand::LineTo([10.0, 1.0e9]), BaselineCommand::LineTo([0.0, 1.0e9]), BaselineCommand::Close];
    assert_eq!(err(&tall, &hair, &fat, &o, &AreaTextLimits { max_bands: 1000, ..lim }), AreaTextError::OverLimit("max_bands"));
    // A huge glyph count inside the caps: 200 000 glyphs into a 100 x 100 box; the rest is overset.
    let many = vec![ComposeGlyph { advance_pt: 1.0, brk: GlyphBreak::None }; 200_000];
    let l = layout_area_text(&rect(0.0, 0.0, 100.0, 100.0), &[run(200_000)], &many, &o, &lim).unwrap();
    assert_eq!(l.lines.len(), 8);
    assert_eq!((l.glyphs.len(), l.overset_from, l.overset_count), (800, Some(800), 199_200));
    // Point text refuses a non-finite origin.
    assert_eq!(layout_point_text([f64::NAN, 0.0], &r1, &g, TextAlign::Left, &lim), Err(AreaTextError::InvalidInput("origin")));
    // Codes are stable.
    assert_eq!(AreaTextError::AutoSizeNeedsRectangle.code(), "area_text_auto_size_needs_rectangle");
}
