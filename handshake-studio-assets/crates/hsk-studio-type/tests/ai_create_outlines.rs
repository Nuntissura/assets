//! Lane SMB-AI-AREA-TYPE-1 slice 4: Create Outlines for point, area and path text (STU-VEC-038).
//! Real glyph outlines come from the hash-pinned checked-in Inter-Regular fixture (V4-03); every
//! expected command is the glyph outline scaled by size / upem and mapped by the layout transform.
//! Illustrator contour order and winding parity: NOT_PROVEN (no oracle case; D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_type::*;
use sha2::{Digest, Sha256};

fn inter() -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts/Inter-Regular.ttf")).expect("fixture font")
}

fn map(m: [f64; 6], k: f64, p: [f32; 2]) -> [f64; 2] {
    let (x, y) = (f64::from(p[0]) * k, f64::from(p[1]) * k);
    [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
}

/// The expected compound path of one glyph (contours closed).
fn expected(o: &GlyphOutline, m: [f64; 6], size: f64) -> Vec<BaselineCommand> {
    let k = size / f64::from(o.units_per_em());
    let mut out = Vec::new();
    let mut open = false;
    for c in o.commands() {
        match *c {
            OutlineCommand::MoveTo(p) => {
                if open {
                    out.push(BaselineCommand::Close);
                }
                out.push(BaselineCommand::MoveTo(map(m, k, p)));
                open = true;
            }
            OutlineCommand::LineTo(p) => out.push(BaselineCommand::LineTo(map(m, k, p))),
            OutlineCommand::QuadTo(a, p) => out.push(BaselineCommand::QuadTo(map(m, k, a), map(m, k, p))),
            OutlineCommand::CurveTo(a, b, p) => out.push(BaselineCommand::CubicTo(map(m, k, a), map(m, k, b), map(m, k, p))),
            OutlineCommand::Close => {
                if open {
                    out.push(BaselineCommand::Close);
                    open = false;
                }
            }
        }
    }
    if open {
        out.push(BaselineCommand::Close);
    }
    out
}

#[test]
fn create_outlines_of_point_and_area_text_matches_glyph_outlines() {
    println!("NOT_PROVEN: Illustrator Create Outlines contour order and winding (no oracle case, D5)");
    let bytes = inter();
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    let resource = FontResource { identity: "Inter", location: "Inter", content_hash: hash, face_index: 0, resource_revision: 1, grant_revision: 1 };
    let font = FontBytes { bytes: &bytes, resource };
    let cancel = CancellationToken::default();
    // The first six glyph ids with ink, then an empty (space) outline.
    let probe: Vec<u32> = (1..64).collect();
    let all = glyph_outlines(&font, &probe, &OutlineLimits::default(), &cancel).expect("outlines");
    let mut outlines: Vec<GlyphOutline> = all.into_iter().filter(|o| !o.commands().is_empty()).take(6).collect();
    assert_eq!(outlines.len(), 6);
    let upem = outlines[0].units_per_em();
    outlines.insert(3, GlyphOutline::synthetic(upem, Vec::new()));
    let size = 24.0;
    // Glyphs: 3 letters, a space, 3 letters; run metrics for 24 pt.
    let glyphs: Vec<ComposeGlyph> = (0..7).map(|i| ComposeGlyph { advance_pt: 14.0, brk: if i == 3 { GlyphBreak::Space } else { GlyphBreak::None } }).collect();
    let runs = [ComposeRun { glyph_count: 7, ascent_pt: 23.0, descent_pt: 6.0, leading_pt: 29.0, cap_height_pt: 17.5, x_height_pt: 13.0 }];
    let sources: Vec<OutlineSource<'_>> = outlines.iter().enumerate().map(|(i, o)| OutlineSource { outline: o, font_size_pt: size, paint: i % 2 }).collect();
    let point = layout_point_text([30.0, 50.0], &runs, &glyphs, TextAlign::Left, &AreaTextLimits::default()).unwrap();
    // A 60 pt wide area: the two words wrap onto two lines.
    let container = vec![
        BaselineCommand::MoveTo([0.0, 0.0]),
        BaselineCommand::LineTo([60.0, 0.0]),
        BaselineCommand::LineTo([60.0, 200.0]),
        BaselineCommand::LineTo([0.0, 200.0]),
        BaselineCommand::Close,
    ];
    let area = layout_area_text(&container, &runs, &glyphs, &AreaTextOptions::default(), &AreaTextLimits::default()).unwrap();
    assert_eq!(area.lines.len(), 2);
    for layout in [&point as &dyn ComposedGlyphs, &area] {
        let out = create_outlines(layout, &sources, &CreateOutlinesLimits::default()).unwrap();
        // The space has no ink: six outlined glyphs, in glyph order, paints carried.
        assert_eq!(out.iter().map(|g| g.index).collect::<Vec<_>>(), vec![0, 1, 2, 4, 5, 6]);
        let placed = layout.placed();
        for g in &out {
            assert_eq!(g.paint, g.index % 2);
            let (_, _, m) = placed.iter().find(|(i, _, _)| *i == g.index).copied().unwrap();
            assert_eq!(g.commands, expected(&outlines[g.index], m, size), "glyph {}", g.index);
            // One closed subpath per contour.
            let moves = g.commands.iter().filter(|c| matches!(c, BaselineCommand::MoveTo(_))).count();
            let closes = g.commands.iter().filter(|c| matches!(c, BaselineCommand::Close)).count();
            assert_eq!((moves, closes), (g.contours, g.contours));
            assert!(matches!(g.commands.last(), Some(BaselineCommand::Close)));
        }
    }
    // Path text uses the same route: outlines equal place_outline's (closed per contour).
    let path = vec![BaselineCommand::MoveTo([0.0, 100.0]), BaselineCommand::LineTo([300.0, 100.0])];
    let prun = PathTextRun { glyph_count: 7, ascent_pt: 23.0, descent_pt: 6.0, ..Default::default() };
    let pl = layout_path_text(&path, &[prun], &[14.0; 7], &PathTextOptions::default(), &PathTextLimits::default()).unwrap();
    let out = create_outlines(&pl, &sources, &CreateOutlinesLimits::default()).unwrap();
    for g in &out {
        let mut via_path = place_outline(&pl.glyphs[g.index], &outlines[g.index], size).unwrap();
        if !matches!(via_path.last(), Some(BaselineCommand::Close)) {
            via_path.push(BaselineCommand::Close);
        }
        let ours: Vec<_> = g.commands.iter().filter(|c| !matches!(c, BaselineCommand::Close)).collect();
        let theirs: Vec<_> = via_path.iter().filter(|c| !matches!(c, BaselineCommand::Close)).collect();
        assert_eq!(ours, theirs, "path glyph {}", g.index);
    }
    // Bounds and bad input are refused.
    assert_eq!(create_outlines(&point, &sources, &CreateOutlinesLimits { max_glyphs: 3, ..Default::default() }), Err(CreateOutlinesError::OverLimit("max_glyphs")));
    assert_eq!(create_outlines(&point, &sources, &CreateOutlinesLimits { max_commands: 10, ..Default::default() }), Err(CreateOutlinesError::OverLimit("max_commands")));
    assert_eq!(create_outlines(&point, &sources[..2], &CreateOutlinesLimits::default()), Err(CreateOutlinesError::InvalidInput("glyph_index")));
    let bad: Vec<OutlineSource<'_>> = sources.iter().map(|s| OutlineSource { font_size_pt: f64::NAN, ..*s }).collect();
    assert_eq!(create_outlines(&point, &bad, &CreateOutlinesLimits::default()), Err(CreateOutlinesError::InvalidInput("font_size")));
}
