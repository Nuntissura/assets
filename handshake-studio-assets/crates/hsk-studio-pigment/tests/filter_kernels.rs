//! Raster filter kernels (lane SMB-PIGMENT-FILTER-KERNELS-1). Goldens come from the declared Studio
//! readings in the module docs; vendor parity is UNVERIFIED (D5). Hostile inputs are typed before
//! allocation (V4-01/02).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::{BlurLimits, RadialBlurKind, blur_window_axis, blur_window_in, box_kernel, directional_blur_window, radial_blur_window, BlurAxis};
use hsk_studio_pigment::filter_kernels::{BoxDimensions, FilterKernel as K, MorphPreserve};
use hsk_studio_pigment::filter_math::{FilterError, FilterLimits, LayerFilter, filter_plane, find_edges_window};
use hsk_studio_pigment::render_plan::SampleRange;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::{SmartFilterStack, StackEntry, StackFilter, StackLimits};
use std::time::{Duration, Instant};

const LIMITS: FilterLimits = FilterLimits { max_bytes: 1 << 33, max_work: 1 << 36 };
const BLUR: BlurLimits = BlurLimits { max_bytes: 1 << 33, max_work: 1 << 36 };

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

/// A busy, partly transparent plane at a non-zero origin.
fn busy(w: u32, h: u32) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let t = (x * 13 + y * 7) % 11;
            let c = |k: u32| ((x * (3 + k) + y * (5 + 2 * k) + k) % 17) as f32 / 16.0;
            let a = if t == 0 { 0.0 } else if t < 4 { 0.25 + 0.05 * t as f32 } else { 1.0 };
            data.push([c(0), c(1), c(2), a]);
        }
    }
    RgbaPlane { bounds: rect(3, -2, w, h), data }
}

fn flat(w: u32, h: u32, p: [f32; 4]) -> RgbaPlane {
    RgbaPlane { bounds: rect(0, 0, w, h), data: vec![p; (w * h) as usize] }
}

fn one(p: &RgbaPlane, x: u32, y: u32) -> [f32; 4] {
    p.data[(y * p.bounds.width + x) as usize]
}

fn run(k: &K, p: &RgbaPlane) -> RgbaPlane {
    k.render_plane(p, LIMITS).unwrap_or_else(|e| panic!("{}: {e:?}", k.key()))
}

fn near(a: [f32; 4], b: [f32; 4], tol: f32) {
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol), "{a:?} vs {b:?}");
}

fn impulse(w: u32, h: u32) -> RgbaPlane {
    let mut p = flat(w, h, [0.0, 0.0, 0.0, 1.0]);
    let at = (h / 2 * w + w / 2) as usize;
    p.data[at] = [1.0, 1.0, 1.0, 1.0];
    p
}

fn all_kinds() -> Vec<K> {
    let mut weights = [0; 25];
    weights[12] = 4;
    weights[7] = -1;
    vec![
        K::BoxBlur { radius_px: 2.0, iterations: 2, dimensions: BoxDimensions::HorizontalAndVertical },
        K::MotionBlur { angle_deg: 30.0, length_px: 6.0 },
        K::RadialBlur { centre_x_px: 8.0, centre_y_px: 4.0, amount: 40.0, kind: RadialBlurKind::Zoom },
        K::FindEdges { invert: false },
        K::CustomConvolution { weights, scale: 3, offset: 2 },
        K::Sharpen,
        K::SharpenMore,
        K::BasicBlur,
        K::BlurMore,
        K::SharpenEdges,
        K::Despeckle,
        K::Emboss { angle_deg: 45.0, height_px: 1.5, amount_pct: 120.0 },
        K::Solarize { threshold_levels: 100 },
        K::Mosaic { horizontal_blocks: 5, vertical_blocks: 3 },
        K::Maximum { radius_px: 2, preserve: MorphPreserve::Squareness },
        K::Minimum { radius_px: 2, preserve: MorphPreserve::Roundness },
        K::DustAndScratches { radius_px: 2, threshold: 0.1 },
        K::SurfaceBlur { radius_px: 2, threshold_levels: 40.0 },
        K::BilateralBlur { radius_px: 2, range_sigma: 0.2 },
    ]
}

// ---------------------------------------------------------------------------------------------
// Item 1: the four kinds that reuse the existing owners.
// ---------------------------------------------------------------------------------------------

