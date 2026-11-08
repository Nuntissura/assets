//! SMB-GUI-FRAME-1 item 4 (build-order B3, icon half): the designer's icon set copied into design/icons.json and
//! generated into src/icon_data.rs, the SVG path subset parsed and flattened here, and two-tint masks behind the
//! Resources port.
use kumihimo_ui::display::IconId;
use kumihimo_ui::icon_data;
use kumihimo_ui::icons::{self, IconSet, PathError, parse_path, rasterize};
use kumihimo_ui::paint::Resources;
use std::path::Path;

fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01B3))
}

/// The generated table is the copy's: hash pinned like tokens.rs SOURCE_FNV64; the copy names its kernel source.
#[test]
fn icon_table_is_generated_from_the_copy() {
    let raw = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("design/icons.json")).unwrap();
    assert_eq!(fnv64(&raw), icon_data::SOURCE_FNV64, "rerun tools/gen_icons.py");
    let s = String::from_utf8(raw).unwrap();
    assert!(s.contains(icon_data::DESIGN_SOURCE) && s.contains(&format!("0x{:016X}", icon_data::DESIGN_SOURCE_FNV64)));
    assert_eq!((icon_data::TOOL.len(), icon_data::UI.len()), (35, 85));
    assert_eq!((icon_data::TOOL_GRID, icon_data::TOOL_STROKE, icon_data::UI_GRID, icon_data::UI_STROKE), (20.0, 1.5, 16.0, 1.25));
    for face in ["select", "smart_select", "marquee", "lasso", "wand", "move", "crop", "paint", "erase", "retouch", "tone", "mask", "fill", "pen", "shape", "type", "warp", "measure", "navigate"] {
        assert!(icons::tool(face).is_some(), "rail face {face}");
    }
    assert_eq!(icons::name(icons::tool("paint").unwrap()), Some("paint"));
    assert_eq!(icons::name(icons::ui("eye_off").unwrap()), Some("eye_off"));
    assert_eq!(icons::all().len(), 35 + 85 + 1);
}

/// The path subset (M L H V C S A Z, absolute and relative, implicit repeats, compact numbers) and typed refusals.
/// V4-08 adversarial cases: separators and case, a decoy command letter, truncated numbers.
#[test]
fn path_subset_parses_and_refuses_with_types() {
    let p = parse_path("M2 3h4v5H1V2Z").unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].points, [(2.0, 3.0), (6.0, 3.0), (6.0, 8.0), (1.0, 8.0), (1.0, 2.0), (2.0, 3.0)]);
    assert!(p[0].closed);
    // Relative moveto then implicit lineto; compact numbers ("1.5.3" is two numbers, "-.8" a sign as separator).
    let p = parse_path("m1 1 2 0 0 2l1.5.3-.8-.2").unwrap();
    let last = *p[0].points.last().unwrap();
    assert!((last.0 - 3.7).abs() < 1e-4 && (last.1 - 3.1).abs() < 1e-4, "{last:?}");
    // Commas, tabs and newlines are separators; lowercase is relative, uppercase absolute.
    assert_eq!(parse_path("M 1,1\tL\n3 3").unwrap()[0].points, [(1.0, 1.0), (3.0, 3.0)]);
    assert_eq!(parse_path("M1 1l3 3").unwrap()[0].points, [(1.0, 1.0), (4.0, 4.0)]);
    // Curves flatten to many points ending on the endpoint; S reflects the previous control point.
    let c = parse_path("M0 0C0 10 10 10 10 0S20 -10 20 0").unwrap();
    let end = *c[0].points.last().unwrap();
    assert!(c[0].points.len() > 8 && (end.0 - 20.0).abs() < 1e-4 && end.1.abs() < 1e-4);
    // Arcs: endpoint to centre (SVG 1.1 F.6.5) with radius correction (F.6.6): a too-small radius still ends on the
    // endpoint, and a half circle of radius 5 passes through its far point.
    let a = parse_path("M0 0A1 1 0 0 1 10 0").unwrap();
    let pts = &a[0].points;
    assert!((pts.last().unwrap().0 - 10.0).abs() < 1e-3);
    let far = pts.iter().map(|p| p.1.abs()).fold(0.0f32, f32::max);
    assert!((far - 5.0).abs() < 0.05, "corrected radius 5, got {far}");
    // Compact arc flags as the set writes them ("a2 2 0 0 1 2-2").
    assert!(parse_path("M3 8V5a2 2 0 0 1 2-2h3").is_ok());
    // Refusals.
    assert_eq!(parse_path("L1 1"), Err(PathError::MissingMoveTo));
    assert!(matches!(parse_path("M1 1 Q2 2 3 3"), Err(PathError::BadCommand(_))), "Q is outside the subset (decoy letter)");
    assert!(matches!(parse_path("M1 1 L"), Err(PathError::BadNumber(_))));
    assert!(matches!(parse_path("M1 1 L2 e"), Err(PathError::BadNumber(_))));
    assert!(matches!(parse_path("M1 1 #"), Err(PathError::BadCommand(_))));
    let long = format!("M0 0{}", "c1 1 2 2 3 3".repeat(4000));
    assert_eq!(parse_path(&long), Err(PathError::TooManySegments));
    // Every path of the set parses.
    for (name, d, ..) in icon_data::TOOL {
        assert!(parse_path(d).is_ok(), "tool {name}");
    }
    for (name, d) in icon_data::UI {
        assert!(parse_path(d).is_ok(), "ui {name}");
    }
}

