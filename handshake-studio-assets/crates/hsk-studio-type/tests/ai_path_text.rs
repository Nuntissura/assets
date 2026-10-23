//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 1: text on a path (STU-TYP-004, STU-TYP-120, STU-TYP-121).
//! Every expected transform is computed by hand from the module rule
//! `doc = (a x + c y + e, b x + d y + f)` with tangent `T`, up-normal `N = (T.y, -T.x)` (y down).
//! Illustrator parity is UNVERIFIED (D5).
use hsk_studio_type::*;

const EPS: f64 = 1e-9;

fn close(a: [f64; 6], b: [f64; 6]) {
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < EPS, "transform {a:?} != expected {b:?}");
    }
}
fn run(n: usize, baseline: f64) -> PathTextRun {
    PathTextRun { glyph_count: n, baseline_offset_pt: baseline, ascent_pt: 8.0, descent_pt: 2.0, em_top_pt: 8.8, em_bottom_pt: -1.2, icf_top_pt: 8.5, icf_bottom_pt: -1.5 }
}
fn l_path() -> Vec<BaselineCommand> {
    vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([100.0, 0.0]), BaselineCommand::LineTo([100.0, 100.0])]
}
fn lay(path: &[BaselineCommand], runs: &[PathTextRun], adv: &[f64], o: &PathTextOptions) -> PathTextLayout {
    layout_path_text(path, runs, adv, o, &PathTextLimits::default()).expect("layout")
}

