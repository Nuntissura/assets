//! Lane SMB-PIGMENT-CONTENT-AWARE-2 slice 1: the retouch two-argument content-aware entry and the
//! crop `content_aware_fill_on_expand` option call the native fill (STU-RAS-143 "content-aware
//! crop-expand"). Synthetic textures whose continuation is known bound the error. Photoshop parity
//! is not claimed (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::canvas_ops::{CropOptions, crop, crop_content_aware};
use hsk_studio_pigment::content_aware::{ContentAwareOptions, SamplingArea, fill};
use hsk_studio_pigment::retouch::{RetouchError, RetouchLimits, RgbaPlane, content_aware_fill};
use hsk_studio_pigment::selection::{CoveragePlane, RectF, SelectionContext, rect as sel_rect};

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

fn plane(bounds: Rect, f: impl Fn(i64, i64) -> [f32; 4]) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..i64::from(bounds.height) {
        for x in 0..i64::from(bounds.width) {
            data.push(f(bounds.x + x, bounds.y + y));
        }
    }
    RgbaPlane::new(bounds, data).unwrap()
}

fn grey(v: f32) -> [f32; 4] {
    [v, v, v, 1.0]
}

/// Vertical stripes of period 4.
fn stripes(x: i64, _y: i64) -> [f32; 4] {
    if x.rem_euclid(4) < 2 { grey(0.1) } else { grey(0.9) }
}

/// Mean and worst absolute RGBA error over the pixels of `out` where `pick` holds.
fn error(out: &RgbaPlane, pick: impl Fn(i64, i64) -> bool, expect: impl Fn(i64, i64) -> [f32; 4]) -> (f64, f64, usize) {
    let (ex, ey) = out.bounds.end().unwrap();
    let (mut sum, mut worst, mut n) = (0.0f64, 0.0f64, 0usize);
    for y in out.bounds.y..ey {
        for x in out.bounds.x..ex {
            if !pick(x, y) {
                continue;
            }
            let (a, b) = (out.at(x, y), expect(x, y));
            for k in 0..4 {
                let e = (f64::from(a[k]) - f64::from(b[k])).abs();
                sum += e;
                worst = worst.max(e);
            }
            n += 1;
        }
    }
    (sum / (4.0 * n.max(1) as f64), worst, n)
}

#[test]
fn retouch_content_aware_fill_is_the_native_fill() {
    let canvas = rect(0, 0, 48, 48);
    let target = plane(canvas, |x, y| {
        if (18..30).contains(&x) && (18..30).contains(&y) { [1.0, 0.0, 0.0, 1.0] } else { stripes(x, y) }
    });
    let h = sel_rect(RectF { x: 18.0, y: 18.0, width: 12.0, height: 12.0 }, &SelectionContext::new(canvas)).unwrap();
    let r = content_aware_fill(&target, &h).unwrap();
    let native = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions::default(), &RetouchLimits::default()).unwrap();
    // Same bytes and damage as content_aware::fill with the defaults.
    assert_eq!(r, native.result);
    let (mean, worst, n) = error(&r.plane, |x, y| h.at(x, y) > 0, stripes);
    println!("retouch content_aware_fill: {n} px mean {mean:.2e} worst {worst:.2e}");
    assert!(n == 144 && mean <= 0.02 && worst <= 0.1, "mean {mean} worst {worst}");
    assert_eq!(r.damage, Some(rect(18, 18, 12, 12)));
}

#[test]
fn crop_expand_fills_the_exposed_area() {
    let limits = RetouchLimits::default();
    let src = plane(rect(0, 0, 40, 40), stripes);
    let ca = CropOptions { content_aware_fill_on_expand: true, ..CropOptions::default() };
    // Expand 8 px to the left and 4 px down: the crop rect (-8, 0, 48, 44) exposes the 8 x 44 left
    // band and the bottom 4 rows; stripes of period 4 continue at the same phase (8 = 2 periods).
    let crop_rect = rect(-8, 0, 48, 44);
    let (r, receipt) = crop_content_aware(&src, crop_rect, &ca, &ContentAwareOptions::default(), &limits).unwrap();
    assert_eq!((r.canvas, r.offset), (rect(0, 0, 48, 44), (8, 0)));
    let exposed = |x: i64, y: i64| x < 8 || y >= 40;
    let (mean, worst, n) = error(&r.plane, exposed, stripes);
    println!("crop expand: exposed {n} px mean {mean:.2e} worst {worst:.2e}");
    assert!(n == 8 * 44 + 40 * 4 && mean <= 0.02 && worst <= 0.1, "mean {mean} worst {worst}");
    // Pixels that were inside the plane keep their bytes.
    let (_, kept, _) = error(&r.plane, |x, y| !exposed(x, y), |x, y| src.at(x - 8, y));
    assert_eq!(kept, 0.0);
    let rc = receipt.expect("fill receipt");
    assert!(!rc.levels.is_empty() && rc.blend.is_some_and(|b| b.converged));
    // `crop` is the same operation with the default fill options.
    assert_eq!(crop(&src, crop_rect, &ca, &limits).unwrap(), r);
    // No expansion: no fill runs, bytes equal the plain crop.
    let inside = rect(4, 4, 20, 20);
    let (r2, none) = crop_content_aware(&src, inside, &ca, &ContentAwareOptions::default(), &limits).unwrap();
    assert!(none.is_none());
    assert_eq!(r2, crop(&src, inside, &CropOptions::default(), &limits).unwrap());
    // Thread count does not change the bytes.
    let t = ContentAwareOptions { threads: 4, ..ContentAwareOptions::default() };
    assert_eq!(crop_content_aware(&src, crop_rect, &ca, &t, &limits).unwrap().0, r);
}

