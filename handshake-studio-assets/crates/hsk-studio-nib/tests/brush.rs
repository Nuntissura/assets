//! Lane SMB-ILLUSTRATOR-TOOLS-2: art, scatter and pattern brushes along paths (STU-VEC-152,
//! STU-VEC-162). Expected bounds are hand-derived from the brush rules (module docs).
use hsk_studio_nib::arrange::{Bounds, path_bounds};
use hsk_studio_nib::brush::*;
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement, PathPoint};

fn line(points: &[(f64, f64)]) -> OwnedPath {
    let mut o = OwnedPath::new(FillRule::NonZero);
    for (i, (x, y)) in points.iter().enumerate() {
        let q = PathPoint::new(*x, *y);
        o.elements.push(if i == 0 { PathElement::MoveTo(q) } else { PathElement::LineTo(q) });
    }
    o
}
fn near(a: Option<Bounds>, e: [f64; 4]) {
    let a = a.expect("bounds");
    let got = [a.min[0], a.min[1], a.max[0], a.max[1]];
    assert!(got.iter().zip(e).all(|(g, w)| (g - w).abs() < 1e-6), "{got:?} != {e:?}");
}
fn has_point(p: &OwnedPath, x: f64, y: f64) -> bool {
    p.elements.iter().any(|e| matches!(e, PathElement::MoveTo(q) | PathElement::LineTo(q) if (q.x - x).abs() < 1e-6 && (q.y - y).abs() < 1e-6))
}

#[test]
fn art_and_scatter_brushes_follow_the_spine() {
    let spine = line(&[(0.0, 0.0), (100.0, 0.0)]);
    let bar = tile_rect(0.0, -1.0, 10.0, 1.0);
    // Stretch keeps the art's width; proportional scales it with the length (x10).
    let stretch = ArtBrush { width: 100.0, scale: ArtScale::Stretch, flip_along: false, flip_across: false };
    near(path_bounds(&art_brush(&spine, std::slice::from_ref(&bar), &stretch).unwrap()[0]), [0.0, -1.0, 100.0, 1.0]);
    let prop = ArtBrush { scale: ArtScale::Proportional, ..stretch };
    near(path_bounds(&art_brush(&spine, std::slice::from_ref(&bar), &prop).unwrap()[0]), [0.0, -10.0, 100.0, 10.0]);
    // A triangle's apex (art x = 10) lands at the spine end; flipped along, at the start.
    let tri = {
        let mut t = line(&[(0.0, -1.0), (10.0, 0.0), (0.0, 1.0)]);
        t.elements.push(PathElement::Close);
        t
    };
    assert!(has_point(&art_brush(&spine, std::slice::from_ref(&tri), &stretch).unwrap()[0], 100.0, 0.0));
    assert!(has_point(&art_brush(&spine, std::slice::from_ref(&tri), &ArtBrush { flip_along: true, ..stretch }).unwrap()[0], 0.0, 0.0));
    // Guides 10..20 on 30-long art: the ends keep 10 px each, the middle stretches over 80.
    let long = tile_rect(0.0, -1.0, 30.0, 1.0);
    let guided = ArtBrush { scale: ArtScale::StretchBetweenGuides { start: 10.0, end: 20.0 }, ..stretch };
    near(path_bounds(&art_brush(&spine, std::slice::from_ref(&long), &guided).unwrap()[0]), [0.0, -1.0, 100.0, 1.0]);
    // Along a right angle the art bends round the corner.
    let corner = line(&[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)]);
    near(path_bounds(&art_brush(&corner, std::slice::from_ref(&bar), &stretch).unwrap()[0]), [0.0, -1.0, 51.0, 50.0]);
    // Scatter: 10 px squares every 10 px from 5 to 95 (10 copies) tile the spine 0..100.
    let square = tile_rect(0.0, 0.0, 10.0, 10.0);
    let fixed = ScatterBrush { size: Dyn::fixed(100.0), spacing: Dyn::fixed(100.0), offset: Dyn::fixed(0.0), rotation: Dyn::fixed(0.0), relative: RotationRelative::Path, seed: 1 };
    near(path_bounds(&scatter_brush(&spine, std::slice::from_ref(&square), &fixed).unwrap()[0]), [0.0, -5.0, 100.0, 5.0]);
    // Offset 100 % of the copy height moves them across the path; 50 % size halves them.
    near(path_bounds(&scatter_brush(&spine, std::slice::from_ref(&square), &ScatterBrush { offset: Dyn::fixed(100.0), ..fixed }).unwrap()[0]), [0.0, 5.0, 100.0, 15.0]);
    near(path_bounds(&scatter_brush(&spine, std::slice::from_ref(&square), &ScatterBrush { size: Dyn::fixed(50.0), ..fixed }).unwrap()[0]), [0.0, -2.5, 100.0, 2.5]);
    // Random size is seeded: the same seed gives the same copies.
    let random = ScatterBrush { size: Dyn { min: 50.0, max: 100.0, random: true }, ..fixed };
    assert_eq!(scatter_brush(&spine, std::slice::from_ref(&square), &random).unwrap(), scatter_brush(&spine, std::slice::from_ref(&square), &random).unwrap());
    // Hostile: out-of-range size, too many copies, zero-width art.
    assert_eq!(scatter_brush(&spine, std::slice::from_ref(&square), &ScatterBrush { size: Dyn::fixed(0.0), ..fixed }), Err(Error::InvalidRequest));
    let far = line(&[(0.0, 0.0), (1.0e6, 0.0)]);
    assert_eq!(scatter_brush(&far, std::slice::from_ref(&square), &ScatterBrush { spacing: Dyn::fixed(1.0), ..fixed }), Err(Error::BudgetExceeded));
    assert_eq!(art_brush(&spine, &[line(&[(0.0, 0.0), (0.0, 5.0)])], &stretch), Err(Error::InvalidGeometry));
}

