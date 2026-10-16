//! Lane SMB-ID-PREPRESS slice 4: Folio grid records, baseline and document grids, guides and
//! snapping, baseline-grid alignment in the threaded reflow, measurement units (STU-LAY-050..053,
//! STU-LAY-101, STU-LAY-114, STU-LAY-115). Pages are 200 x 200 pt with 10 pt margins (support_layout);
//! every value is hand-computed. InDesign parity UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

const GREY: StudioRgba = StudioRgba { r: 0.5, g: 0.5, b: 0.5, a: 1.0 };

fn doc_grid(relative: StudioBaselineRelative) -> StudioDocumentGrid {
    StudioDocumentGrid {
        baseline_start: len(20.0),
        baseline_division: len(15.0),
        baseline_view_threshold: 75.0,
        horizontal_gridline_division: len(40.0),
        vertical_gridline_division: len(50.0),
        horizontal_grid_subdivision: 1,
        vertical_grid_subdivision: 2,
        baseline_grid_shown: true,
        document_grid_shown: true,
        document_grid_snap_to: true,
        grids_in_back: true,
        grid_colour: GREY,
        baseline_colour: GREY,
        baseline_grid_relative_option: relative,
    }
}

fn grids(relative: StudioBaselineRelative) -> StudioLayoutGrids {
    let mut g = StudioLayoutGrids::new(id("SDOC", 1));
    g.document_grid = Some(doc_grid(relative));
    g
}

fn one_frame(w: f64) -> StudioDocument {
    document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, w, 100.0), None, None)], [PageSide::RightHand, PageSide::LeftHand])
}

fn guide(key: &str, page: Option<&str>, orientation: StudioGuideOrientation, at: f64) -> StudioRulerGuide {
    StudioRulerGuide {
        guide_id: key.into(),
        page_spread_id: id("SPGS", 1),
        page_id: page.map(str::to_owned),
        orientation,
        location: len(at),
        guide_colour: GREY,
        guide_type: StudioGuideType::Ruler,
        guide_view_threshold: 5.0,
        fit_to_page: false,
        locked: true,
        overridden: false,
        allow_overrides: false,
    }
}

#[test]
fn grid_records_validate_and_baseline_lines() {
    let doc = one_frame(120.0);
    let tok = CancellationToken::default();
    let g = grids(StudioBaselineRelative::TopOfPage);
    validate_layout_grids(&g, &doc, &tok).unwrap();
    // 20, 35, ..., 200: thirteen lines.
    let lines = baseline_grid_lines(&doc, &g, "page-1").unwrap();
    assert_eq!((lines.len(), lines[0], lines[1], lines[12]), (13, 20.0, 35.0, 200.0));
    // Relative to the top margin (10): 30 .. 195.
    let m = baseline_grid_lines(&doc, &grids(StudioBaselineRelative::TopMargin), "page-1").unwrap();
    assert_eq!((m.len(), m[0], m[11]), (12, 30.0, 195.0));
    // Captured bounds: view threshold 5..=4000, snap zone 1..=36, positive divisions, live references.
    let mut bad = g.clone();
    bad.document_grid.as_mut().unwrap().baseline_view_threshold = 4.0;
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err());
    let mut bad = g.clone();
    bad.document_grid.as_mut().unwrap().baseline_division = len(0.0);
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err());
    assert_eq!(baseline_grid_lines(&doc, &bad, "page-1").unwrap_err(), LayoutError::InvalidGeometry);
    let mut bad = g.clone();
    bad.guide_preferences = Some(StudioGuidePreferences {
        guides_shown: true,
        guides_locked: false,
        guides_snap_to: true,
        guides_in_back: false,
        ruler_guides_colour: GREY,
        ruler_guides_view_threshold: 100.0,
        guide_snap_to_zone: 37,
    });
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err());
    let mut bad = g.clone();
    let mut gd = guide("g1", Some("page-9"), StudioGuideOrientation::Vertical, 5.0);
    bad.guides.push(gd.clone());
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err());
    gd.page_id = Some("page-1".into());
    bad.guides = vec![gd.clone(), gd];
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err(), "duplicate guide key");
    let mut bad = g.clone();
    bad.paragraph_alignments.push(StudioGridAlignment { story_id: id("STXT", 7), paragraph_start: 0, align_to_grid: StudioAlignToGrid::AllLines });
    assert!(validate_layout_grids(&bad, &doc, &tok).is_err(), "story no frame shows");
    // Hostile: a 1e-6 pt division over a 200 pt page is refused, not allocated.
    let mut tiny = g.clone();
    tiny.document_grid.as_mut().unwrap().baseline_division = len(1e-6);
    assert_eq!(baseline_grid_lines(&doc, &tiny, "page-1").unwrap_err(), LayoutError::BudgetExceeded);
}

