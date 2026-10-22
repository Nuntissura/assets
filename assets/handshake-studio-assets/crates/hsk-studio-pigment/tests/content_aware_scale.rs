//! Lane SMB-PIGMENT-CONTENT-AWARE-2 slice 3: content-aware scale (seam carving with a protection
//! plane) and inpaint as the named fill (STU-RAS-143 "content-aware scale ... and inpaint/remove").
//! Synthetic scenes whose low-energy columns are known bound the result exactly. Photoshop parity
//! is not claimed (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::content_aware::*;
use hsk_studio_pigment::retouch::{RetouchLimits, RgbaPlane};
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

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

/// Bounding box and count of the pixels equal to `c`.
fn blob(p: &RgbaPlane, c: [f32; 4]) -> (Option<Rect>, usize) {
    let (ex, ey) = p.bounds.end().unwrap();
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN, 0usize);
    for y in p.bounds.y..ey {
        for x in p.bounds.x..ex {
            if p.at(x, y) == c {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                n += 1;
            }
        }
    }
    if n == 0 {
        return (None, 0);
    }
    (Some(rect(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32)), n)
}

#[test]
fn narrowing_and_widening_keep_objects_intact() {
    let limits = RetouchLimits::default();
    // Two 8 x 8 red objects on flat grey, 64 x 32 at origin (3, -2).
    let b = rect(3, -2, 64, 32);
    let object = |x: i64, y: i64| ((13..21).contains(&x) || (43..51).contains(&x)) && (8..16).contains(&y);
    let src = plane(b, |x, y| if object(x, y) { RED } else { GREY });
    let r = content_aware_scale(&src, 48, 32, None, &limits).unwrap();
    assert_eq!(r.plane.bounds, rect(3, -2, 48, 32));
    assert_eq!(r.receipt.removed_columns, 16);
    assert_eq!((r.receipt.inserted_columns, r.receipt.removed_rows, r.receipt.inserted_rows), (0, 0, 0));
    // Seams go through the flat columns: both objects keep 64 exact pixels each, 8 x 8 blocks;
    // a plain 3/4 resample would make them 6 wide.
    let (bb, n) = blob(&r.plane, RED);
    println!("narrow: red bbox {bb:?} n {n}; receipt {:?}", r.receipt);
    assert_eq!(n, 128);
    let bb = bb.unwrap();
    assert_eq!((bb.height, bb.y), (8, 8));
    for x in bb.x..bb.x + i64::from(bb.width) {
        let red_in_col = (8..16).filter(|y| r.plane.at(x, *y) == RED).count();
        assert!(red_in_col == 0 || red_in_col == 8, "column {x} cut an object");
    }
    // Widening (one step: 16 <= 64 / 2) and height changes keep the objects too.
    for (w, h) in [(96u32, 32u32), (64, 24), (64, 44), (40, 20)] {
        let r = content_aware_scale(&src, w, h, None, &limits).unwrap();
        let (bb, n) = blob(&r.plane, RED);
        println!("{w}x{h}: red bbox {bb:?} n {n}; receipt {:?}", r.receipt);
        assert_eq!(n, 128, "{w}x{h}");
        assert_eq!(r.plane.bounds, rect(3, -2, w, h));
        // Every non-red pixel is still the flat grey (copies or means of grey neighbours).
        assert!(r.plane.data.iter().all(|p| *p == RED || *p == GREY), "{w}x{h}");
    }
    // Same size: identical bytes, nothing done.
    let same = content_aware_scale(&src, 64, 32, None, &limits).unwrap();
    assert_eq!(same.plane, src);
    assert_eq!(same.receipt.removed_columns + same.receipt.inserted_columns, 0);
}

#[test]
fn protection_plane_keeps_flat_content_that_seams_prefer() {
    let limits = RetouchLimits::default();
    // Horizontal grey ramp (every pixel carries gradient energy) with a flat full-height 12 px band
    // whose inner columns are zero-energy seams (a shorter block would put its top and bottom
    // edges in every seam through it); narrowing by 16 cuts the band unless it is protected.
    let b = rect(0, 0, 64, 32);
    let in_block = |x: i64, _y: i64| (20..32).contains(&x);
    let src = plane(b, |x, y| {
        if in_block(x, y) {
            [0.5, 0.25, 0.75, 1.0]
        } else {
            let v = 0.2 + 0.6 * x as f32 / 63.0;
            [v, v, v, 1.0]
        }
    });
    let block_width = |p: &RgbaPlane| (0..p.bounds.width as i64).filter(|x| p.at(*x, 15) == [0.5, 0.25, 0.75, 1.0]).count();
    let plain = content_aware_scale(&src, 48, 32, None, &limits).unwrap();
    let protect: CoveragePlane = sel_rect(RectF { x: 20.0, y: 0.0, width: 12.0, height: 32.0 }, &SelectionContext::new(b)).unwrap();
    let kept = content_aware_scale(&src, 48, 32, Some(&protect), &limits).unwrap();
    println!(
        "unprotected block width {} crossings {}; protected width {} crossings {}",
        block_width(&plain.plane),
        plain.receipt.protected_crossings,
        block_width(&kept.plane),
        kept.receipt.protected_crossings
    );
    assert!(block_width(&plain.plane) < 12, "the control cuts the flat block");
    assert_eq!(block_width(&kept.plane), 12, "the protected block survives");
    assert_eq!(kept.receipt.protected_crossings, 0);
    // Protection outside the plane or empty changes nothing.
    let elsewhere = sel_rect(RectF { x: 100.0, y: 0.0, width: 4.0, height: 4.0 }, &SelectionContext::new(rect(100, 0, 4, 4))).unwrap();
    assert_eq!(content_aware_scale(&src, 48, 32, Some(&elsewhere), &limits).unwrap(), plain);
    // When every seam must cross protection, the crossings are reported, not hidden.
    let all = sel_rect(RectF { x: 0.0, y: 0.0, width: 64.0, height: 32.0 }, &SelectionContext::new(b)).unwrap();
    let forced = content_aware_scale(&src, 60, 32, Some(&all), &limits).unwrap();
    assert_eq!(forced.receipt.protected_crossings, 4 * 32);
}