#[test]
fn rotated_and_keep_pixels_crop_expand_leave_no_transparent_pixel() {
    let limits = RetouchLimits::default();
    // Rotated crop of a flat opaque plane: the rotated corners leave the plane and read
    // transparent without the option; with it every pixel is the flat grey again.
    let flat = plane(rect(0, 0, 48, 48), |_, _| grey(0.5));
    let rot = CropOptions { angle_deg: 12.0, ..CropOptions::default() };
    let r0 = crop(&flat, rect(2, 2, 44, 44), &rot, &limits).unwrap();
    assert!(r0.plane.data.iter().any(|p| p[3] < 0.5), "the rotated corners are exposed");
    let ca = CropOptions { content_aware_fill_on_expand: true, ..rot };
    let (r, receipt) = crop_content_aware(&flat, rect(2, 2, 44, 44), &ca, &ContentAwareOptions::default(), &limits).unwrap();
    let (mean, worst, _) = error(&r.plane, |_, _| true, |_, _| grey(0.5));
    println!("rotated crop expand: mean {mean:.2e} worst {worst:.2e}");
    assert!(worst <= 1e-3, "rotated crop expand worst {worst}");
    assert!(receipt.is_some() && r.rotation_deg == 12.0);
    // Keep pixels (angle 0): the plane moves and grows to the union with the canvas; only canvas
    // pixels outside the moved plane are filled, hidden pixels survive.
    let src = plane(rect(0, 0, 32, 32), stripes);
    let keep = CropOptions { delete_cropped_pixels: false, content_aware_fill_on_expand: true, ..CropOptions::default() };
    let (k, kr) = crop_content_aware(&src, rect(4, -8, 24, 44), &keep, &ContentAwareOptions::default(), &limits).unwrap();
    // Moved plane (-4, 8, 32, 32); canvas (0, 0, 24, 44); union (-4, 0, 32, 44).
    assert_eq!((k.canvas, k.offset, k.plane.bounds), (rect(0, 0, 24, 44), (-4, 8), rect(-4, 0, 32, 44)));
    let exposed = |x: i64, y: i64| (0..24).contains(&x) && !(8..40).contains(&y);
    let (mean, worst, n) = error(&k.plane, exposed, |x, y| stripes(x + 4, y));
    println!("keep-pixels crop expand: {n} px mean {mean:.2e} worst {worst:.2e}");
    assert!(n == 24 * 12 && mean <= 0.02 && worst <= 0.1, "mean {mean} worst {worst}");
    assert_eq!(k.plane.at(-4, 8), src.at(0, 0), "hidden pixel kept");
    assert_eq!(k.plane.at(-4, 0), [0.0; 4], "outside the canvas and the plane: not filled");
    assert!(kr.is_some());
}

#[test]
fn hostile_crop_expand_inputs_are_typed_refusals() {
    let limits = RetouchLimits::default();
    let src = plane(rect(0, 0, 32, 32), stripes);
    let ca = CropOptions { content_aware_fill_on_expand: true, ..CropOptions::default() };
    let code = |r: Result<_, RetouchError>| r.map(|_: (_, _)| ()).unwrap_err().code();
    let d = ContentAwareOptions::default();
    // Fill options are validated (never clamped), even before the crop runs.
    let bad = ContentAwareOptions { patch_radius: 0, ..d };
    assert_eq!(code(crop_content_aware(&src, rect(-4, 0, 36, 32), &ca, &bad, &limits)), "retouch_out_of_range");
    let threads = ContentAwareOptions { threads: 65, ..d };
    assert_eq!(code(crop_content_aware(&src, rect(0, 0, 8, 8), &ca, &threads, &limits)), "retouch_out_of_range");
    // Byte and work budgets of the fill.
    let small = RetouchLimits { max_bytes: 36 * 32 * 16 * 2, ..limits };
    assert_eq!(code(crop_content_aware(&src, rect(-4, 0, 36, 32), &ca, &d, &small)), "budget_exceeded");
    let work = RetouchLimits { max_work: 1_000, ..limits };
    assert_eq!(code(crop_content_aware(&src, rect(-4, 0, 36, 32), &ca, &d, &work)), "budget_exceeded");
    // A crop entirely outside the plane leaves nothing to sample.
    assert_eq!(code(crop_content_aware(&src, rect(100, 100, 16, 16), &ca, &d, &limits)), "content_aware_no_source");
    // Crop rect overflow and malformed planes keep the crop's own refusals.
    assert_eq!(code(crop_content_aware(&src, rect(i64::MAX - 2, 0, 8, 8), &ca, &d, &limits)), "overflow");
    let bad_plane = RgbaPlane { bounds: rect(0, 0, u32::MAX, u32::MAX), data: Vec::new() };
    assert_eq!(code(crop_content_aware(&bad_plane, rect(0, 0, 8, 8), &ca, &d, &limits)), "retouch_shape");
    // The two-argument retouch entry: hostile region planes are typed refusals.
    let huge = CoveragePlane { bounds: rect(0, 0, u32::MAX, u32::MAX), data: Vec::new() };
    assert_eq!(content_aware_fill(&src, &huge).unwrap_err().code(), "retouch_shape");
}