#[test]
fn ai_path_text_rainbow_hand_positions_runs_alignment_overset() {
    let o = PathTextOptions { start_t: Some(0.5), ..Default::default() };
    let adv = [10.0; 6];
    let l = lay(&l_path(), &[run(3, 0.0), run(3, 2.0)], &adv, &o);
    assert!((l.path_length_pt - 200.0).abs() < EPS && (l.available_pt - 150.0).abs() < EPS);
    assert_eq!((l.glyphs.len(), l.overset_from, l.overset_count), (6, None, 0));
    // Glyph 0 on [50, 60] of the horizontal segment: no rotation, y up = document -y.
    close(l.glyphs[0].transform, [1.0, 0.0, 0.0, -1.0, 50.0, 0.0]);
    // Glyph 4 (run 1, baseline +2): [90, 100], midpoint (95, 0), origin (95,0) + 2 N - 5 T = (90, -2).
    close(l.glyphs[4].transform, [1.0, 0.0, 0.0, -1.0, 90.0, -2.0]);
    // Glyph 5 turns the corner: [100, 110], midpoint (100, 5), T = (0, 1), N = (1, 0),
    // origin (100, 5) + 2 (1, 0) - 5 (0, 1) = (102, 0).
    close(l.glyphs[5].transform, [0.0, 1.0, 1.0, 0.0, 102.0, 0.0]);
    assert_eq!((l.glyphs[5].run, l.glyphs[5].arc_start_pt, l.glyphs[5].arc_end_pt), (1, 100.0, 110.0));

    // text_type_alignment ascender: the ascent (8) meets the path, baseline 8 below it.
    let asc = lay(&l_path(), &[run(1, 0.0)], &[10.0], &PathTextOptions { text_alignment: TextTypeAlignment::Ascender, ..o });
    close(asc.glyphs[0].transform, [1.0, 0.0, 0.0, -1.0, 50.0, 8.0]);
    // path_type_alignment top_path on a 4 pt stroke: baseline 2 above the centre line.
    let top = lay(&l_path(), &[run(1, 0.0)], &[10.0], &PathTextOptions { path_alignment: PathTypeAlignment::TopPath, stroke_width_pt: 4.0, ..o });
    close(top.glyphs[0].transform, [1.0, 0.0, 0.0, -1.0, 50.0, -2.0]);

    // Extent alignment: 30 pt of text in 150 pt: centre lead 60 -> [110, 120] on the vertical leg.
    let c = lay(&l_path(), &[run(3, 0.0)], &[10.0; 3], &PathTextOptions { align: PathTextAlign::Center, ..o });
    close(c.glyphs[0].transform, [0.0, 1.0, 1.0, 0.0, 100.0, 10.0]);
    let e = lay(&l_path(), &[run(3, 0.0)], &[10.0; 3], &PathTextOptions { align: PathTextAlign::End, ..o });
    close(e.glyphs[0].transform, [0.0, 1.0, 1.0, 0.0, 100.0, 70.0]);
    // Right-to-left flow anchors `start` at the end handle.
    let rtl = lay(&l_path(), &[run(3, 0.0)], &[10.0; 3], &PathTextOptions { flow_direction: PathFlowDirection::RightToLeft, ..o });
    assert_eq!(rtl.glyphs, e.glyphs);

    // Overset: 16 x 10 pt into 150 pt -> 15 placed, glyph 15 overset, reported.
    let over = lay(&l_path(), &[run(16, 0.0)], &[10.0; 16], &o);
    assert_eq!((over.glyphs.len(), over.overset_from, over.overset_count), (15, Some(15), 1));
    assert!((over.text_length_pt - 160.0).abs() < EPS);

    // The outline follows the glyph transform: font units x size / upem, then the transform.
    let outline = GlyphOutline::synthetic(1000, vec![OutlineCommand::MoveTo([0.0, 0.0]), OutlineCommand::LineTo([1000.0, 500.0])]);
    let placed = place_outline(&l.glyphs[0], &outline, 10.0).unwrap();
    match placed.as_slice() {
        [BaselineCommand::MoveTo(a), BaselineCommand::LineTo(b)] => {
            assert!((a[0] - 50.0).abs() < EPS && a[1].abs() < EPS && (b[0] - 60.0).abs() < EPS && (b[1] + 5.0).abs() < EPS, "{placed:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ai_path_text_effects_flip_closed_wrap_and_cubic_arc() {
    // (0,0) -> (30,40) -> (60,0): first leg length 50, T = (0.6, 0.8), N = (0.8, -0.6);
    // bounding-box centre (30, 20). One 10 pt glyph at s = 20..30, midpoint (15, 20).
    let path = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([30.0, 40.0]), BaselineCommand::LineTo([60.0, 0.0])];
    let at = |effect| lay(&path, &[run(1, 0.0)], &[10.0], &PathTextOptions { start_t: Some(0.4), effect, ..Default::default() }).glyphs[0].transform;
    // rainbow: x = T, y = N, origin P - 5 T = (12, 16).
    close(at(PathEffect::Rainbow), [0.6, 0.8, 0.8, -0.6, 12.0, 16.0]);
    // skew: x = T, y vertical (0, -1) (N points up), origin (12, 16).
    close(at(PathEffect::Skew), [0.6, 0.8, 0.0, -1.0, 12.0, 16.0]);
    // 3D ribbon: x = (T.x, 0), y = N, origin P - 5 (0.6, 0) = (12, 20).
    close(at(PathEffect::Ribbon), [0.6, 0.0, 0.8, -0.6, 12.0, 20.0]);
    // stair step: unrotated, origin at the glyph start point (12, 16).
    close(at(PathEffect::StairStep), [1.0, 0.0, 0.0, -1.0, 12.0, 16.0]);
    // gravity: y = unit(P - centre) = (-1, 0), against N -> (1, 0); x = (-y.y, y.x) = (0, 1);
    // origin P - 5 x = (15, 15).
    close(at(PathEffect::Gravity), [0.0, 1.0, 1.0, 0.0, 15.0, 15.0]);

    // flipped: the text runs along (60,0) -> (30,40) -> (0,0); handles [0.4, 2] -> [0, 80] reversed.
    // First glyph [0, 10]: T = (-0.6, 0.8), N = (0.8, 0.6), midpoint (57, 4), origin (60, 0).
    let f = lay(&path, &[run(1, 0.0)], &[10.0], &PathTextOptions { start_t: Some(0.4), flipped: PathFlip::Flipped, ..Default::default() });
    close(f.glyphs[0].transform, [-0.6, 0.8, 0.8, 0.6, 60.0, 0.0]);
    assert!((f.available_pt - 80.0).abs() < EPS);

    // Square 100 x 100, closed: 4 segments, length 400. start_t 3.5 = (0, 50) on the closing leg,
    // end_t 0.5 = (50, 0): the extent wraps through the closing point, 100 pt available.
    let square = vec![
        BaselineCommand::MoveTo([0.0, 0.0]),
        BaselineCommand::LineTo([100.0, 0.0]),
        BaselineCommand::LineTo([100.0, 100.0]),
        BaselineCommand::LineTo([0.0, 100.0]),
        BaselineCommand::Close,
    ];
    let o = PathTextOptions { start_t: Some(3.5), end_t: Some(0.5), ..Default::default() };
    let l = lay(&square, &[run(11, 0.0)], &[10.0; 11], &o);
    assert!((l.available_pt - 100.0).abs() < EPS);
    assert_eq!((l.glyphs.len(), l.overset_from), (10, Some(10)));
    // Glyph 0 on the closing leg going up: midpoint (0, 45), T = (0, -1), N = (-1, 0), origin (0, 50).
    close(l.glyphs[0].transform, [0.0, -1.0, -1.0, 0.0, 0.0, 50.0]);
    // Glyph 5 wraps through the closing point onto the first leg: [0, 10], origin (0, 0).
    close(l.glyphs[5].transform, [1.0, 0.0, 0.0, -1.0, 0.0, 0.0]);
    assert_eq!((l.glyphs[5].arc_start_pt, l.glyphs[5].arc_end_pt), (0.0, 10.0));

    // A collinear cubic with coincident control points: x(t) = 30 (3t^2 - 2t^3), length 30, cusp
    // derivative at both ends. Arc position s must sit at x = s exactly.
    let cubic = vec![BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::CubicTo([0.0, 0.0], [30.0, 0.0], [30.0, 0.0])];
    let c = lay(&cubic, &[run(5, 0.0)], &[6.0; 5], &PathTextOptions::default());
    assert!((c.path_length_pt - 30.0).abs() < EPS);
    for (i, g) in c.glyphs.iter().enumerate() {
        close(g.transform, [1.0, 0.0, 0.0, -1.0, 6.0 * i as f64, 0.0]);
    }
    // Handle 0.5 is the curve parameter: x(0.5) = 15.
    let h = lay(&cubic, &[run(1, 0.0)], &[6.0], &PathTextOptions { start_t: Some(0.5), ..Default::default() });
    close(h.glyphs[0].transform, [1.0, 0.0, 0.0, -1.0, 15.0, 0.0]);
}

#[test]
fn ai_path_text_refuses_hostile_input_before_allocation() {
    let lim = PathTextLimits::default();
    let o = PathTextOptions::default();
    let r1 = [run(1, 0.0)];
    let err = |p: &[BaselineCommand], runs: &[PathTextRun], adv: &[f64], o: &PathTextOptions, lim: &PathTextLimits| layout_path_text(p, runs, adv, o, lim).unwrap_err();
    // Degenerate paths.
    assert_eq!(err(&[BaselineCommand::MoveTo([5.0, 5.0]), BaselineCommand::LineTo([5.0, 5.0])], &r1, &[1.0], &o, &lim), PathTextError::DegeneratePath);
    assert_eq!(err(&[BaselineCommand::MoveTo([5.0, 5.0])], &r1, &[1.0], &o, &lim), PathTextError::DegeneratePath);
    assert_eq!(err(&[], &r1, &[1.0], &o, &lim), PathTextError::DegeneratePath);
    assert_eq!(err(&[BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([f64::NAN, 1.0])], &r1, &[1.0], &o, &lim), PathTextError::InvalidInput("path"));
    // One subpath only.
    let two = [BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([1.0, 0.0]), BaselineCommand::MoveTo([2.0, 0.0]), BaselineCommand::LineTo([3.0, 0.0])];
    assert_eq!(err(&two, &r1, &[1.0], &o, &lim), PathTextError::MultipleSubpaths);
    let reopened = [BaselineCommand::MoveTo([0.0, 0.0]), BaselineCommand::LineTo([1.0, 0.0]), BaselineCommand::LineTo([1.0, 1.0]), BaselineCommand::Close, BaselineCommand::LineTo([3.0, 0.0])];
    assert_eq!(err(&reopened, &r1, &[1.0], &o, &lim), PathTextError::MultipleSubpaths);
    let line = l_path();
    // Glyph, run and command caps refuse before allocating; overflowing run counts too.
    let small = PathTextLimits { max_glyphs: 1000, max_runs: 2, max_commands: 2 };
    assert_eq!(err(&line, &[run(1001, 0.0)], &vec![1.0; 1001], &o, &small), PathTextError::OverLimit("max_glyphs"));
    assert_eq!(err(&line, &[run(0, 0.0); 3], &[], &o, &small), PathTextError::OverLimit("max_runs"));
    assert_eq!(err(&line, &[run(1, 0.0)], &[1.0], &o, &small), PathTextError::OverLimit("max_commands"));
    assert_eq!(err(&line, &[run(usize::MAX, 0.0), run(1, 0.0)], &[1.0], &o, &lim), PathTextError::OverLimit("max_glyphs"));
    assert_eq!(err(&line, &[run(2, 0.0)], &[1.0], &o, &lim), PathTextError::InvalidInput("run_glyph_counts"));
    assert_eq!(err(&line, &r1, &[-1.0], &o, &lim), PathTextError::InvalidInput("advance"));
    assert_eq!(err(&line, &[run(1, f64::INFINITY)], &[1.0], &o, &lim), PathTextError::InvalidInput("run_metrics"));
    // Handles and the mixed flip state.
    assert_eq!(err(&line, &r1, &[1.0], &PathTextOptions { flipped: PathFlip::Undefined, ..o }, &lim), PathTextError::UndefinedFlip);
    assert_eq!(err(&line, &r1, &[1.0], &PathTextOptions { start_t: Some(1.5), end_t: Some(0.5), ..o }, &lim), PathTextError::InvalidInput("end_t_before_start_t"));
    assert_eq!(err(&line, &r1, &[1.0], &PathTextOptions { start_t: Some(2.5), ..o }, &lim), PathTextError::InvalidInput("handle_t"));
    assert_eq!(err(&line, &r1, &[1.0], &PathTextOptions { spacing_pt: f64::NAN, ..o }, &lim), PathTextError::InvalidInput("options"));
    // A huge glyph count inside the caps: 200 000 glyphs of 0.5 pt on 200 pt -> 400 placed,
    // the rest reported overset (never silently dropped).
    let many = vec![0.5; 200_000];
    let l = lay(&line, &[run(200_000, 0.0)], &many, &o);
    assert_eq!((l.glyphs.len(), l.overset_from, l.overset_count), (400, Some(400), 199_600));
}
