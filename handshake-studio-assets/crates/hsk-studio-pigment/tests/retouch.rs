//! Retouch family (lane SMB-PIGMENT-RETOUCH): clone stamp (slice 1). Expected values are derived by
//! hand from the documented rules in `src/retouch.rs`.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::Dab;
use hsk_studio_pigment::retouch::*;
use hsk_studio_pigment::selection::{RectF, SelectionContext, rect as sel_rect};
use hsk_studio_pigment::transform_math::ResampleFilter;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

/// Opaque plane whose every pixel encodes its own coordinates (distinct values).
fn coded(bounds: Rect) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..bounds.height {
        for x in 0..bounds.width {
            data.push([x as f32 / 64.0, y as f32 / 64.0, 0.25, 1.0]);
        }
    }
    RgbaPlane::new(bounds, data).unwrap()
}

/// A hard round dab (hardness 100: coverage exactly 1 inside the radius).
fn dab(x: f64, y: f64, d: f64) -> Dab {
    Dab { x, y, diameter: d, opacity: 1.0, flow: 1.0, angle: 0.0, roundness: 1.0 }
}

#[test]
fn clone_stamp_copies_exact_pixels_and_aligned_keeps_one_offset() {
    let canvas = rect(0, 0, 48, 48);
    let target = coded(canvas);
    let limits = RetouchLimits::default();
    for aligned in [true, false] {
        let options = CloneOptions { aligned, ..CloneOptions::default() };
        let mut src = CloneSource::new(10.0, 10.0, options).unwrap();
        // Stroke 1 at (30, 10): offset +20 in x.
        let d1 = [dab(30.0, 10.0, 6.0)];
        let map1 = src.begin_stroke(&d1[0]).unwrap();
        let fp1 = StrokeFootprint { dabs: &d1, hardness_pct: 100.0, selection: None };
        let r1 = clone_stamp(&target, &target, &map1, &fp1, &limits).unwrap();
        // Pixel (30, 10) covers [30, 31): centre (30.5, 10.5) at distance 0.707 < 3 -> coverage 1.
        assert_eq!(r1.plane.at(30, 10), target.at(10, 10));
        assert_eq!(r1.plane.at(31, 11), target.at(11, 11));
        assert_eq!(r1.plane.at(28, 9), target.at(8, 9));
        // Outside the footprint the bytes are unchanged.
        assert_eq!(r1.plane.at(40, 40), target.at(40, 40));
        assert_eq!(r1.plane.at(30, 20), target.at(30, 20));
        assert!(r1.damage.is_some());
        // Stroke 2 starts at (30, 30).
        let d2 = [dab(30.0, 30.0, 6.0)];
        let map2 = src.begin_stroke(&d2[0]).unwrap();
        let fp2 = StrokeFootprint { dabs: &d2, hardness_pct: 100.0, selection: None };
        let r2 = clone_stamp(&target, &target, &map2, &fp2, &limits).unwrap();
        if aligned {
            // Same offset (+20, 0): (30, 30) samples (10, 30).
            assert_eq!(r2.plane.at(30, 30), target.at(10, 30));
        } else {
            // Restart at the source point: (30, 30) samples (10, 10).
            assert_eq!(r2.plane.at(30, 30), target.at(10, 10));
            assert_eq!(r2.plane.at(31, 29), target.at(11, 9));
        }
    }
}

#[test]
fn clone_stamp_is_clipped_by_the_selection_and_blends_soft_coverage() {
    let canvas = rect(0, 0, 32, 32);
    let target = RgbaPlane::filled(canvas, [0.0, 0.0, 1.0, 1.0], &RetouchLimits::default()).unwrap();
    let sample = RgbaPlane::filled(canvas, [1.0, 0.0, 0.0, 1.0], &RetouchLimits::default()).unwrap();
    // Selection: left half x < 16 only.
    let sel = sel_rect(RectF { x: 0.0, y: 0.0, width: 16.0, height: 32.0 }, &SelectionContext::new(canvas)).unwrap();
    let d = [dab(16.0, 16.0, 10.0)];
    let map = CloneMap::offset(0.0, 0.0, ResampleFilter::Bilinear).unwrap();
    let fp = StrokeFootprint { dabs: &d, hardness_pct: 100.0, selection: Some(&sel) };
    let r = clone_stamp(&target, &sample, &map, &fp, &RetouchLimits::default()).unwrap();
    assert_eq!(r.plane.at(14, 16), [1.0, 0.0, 0.0, 1.0], "inside selection and footprint");
    assert_eq!(r.plane.at(17, 16), [0.0, 0.0, 1.0, 1.0], "outside the selection: unchanged");
    // Soft edge: hardness 0 gives partial coverage c at the dab rim; red over blue opaque gives
    // (c, 0, 1 - c) exactly as source-over with alpha c (one binary32 rounding).
    let fp_soft = StrokeFootprint { dabs: &d, hardness_pct: 0.0, selection: None };
    let soft = clone_stamp(&target, &sample, &map, &fp_soft, &RetouchLimits::default()).unwrap();
    let p = soft.plane.at(19, 16);
    assert!(p[0] > 0.0 && p[0] < 1.0, "{p:?}");
    assert_eq!(p[3], 1.0);
    assert!(((p[0] + p[2]) - 1.0).abs() < 1e-6, "{p:?}");
}

