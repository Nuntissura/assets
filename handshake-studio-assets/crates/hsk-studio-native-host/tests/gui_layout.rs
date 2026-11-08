//! Feature `document`, CPU only: the frame layout of SMB-GUI-FRAME-1 item 5 as one pure function. AC-GEOM-01
//! (partition at the window and scale matrix, down to 1 x 1), AC-LOOK-02 machine half (the 1920 x 1080 rects of
//! mock-raster.html, read from tools/mock_lib.py and mock_raster.py), the fit_table raster rows (AC-GEOM-06 sizes) and
//! AC-CONS-04 (the bars are identical under all ten layouts.json presets).
#![cfg(feature = "document")]

use hsk_studio_native_host::gui::frame::{self, EdgeState, Edges, FrameLayout};
use hsk_studio_native_host::gui::preset;
use kumihimo_ui::checks;
use kumihimo_ui::geom::{Rect, Scale};

fn raster(w: u32, h: u32, s: f32) -> FrameLayout {
    frame::layout((w, h), Scale::new(s).unwrap(), &preset::raster_edges())
}
fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::new(x0, y0, x1, y1)
}
fn close(a: Rect, b: Rect, tol: f32) -> bool {
    (a.x0 - b.x0).abs() <= tol && (a.y0 - b.y0).abs() <= tol && (a.x1 - b.x1).abs() <= tol && (a.y1 - b.y1).abs() <= tol
}

/// AC-GEOM-01 over every band and region at the window and scale matrix, and below the floor down to 1 x 1.
#[test]
fn bands_partition_the_window_at_every_size_and_scale() {
    let windows = [(1920, 1080), (1280, 720), (1024, 640), (2560, 1440), (3840, 2160), (800, 480), (1, 1), (44, 118), (1023, 639)];
    for (w, h) in windows {
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            let l = raster(w, h, s);
            let rects: Vec<Rect> = l.regions().iter().map(|(_, q)| *q).collect();
            let faults = checks::partition(l.window, &rects, l.scale);
            assert!(faults.is_empty(), "{w}x{h}@{s}: {faults:?} {:?}", l.regions());
            for (name, q) in l.regions() {
                assert!(q.w() >= 0.0 && q.h() >= 0.0 && q.x0 >= 0.0 && q.y0 >= 0.0, "{w}x{h}@{s} {name} {q:?}");
            }
            let logical_min = (w as f32 / s) >= 1024.0 && (h as f32 / s) >= 640.0;
            assert_eq!(l.below_min, !logical_min, "{w}x{h}@{s}");
        }
    }
    // The viewport's rulers and canvas partition the viewport.
    let l = raster(1920, 1080, 1.0);
    assert!(checks::partition(l.centre, &[l.ruler_corner, l.ruler_x, l.ruler_y, l.canvas], l.scale).is_empty());
}

/// AC-LOOK-02, machine half: the 1920 x 1080 scale 1 rects equal the mock's (exact).
#[test]
fn rects_at_1920x1080_equal_the_mock() {
    let l = raster(1920, 1080, 1.0);
    let want = [
        ("menu_bar", r(0.0, 0.0, 1920.0, 28.0)),
        ("tab_strip", r(0.0, 28.0, 1920.0, 60.0)),
        ("context_bar", r(0.0, 60.0, 1920.0, 94.0)),
        ("tool_rail", r(0.0, 94.0, 44.0, 1056.0)),
        ("left", r(44.0, 94.0, 76.0, 1028.0)),
        ("centre", r(76.0, 94.0, 1620.0, 1028.0)),
        ("right", r(1620.0, 94.0, 1920.0, 1028.0)),
        ("bottom", r(44.0, 1028.0, 1920.0, 1056.0)),
        ("status", r(0.0, 1056.0, 1920.0, 1080.0)),
    ];
    let got = [l.menu_bar, l.tab_strip, l.context_bar, l.tool_rail, l.left, l.centre, l.right, l.bottom, l.status];
    for ((name, w), g) in want.iter().zip(got) {
        assert!(close(*w, g, 0.0), "{name}: {g:?} want {w:?}");
    }
    assert_eq!(l.groups.iter().map(|g| g.h()).collect::<Vec<_>>(), [280.0, 224.0, 430.0]);
    assert_eq!(l.groups[0].y0, 94.0);
    assert!(close(l.ruler_x, r(96.0, 94.0, 1620.0, 114.0), 0.0) && close(l.ruler_y, r(76.0, 114.0, 96.0, 1028.0), 0.0));
    assert!(close(l.canvas, r(96.0, 114.0, 1620.0, 1028.0), 0.0));
    assert_eq!((l.rail_pitch, l.rail_shown, l.rail_hidden), (32.0, 19, 0));
    assert!(l.applied.is_empty() && !l.below_min);
    assert_eq!((l.left_state, l.right_state, l.bottom_state), (EdgeState::Rail, EdgeState::Expanded, EdgeState::Rail));
}