#[test]
fn inpaint_is_the_native_fill_over_the_whole_plane() {
    let canvas = rect(0, 0, 48, 48);
    let stripes = |x: i64, _y: i64| if x.rem_euclid(4) < 2 { [0.1f32, 0.1, 0.1, 1.0] } else { [0.9, 0.9, 0.9, 1.0] };
    let target = plane(canvas, |x, y| if (20..28).contains(&x) && (20..28).contains(&y) { RED } else { stripes(x, y) });
    let mask = sel_rect(RectF { x: 20.0, y: 20.0, width: 8.0, height: 8.0 }, &SelectionContext::new(canvas)).unwrap();
    let limits = RetouchLimits::default();
    let o = ContentAwareOptions::default();
    let r = inpaint(&target, &mask, &o, &limits).unwrap();
    assert_eq!(r, fill(&target, &mask, SamplingArea::Auto, &o, &limits).unwrap());
    // The removed object is gone: the hole continues the stripes.
    for y in 20..28 {
        for x in 20..28 {
            let (a, e) = (r.result.plane.at(x, y), stripes(x, y));
            assert!((0..4).all(|k| (a[k] - e[k]).abs() <= 1e-5), "({x}, {y}): {a:?} vs {e:?}");
        }
    }
    // Same refusals as the fill.
    let bad = ContentAwareOptions { patch_radius: 0, ..o };
    assert_eq!(inpaint(&target, &mask, &bad, &limits).unwrap_err().code(), "retouch_out_of_range");
}

#[test]
fn hostile_scale_inputs_are_typed_refusals() {
    let limits = RetouchLimits::default();
    let src = plane(rect(0, 0, 16, 16), |x, _| if x % 2 == 0 { RED } else { GREY });
    let code = |r: Result<ScaleResult, hsk_studio_pigment::retouch::RetouchError>| r.unwrap_err().code();
    assert_eq!(code(content_aware_scale(&src, 0, 16, None, &limits)), "retouch_out_of_range");
    assert_eq!(code(content_aware_scale(&src, 16, 0, None, &limits)), "retouch_out_of_range");
    let empty = RgbaPlane { bounds: rect(0, 0, 0, 4), data: Vec::new() };
    assert_eq!(code(content_aware_scale(&empty, 4, 4, None, &limits)), "retouch_out_of_range");
    // Malformed planes.
    let bad_cov = CoveragePlane { bounds: rect(0, 0, 4, 4), data: vec![1; 3] };
    assert_eq!(code(content_aware_scale(&src, 8, 8, Some(&bad_cov), &limits)), "retouch_shape");
    let huge = RgbaPlane { bounds: rect(0, 0, u32::MAX, u32::MAX), data: Vec::new() };
    assert_eq!(code(content_aware_scale(&huge, 8, 8, None, &limits)), "retouch_shape");
    let mut nan = src.clone();
    nan.data[5] = [f32::NAN, 0.0, 0.0, 1.0];
    assert_eq!(code(content_aware_scale(&nan, 8, 8, None, &limits)), "retouch_out_of_domain");
    // u32::MAX output sizes: charged before allocation (bytes overflow or exceed the budget).
    let c = code(content_aware_scale(&src, u32::MAX, u32::MAX, None, &limits));
    assert!(c == "overflow" || c == "budget_exceeded", "{c}");
    let end = RgbaPlane { bounds: rect(i64::MAX - 20, 0, 16, 16), data: src.data.clone() };
    assert_eq!(code(content_aware_scale(&end, 32, 16, None, &limits)), "overflow");
    // Byte and work budgets.
    let bytes = RetouchLimits { max_bytes: 16 * 16 * 16, ..limits };
    assert_eq!(code(content_aware_scale(&src, 8, 8, None, &bytes)), "budget_exceeded");
    let work = RetouchLimits { max_work: 100, ..limits };
    assert_eq!(code(content_aware_scale(&src, 8, 8, None, &work)), "budget_exceeded");
    // The charged work is reported and bounds the run.
    let ok = content_aware_scale(&src, 8, 24, None, &limits).unwrap();
    assert!(ok.receipt.work > 0 && ok.receipt.removed_columns == 8 && ok.receipt.inserted_rows == 8);
    assert!(content_aware_scale(&src, 8, 24, None, &RetouchLimits { max_work: ok.receipt.work, ..limits }).is_ok());
}
