//! Lane SMB-AI-AREA-TYPE-4: area text in slanted and concave closed containers. Reproduces
//! SMB-VIEW-POLISH-1 run 2 (one corner of the 10,10 - 40,30 area frame dragged to 90,60 in the
//! viewer placed 0 of 32 glyphs) and pins the band rule on a parallelogram, a trapezoid, a concave
//! L-shape and a triangle with a too-narrow tip. Expected geometry is closed-form (straight edges:
//! the narrowest scanline of a band is one of its two edges). Illustrator parity of the band rule
//! and of where a word wider than every band breaks is UNVERIFIED (D5).
use hsk_studio_type::*;

const EPS: f64 = 1e-6;

fn poly(points: &[[f64; 2]]) -> Vec<BaselineCommand> {
    let mut c = vec![BaselineCommand::MoveTo(points[0])];
    c.extend(points[1..].iter().map(|p| BaselineCommand::LineTo(*p)));
    c.push(BaselineCommand::Close);
    c
}
fn lay(c: &[BaselineCommand], runs: &[ComposeRun], g: &[ComposeGlyph]) -> AreaTextLayout {
    layout_area_text(c, runs, g, &AreaTextOptions::default(), &AreaTextLimits::default()).expect("layout")
}
fn counts(l: &AreaTextLayout) -> Vec<usize> {
    l.lines.iter().map(|x| x.glyph_count).collect()
}
/// Every glyph is placed or overset, placed glyphs are the leading prefix in order.
fn accounted(l: &AreaTextLayout, total: usize) {
    assert_eq!(l.glyphs.len() + l.overset_count, total, "placed + overset = every glyph");
    assert!(l.glyphs.iter().enumerate().all(|(i, g)| g.index == i), "placed glyphs are the leading prefix");
    assert_eq!(l.overset_from, (l.overset_count > 0).then_some(l.glyphs.len()));
}

// ---- the viewer story -------------------------------------------------------------------------------

/// "Overset area text flows on and on" as the viewer shapes it (Ubuntu-Light, the viewer's
/// DEFAULT_FONT, 12 pt; `fl` is one ligature glyph, 32 glyphs). Advances are the font's hmtx
/// advances x 12 / 1000 (measured from the font file; harfrust kerning, if any, is not applied:
/// the reproduction does not depend on tenths of a point). Ascent / descent are the viewer's
/// shaped_story rule (outline top / bottom of the story's glyphs), leading 12 x 1.2.
fn viewer_story() -> (ComposeRun, Vec<ComposeGlyph>) {
    let words: [&[f64]; 7] = [
        &[9.288, 5.76, 6.576, 4.488, 5.136, 6.576, 4.644], // Overset 42.468
        &[6.108, 4.488, 6.576, 6.108],                     // area 23.28
        &[4.644, 6.576, 5.988, 4.644],                     // text 21.852
        &[7.572, 6.984, 9.3, 5.136],                       // flows (fl ligature) 28.992
        &[6.984, 6.828],                                   // on
        &[6.108, 6.828, 6.996],                            // and
        &[6.984, 6.828],                                   // on
    ];
    let mut g = Vec::new();
    for (k, w) in words.iter().enumerate() {
        g.extend(w.iter().map(|a| ComposeGlyph { advance_pt: *a, brk: GlyphBreak::None }));
        if k + 1 < words.len() {
            g.push(ComposeGlyph { advance_pt: 2.736, brk: GlyphBreak::Space });
        }
    }
    let run = ComposeRun { glyph_count: g.len(), ascent_pt: 9.312, descent_pt: 0.18, leading_pt: 14.4, cap_height_pt: 9.312, x_height_pt: 9.312 };
    (run, g)
}