#[test]
fn item1_box_blur_is_the_shared_kernel() {
    let p = busy(16, 12);
    let taps = box_kernel(3, 2).unwrap();
    let got = run(&K::BoxBlur { radius_px: 2.5, iterations: 2, dimensions: BoxDimensions::HorizontalAndVertical }, &p);
    assert_eq!(got.data, blur_window_in(SampleRange::Unit, &p.data, p.bounds, p.bounds, &taps, BLUR).unwrap(), "half = floor(2.5 + 0.5)");
    let got = run(&K::BoxBlur { radius_px: 3.0, iterations: 2, dimensions: BoxDimensions::Horizontal }, &p);
    assert_eq!(got.data, blur_window_axis(SampleRange::Unit, &p.data, p.bounds, p.bounds, &taps, BlurAxis::Horizontal, BLUR).unwrap());
    let got = run(&K::BoxBlur { radius_px: 3.0, iterations: 2, dimensions: BoxDimensions::Vertical }, &p);
    assert_eq!(got.data, blur_window_axis(SampleRange::Unit, &p.data, p.bounds, p.bounds, &taps, BlurAxis::Vertical, BLUR).unwrap());
    let id = run(&K::BoxBlur { radius_px: 0.0, iterations: 7, dimensions: BoxDimensions::HorizontalAndVertical }, &p);
    assert_eq!(id.data, p.data, "radius 0 is the identity");
}

/// Inside its hard bound but above the taps the shared kernel admits: a budget refusal, not a
/// parameter error.
#[test]
fn item1_box_blur_hard_bound_and_budget_are_distinct() {
    let k = |radius_px, iterations| K::BoxBlur { radius_px, iterations, dimensions: BoxDimensions::Horizontal };
    let p = flat(2, 2, [0.5; 4]);
    assert!(matches!(k(30_000.0, 50).render_plane(&p, LIMITS), Err(FilterError::OverBudget { .. })));
    assert!(matches!(k(3_001.0, 1).render_plane(&p, LIMITS), Err(FilterError::OverBudget { .. })));
    k(3_000.0, 1).render_plane(&p, LIMITS).unwrap();
    assert_eq!(k(30_000.5, 1).validate(), Err(FilterError::Parameter));
    assert_eq!(k(-0.5, 1).validate(), Err(FilterError::Parameter));
    assert_eq!(k(1.0, 0).validate(), Err(FilterError::Parameter));
    assert_eq!(k(1.0, 51).validate(), Err(FilterError::Parameter));
    k(30_000.0, 50).validate().unwrap();
}

#[test]
fn item1_motion_and_radial_blur_are_the_shared_kernels() {
    let p = busy(16, 12);
    let got = run(&K::MotionBlur { angle_deg: 30.0, length_px: 7.0 }, &p);
    assert_eq!(got.data, directional_blur_window(SampleRange::Unit, &p.data, p.bounds, p.bounds, 30.0, 7.0, BLUR).unwrap());
    let neg = run(&K::MotionBlur { angle_deg: -330.0, length_px: 7.0 }, &p);
    assert_eq!(neg.data, got.data, "a negative angle is legal and reduced modulo 360");
    for kind in [RadialBlurKind::Spin, RadialBlurKind::Zoom] {
        let got = run(&K::RadialBlur { centre_x_px: 9.0, centre_y_px: 3.5, amount: 60.0, kind }, &p);
        assert_eq!(got.data, radial_blur_window(SampleRange::Unit, &p.data, p.bounds, p.bounds, [9.0, 3.5], 60.0, kind, BLUR).unwrap());
    }
    let m = |length_px| K::MotionBlur { angle_deg: 0.0, length_px };
    m(1_000.0).validate().unwrap();
    assert_eq!(m(1_000.5).validate(), Err(FilterError::Parameter), "the table's 1000, not the kernel's own 6000");
    let r = |amount| K::RadialBlur { centre_x_px: 0.0, centre_y_px: 0.0, amount, kind: RadialBlurKind::Spin };
    r(1_000.0).validate().unwrap();
    assert_eq!(r(1_000.5).validate(), Err(FilterError::Parameter));
}

#[test]
fn item1_find_edges_is_the_shared_kernel() {
    let p = busy(16, 12);
    for invert in [false, true] {
        let got = run(&K::FindEdges { invert }, &p);
        assert_eq!(got.data, find_edges_window(&p.data, p.bounds, p.bounds, invert, LIMITS).unwrap());
    }
}

// ---------------------------------------------------------------------------------------------
// Item 2: the convolution family.
// ---------------------------------------------------------------------------------------------