#[test]
fn clone_stamp_fractional_offset_uses_bilinear_and_rotation_maps_through_the_source_point() {
    let canvas = rect(0, 0, 16, 16);
    let target = coded(canvas);
    let limits = RetouchLimits::default();
    // Offset +0.5 in x: destination centre lands half way between two texel centres.
    let map = CloneMap::offset(0.5, 0.0, ResampleFilter::Bilinear).unwrap();
    let s = map.sample(&target, rect(5, 5, 1, 1), &limits).unwrap();
    let want = (target.at(4, 5)[0] + target.at(5, 5)[0]) / 2.0;
    assert!((s.data[0][0] - want).abs() < 1e-7, "{:?} vs {want}", s.data[0]);
    // Rotation 90 about the source point (8, 8), anchored at (8, 8): destination (8.5, 8.5) - S =
    // (0.5, 0.5) rotated back by -90 lands on sample texel centre (8.5, 7.5): texel (8, 7).
    let mut src = CloneSource::new(8.0, 8.0, CloneOptions { source_rotation_deg: 90.0, ..CloneOptions::default() }).unwrap();
    let map = src.begin_stroke(&dab(8.0, 8.0, 4.0)).unwrap();
    let s = map.sample(&target, rect(8, 8, 1, 1), &limits).unwrap();
    assert_eq!(s.data[0], target.at(8, 7));
}

#[test]
fn hostile_clone_inputs_are_typed_refusals_before_allocation() {
    let o = CloneOptions::default();
    assert_eq!(CloneSource::new(f64::NAN, 0.0, o).unwrap_err().code(), "nonfinite");
    let bad = CloneOptions { source_rotation_deg: 181.0, ..o };
    assert_eq!(CloneSource::new(0.0, 0.0, bad).unwrap_err(), RetouchError::OutOfRange);
    // Plane length mismatch and out-of-domain samples.
    assert_eq!(RgbaPlane::new(rect(0, 0, 2, 2), vec![[0.0; 4]; 3]).unwrap_err(), RetouchError::Shape);
    assert_eq!(RgbaPlane::new(rect(0, 0, 1, 1), vec![[1.5, 0.0, 0.0, 1.0]]).unwrap_err(), RetouchError::OutOfDomain);
    // A u32::MAX square plane overflows the byte count; a 2^20 square exceeds the byte budget;
    // neither is allocated.
    let max = rect(0, 0, u32::MAX, u32::MAX);
    let code = RgbaPlane::filled(max, [0.0; 4], &RetouchLimits::default()).unwrap_err().code();
    assert!(code == "overflow" || code == "budget_exceeded", "{code}");
    let huge = rect(0, 0, 1 << 20, 1 << 20);
    assert_eq!(RgbaPlane::filled(huge, [0.0; 4], &RetouchLimits::default()).unwrap_err().code(), "budget_exceeded");
    let map = CloneMap::offset(1.0, 0.0, ResampleFilter::Bilinear).unwrap();
    let small = RgbaPlane::filled(rect(0, 0, 4, 4), [0.0; 4], &RetouchLimits::default()).unwrap();
    assert_eq!(map.sample(&small, huge, &RetouchLimits::default()).unwrap_err().code(), "budget_exceeded");
    // Singular / non-finite maps.
    assert!(CloneMap::offset(f64::INFINITY, 0.0, ResampleFilter::Bilinear).is_err());
    // Content-aware fill is a typed refusal.
    let cov = hsk_studio_pigment::selection::CoveragePlane { bounds: rect(0, 0, 0, 0), data: vec![] };
    assert_eq!(content_aware_fill(&small, &cov).unwrap_err().code(), "content_aware_unsupported");
}