fn coverage(m: &kumihimo_ui::paint::IconMask, ch: usize, x0: u32, y0: u32, x1: u32, y1: u32) -> u64 {
    let mut s = 0;
    for y in y0..y1.min(m.height) {
        for x in x0..x1.min(m.width) {
            s += u64::from(m.rg[(y * m.width + x) as usize][ch]);
        }
    }
    s
}

/// Masks are deterministic; the hot point sits where the data puts it; a straight stroke measures 1.5 x scale.
#[test]
fn masks_deterministic_hot_point_and_stroke_width() {
    let paint = icons::tool("paint").unwrap();
    let a = rasterize(paint, 20).unwrap();
    assert_eq!(a, rasterize(paint, 20).unwrap());
    assert_eq!((a.width, a.height, a.rg.len()), (20, 20, 400));
    // Paint's hot point is the 3 x 3 square at (2, 14): G covers it and almost nothing else.
    let inside = coverage(&a, 1, 2, 14, 5, 17);
    let total = coverage(&a, 1, 0, 0, 20, 20);
    assert!(inside > 9 * 200 && inside * 10 >= total * 9, "inside {inside} total {total}");
    // UI icons have no hot point.
    let eye = rasterize(icons::ui("eye").unwrap(), 16).unwrap();
    assert_eq!(coverage(&eye, 1, 0, 0, 16, 16), 0);
    // The type icon's stem "M10 2.5v15": row 12 crosses only that stroke; its summed coverage is the stroke width.
    let ty = icons::tool("type").unwrap();
    for s in [1u32, 2, 3, 4] {
        let m = rasterize(ty, 20 * s).unwrap();
        let row = 12 * s + s / 2;
        let w = coverage(&m, 0, 0, row, m.width, row + 1) as f32 / 255.0;
        assert!((w - 1.5 * s as f32).abs() <= 0.5, "scale {s}: stroke {w}");
    }
}

/// Every rail face and frame icon draws inside its live area (16 of 20, 13 of 16); masks at scale 8 stay bounded and
/// the cache keeps at most CACHE_CAP masks; an unknown id is None and counted.
#[test]
fn icons_live_area_scale_8_and_cache_cap() {
    for (i, (name, ..)) in icon_data::TOOL.iter().enumerate() {
        let m = rasterize(IconId(icons::TOOL_BASE + i as u32), 20).unwrap();
        assert!(coverage(&m, 0, 2, 2, 18, 18) > 0, "tool {name} empty in its live area");
    }
    for (i, (name, _)) in icon_data::UI.iter().enumerate() {
        let m = rasterize(IconId(icons::UI_BASE + i as u32), 16).unwrap();
        assert!(coverage(&m, 0, 1, 1, 15, 15) > 0, "ui {name} empty in its live area");
    }
    let marker = rasterize(icons::VARIANT_MARKER, 4).unwrap();
    assert!(marker.rg[15][0] > 200 && marker.rg[0][0] < 50, "filled bottom right triangle");
    let big = rasterize(icons::tool("navigate").unwrap(), 160).unwrap();
    assert_eq!((big.width, big.height), (160, 160));
    assert_eq!(rasterize(icons::tool("navigate").unwrap(), 100_000).map(|m| m.width), Some(icons::MAX_ICON_PX));
    assert!(rasterize(IconId(999_999), 20).is_none());
    let mut set = IconSet::default();
    for px in 1..=(icons::CACHE_CAP as u32 + 40) {
        assert!(set.icon(icons::tool("crop").unwrap(), px % 200 + 1).is_some());
    }
    assert!(set.cached() <= icons::CACHE_CAP);
    let before = set.stats().hits;
    set.icon(icons::tool("crop").unwrap(), 20);
    set.icon(icons::tool("crop").unwrap(), 20);
    assert!(set.stats().hits > before);
    assert!(set.icon(IconId(42), 20).is_none());
    assert_eq!(set.stats().unknown, 1);
}