/// fit_table.results.raster (layouts.json, the two 1 px rows fixed by the remainder rule): every row's sizes.
#[test]
fn fit_table_raster_rows() {
    // (window, scale, left, right, bottom, centre, right_group_heights, rail_pitch, rail_overflow_slots, applied)
    type Row = ((u32, u32), f32, f32, f32, f32, (f32, f32), [f32; 3], f32, usize, &'static [&'static str]);
    let rows: [Row; 6] = [
        ((1280, 720), 1.0, 32.0, 300.0, 28.0, (904.0, 574.0), [172.0, 138.0, 264.0], 28.0, 2, &["FIT-R1", "FIT-R2"]),
        ((1920, 1080), 1.0, 32.0, 300.0, 28.0, (1544.0, 934.0), [280.0, 224.0, 430.0], 32.0, 0, &[]),
        ((2560, 1440), 1.0, 260.0, 300.0, 28.0, (1956.0, 1294.0), [388.0, 311.0, 595.0], 32.0, 0, &["FIT-L1"]),
        ((2560, 1440), 1.5, 32.0, 300.0, 28.0, (1331.0, 814.0), [244.0, 195.0, 375.0], 32.0, 0, &[]),
        ((3840, 2160), 1.0, 260.0, 300.0, 28.0, (3236.0, 2014.0), [604.0, 483.0, 927.0], 32.0, 0, &["FIT-L1"]),
        ((3840, 2160), 2.0, 32.0, 300.0, 28.0, (1544.0, 934.0), [280.0, 224.0, 430.0], 32.0, 0, &[]),
    ];
    for ((w, h), s, left, right, bottom, centre, groups, pitch, overflow, applied) in rows {
        let l = raster(w, h, s);
        let tag = format!("{w}x{h}@{s}");
        assert!((l.left.w() - left).abs() <= 1.0 && (l.right.w() - right).abs() <= 1.0 && (l.bottom.h() - bottom).abs() <= 1.0, "{tag}");
        assert!((l.centre.w() - centre.0).abs() <= 1.0 && (l.centre.h() - centre.1).abs() <= 1.0, "{tag}: {:?}", l.centre);
        assert_eq!(l.groups.iter().map(|g| g.h()).collect::<Vec<_>>(), groups, "{tag}");
        assert_eq!(l.groups.iter().map(|g| g.h()).sum::<f32>(), l.right.h(), "{tag}: groups partition the dock");
        assert_eq!((l.rail_pitch, l.rail_hidden), (pitch, overflow), "{tag}");
        assert_eq!(l.applied, applied, "{tag}");
    }
    // 1280 x 720: 16 slots, the chevron, navigate.
    let l = raster(1280, 720, 1.0);
    assert_eq!(l.rail_shown, 17);
}