#[test]
fn item2_custom_convolution_formula_and_border() {
    // The identity kernel returns every pixel with alpha > 0 unchanged.
    let mut id = [0; 25];
    id[12] = 1;
    let p = busy(12, 9);
    let got = run(&K::CustomConvolution { weights: id, scale: 1, offset: 0 }, &p);
    for (g, o) in got.data.iter().zip(&p.data) {
        assert_eq!(*g, if o[3] == 0.0 { [0.0; 4] } else { *o });
    }
    // Replicated border: a kernel reading only (-2, -2) copies the nearest pixel at the corner.
    let mut corner = [0; 25];
    corner[0] = 1;
    let got = run(&K::CustomConvolution { weights: corner, scale: 1, offset: 0 }, &flat(3, 3, [0.25, 0.5, 0.75, 1.0]));
    assert_eq!(one(&got, 0, 0), [0.25, 0.5, 0.75, 1.0]);
    // Scale and offset by hand: (2 * 0.5) / 4 + 51 / 255 = 0.45; a negative scale clamps to 0.
    let mut two = [0; 25];
    two[12] = 2;
    let got = run(&K::CustomConvolution { weights: two, scale: 4, offset: 51 }, &flat(3, 3, [0.5, 0.5, 0.5, 1.0]));
    near(one(&got, 1, 1), [0.45, 0.45, 0.45, 1.0], 1e-6);
    let got = run(&K::CustomConvolution { weights: two, scale: -1, offset: 0 }, &flat(3, 3, [0.5, 0.5, 0.5, 1.0]));
    assert_eq!(one(&got, 1, 1), [0.0, 0.0, 0.0, 1.0]);
    // Alpha is the source's; un-premultiplied by the source alpha.
    let got = run(&K::CustomConvolution { weights: id, scale: 1, offset: 0 }, &flat(3, 3, [0.5, 0.25, 1.0, 0.5]));
    assert_eq!(one(&got, 1, 1), [0.5, 0.25, 1.0, 0.5]);
    assert_eq!(K::CustomConvolution { weights: id, scale: 0, offset: 0 }.validate(), Err(FilterError::Parameter));
}

#[test]
fn item2_fixed_kernels_by_hand() {
    // Sharpen on a one-pixel bump in flat 0.25: centre 5 * 0.5 - 4 * 0.25 = 1.5 clamps to 1; the
    // 4-neighbour 5 * 0.25 - 3 * 0.25 - 0.5 = 0.
    let mut p = flat(5, 5, [0.25, 0.25, 0.25, 1.0]);
    p.data[12] = [0.5, 0.5, 0.5, 1.0];
    let s = run(&K::Sharpen, &p);
    assert_eq!(one(&s, 2, 2)[0], 1.0);
    assert_eq!(one(&s, 2, 1)[0], 0.0);
    assert_eq!(one(&s, 1, 1)[0], 0.25, "sharpen has no corner weight");
    let m = run(&K::SharpenMore, &p);
    assert_eq!(one(&m, 1, 1)[0], 0.0, "sharpen more has corner weight -1: 9*0.25 - 7*0.25 - 0.5");
    // Blurs preserve a flat plane exactly.
    let f = flat(7, 7, [0.25, 0.5, 0.75, 1.0]);
    for k in [K::Sharpen, K::SharpenMore, K::BasicBlur, K::BlurMore] {
        assert_eq!(run(&k, &f).data, f.data, "{}", k.key());
    }
}

/// STU-FX-013 row: the more step is about 3 to 4 times stronger. Measured as the impulse variance
/// per axis: 0.5 and 1.5.
#[test]
fn item2_blur_more_is_three_times_basic_blur_by_impulse_variance() {
    let p = impulse(9, 9);
    let var = |k: &K| {
        let g = run(k, &p);
        let (mut sum, mut v) = (0.0_f64, 0.0_f64);
        for y in 0..9u32 {
            for x in 0..9u32 {
                let w = f64::from(one(&g, x, y)[0]);
                sum += w;
                v += w * f64::from(x as i32 - 4).powi(2);
            }
        }
        v / sum
    };
    let (b, m) = (var(&K::BasicBlur), var(&K::BlurMore));
    assert!((b - 0.5).abs() < 1e-6, "{b}");
    assert!((m - 1.5).abs() < 1e-6, "{m}");
    assert!((m / b - 3.0).abs() < 1e-5);
}

// ---------------------------------------------------------------------------------------------
// Item 3: edge-gated and point kinds.
// ---------------------------------------------------------------------------------------------

fn edge_plane() -> RgbaPlane {
    // Left half 0.2 with a weak speck, right half 0.9: a strong edge at x = 6.
    let mut data = Vec::new();
    for y in 0..9u32 {
        for x in 0..12u32 {
            let v = if x < 6 { 0.2 } else { 0.9 };
            data.push([v, v, v, 1.0]);
            let _ = y;
        }
    }
    data[12 * 2 + 2] = [0.21, 0.21, 0.21, 1.0];
    RgbaPlane { bounds: rect(0, 0, 12, 9), data }
}