/// The viewer test's frame: `draw_shape(Rectangle, [10, 10], [40, 30])`, then ONE direct-select
/// drag of the bottom-right anchor from (40, 30) to (90, 60) (`PathEdit::MoveAnchor`): a convex
/// quadrilateral, no self-intersection, signed area 1150, bbox 10,10 - 90,60.
#[test]
fn one_corner_drag_quadrilateral_places_the_story() {
    let (run, g) = viewer_story();
    assert_eq!(g.len(), 32);
    let rect = lay(&poly(&[[10.0, 10.0], [40.0, 10.0], [40.0, 30.0], [10.0, 30.0]]), &[run], &g);
    // The 30 pt wide rectangle breaks "Overset" (42.468) between glyphs: O v e r (26.112).
    assert_eq!((counts(&rect), rect.overset_count), (vec![4], 28));
    let quad = poly(&[[10.0, 10.0], [40.0, 10.0], [90.0, 60.0], [10.0, 30.0]]);
    let l = lay(&quad, &[run], &g);
    // Hand-computed bands (first baseline 10 + 9.312, leading 14.4; band [b - 9.312, b + 0.18]):
    // line 1 [10, 19.492]: right edge x = 40 + (y - 10), left edge x = 10, narrowest at the top
    // scanline -> [10, 40] (30 pt); line 2 [24.4, 33.892]: left = the bottom edge
    // 10 + (y - 30) 8 / 3 at 33.892 = 20.379, right = 40 + 14.4 = 54.4 (34.02 pt); line 3
    // [38.8, 48.292]: [58.779, 68.8] (10.02 pt); line 4 ends at 62.692 > 60. No band holds
    // "Overset", so it breaks in line 1 as in the rectangle; at d208bc0e every band was skipped
    // (narrower than the 80 pt bbox) and 0 of 32 were placed.
    assert!(!l.glyphs.is_empty(), "the slanted frame placed nothing: {:?}", l.lines);
    let first = l.lines[0];
    assert!((first.baseline_y - 19.312).abs() < EPS && (first.left - 10.0).abs() < EPS && (first.right - 40.0).abs() < EPS, "{first:?}");
    let second = l.lines[1];
    assert!((second.left - (10.0 + 3.892 * 80.0 / 30.0)).abs() < EPS && (second.right - 54.4).abs() < EPS, "{second:?}");
    // Line 1 "Over"; line 2 "set " (16.356 + hanging space; "set area" is 42.372 > 34.02);
    // line 3 holds no whole "area" and no later band exists: "a" (6.108; "ar" 10.596 > 10.02).
    assert_eq!(counts(&l), vec![4, 4, 1]);
    assert_eq!((l.overset_from, l.overset_count), (Some(9), 23));
    accounted(&l, g.len());
    // The same drag inward to (20, 20) makes a concave dart: line 1 [10, 21.016] holds "O".
    let dart = lay(&poly(&[[10.0, 10.0], [40.0, 10.0], [20.0, 20.0], [10.0, 30.0]]), &[run], &g);
    assert_eq!((counts(&dart), dart.overset_count), (vec![1], 31));
    accounted(&dart, g.len());
}

// ---- synthetic stories ---------------------------------------------------------------------------

fn run(n: usize) -> ComposeRun {
    ComposeRun { glyph_count: n, ascent_pt: 8.0, descent_pt: 2.0, leading_pt: 12.0, cap_height_pt: 7.0, x_height_pt: 5.0 }
}
/// Words of the given glyph counts (5 pt each) separated by 5 pt spaces.
fn words(letters: &[usize]) -> Vec<ComposeGlyph> {
    let mut g = Vec::new();
    for (k, n) in letters.iter().enumerate() {
        g.extend(std::iter::repeat_n(ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::None }, *n));
        if k + 1 < letters.len() {
            g.push(ComposeGlyph { advance_pt: 5.0, brk: GlyphBreak::Space });
        }
    }
    g
}

#[test]
fn parallelogram_sets_every_line_and_breaks_a_word_wider_than_every_band() {
    // Edges x = y / 2 and x = 100 + y / 2, y 0..80. Band [t, b] (b = baseline + 2): left = b / 2,
    // right = 100 + t / 2, width 95 on every full band; baselines 8, 20, ..., 68 (6 lines).
    let c = poly(&[[0.0, 0.0], [100.0, 0.0], [140.0, 80.0], [40.0, 80.0]]);
    let g = words(&[5; 15]);
    let l = lay(&c, &[run(g.len())], &g);
    assert_eq!(counts(&l), vec![18, 18, 18, 18, 17], "three 25 pt words (85 pt) per 95 pt line");
    for (k, line) in l.lines.iter().enumerate() {
        let b = 8.0 + 12.0 * k as f64;
        let (t, bot) = ((b - 8.0_f64).max(0.0), b + 2.0);
        assert!((line.baseline_y - b).abs() < EPS && (line.left - bot / 2.0).abs() < EPS && (line.right - (100.0 + t / 2.0)).abs() < EPS, "{line:?}");
        assert!((line.x - line.left).abs() < EPS);
    }
    accounted(&l, g.len());
    // A 150 pt word: wider than every band (95 < the 140 pt bbox). It breaks between glyphs, 19
    // glyphs (95 pt) per band, instead of skipping every band and placing nothing.
    let long = words(&[30, 2]);
    let w = lay(&c, &[run(long.len())], &long);
    assert_eq!(counts(&w), vec![19, 14]);
    assert_eq!(w.overset_from, None);
    accounted(&w, long.len());
}

