//! Lane SMB-NIB-VECTOR-RENDER slice 3: analytic coverage planes and paint evaluation over coverage.
use hsk_studio_nib::raster::{
    CoveragePlane, Gradient, GradientGeometry, GradientStop, IDENTITY, Interpolation, Paint,
    PatternTile, PixelFrame, RasterOptions, Spread, paint_coverage, rasterize,
};
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement, PathPoint};

fn polygon(points: &[(f64, f64)], rule: FillRule) -> OwnedPath {
    let mut path = OwnedPath::new(rule);
    for (i, &(x, y)) in points.iter().enumerate() {
        let q = PathPoint::new(x, y);
        path.elements.push(if i == 0 { PathElement::MoveTo(q) } else { PathElement::LineTo(q) });
    }
    path.elements.push(PathElement::Close);
    path
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> OwnedPath {
    polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], FillRule::NonZero)
}

fn frame(x: i32, y: i32, width: u32, height: u32) -> PixelFrame {
    PixelFrame { x, y, width, height }
}

fn cover(path: &OwnedPath, f: PixelFrame) -> CoveragePlane {
    let (plane, _) = rasterize(path, f, &RasterOptions::default()).unwrap();
    let (again, _) = rasterize(path, f, &RasterOptions::default()).unwrap();
    assert_eq!(plane.digest(), again.digest(), "deterministic");
    plane
}

fn rows(p: &CoveragePlane) -> Vec<Vec<u8>> {
    p.data.chunks(p.frame.width as usize).map(|r| r.to_vec()).collect()
}