#[test]
fn item3_sharpen_edges_touches_only_edges_and_despeckle_only_flat_areas() {
    let p = edge_plane();
    let s = run(&K::SharpenEdges, &p);
    let d = run(&K::Despeckle, &p);
    let sharp = run(&K::Sharpen, &p);
    let blur = run(&K::BasicBlur, &p);
    assert_eq!(one(&s, 6, 4), one(&sharp, 6, 4), "edge takes sharpen");
    assert_eq!(one(&s, 2, 2), one(&p, 2, 2), "the weak speck is below the gate: source bytes");
    assert_eq!(one(&s, 10, 7), one(&p, 10, 7));
    assert_eq!(one(&d, 2, 2), one(&blur, 2, 2), "flat area takes the blur");
    assert_ne!(one(&d, 2, 2), one(&p, 2, 2));
    assert_eq!(one(&d, 6, 4), one(&p, 6, 4), "edge keeps the source");
    assert_eq!(one(&d, 5, 4), one(&p, 5, 4));
}

/// Find Edges through the extracted Sobel magnitude keeps its golden: unit step gives magnitude 1.
#[test]
fn item3_sobel_extraction_keeps_find_edges() {
    let p = edge_plane();
    let e = run(&K::FindEdges { invert: true }, &p);
    let want = |x: u32, y: u32| {
        let at = |dx: i32, dy: i32| -> f64 {
            let nx = (x as i32 + dx).clamp(0, 11) as u32;
            let ny = (y as i32 + dy).clamp(0, 8) as u32;
            f64::from(one(&p, nx, ny)[0])
        };
        let gx = (at(1, -1) + 2.0 * at(1, 0) + at(1, 1)) - (at(-1, -1) + 2.0 * at(-1, 0) + at(-1, 1));
        let gy = (at(-1, 1) + 2.0 * at(0, 1) + at(1, 1)) - (at(-1, -1) + 2.0 * at(0, -1) + at(1, -1));
        (((gx * gx + gy * gy).sqrt() / 4.0).clamp(0.0, 1.0)) as f32
    };
    for (x, y) in [(6, 4), (5, 4), (2, 2), (0, 0), (11, 8)] {
        assert_eq!(one(&e, x, y)[0], want(x, y), "({x},{y})");
    }
}

#[test]
fn item3_emboss_by_hand() {
    // A vertical unit-luma step: angle 0 reads L(x + h) - L(x - h).
    let mut data = Vec::new();
    for _ in 0..5u32 {
        for x in 0..9u32 {
            let v = if x < 4 { 0.0 } else { 1.0 };
            data.push([v, v, v, 1.0]);
        }
    }
    let p = RgbaPlane { bounds: rect(0, 0, 9, 5), data };
    let g = run(&K::Emboss { angle_deg: 0.0, height_px: 1.0, amount_pct: 25.0 }, &p);
    // At x = 3: L(4) - L(2) = 1, so 0.5 + 0.25.
    near(one(&g, 3, 2), [0.75, 0.75, 0.75, 1.0], 1e-6);
    near(one(&g, 6, 2), [0.5, 0.5, 0.5, 1.0], 1e-6);
    // 180 degrees flips the relief; a half pixel height reads bilinear.
    let flip = run(&K::Emboss { angle_deg: 180.0, height_px: 1.0, amount_pct: 25.0 }, &p);
    near(one(&flip, 3, 2), [0.25, 0.25, 0.25, 1.0], 1e-6);
    let half = run(&K::Emboss { angle_deg: 0.0, height_px: 0.5, amount_pct: 100.0 }, &p);
    near(one(&half, 3, 2), [1.0, 1.0, 1.0, 1.0], 1e-6);
    // Angle 90 reads along screen up: a horizontal step only.
    let up = run(&K::Emboss { angle_deg: 90.0, height_px: 1.0, amount_pct: 100.0 }, &p);
    near(one(&up, 3, 2), [0.5, 0.5, 0.5, 1.0], 1e-6);
    // Alpha is the source's; transparent stays transparent.
    let t = run(&K::Emboss { angle_deg: 0.0, height_px: 1.0, amount_pct: 100.0 }, &flat(3, 3, [0.5, 0.5, 0.5, 0.0]));
    assert_eq!(one(&t, 1, 1), [0.0; 4]);
    let bad = |angle_deg, height_px, amount_pct| K::Emboss { angle_deg, height_px, amount_pct }.validate();
    assert_eq!(bad(0.0, -0.5, 1.0), Err(FilterError::Parameter));
    assert_eq!(bad(0.0, 1.0, -0.5), Err(FilterError::Parameter));
    bad(-400.0, 1.0, 1.0).unwrap();
}