#[test]
fn trapezoid_widens_downward_and_a_word_skips_to_the_first_band_that_holds_it() {
    // Edges x = 20 - y / 3 and x = 80 + y / 3, y 0..60: narrowest at a band's top t, width
    // 60 + 2 t / 3: 60, 68, 76, 84, 92 at baselines 8, 20, 32, 44, 56.
    let c = poly(&[[20.0, 0.0], [80.0, 0.0], [100.0, 60.0], [0.0, 60.0]]);
    let g = words(&[5; 11]);
    let l = lay(&c, &[run(g.len())], &g);
    assert_eq!(counts(&l), vec![12, 12, 12, 12, 17], "2, 2, 2, 2, 3 words");
    for (line, t) in l.lines.iter().zip([0.0, 12.0, 24.0, 36.0, 48.0]) {
        assert!((line.left - (20.0 - t / 3.0)).abs() < EPS && (line.right - (80.0 + t / 3.0)).abs() < EPS, "{line:?}");
    }
    accounted(&l, g.len());
    // A 70 pt word does not fit 60 or 68 but a later band holds it: those bands are skipped.
    let w = words(&[14, 5]);
    let s = lay(&c, &[run(w.len())], &w);
    assert_eq!(s.lines.iter().map(|x| x.baseline_y).collect::<Vec<_>>(), vec![32.0, 44.0]);
    assert_eq!((counts(&s), s.overset_from), (vec![15, 5], None));
}

#[test]
fn concave_l_shape_uses_the_arm_then_the_foot() {
    // L: arm 0..40 wide for y 0..40, foot 0..100 wide for y 40..80 (concave corner at 40, 40).
    let c = poly(&[[0.0, 0.0], [40.0, 0.0], [40.0, 40.0], [100.0, 40.0], [100.0, 80.0], [0.0, 80.0]]);
    let g = words(&[5; 10]);
    let l = lay(&c, &[run(g.len())], &g);
    // Bands at 8, 20, 32, 44 ([36, 46] crosses y = 40: the arm's 40 pt bounds it) hold one word;
    // 56 and 68 lie in the foot (100 pt): three words.
    assert_eq!(l.lines.iter().map(|x| (x.baseline_y, x.right - x.left)).collect::<Vec<_>>(), vec![(8.0, 40.0), (20.0, 40.0), (32.0, 40.0), (44.0, 40.0), (56.0, 100.0), (68.0, 100.0)]);
    assert_eq!(counts(&l), vec![6, 6, 6, 6, 18, 17]);
    accounted(&l, g.len());
    // A 60 pt word skips the arm to the foot.
    let w = words(&[12]);
    let s = lay(&c, &[run(w.len())], &w);
    assert_eq!((s.lines.iter().map(|x| x.baseline_y).collect::<Vec<_>>(), s.overset_from), (vec![56.0], None));
}

#[test]
fn triangle_with_a_narrow_tip_reports_overset_and_drops_nothing() {
    // Tip down: edges x = y / 2 and x = 120 - y / 2, y 0..120; a band is narrowest at its bottom
    // b + 2: width 120 - (b + 2).
    let c = poly(&[[0.0, 0.0], [120.0, 0.0], [60.0, 120.0]]);
    let g = words(&[5; 20]);
    let l = lay(&c, &[run(g.len())], &g);
    // Widths 110, 98, 86, 74, 62, 50, 38, 26 hold 3, 3, 3, 2, 2, 1, 1, 1 words; the band at 104
    // (14 pt) holds no 25 pt word and no later band does (the one at 116 is 2 pt): the word breaks,
    // two glyphs; the 2 pt band holds no glyph; the rest is overset.
    assert_eq!(counts(&l), vec![18, 18, 18, 12, 12, 6, 6, 6, 2]);
    let tip = l.lines.last().unwrap();
    assert!((tip.baseline_y - 104.0).abs() < EPS && (tip.left - 53.0).abs() < EPS && (tip.right - 67.0).abs() < EPS, "{tip:?}");
    assert_eq!((l.overset_from, l.overset_count), (Some(98), g.len() - 98));
    accounted(&l, g.len());
}

#[test]
fn look_ahead_is_bounded_by_the_band_budget() {
    // A tall slanted sliver with a hair leading and a word no band holds: the look-ahead walks
    // bands, and every band it visits counts against max_bands.
    let hair = [ComposeRun { leading_pt: 1e-3, ascent_pt: 0.0, descent_pt: 0.0, ..run(2) }];
    let g = [ComposeGlyph { advance_pt: 50.0, brk: GlyphBreak::None }, ComposeGlyph { advance_pt: 50.0, brk: GlyphBreak::None }];
    let c = poly(&[[0.0, 0.0], [10.0, 0.0], [20.0, 1.0e9], [10.0, 1.0e9]]);
    let lim = AreaTextLimits { max_bands: 1000, ..AreaTextLimits::default() };
    assert_eq!(layout_area_text(&c, &hair, &g, &AreaTextOptions::default(), &lim), Err(AreaTextError::OverLimit("max_bands")));
}