fn hex(d: [u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn coverage_hand_values_and_fill_rules() {
    // Half-pixel edges: quarter and half cells (63.75 -> 64, 127.5 -> 128).
    let p = cover(&rect(0.5, 0.5, 2.5, 1.5), frame(0, 0, 4, 3));
    assert_eq!(rows(&p), vec![vec![64, 128, 64, 0], vec![64, 128, 64, 0], vec![0, 0, 0, 0]]);
    // Integer edges are exact 255 / 0; the frame origin shifts the window.
    let p = cover(&rect(1.0, 1.0, 3.0, 3.0), frame(1, 0, 3, 4));
    assert_eq!(rows(&p), vec![vec![0, 0, 0], vec![255, 255, 0], vec![255, 255, 0], vec![0, 0, 0]]);
    // Triangle x + y <= 4: diagonal pixels are cut in half, the rest is full or empty.
    let p = cover(&polygon(&[(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)], FillRule::NonZero), frame(0, 0, 4, 4));
    assert_eq!(
        rows(&p),
        vec![vec![255, 255, 255, 128], vec![255, 255, 128, 0], vec![255, 128, 0, 0], vec![128, 0, 0, 0]]
    );
    // Same-direction nested squares: non-zero fills the centre, even-odd leaves it empty.
    let nested = |rule| {
        let mut path = rect(0.0, 0.0, 3.0, 3.0);
        path.fill_rule = rule;
        path.elements.extend(rect(1.0, 1.0, 2.0, 2.0).elements);
        cover(&path, frame(0, 0, 3, 3))
    };
    assert_eq!(nested(FillRule::NonZero).get(1, 1), Some(255));
    assert_eq!(nested(FillRule::EvenOdd).get(1, 1), Some(0));
    assert_eq!(nested(FillRule::EvenOdd).get(0, 0), Some(255));
    // Edges far outside the frame clip exactly; an empty path is an all-zero plane.
    let big = cover(&rect(-1e9, -1e9, 1e9, 1e9), frame(-5, 7, 3, 2));
    assert!(big.data.iter().all(|c| *c == 255));
    let (empty, receipt) = rasterize(&OwnedPath::new(FillRule::NonZero), frame(0, 0, 2, 2), &RasterOptions::default()).unwrap();
    assert_eq!((empty.data, receipt), (vec![0; 4], None));
    // Circle r=8 at (8,8) in 16x16: total coverage ~ area (chords inside, tolerance 0.1).
    let (k, r, c) = (0.552_284_749_830_793_4 * 8.0, 8.0, 8.0);
    let q = PathPoint::new;
    let mut circle = OwnedPath::new(FillRule::NonZero);
    circle.elements.push(PathElement::MoveTo(q(c + r, c)));
    circle.elements.push(PathElement::CubicTo(q(c + r, c + k), q(c + k, c + r), q(c, c + r)));
    circle.elements.push(PathElement::CubicTo(q(c - k, c + r), q(c - r, c + k), q(c - r, c)));
    circle.elements.push(PathElement::CubicTo(q(c - r, c - k), q(c - k, c - r), q(c, c - r)));
    circle.elements.push(PathElement::CubicTo(q(c + k, c - r), q(c + r, c - k), q(c + r, c)));
    circle.elements.push(PathElement::Close);
    let plane = cover(&circle, frame(0, 0, 16, 16));
    let total: f64 = plane.data.iter().map(|v| f64::from(*v) / 255.0).sum();
    let area = std::f64::consts::PI * 64.0;
    assert!((total - area).abs() < 2.0 * std::f64::consts::PI * 8.0 * 0.1 + 0.5, "{total} vs {area}");
    // Digest pins (STU-VEC-179 byte identity): any change to the rasteriser changes these.
    assert_eq!(hex(cover(&rect(0.5, 0.5, 2.5, 1.5), frame(0, 0, 4, 3)).digest()), PIN_RECT);
    assert_eq!(hex(plane.digest()), PIN_CIRCLE);
}

// Recomputed independently: sha256(LE i32 0, i32 0, u32 4, u32 3 ++ the 12 bytes above).
const PIN_RECT: &str = "88bd9d143f5f57f73555a99440038e592bc1e62a47c15d4855c5bca3137ca5c9";
const PIN_CIRCLE: &str = "pending";

fn stop(offset: f64, v: f32) -> GradientStop {
    GradientStop { offset, colour: [v, v, v, 1.0], midpoint: None }
}

fn gradient(geometry: GradientGeometry, spread: Spread, stops: Vec<GradientStop>) -> Paint {
    Paint::Gradient(Gradient {
        geometry,
        transform: IDENTITY,
        spread,
        interpolation: Interpolation::Classic,
        dither: false,
        stops,
    })
}

fn full(width: u32, height: u32) -> CoveragePlane {
    CoveragePlane { frame: frame(0, 0, width, height), data: vec![255; (width * height) as usize] }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn paint_gradients_and_patterns_over_coverage() {
    // Solid at half coverage: premultiplied colour scaled by 128/255.
    let half = CoveragePlane { frame: frame(0, 0, 1, 1), data: vec![128] };
    let out = paint_coverage(&half, &Paint::Solid([1.0, 0.5, 0.0, 0.5])).unwrap();
    let k = 128.0 / 255.0;
    assert_eq!(out.data[0], [0.5 * k, 0.25 * k, 0.0, 0.5 * k]);
    // Linear black -> white over x 0..4: pixel centres t = 0.125, 0.375, 0.625, 0.875.
    let lin = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 4.0, y2: 0.0 }, Spread::Pad, vec![stop(0.0, 0.0), stop(1.0, 1.0)]);
    let out = paint_coverage(&full(4, 1), &lin).unwrap();
    for (px, t) in out.data.iter().zip([0.125, 0.375, 0.625, 0.875]) {
        assert!(close(px[0], t) && px[3] == 1.0, "{px:?} {t}");
    }
    // Spread: axis 0..2 over 4 px -> t = 0.25 0.75 1.25 1.75.
    for (spread, want) in [
        (Spread::Pad, [0.25, 0.75, 1.0, 1.0]),
        (Spread::Repeat, [0.25, 0.75, 0.25, 0.75]),
        (Spread::Reflect, [0.25, 0.75, 0.75, 0.25]),
    ] {
        let g = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 2.0, y2: 0.0 }, spread, vec![stop(0.0, 0.0), stop(1.0, 1.0)]);
        let out = paint_coverage(&full(4, 1), &g).unwrap();
        let got: Vec<f32> = out.data.iter().map(|p| p[0]).collect();
        assert!(got.iter().zip(want).all(|(a, b)| close(*a, b)), "{spread:?} {got:?}");
    }
    // Midpoint 0.25: the 50 % colour sits at a quarter of the span (pixel centre t = 0.25).
    let mut s0 = stop(0.0, 0.0);
    s0.midpoint = Some(0.25);
    let g = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 2.0, y2: 0.0 }, Spread::Pad, vec![s0, stop(1.0, 1.0)]);
    let out = paint_coverage(&full(1, 1), &g).unwrap();
    assert!(close(out.data[0][0], 0.5), "{:?}", out.data[0]);
    // Radial centred: t = distance / r. Pixel (2,2) centre (2.5,2.5) is sqrt(0.5) from (2,2).
    let rad = gradient(GradientGeometry::Radial { cx: 2.0, cy: 2.0, r: 2.0, fx: 2.0, fy: 2.0, fr: 0.0 }, Spread::Pad, vec![stop(0.0, 0.0), stop(1.0, 1.0)]);
    let out = paint_coverage(&full(4, 4), &rad).unwrap();
    assert!(close(out.data[2 * 4 + 2][0], (0.5f64.sqrt() / 2.0) as f32));
    // Focal circle with radius: t = 0 on the focal circle, 1 on the end circle.
    let cone = gradient(GradientGeometry::Radial { cx: 0.0, cy: 0.5, r: 4.0, fx: 0.0, fy: 0.5, fr: 1.0 }, Spread::Pad, vec![stop(0.0, 0.0), stop(1.0, 1.0)]);
    let out = paint_coverage(&full(4, 1), &cone).unwrap();
    // centres x = 0.5 .. 3.5, distance d -> t = (d - 1) / 3, padded below 0.
    for (px, x) in out.data.iter().zip([0.5f32, 1.5, 2.5, 3.5]) {
        assert!(close(px[0], ((x - 1.0) / 3.0).clamp(0.0, 1.0)), "{px:?} {x}");
    }
    // Pattern: 2x1 tile red|blue, identity: texel centres hit exactly, wrap repeats.
    let tile = PatternTile { width: 2, height: 1, pixels: vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]], transform: IDENTITY };
    let out = paint_coverage(&full(3, 1), &Paint::Pattern(tile.clone())).unwrap();
    assert_eq!(out.data, vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);
    // Scaled x2: pixel centre 0.5 is tile x 0.25, a quarter texel left of the red centre (wraps to blue).
    let mut scaled = tile;
    scaled.transform = [2.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let out = paint_coverage(&full(1, 1), &Paint::Pattern(scaled)).unwrap();
    assert!(close(out.data[0][0], 0.75) && close(out.data[0][2], 0.25), "{:?}", out.data[0]);
    // Zero coverage paints nothing.
    let none = CoveragePlane { frame: frame(0, 0, 1, 1), data: vec![0] };
    assert_eq!(paint_coverage(&none, &lin).unwrap().data, vec![[0.0; 4]]);
}