#[test]
fn item3_solarize_formula_and_bounds() {
    let p = RgbaPlane { bounds: rect(0, 0, 3, 1), data: vec![[0.25, 0.75, 1.0, 0.5], [0.5, 0.5, 0.5, 1.0], [0.9, 0.1, 0.0, 0.0]] };
    let g = run(&K::Solarize { threshold_levels: 127 }, &p);
    assert_eq!(g.data[0], [0.25, 0.25, 0.0, 0.5], "alpha kept");
    assert_eq!(g.data[1], [0.5, 0.5, 0.5, 1.0], "0.5 * 255 > 127, so 1 - 0.5");
    assert_eq!(g.data[2], [0.0; 4], "transparent source");
    let zero = run(&K::Solarize { threshold_levels: 0 }, &flat(1, 1, [0.0, 0.25, 1.0, 1.0]));
    assert_eq!(zero.data[0], [0.0, 0.75, 0.0, 1.0], "strictly above the threshold");
    K::Solarize { threshold_levels: 254 }.validate().unwrap();
    assert_eq!(K::Solarize { threshold_levels: 255 }.validate(), Err(FilterError::Parameter));
}

#[test]
fn item3_mosaic_grid_and_mean() {
    // 5 x 2 into 2 x 1: columns split at floor(1 * 5 / 2) = 2.
    let data = vec![
        [1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.0],
    ];
    let p = RgbaPlane { bounds: rect(10, 20, 5, 2), data };
    let g = run(&K::Mosaic { horizontal_blocks: 2, vertical_blocks: 1 }, &p);
    for x in 0..2 {
        for y in 0..2 {
            assert_eq!(one(&g, x, y), [0.5, 0.0, 0.0, 1.0]);
        }
    }
    // Right cell: premultiplied mean, alpha 5/6, colour stays pure green.
    for x in 2..5 {
        near(one(&g, x, 0), [0.0, 1.0, 0.0, 5.0 / 6.0], 1e-6);
    }
    // More blocks than pixels is the identity up to the premultiplied round trip.
    let many = busy(7, 5);
    let id = run(&K::Mosaic { horizontal_blocks: 4_000, vertical_blocks: 4_000 }, &many);
    for (a, b) in id.data.iter().zip(&many.data) {
        if b[3] > 0.0 {
            near(*a, *b, 2e-6);
        } else {
            assert_eq!(*a, [0.0; 4]);
        }
    }
    let m = |h, v| K::Mosaic { horizontal_blocks: h, vertical_blocks: v }.validate();
    m(1, 4_000).unwrap();
    assert_eq!(m(0, 1), Err(FilterError::Parameter));
    assert_eq!(m(1, 4_001), Err(FilterError::Parameter));
}

/// V4-06 class (c): the plane a kernel sees is the plan extent cut to the content, so these are
/// pinned as plane-dependent. No extent independence is claimed.
#[test]
fn item3_a_cropped_plane_moves_the_mosaic_grid_and_the_replicated_border() {
    let p = busy(12, 10);
    let crop = |x0: u32, w: u32| {
        let mut data = Vec::new();
        for y in 0..10 {
            for x in x0..x0 + w {
                data.push(one(&p, x, y));
            }
        }
        RgbaPlane { bounds: rect(p.bounds.x + i64::from(x0), p.bounds.y, w, 10), data }
    };
    let c = crop(3, 9);
    let k = K::Mosaic { horizontal_blocks: 3, vertical_blocks: 2 };
    assert_ne!(one(&run(&k, &p), 6, 2), one(&run(&k, &c), 3, 2), "the grid is anchored to the plane");
    let s = K::BasicBlur;
    assert_ne!(one(&run(&s, &p), 3, 5), one(&run(&s, &c), 0, 5), "a neighbourhood kind differs at the cropped edge");
    assert_eq!(one(&run(&s, &p), 8, 5), one(&run(&s, &c), 5, 5), "and agrees away from it");
}

// ---------------------------------------------------------------------------------------------
// Item 4: morphology.
// ---------------------------------------------------------------------------------------------

fn pm(p: [f32; 4]) -> [f32; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

/// Brute force over the premultiplied plane, transparent beyond it.
fn morph_oracle(p: &RgbaPlane, r: i64, round: bool, maximum: bool) -> Vec<[f32; 4]> {
    let (w, h) = (i64::from(p.bounds.width), i64::from(p.bounds.height));
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let mut acc = if maximum { [f32::NEG_INFINITY; 4] } else { [f32::INFINITY; 4] };
            for dy in -r..=r {
                for dx in -r..=r {
                    if round && dx * dx + dy * dy > r * r {
                        continue;
                    }
                    let (nx, ny) = (x + dx, y + dy);
                    let q = if nx < 0 || ny < 0 || nx >= w || ny >= h { [0.0; 4] } else { pm(p.data[(ny * w + nx) as usize]) };
                    for k in 0..4 {
                        acc[k] = if maximum { acc[k].max(q[k]) } else { acc[k].min(q[k]) };
                    }
                }
            }
            let a = acc[3];
            let c = |v: f32| if a <= 0.0 { 0.0 } else { (v / a).clamp(0.0, 1.0) };
            out.push(if a <= 0.0 { [0.0; 4] } else { [c(acc[0]), c(acc[1]), c(acc[2]), a.min(1.0)] });
        }
    }
    out
}