#[test]
fn frame_and_document_grid_lines() {
    let doc = one_frame(120.0);
    let mut g = grids(StudioBaselineRelative::TopOfPage);
    // Document grid seen from a frame at y = 10: origin 20 - 10 = 10, step 15: 10 .. 100.
    assert_eq!(frame_baseline_lines(&doc, &g, &id("SLYR", 1)).unwrap(), vec![10.0, 25.0, 40.0, 55.0, 70.0, 85.0, 100.0]);
    g.frame_baseline_grids.push(StudioFrameBaselineGrid {
        layer_id: id("SLYR", 1),
        use_custom_baseline_frame_grid: true,
        starting_offset_for_baseline_frame_grid: len(5.0),
        baseline_frame_grid_relative_option: StudioFrameBaselineRelative::TopOfFrame,
        baseline_frame_grid_increment: len(12.0),
        baseline_frame_grid_colour: GREY,
    });
    validate_layout_grids(&g, &doc, &CancellationToken::default()).unwrap();
    assert_eq!(frame_baseline_lines(&doc, &g, &id("SLYR", 1)).unwrap(), vec![5.0, 17.0, 29.0, 41.0, 53.0, 65.0, 77.0, 89.0]);
    // Custom grid relative to the page top: origin -10 + 5 = -5, lines inside the frame from 7.
    g.frame_baseline_grids[0].baseline_frame_grid_relative_option = StudioFrameBaselineRelative::TopOfPage;
    assert_eq!(frame_baseline_lines(&doc, &g, &id("SLYR", 1)).unwrap()[..2], [7.0, 19.0]);
    // Document grid: vertical 50 / 2 = 25 pt steps over 200 (9 lines, majors at 0, 50, ...).
    let (v, h) = document_grid_lines(&doc, &g, "page-1").unwrap();
    assert_eq!(v.len(), 9);
    assert_eq!(v.iter().filter(|l| l.major).map(|l| l.position_pt).collect::<Vec<_>>(), vec![0.0, 50.0, 100.0, 150.0, 200.0]);
    assert_eq!((v[1].position_pt, v[1].major), (25.0, false));
    assert_eq!(h.iter().map(|l| l.position_pt).collect::<Vec<_>>(), vec![0.0, 40.0, 80.0, 120.0, 160.0, 200.0]);
    assert!(h.iter().all(|l| l.major));
}

#[test]
fn snapping_to_guides_and_document_grid() {
    let doc = one_frame(120.0);
    let mut g = grids(StudioBaselineRelative::TopOfPage);
    g.guide_preferences = Some(StudioGuidePreferences {
        guides_shown: true,
        guides_locked: true,
        guides_snap_to: true,
        guides_in_back: false,
        ruler_guides_colour: GREY,
        ruler_guides_view_threshold: 100.0,
        guide_snap_to_zone: 4,
    });
    g.guides = vec![
        guide("v33", Some("page-1"), StudioGuideOrientation::Vertical, 33.0),
        guide("v50", Some("page-1"), StudioGuideOrientation::Vertical, 50.0),
        // A spread guide at spread x 230 is x 30 on page 2 (page 2 starts at spread x 200).
        guide("spread", None, StudioGuideOrientation::Vertical, 230.0),
    ];
    validate_layout_grids(&g, &doc, &CancellationToken::default()).unwrap();
    let snap = |page: &str, x: f64, y: f64, zone: f64| snap_point(&doc, &g, page, x, y, zone).unwrap();
    let src = |h: Option<SnapHit>| h.map(|h| (h.value_pt, h.source));
    // Guide 33 at distance 3; grid line 25 at 5 is outside the zone.
    assert_eq!(src(snap("page-1", 30.0, 100.0, 4.0).0), Some((33.0, SnapSource::Guide("v33".into()))));
    // Grid line 25 at distance 1 beats guide 33.
    assert_eq!(src(snap("page-1", 26.0, 100.0, 4.0).0), Some((25.0, SnapSource::DocumentGrid)));
    // A tie between guide 50 and grid line 50 goes to the guide.
    assert_eq!(src(snap("page-1", 52.0, 100.0, 4.0).0), Some((50.0, SnapSource::Guide("v50".into()))));
    // Page 2: the spread guide maps to x 30; page-1 guides do not apply.
    assert_eq!(src(snap("page-2", 28.0, 100.0, 2.5).0), Some((30.0, SnapSource::Guide("spread".into()))));
    // y snaps to the horizontal grid (every 40); nothing within the zone -> None.
    let (_, y) = snap("page-1", 0.0, 41.0, 2.0);
    assert_eq!(src(y), Some((40.0, SnapSource::DocumentGrid)));
    assert_eq!(snap("page-1", 12.5, 20.0, 1.0), (None, None));
    // Snapping off: no targets.
    g.document_grid.as_mut().unwrap().document_grid_snap_to = false;
    g.guide_preferences.as_mut().unwrap().guides_snap_to = false;
    assert_eq!(snap("page-1", 30.0, 41.0, 4.0), (None, None));
    assert_eq!(snap_point(&doc, &g, "page-1", f64::NAN, 0.0, 1.0).unwrap_err(), LayoutError::InvalidGeometry);
}