#[test]
fn pattern_brush_lays_side_start_end_and_corner_tiles() {
    let side = TileArt { slot: TileSlot::Side, path: tile_rect(0.0, -2.0, 30.0, 2.0) };
    let start = TileArt { slot: TileSlot::Start, path: tile_rect(0.0, -3.0, 10.0, 3.0) };
    let end = TileArt { slot: TileSlot::End, path: tile_rect(0.0, -4.0, 10.0, 4.0) };
    let inner = TileArt { slot: TileSlot::InnerCorner, path: tile_rect(0.0, -5.0, 8.0, 5.0) };
    let outer = TileArt { slot: TileSlot::OuterCorner, path: tile_rect(0.0, -6.0, 8.0, 6.0) };
    let stretch = PatternBrush { scale: 100.0, fit: PatternFit::StretchToFit, flip_along: false, flip_across: false };
    // Straight 100 px: start 0..10, end 90..100, the 80 px run holds round(80 / 30) = 3 stretched tiles.
    let straight = line(&[(0.0, 0.0), (100.0, 0.0)]);
    let tiles = [side.clone(), start.clone(), end.clone()];
    let out = pattern_brush(&straight, &tiles, &stretch).unwrap();
    near(path_bounds(&out[0]), [10.0, -2.0, 90.0, 2.0]);
    near(path_bounds(&out[1]), [0.0, -3.0, 10.0, 3.0]);
    near(path_bounds(&out[2]), [90.0, -4.0, 100.0, 4.0]);
    // Add space without end tiles: floor(100 / 30) = 3 tiles, gaps 10 / 3, the first half a gap in.
    let spaced = pattern_brush(&straight, std::slice::from_ref(&side), &PatternBrush { fit: PatternFit::AddSpaceToFit, ..stretch }).unwrap();
    near(path_bounds(&spaced[0]), [10.0 / 6.0, -2.0, 100.0 - 10.0 / 6.0, 2.0]);
    // A right turn on screen (east, then south) takes the inner corner tile, centred on the corner.
    let bend = line(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]);
    let out = pattern_brush(&bend, &[side.clone(), inner.clone(), outer.clone()], &stretch).unwrap();
    assert!(!out[1].elements.is_empty(), "inner corner tile placed");
    assert!(out[2].elements.is_empty(), "no outer corner");
    assert!(has_point(&out[1], 96.0, -5.0), "inner tile starts 4 px before the corner");
    // Hostile: no side tile, non-finite scale.
    assert_eq!(pattern_brush(&straight, &[start], &stretch), Err(Error::InvalidRequest));
    assert_eq!(pattern_brush(&straight, &tiles, &PatternBrush { scale: f64::NAN, ..stretch }), Err(Error::InvalidRequest));
}

#[test]
fn calligraphic_and_bristle_brushes_sweep_the_spine() {
    let across = line(&[(0.0, 0.0), (100.0, 0.0)]);
    let down = line(&[(0.0, 0.0), (0.0, 100.0)]);
    // A round 10 px nib: the sweep is the 10 px band with round ends.
    let round = CalligraphicBrush { angle: 0.0, roundness: 100.0, size: 10.0 };
    near(path_bounds(&calligraphic_brush(&across, &round).unwrap()), [-5.0, -5.0, 105.0, 5.0]);
    // A flat nib (10 % round) held at 0 degrees: thin along a horizontal stroke, wide down a vertical one.
    let flat = CalligraphicBrush { angle: 0.0, roundness: 10.0, size: 10.0 };
    near(path_bounds(&calligraphic_brush(&across, &flat).unwrap()), [-5.0, -0.5, 105.0, 0.5]);
    near(path_bounds(&calligraphic_brush(&down, &flat).unwrap()), [-5.0, -0.5, 5.0, 100.5]);
    // Turned 90 degrees the flat nib swaps.
    near(path_bounds(&calligraphic_brush(&across, &CalligraphicBrush { angle: 90.0, ..flat }).unwrap()), [-0.5, -5.0, 100.5, 5.0]);
    // Bristles: 32 bristles of 10 / 32 px across a 10 px brush, round caps.
    let half = 10.0 / 32.0 / 2.0;
    let bristles = bristle_brush(&across, &BristleBrush { size: 10.0, length: 100.0, density: 100.0 }).unwrap();
    let b = path_bounds(&bristles).unwrap();
    assert!((b.min[1] + 5.0 + half).abs() < 0.01 && (b.max[1] - 5.0 - half).abs() < 0.01 && (b.max[0] - 100.0 - half).abs() < 0.01, "{b:?}");
    // Hostile: zero roundness, non-finite size, density out of range.
    assert_eq!(calligraphic_brush(&across, &CalligraphicBrush { roundness: 0.0, ..round }), Err(Error::InvalidRequest));
    assert_eq!(calligraphic_brush(&across, &CalligraphicBrush { size: f64::NAN, ..round }), Err(Error::InvalidRequest));
    assert_eq!(bristle_brush(&across, &BristleBrush { size: 10.0, length: 100.0, density: 0.0 }), Err(Error::InvalidRequest));
}