#[test]
fn raster_and_paint_hostile_inputs_are_typed() {
    let r = rect(0.0, 0.0, 1.0, 1.0);
    let opts = RasterOptions::default();
    // Pixel budget before allocation, including u32 x u32 overflow of the product.
    assert_eq!(rasterize(&r, frame(0, 0, 1 << 20, 1 << 20), &opts).err(), Some(Error::BudgetExceeded));
    assert_eq!(rasterize(&r, frame(0, 0, u32::MAX, u32::MAX), &opts).err(), Some(Error::BudgetExceeded));
    let mut small = opts;
    small.max_work = 4;
    assert_eq!(rasterize(&r, frame(0, 0, 8, 8), &small).err(), Some(Error::BudgetExceeded));
    let mut nan = r.clone();
    nan.elements[1] = PathElement::LineTo(PathPoint::new(f64::NAN, 0.0));
    assert_eq!(rasterize(&nan, frame(0, 0, 2, 2), &opts).err(), Some(Error::InvalidGeometry));
    let plane = full(2, 2);
    let lin = |stops: Vec<GradientStop>| gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 }, Spread::Pad, stops);
    let bad_paints = vec![
        Paint::Solid([f32::NAN, 0.0, 0.0, 1.0]),
        Paint::Solid([2.0, 0.0, 0.0, 1.0]),
        lin(vec![]),
        lin(vec![stop(0.5, 0.0), stop(0.2, 1.0)]),
        lin(vec![stop(-0.1, 0.0)]),
        lin(vec![GradientStop { offset: 0.0, colour: [0.0; 4], midpoint: Some(0.9) }, stop(1.0, 1.0)]),
        lin(vec![GradientStop { offset: 0.0, colour: [0.0; 4], midpoint: Some(0.1) }, stop(1.0, 1.0)]),
        Paint::Gradient(Gradient { geometry: GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 }, transform: [0.0; 6], spread: Spread::Pad, interpolation: Interpolation::Classic, dither: false, stops: vec![stop(0.0, 0.0)] }),
        Paint::Gradient(Gradient { geometry: GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 }, transform: IDENTITY, spread: Spread::Pad, interpolation: Interpolation::Perceptual, dither: false, stops: vec![stop(0.0, 0.0)] }),
        Paint::Gradient(Gradient { geometry: GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 1.0, y2: 0.0 }, transform: IDENTITY, spread: Spread::Pad, interpolation: Interpolation::Classic, dither: true, stops: vec![stop(0.0, 0.0)] }),
        gradient(GradientGeometry::Radial { cx: 0.0, cy: 0.0, r: -1.0, fx: 0.0, fy: 0.0, fr: 0.0 }, Spread::Pad, vec![stop(0.0, 0.0)]),
        Paint::Pattern(PatternTile { width: 2, height: 2, pixels: vec![[0.0; 4]; 3], transform: IDENTITY }),
        Paint::Pattern(PatternTile { width: 1, height: 1, pixels: vec![[1.0, 0.0, 0.0, 0.5]], transform: IDENTITY }),
        Paint::Pattern(PatternTile { width: 0, height: 1, pixels: vec![], transform: IDENTITY }),
        Paint::Pattern(PatternTile { width: 5000, height: 1, pixels: vec![[0.0; 4]; 5000], transform: IDENTITY }),
    ];
    for (i, paint) in bad_paints.iter().enumerate() {
        assert_eq!(paint_coverage(&plane, paint).err(), Some(Error::UnsupportedOptions), "case {i}");
    }
    let torn = CoveragePlane { frame: frame(0, 0, 2, 2), data: vec![255; 3] };
    assert_eq!(paint_coverage(&torn, &Paint::Solid([0.0; 4])).err(), Some(Error::InvalidRequest));
    // Coverage-level clip: product with exact integer rounding; frames must match.
    let a = CoveragePlane { frame: frame(0, 0, 3, 1), data: vec![255, 128, 1] };
    let b = CoveragePlane { frame: frame(0, 0, 3, 1), data: vec![128, 128, 127] };
    assert_eq!(a.intersect(&b).unwrap().data, vec![128, 64, 0]);
    assert_eq!(a.intersect(&full(3, 1)).unwrap().data, vec![255, 128, 1]);
    assert_eq!(a.intersect(&full(1, 3)).err(), Some(Error::InvalidRequest));
}