#[test]
fn reflow_aligns_paragraphs_to_the_baseline_grid() {
    // Width 30: "aaaa aaaa aaaa" breaks into three lines; then "bb", then "cc". Natural baselines
    // 10, 22, 34, 46, 58. The frame's grid (document grid seen from y = 10): 10, 25, 40, 55, 70, ...
    // all_lines: 10, 22 -> 25, 37 -> 40; none: 52; first_line_only: 64 -> 70.
    let text = "aaaa aaaa aaaa\nbb\ncc";
    let (st, paras) = story(1, text, &[(0, 14), (15, 17), (18, 20)]);
    let doc = one_frame(30.0);
    let mut g = grids(StudioBaselineRelative::TopOfPage);
    let align = |start: u64, a: StudioAlignToGrid| StudioGridAlignment { story_id: id("STXT", 1), paragraph_start: start, align_to_grid: a };
    g.paragraph_alignments = vec![align(0, StudioAlignToGrid::AllLines), align(18, StudioAlignToGrid::FirstLineOnly)];
    validate_layout_grids(&g, &doc, &CancellationToken::default()).unwrap();
    let req = ReflowRequest {
        document: &doc,
        story: &st,
        resolved_text: None,
        paragraphs: &paras,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
    };
    let tok = CancellationToken::default();
    let run_g = |g: &StudioLayoutGrids| reflow_grid(&req, &TypesetOptions::default(), &records(), g, &tok);
    let p = run_g(&g).unwrap();
    let b: Vec<f64> = p.frames[0].lines.iter().map(|l| l.baseline_y_pt).collect();
    assert_eq!(b, vec![10.0, 25.0, 40.0, 52.0, 70.0]);
    // Without alignments the reflow is the plain one.
    let plain = run_g(&grids(StudioBaselineRelative::TopOfPage)).unwrap();
    assert_eq!(plain, run(&doc, &st, &paras, &records()).unwrap());
    // Grid step 30 (10, 40, 70, 100): 10, 40, 70; "bb" follows at 82; "cc" would snap 94 -> 100 and
    // end at 102 > 100, so it is overset (frame height 100).
    let mut tall = g.clone();
    tall.document_grid.as_mut().unwrap().baseline_division = len(30.0);
    let o = run_g(&tall).unwrap();
    assert_eq!(o.frames[0].lines.iter().map(|l| l.baseline_y_pt).collect::<Vec<_>>(), vec![10.0, 40.0, 70.0, 82.0]);
    assert_eq!(o.overset.map(|s| s.start), Some(18));
    // Records of another document and alignments off any paragraph start are refused.
    let mut other = g.clone();
    other.document_id = id("SDOC", 2);
    assert_eq!(run_g(&other).unwrap_err(), LayoutError::InvalidInput);
    let mut off = g.clone();
    off.paragraph_alignments.push(align(3, StudioAlignToGrid::AllLines));
    assert_eq!(run_g(&off).unwrap_err(), LayoutError::InvalidInput);
}

#[test]
fn measurement_units_and_input_suffixes() {
    let ctx = MeasureContext { percent_of_pt: 200.0, pixels_per_inch: 96.0, viewport_pt: [500.0, 300.0] };
    let pt = |s: &str| parse_measure(s, MeasureSystem::Picas, &ctx).unwrap();
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(pt("12pt"), 12.0) && close(pt("1in"), 72.0) && close(pt("25.4mm"), 72.0) && close(pt("2.54 CM"), 72.0));
    assert!(close(pt("50%"), 100.0) && close(pt("96px"), 72.0) && close(pt("10vw"), 50.0) && close(pt("10vh"), 30.0));
    assert!(close(pt("10vmin"), 30.0) && close(pt("10vmax"), 50.0) && close(pt("3"), 36.0) && close(pt("-1.5pt"), -1.5));
    assert_eq!(parse_measure("12p6", MeasureSystem::Points, &ctx).unwrap_err().code(), "unit_suffix_unknown");
    assert_eq!(parse_measure("3em", MeasureSystem::Points, &ctx).unwrap_err().code(), "unit_suffix_unknown");
    assert_eq!(parse_measure("pt", MeasureSystem::Points, &ctx).unwrap_err().code(), "measure_not_a_number");
    assert_eq!(parse_measure(&"9".repeat(400), MeasureSystem::Points, &ctx).unwrap_err().code(), "measure_not_a_number");
    // Ruler systems: 72 pt/in, pica 12, agate 1/14 in, Q and Ha 0.25 mm, American point 1/72.27 in.
    assert!(close(to_points(MeasureSystem::Agates), 72.0 / 14.0));
    assert!(close(to_points(MeasureSystem::Q), 0.25 * 72.0 / 25.4) && close(to_points(MeasureSystem::Ha), to_points(MeasureSystem::Q)));
    assert!(close(to_points(MeasureSystem::AmericanPoints) * 72.27, 72.0));
    assert!(close(to_points(MeasureSystem::Custom(7.5)), 7.5) && close(to_points(MeasureSystem::InchesDecimal), 72.0));
}