#[test]
fn item4_morphology_equals_the_brute_force_oracle() {
    let p = busy(13, 9);
    for r in [1u32, 2, 3, 5, 6, 8, 14, 40, 32_000] {
        for preserve in [MorphPreserve::Squareness, MorphPreserve::Roundness] {
            if r > 40 && preserve == MorphPreserve::Roundness {
                continue;
            }
            let oracle_r = i64::from(r.min(40));
            let round = preserve == MorphPreserve::Roundness;
            let g = run(&K::Maximum { radius_px: r, preserve }, &p);
            assert_eq!(g.data, morph_oracle(&p, oracle_r, round, true), "max r{r} {preserve:?}");
            let g = run(&K::Minimum { radius_px: r, preserve }, &p);
            assert_eq!(g.data, morph_oracle(&p, oracle_r, round, false), "min r{r} {preserve:?}");
        }
    }
}

#[test]
fn item4_radius_zero_is_the_identity_and_the_bound_holds() {
    let p = busy(9, 7);
    for preserve in [MorphPreserve::Squareness, MorphPreserve::Roundness] {
        assert_eq!(run(&K::Maximum { radius_px: 0, preserve }, &p).data, p.data);
        assert_eq!(run(&K::Minimum { radius_px: 0, preserve }, &p).data, p.data);
    }
    K::Maximum { radius_px: 32_000, preserve: MorphPreserve::Squareness }.validate().unwrap();
    assert_eq!(K::Minimum { radius_px: 32_001, preserve: MorphPreserve::Squareness }.validate(), Err(FilterError::Parameter));
}