/// AC-CONS-04: the edge rows of all ten layouts.json presets give identical bars at the four matrix windows.
/// One preset's edge row: (name, left state, left w, right state, right w, shares, right foot, meter, bottom state,
/// bottom h, slots, viewers, time panel).
type PresetRow = (&'static str, EdgeState, f32, EdgeState, f32, &'static [f32], bool, bool, EdgeState, f32, usize, u32, bool);

#[test]
fn bars_are_identical_under_every_preset() {
    use EdgeState::{Expanded as E, Rail as R};
    // (name, left state, left w, right state, right w, shares, right foot (registered), meter, bottom state, bottom h,
    // slots, viewers, time panel), transcribed from layouts.json presets.
    let presets: [PresetRow; 10] = [
        ("raster", R, 260.0, E, 300.0, &[0.3, 0.24, 0.46], false, false, R, 240.0, 19, 1, true),
        ("develop", E, 260.0, E, 320.0, &[0.56, 0.2, 0.24], true, false, E, 112.0, 7, 1, false),
        ("vector", R, 260.0, E, 300.0, &[0.32, 0.24, 0.44], false, false, R, 240.0, 15, 1, false),
        ("layout", E, 260.0, E, 300.0, &[0.36, 0.26, 0.38], false, false, E, 180.0, 10, 1, false),
        ("motion", E, 260.0, E, 300.0, &[0.14, 0.42, 0.44], true, false, E, 300.0, 10, 1, true),
        ("video", E, 300.0, E, 300.0, &[0.6, 0.4], true, true, E, 320.0, 6, 2, true),
        ("audio", E, 260.0, E, 300.0, &[0.6, 0.4], true, true, E, 340.0, 4, 1, true),
        ("design", E, 260.0, E, 300.0, &[0.52, 0.18, 0.3], true, false, R, 240.0, 10, 1, false),
        ("board", R, 260.0, R, 300.0, &[0.5, 0.2, 0.3], false, false, R, 240.0, 12, 1, false),
        ("web", E, 260.0, E, 300.0, &[0.42, 0.2, 0.38], true, false, E, 180.0, 7, 2, false),
    ];
    for (w, h, s) in [(1280, 720, 1.0f32), (1920, 1080, 1.0), (2560, 1440, 1.5), (3840, 2160, 2.0)] {
        let base = raster(w, h, s);
        for (name, left, left_w, right, right_w, shares, foot, meter, bottom, bottom_h, slots, viewers, time) in presets {
            let e = Edges { left, left_w, right, right_w, shares: shares.to_vec(), right_foot: foot, meter, bottom, bottom_h, time_panel: time, viewers, slots };
            let l = frame::layout((w, h), Scale::new(s).unwrap(), &e);
            let bars = |q: &FrameLayout| [q.menu_bar, q.tab_strip, q.context_bar, q.tool_rail, q.status];
            assert_eq!(bars(&l), bars(&base), "{name} at {w}x{h}@{s}");
            let rects: Vec<Rect> = l.regions().iter().map(|(_, q)| *q).collect();
            assert!(checks::partition(l.window, &rects, l.scale).is_empty(), "{name} at {w}x{h}@{s}: {:?}", l.regions());
        }
    }
}

/// FIT-H2: groups whose body is under 90 collapse to their 26 px header; the largest-share group stays open, and the
/// last open group takes the remainder (exact partition).
#[test]
fn short_dock_collapses_groups_and_keeps_one_open() {
    let l = raster(800, 480, 1.0);
    assert_eq!(l.collapsed, [true, true, false]);
    assert_eq!(l.groups.iter().map(|g| g.h()).collect::<Vec<_>>(), [26.0, 26.0, l.right.h() - 52.0]);
    assert!(l.applied.contains(&"FIT-H2"));
    let l = raster(800, 324, 1.0);
    assert_eq!(l.collapsed, [true, true, false], "the Layers group stays open");
    assert_eq!(l.groups[2].h() - 26.0, 100.0);
}