#[test]
fn item4_squareness_work_is_independent_of_radius() {
    let out = rect(0, 0, 4_000, 3_000);
    let c = |r| K::Maximum { radius_px: r, preserve: MorphPreserve::Squareness }.cost(out, LIMITS).unwrap();
    assert_eq!(c(1).1, c(32_000).1);
    let round = |r| K::Maximum { radius_px: r, preserve: MorphPreserve::Roundness }.cost(out, LIMITS);
    assert!(round(8).unwrap().1 > c(8).1);
    // The disc's cost walks only O(r) rows and is refused before the disc exists.
    let start = Instant::now();
    assert!(matches!(K::Minimum { radius_px: 32_000, preserve: MorphPreserve::Roundness }.admit(out, LIMITS), Err(FilterError::OverBudget { .. })));
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ---------------------------------------------------------------------------------------------
// Item 5: rank and edge-preserving.
// ---------------------------------------------------------------------------------------------

#[test]
fn item5_dust_and_scratches_threshold_zero_is_median() {
    let p = busy(12, 9);
    for r in [1u32, 2, 4] {
        let d = run(&K::DustAndScratches { radius_px: r, threshold: 0.0 }, &p);
        let m = filter_plane(&p, &LayerFilter::Median { radius_px: r }, LIMITS).unwrap();
        assert_eq!(d.data, m.data, "radius {r}");
    }
}

#[test]
fn item5_dust_and_scratches_keeps_what_is_inside_the_threshold() {
    let mut p = flat(9, 9, [0.5, 0.5, 0.5, 1.0]);
    p.data[40] = [0.9, 0.55, 0.5, 1.0];
    p.data[4] = [0.52, 0.5, 0.5, 1.0];
    let g = run(&K::DustAndScratches { radius_px: 1, threshold: 0.1 }, &p);
    assert_eq!(one(&g, 4, 4), [0.5, 0.55, 0.5, 1.0], "per channel: only the outlier channel is replaced");
    assert_eq!(one(&g, 4, 0), [0.52, 0.5, 0.5, 1.0], "inside the threshold");
    assert_eq!(run(&K::DustAndScratches { radius_px: 0, threshold: 0.1 }, &p).data, p.data, "radius 0 is the identity");
    let v = |r, t| K::DustAndScratches { radius_px: r, threshold: t }.validate();
    v(255, 1.0).unwrap();
    assert_eq!(v(256, 0.5), Err(FilterError::Parameter));
    assert_eq!(v(1, 1.01), Err(FilterError::Parameter));
    assert_eq!(v(1, -0.01), Err(FilterError::Parameter));
}

#[test]
fn item5_surface_blur_formula_by_hand() {
    // Three pixels in a row, radius 1, t = 51 levels: weight 1 - d * 255 / (2.5 * 51) = 1 - 2 d.
    // o = 0.5, neighbours 0.25 (w 0.5) and 1.0 (w 0): (0.5 + 0.125) / 1.5.
    let p = RgbaPlane { bounds: rect(0, 0, 3, 1), data: vec![[0.25, 0.25, 0.25, 1.0], [0.5, 0.5, 0.5, 1.0], [1.0, 1.0, 1.0, 1.0]] };
    let g = run(&K::SurfaceBlur { radius_px: 1, threshold_levels: 51.0 }, &p);
    let want = (0.5 + 0.125) / 1.5;
    near(g.data[1], [want, want, want, 1.0], 1e-6);
    assert_eq!(g.data[2][0], 1.0, "the far neighbour has weight 0: the edge survives");
    assert_eq!(run(&K::SurfaceBlur { radius_px: 0, threshold_levels: 5.0 }, &p).data, p.data);
    let f = flat(6, 6, [0.25, 0.5, 0.75, 1.0]);
    assert_eq!(run(&K::SurfaceBlur { radius_px: 2, threshold_levels: 5.0 }, &f).data, f.data);
    assert_eq!(K::SurfaceBlur { radius_px: 1, threshold_levels: 0.0 }.validate(), Err(FilterError::Parameter));
}

#[test]
fn item5_bilateral_blur_formula_by_hand() {
    // Radius 2: spatial sigma 1. o = 0.5 in the centre of five; neighbours at d = 1 equal o, so with
    // a tiny range sigma the far-valued neighbours vanish and the pixel keeps its value.
    let v = [0.0, 0.5, 0.5, 0.5, 1.0].map(|c: f32| [c, c, c, 1.0]);
    let p = RgbaPlane { bounds: rect(0, 0, 5, 1), data: v.to_vec() };
    let sharp = run(&K::BilateralBlur { radius_px: 2, range_sigma: 1e-3 }, &p);
    assert_eq!(sharp.data[2][0], 0.5);
    assert_eq!(sharp.data[0][0], 0.0, "the step survives");
    // A huge range sigma is the plain spatial Gaussian, normalised over the samples in the plane.
    let wide = run(&K::BilateralBlur { radius_px: 2, range_sigma: 1e6 }, &p);
    let s = |d: f64| (-(d * d) / 2.0).exp();
    let want = (s(1.0) * 0.5 * 2.0 + 0.5 + s(2.0) * 1.0) / (1.0 + 2.0 * s(1.0) + 2.0 * s(2.0));
    assert!((f64::from(wide.data[2][0]) - want).abs() < 1e-5, "{} vs {want}", wide.data[2][0]);
    assert_eq!(run(&K::BilateralBlur { radius_px: 0, range_sigma: 1.0 }, &p).data, p.data);
    assert_eq!(K::BilateralBlur { radius_px: 1, range_sigma: 0.0 }.validate(), Err(FilterError::Parameter));
}

/// V4-06 class (d): the cost of an unbounded radius is O(1).
#[test]
fn item5_unbounded_radii_are_refused_quickly() {
    let out = rect(0, 0, 64, 64);
    let start = Instant::now();
    for _ in 0..1_000 {
        assert!(matches!(K::SurfaceBlur { radius_px: u32::MAX, threshold_levels: 1.0 }.admit(out, LIMITS), Err(FilterError::OverBudget { .. })));
        assert!(matches!(K::BilateralBlur { radius_px: u32::MAX, range_sigma: 1.0 }.admit(out, LIMITS), Err(FilterError::OverBudget { .. })));
    }
    assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
}

// ---------------------------------------------------------------------------------------------
// Common contract and hostile input, every kind.
// ---------------------------------------------------------------------------------------------

#[test]
fn every_kind_stays_in_the_unit_range_and_is_deterministic() {
    let p = busy(17, 13);
    for k in all_kinds() {
        let g = run(&k, &p);
        assert_eq!(g.bounds, p.bounds);
        assert_eq!(g.data.len(), p.data.len());
        assert!(g.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{}", k.key());
        assert_ne!(g.data, p.data, "{} changed nothing", k.key());
        assert_eq!(g.data, run(&k, &p).data, "{}", k.key());
    }
}

#[test]
fn every_kind_refuses_a_bad_sample_and_a_bad_shape() {
    let mut bad = busy(9, 7);
    bad.data[20] = [f32::NAN, 0.0, 0.0, 1.0];
    let mut over = busy(9, 7);
    over.data[20] = [1.5, 0.0, 0.0, 1.0];
    let mut short = busy(9, 7);
    short.data.pop();
    for k in all_kinds() {
        assert!(matches!(k.render_plane(&bad, LIMITS), Err(FilterError::OutOfDomain)), "{} NaN", k.key());
        assert!(matches!(k.render_plane(&over, LIMITS), Err(FilterError::OutOfDomain)), "{} 1.5", k.key());
        assert_eq!(k.render_plane(&short, LIMITS).unwrap_err(), FilterError::Shape, "{}", k.key());
    }
}

#[test]
fn every_kind_takes_a_zero_size_plane_and_refuses_a_huge_one_before_allocating() {
    let tiny = FilterLimits { max_bytes: 1 << 20, max_work: 1 << 20 };
    for k in all_kinds() {
        let empty = RgbaPlane { bounds: rect(5, 5, 0, 0), data: Vec::new() };
        assert_eq!(k.render_plane(&empty, LIMITS).unwrap().data.len(), 0, "{}", k.key());
        let huge = RgbaPlane { bounds: rect(0, 0, 65_536, 65_536), data: Vec::new() };
        let start = Instant::now();
        let e = k.render_plane(&huge, StackLimits::default().filter).unwrap_err();
        assert!(matches!(e, FilterError::OverBudget { .. }), "{}: {e:?}", k.key());
        assert!(start.elapsed() < Duration::from_secs(1), "{}", k.key());
        assert!(matches!(k.render_plane(&busy(512, 512), tiny), Err(FilterError::OverBudget { .. })), "{}", k.key());
    }
}

#[test]
fn every_float_refuses_nan_and_infinity() {
    let d = BoxDimensions::Horizontal;
    let spin = RadialBlurKind::Spin;
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for k in [
            K::BoxBlur { radius_px: v, iterations: 1, dimensions: d },
            K::MotionBlur { angle_deg: v, length_px: 1.0 },
            K::MotionBlur { angle_deg: 0.0, length_px: v },
            K::RadialBlur { centre_x_px: v, centre_y_px: 0.0, amount: 1.0, kind: spin },
            K::RadialBlur { centre_x_px: 0.0, centre_y_px: v, amount: 1.0, kind: spin },
            K::RadialBlur { centre_x_px: 0.0, centre_y_px: 0.0, amount: v, kind: spin },
            K::Emboss { angle_deg: v, height_px: 1.0, amount_pct: 1.0 },
            K::Emboss { angle_deg: 0.0, height_px: v, amount_pct: 1.0 },
            K::Emboss { angle_deg: 0.0, height_px: 1.0, amount_pct: v },
            K::DustAndScratches { radius_px: 1, threshold: v },
            K::SurfaceBlur { radius_px: 1, threshold_levels: v },
            K::BilateralBlur { radius_px: 1, range_sigma: v },
        ] {
            assert_eq!(k.validate(), Err(FilterError::Parameter), "{k:?}");
            assert_eq!(k.render_plane(&flat(2, 2, [0.5; 4]), LIMITS).unwrap_err(), FilterError::Parameter, "{k:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The stack.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_stack_runs_every_kind_with_enable_opacity_and_refuses_a_bad_one_before_any_filter() {
    let p = busy(17, 13);
    for k in all_kinds() {
        let mut e = StackEntry::new(StackFilter::Kernel(k.clone()));
        let full = SmartFilterStack { entries: vec![e.clone()], stack_mask: None }.render(&p, &StackLimits::default()).unwrap();
        assert_eq!(full.data, run(&k, &p).data, "{}: stack is the kernel", k.key());
        e.enabled = false;
        let off = SmartFilterStack { entries: vec![e], stack_mask: None }.render(&p, &StackLimits::default()).unwrap();
        assert_eq!(off.data, p.data, "{}: disabled", k.key());
    }
    let bad = StackEntry::new(StackFilter::Kernel(K::Solarize { threshold_levels: 255 }));
    let good = StackEntry::new(StackFilter::Kernel(K::Sharpen));
    let e = SmartFilterStack { entries: vec![good, bad], stack_mask: None }.render(&p, &StackLimits::default()).unwrap_err();
    assert_eq!(e.code(), "filter_parameter");
    let many = vec![StackEntry::new(StackFilter::Kernel(K::Sharpen)); 17];
    let e = SmartFilterStack { entries: many, stack_mask: None }.render(&p, &StackLimits::default()).unwrap_err();
    assert_eq!(e.code(), "smart_filter_too_many_entries");
}

#[test]
fn the_descriptor_names_every_kind() {
    let v: serde_json::Value = serde_json::from_str(hsk_studio_pigment::filter_kernels::DESCRIPTOR).unwrap();
    assert_eq!(v["parity"], "NOT_PROVEN");
    for k in all_kinds() {
        assert!(v["kinds"].get(k.key()).is_some(), "{}", k.key());
    }
    assert!(!v["gaps"].as_array().unwrap().is_empty());
}
