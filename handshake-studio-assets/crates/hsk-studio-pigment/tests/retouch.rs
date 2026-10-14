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

fn grey(bounds: Rect, f: impl Fn(i64, i64) -> f32) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..i64::from(bounds.height) {
        for x in 0..i64::from(bounds.width) {
            let v = f(bounds.x + x, bounds.y + y);
            data.push([v, v, v, 1.0]);
        }
    }
    RgbaPlane::new(bounds, data).unwrap()
}

#[test]
fn heal_keeps_source_texture_and_takes_destination_lighting() {
    let canvas = rect(0, 0, 40, 40);
    let tex = |x: i64, y: i64| 0.2 + 0.1 * ((x * 7 + y * 3).rem_euclid(5) as f32) / 5.0;
    let sample = grey(canvas, tex);
    let target = grey(canvas, |x, y| tex(x, y) + 0.4);
    let d = [dab(20.0, 20.0, 12.0)];
    let fp = StrokeFootprint { dabs: &d, hardness_pct: 100.0, selection: None };
    let map = CloneMap::offset(0.0, 0.0, ResampleFilter::NearestNeighbour).unwrap();
    let limits = RetouchLimits::default();
    let healed = heal(&target, &sample, &map, &fp, &limits).unwrap();
    assert!(healed.receipt.converged, "{:?}", healed.receipt);
    assert!(healed.receipt.iterations > 1);
    // The boundary difference d - g is the constant 0.4, so the harmonic correction is 0.4 and the
    // healed core is the source texture lifted to the destination lighting: tex + 0.4.
    for (x, y) in [(20, 20), (18, 21), (23, 17), (16, 20)] {
        let p = healed.result.plane.at(x, y);
        let want = tex(x, y) + 0.4;
        assert!((p[0] - want).abs() < 1e-5 && p[3] == 1.0, "({x},{y}) {p:?} want {want}");
    }
    // The clone stamp of the same stroke copies the source unlifted.
    let cloned = clone_stamp(&target, &sample, &map, &fp, &limits).unwrap();
    assert_eq!(cloned.plane.at(20, 20)[0], tex(20, 20));
    // Outside the footprint nothing changes.
    assert_eq!(healed.result.plane.at(2, 2), target.at(2, 2));
    // A work budget too small for one sweep is a typed refusal.
    let tight = RetouchLimits { max_work: 0, ..limits };
    assert_eq!(heal(&target, &sample, &map, &fp, &tight).unwrap_err().code(), "budget_exceeded");
}

#[test]
fn spot_heal_picks_the_best_matching_neighbourhood_and_removes_the_blemish() {
    let canvas = rect(0, 0, 64, 64);
    let blemish = |x: i64, y: i64| (x - 32) * (x - 32) + (y - 32) * (y - 32) <= 4;
    // Flat field except the blemish: the first candidate (east) scores 0 and wins.
    let flat = grey(canvas, |x, y| if blemish(x, y) { 0.05 } else { 0.5 });
    let d = [dab(32.0, 32.0, 8.0)];
    let fp = StrokeFootprint { dabs: &d, hardness_pct: 100.0, selection: None };
    let limits = RetouchLimits::default();
    let r = spot_heal(&flat, &flat, &fp, &limits).unwrap();
    // Footprint bounds 8x8 at (28, 28); step = 8 + 2*2 + 1 = 13.
    assert_eq!(r.receipt.source_offset, Some((13, 0)));
    assert!(r.receipt.converged);
    assert_eq!(r.result.plane.at(32, 32), [0.5, 0.5, 0.5, 1.0]);
    // East half brighter: the east candidate's ring mismatches, the west one (score 0) wins.
    let split = grey(canvas, |x, y| if blemish(x, y) { 0.05 } else if x >= 40 { 0.9 } else { 0.5 });
    let r = spot_heal(&split, &split, &fp, &limits).unwrap();
    assert_eq!(r.receipt.source_offset, Some((-13, 0)));
    assert!((r.result.plane.at(32, 32)[0] - 0.5).abs() < 1e-6);
    // No candidate inside a tiny sample plane: typed refusal.
    let tiny = grey(rect(0, 0, 12, 12), |_, _| 0.5);
    let d = [dab(6.0, 6.0, 8.0)];
    let fp = StrokeFootprint { dabs: &d, hardness_pct: 100.0, selection: None };
    assert_eq!(spot_heal(&tiny, &tiny, &fp, &limits).unwrap_err(), RetouchError::SpotHealNoSource);
}

mod canvas {
    use super::{coded, rect};
    use hsk_studio_pigment::canvas_ops::*;
    use hsk_studio_pigment::retouch::*;
    use hsk_studio_pigment::transform_math::{ResampleFilter, resample_reference};

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn canvas_resize_places_content_by_the_nine_member_anchor() {
        let plane = coded(rect(0, 0, 4, 4));
        let limits = RetouchLimits::default();
        let grow = |anchor: Anchor, w: u32, h: u32| {
            let resize = CanvasResize { old_width: 4, old_height: 4, new_width: w, new_height: h, anchor, fill: RED };
            canvas_resize(&plane, &resize, &limits).unwrap()
        };
        // 4x4 -> 6x8: top_left (0,0), middle_centre (1,2), bottom_right (2,4).
        for (anchor, off) in [(Anchor::TopLeft, (0, 0)), (Anchor::MiddleCentre, (1, 2)), (Anchor::BottomRight, (2, 4))] {
            let r = grow(anchor, 6, 8);
            assert_eq!(r.offset, off, "{}", anchor.key());
            assert_eq!(r.canvas, rect(0, 0, 6, 8));
            assert_eq!(r.plane.at(off.0, off.1), plane.at(0, 0));
            assert_eq!(r.plane.at(off.0 + 3, off.1 + 3), plane.at(3, 3));
            let outside = if off.0 > 0 { (0, 0) } else { (5, 7) };
            assert_eq!(r.plane.at(outside.0, outside.1), RED);
        }
        // Shrink 4x4 -> 1x1 centred: floor(-3/2) = -2, so old (2, 2) is kept.
        let r = grow(Anchor::MiddleCentre, 1, 1);
        assert_eq!(r.offset, (-2, -2));
        assert_eq!(r.plane.data, vec![plane.at(2, 2)]);
        // Spec discriminants round-trip; 0 and 10 are not anchors.
        for v in 1..=9u8 {
            assert_eq!(Anchor::from_u8(v).unwrap() as u8, v);
        }
        assert!(Anchor::from_u8(0).is_none() && Anchor::from_u8(10).is_none());
    }

    #[test]
    fn crop_copies_exactly_and_rotation_is_the_transform_operator() {
        let plane = coded(rect(0, 0, 16, 16));
        let limits = RetouchLimits::default();
        let o = CropOptions::default();
        let r = crop(&plane, rect(3, 5, 6, 4), &o, &limits).unwrap();
        assert_eq!((r.canvas, r.offset), (rect(0, 0, 6, 4), (-3, -5)));
        assert_eq!(r.plane.at(0, 0), plane.at(3, 5));
        assert_eq!(r.plane.at(5, 3), plane.at(8, 8));
        // Crop-expand beyond the plane reads transparent; content-aware expand is refused.
        let r = crop(&plane, rect(14, 0, 4, 2), &o, &limits).unwrap();
        assert_eq!(r.plane.at(1, 0), plane.at(15, 0));
        assert_eq!(r.plane.at(2, 0), [0.0; 4]);
        let ca = CropOptions { content_aware_fill_on_expand: true, ..o };
        assert_eq!(crop(&plane, rect(14, 0, 4, 2), &ca, &limits).unwrap_err().code(), "content_aware_unsupported");
        // Keep cropped pixels: the plane only moves.
        let keep = CropOptions { delete_cropped_pixels: false, ..o };
        let r = crop(&plane, rect(3, 5, 6, 4), &keep, &limits).unwrap();
        assert_eq!(r.plane.bounds, rect(-3, -5, 16, 16));
        assert_eq!(r.plane.data, plane.data);
        let keep_rot = CropOptions { angle_deg: 10.0, ..keep };
        assert_eq!(crop(&plane, rect(3, 5, 6, 4), &keep_rot, &limits).unwrap_err().code(), "crop_rotate_keep_pixels_unsupported");
        // Quarter turn of a square crop is an exact texel permutation.
        let q = CropOptions { angle_deg: 90.0, ..o };
        let r = crop(&plane, rect(4, 4, 8, 8), &q, &limits).unwrap();
        let mut sorted_out: Vec<[u32; 4]> = r.plane.data.iter().map(|p| p.map(f32::to_bits)).collect();
        let mut sorted_in: Vec<[u32; 4]> = (4..12).flat_map(|y| (4..12).map(move |x| (x, y))).map(|(x, y)| plane.at(x, y).map(f32::to_bits)).collect();
        sorted_out.sort_unstable();
        sorted_in.sort_unstable();
        assert_eq!(sorted_out, sorted_in);
        // Any angle: bytes equal the transform operator's reference resampler.
        for (angle, filter) in [(30.0, ResampleFilter::Bicubic), (-12.5, ResampleFilter::Bilinear)] {
            let opts = CropOptions { angle_deg: angle, filter, ..o };
            let crop_rect = rect(2, 3, 9, 7);
            let r = crop(&plane, crop_rect, &opts, &limits).unwrap();
            assert_eq!(r.rotation_deg, angle);
            let reference = resample_reference(&crop_transform(crop_rect, &opts), plane.bounds, r.canvas, |i, j| plane.at(i64::from(i), i64::from(j))).unwrap();
            assert_eq!(r.plane.data, reference, "angle {angle}");
        }
        assert_eq!(crop(&plane, rect(0, 0, 4, 4), &CropOptions { angle_deg: f64::NAN, ..o }, &limits).unwrap_err(), RetouchError::OutOfRange);
        assert_eq!(crop(&plane, rect(0, 0, 1 << 20, 1 << 20), &o, &limits).unwrap_err().code(), "budget_exceeded");
    }

    #[test]
    fn trim_moves_selected_edges_to_the_content_bounds() {
        let limits = RetouchLimits::default();
        let bounds = rect(0, 0, 10, 8);
        let mut data = vec![[0.0f32; 4]; 80];
        for (x, y) in [(3usize, 2usize), (6, 5)] {
            data[y * 10 + x] = RED;
        }
        let plane = RgbaPlane::new(bounds, data).unwrap();
        assert_eq!(trim_rect(&plane, TrimBasis::TransparentPixels, TrimEdges::ALL).unwrap(), Some(rect(3, 2, 4, 4)));
        let edges = TrimEdges { top: true, left: false, bottom: false, right: true };
        assert_eq!(trim_rect(&plane, TrimBasis::TransparentPixels, edges).unwrap(), Some(rect(0, 2, 7, 6)));
        let r = trim(&plane, TrimBasis::TransparentPixels, TrimEdges::ALL, &limits).unwrap();
        assert_eq!((r.canvas, r.offset), (rect(0, 0, 4, 4), (-3, -2)));
        assert_eq!(r.plane.at(0, 0), RED);
        // Colour bases: a red border trimmed by its top-left / bottom-right pixel colour.
        let mut data = vec![RED; 36];
        data[2 * 6 + 3] = [0.0, 1.0, 0.0, 1.0];
        let framed = RgbaPlane::new(rect(0, 0, 6, 6), data).unwrap();
        for basis in [TrimBasis::TopLeftPixel, TrimBasis::BottomRightPixel] {
            assert_eq!(trim_rect(&framed, basis, TrimEdges::ALL).unwrap(), Some(rect(3, 2, 1, 1)));
        }
        assert_eq!(TrimBasis::from_u8(9), Some(TrimBasis::BottomRightPixel));
        assert_eq!(TrimBasis::BottomRightPixel as u8, 9);
        assert!(TrimBasis::from_u8(2).is_none());
        // Everything trimmed is a typed refusal.
        let empty = RgbaPlane::filled(bounds, [0.0; 4], &limits).unwrap();
        assert_eq!(trim(&empty, TrimBasis::TransparentPixels, TrimEdges::ALL, &limits).unwrap_err(), RetouchError::TrimEverything);
    }
}

mod filters {
    use super::rect;
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_pigment::Rect;
    use hsk_studio_pigment::blur_math::{BlurLimits, BlurScope, LayerBlur, blur_window};
    use hsk_studio_pigment::filter_math::*;
    use hsk_studio_pigment::render_plan::*;
    use hsk_studio_pigment::retouch::RgbaPlane;

    const WIDE: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };

    fn plane(bounds: Rect, f: impl Fn(i64, i64) -> [f32; 4]) -> RgbaPlane {
        let mut data = Vec::new();
        for y in bounds.y..bounds.y + i64::from(bounds.height) {
            for x in bounds.x..bounds.x + i64::from(bounds.width) {
                data.push(f(x, y));
            }
        }
        RgbaPlane::new(bounds, data).unwrap()
    }

    fn varied(x: i64, y: i64) -> [f32; 4] {
        let v = ((x * 37 + y * 11).rem_euclid(17)) as f32 / 16.0;
        [v, 1.0 - v, 0.5, if (x + y).rem_euclid(7) == 0 { 0.0 } else { 0.25 + 0.75 * v }]
    }

    fn all_kinds() -> Vec<LayerFilter> {
        vec![
            LayerFilter::GaussianBlur { radius_px: 2.5 },
            LayerFilter::UnsharpMask { amount_pct: 150.0, radius_px: 1.5, threshold_levels: 2 },
            LayerFilter::HighPass { radius_px: 3.0 },
            LayerFilter::Median { radius_px: 2 },
            LayerFilter::AddNoise { amount_pct: 40.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed: 7 },
            LayerFilter::AddNoise { amount_pct: 25.0, distribution: NoiseDistribution::Uniform, monochromatic: true, seed: 9 },
        ]
    }

    #[test]
    fn filters_follow_their_documented_formulas() {
        let b = rect(0, 0, 9, 9);
        // Median r=1 (plus-shaped disc of 5) removes an impulse and keeps a step edge exactly.
        let impulse = plane(b, |x, y| if (x, y) == (4, 4) { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] });
        let m = filter_plane(&impulse, &LayerFilter::Median { radius_px: 1 }, WIDE).unwrap();
        assert_eq!(m.at(4, 4), [0.0, 0.0, 0.0, 1.0]);
        let step = plane(b, |x, _| if x >= 4 { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] });
        let m = filter_plane(&step, &LayerFilter::Median { radius_px: 1 }, WIDE).unwrap();
        for y in 1..8 {
            assert_eq!((m.at(3, y), m.at(4, y)), (step.at(3, y), step.at(4, y)), "row {y}");
        }
        // Gaussian blur is the blur_math window.
        let src = plane(b, varied);
        let taps = LayerBlur::new(BlurScope::Layer, 2.5).taps().unwrap();
        let want = blur_window(&src.data, b, b, &taps, BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 }).unwrap();
        assert_eq!(filter_plane(&src, &LayerFilter::GaussianBlur { radius_px: 2.5 }, WIDE).unwrap().data, want);
        // High pass of a flat plane is mid grey; unsharp mask above the threshold changes nothing
        // there, and on a step edge pushes the dark side darker and the bright side brighter.
        let flat = plane(rect(0, 0, 20, 20), |_, _| [0.3, 0.6, 0.9, 1.0]);
        let hp = filter_plane(&flat, &LayerFilter::HighPass { radius_px: 2.0 }, WIDE).unwrap();
        let p = hp.at(10, 10);
        assert!(p[..3].iter().all(|c| (c - 0.5).abs() < 1e-6) && p[3] == 1.0, "{p:?}");
        let usm = LayerFilter::UnsharpMask { amount_pct: 100.0, radius_px: 1.0, threshold_levels: 1 };
        assert_eq!(filter_plane(&flat, &usm, WIDE).unwrap().at(10, 10), flat.at(10, 10));
        let grey_step = plane(b, |x, _| if x >= 4 { [0.8, 0.8, 0.8, 1.0] } else { [0.2, 0.2, 0.2, 1.0] });
        let u = filter_plane(&grey_step, &usm, WIDE).unwrap();
        assert!(u.at(3, 4)[0] < 0.2 && u.at(4, 4)[0] > 0.8, "{:?} {:?}", u.at(3, 4), u.at(4, 4));
        assert_eq!(u.at(3, 4)[3], 1.0);
        // Add noise: monochromatic shifts R, G, B by one value; uniform stays within +-A (A = 0.1
        // at 20 %); the gaussian (Irwin-Hall) has mean near 0; transparent stays transparent; the
        // seed decides the pattern.
        let mid = plane(rect(0, 0, 64, 64), |x, y| if x == 0 && y == 0 { [0.0; 4] } else { [0.5, 0.5, 0.5, 1.0] });
        let mono = LayerFilter::AddNoise { amount_pct: 20.0, distribution: NoiseDistribution::Uniform, monochromatic: true, seed: 1 };
        let n = filter_plane(&mid, &mono, WIDE).unwrap();
        assert_eq!(n.at(0, 0), [0.0; 4]);
        let mut changed = 0;
        for p in &n.data[1..] {
            assert!(p[0] == p[1] && p[1] == p[2] && (p[0] - 0.5).abs() <= 0.1 + 1e-7, "{p:?}");
            changed += usize::from(p[0] != 0.5);
        }
        assert!(changed > 4000);
        let gauss = |seed| LayerFilter::AddNoise { amount_pct: 20.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed };
        let g1 = filter_plane(&mid, &gauss(3), WIDE).unwrap();
        let mean: f64 = g1.data[1..].iter().map(|p| f64::from(p[0]) - 0.5).sum::<f64>() / 4095.0;
        assert!(mean.abs() < 0.005, "mean {mean}");
        assert_eq!(g1, filter_plane(&mid, &gauss(3), WIDE).unwrap());
        assert_ne!(g1, filter_plane(&mid, &gauss(4), WIDE).unwrap());
    }

    #[test]
    fn filters_are_chunk_and_thread_count_independent() {
        let src = plane(rect(-5, 3, 23, 17), varied);
        for f in all_kinds() {
            let whole = filter_plane(&src, &f, WIDE).unwrap();
            for (band, threads) in [(1, 1), (3, 2), (5, 4), (7, 8), (17, 3), (100, 16)] {
                let chunked = filter_plane_chunked(&src, &f, WIDE, band, threads).unwrap();
                let bits = |p: &RgbaPlane| p.data.iter().map(|q| q.map(f32::to_bits)).collect::<Vec<_>>();
                assert_eq!(bits(&chunked), bits(&whole), "{:?} band {band} threads {threads}", f.kind());
            }
        }
        assert_eq!(filter_plane_chunked(&src, &all_kinds()[0], WIDE, 0, 1).unwrap_err(), FilterError::Parameter);
    }

    #[test]
    fn hostile_filter_parameters_and_sizes_are_refused_before_allocation() {
        for f in [
            LayerFilter::UnsharpMask { amount_pct: f32::NAN, radius_px: 1.0, threshold_levels: 0 },
            LayerFilter::UnsharpMask { amount_pct: 501.0, radius_px: 1.0, threshold_levels: 0 },
            LayerFilter::HighPass { radius_px: 0.0 },
            LayerFilter::Median { radius_px: 0 },
            LayerFilter::Median { radius_px: 501 },
            LayerFilter::AddNoise { amount_pct: f32::INFINITY, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 0 },
            LayerFilter::GaussianBlur { radius_px: -1.0 },
        ] {
            assert_eq!(f.validate(), Err(FilterError::Parameter), "{f:?}");
        }
        // dims_u32_max: overflow or over budget, never an allocation.
        let huge = rect(0, 0, u32::MAX, u32::MAX);
        for f in all_kinds() {
            let r = filter_window(&[], rect(0, 0, 0, 0), huge, &f, WIDE);
            assert!(matches!(r, Err(FilterError::OverBudget { .. } | FilterError::Overflow)), "{:?} {r:?}", f.kind());
        }
        // aggregate_over_budget: the exact predicted cost passes, one less is refused.
        let src = plane(rect(0, 0, 6, 6), varied);
        let out = rect(0, 0, 6, 6);
        let f = LayerFilter::Median { radius_px: 2 };
        let (bytes, work) = filter_window_cost(&f, out).unwrap();
        let exact = FilterLimits { max_bytes: bytes, max_work: work };
        assert!(filter_window(&src.data, src.bounds, out, &f, exact).is_ok());
        assert_eq!(
            filter_window(&src.data, src.bounds, out, &f, FilterLimits { max_work: work - 1, ..exact }),
            Err(FilterError::OverBudget { needed: work, limit: work - 1 })
        );
        assert_eq!(filter_window(&src.data, rect(0, 0, 5, 6), out, &f, WIDE), Err(FilterError::Shape));
        assert_eq!(filter_window(&[[2.0, 0.0, 0.0, 1.0]], rect(0, 0, 1, 1), rect(0, 0, 1, 1), &f, WIDE), Err(FilterError::OutOfDomain));
    }

    fn plan() -> RenderPlan {
        let r = rect(0, 0, 4, 4);
        let layer = |input: u32, mode: StudioBlendMode| Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask: None };
        RenderPlan {
            revision: 1,
            target: OutputTarget {
                extent: r,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
            },
            nodes: vec![
                PlanNode::Solid { rect: r, rgba: [0.2, 0.4, 0.6, 1.0] },
                PlanNode::Solid { rect: rect(1, 1, 2, 2), rgba: [1.0, 1.0, 1.0, 0.5] },
                PlanNode::Group(Group { isolated: true, layers: vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)] }),
            ],
            root: PlanNodeId(2),
        }
    }

    struct Backend {
        declares_filters: bool,
    }
    impl RenderBackend for Backend {
        fn identity(&self) -> BackendIdentity {
            BackendIdentity { name: "retouch-test", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
        }
        fn exactness(&self, _: Operator) -> Exactness {
            Exactness::Exact
        }
        fn extension_exactness(&self, op: ExtOperator) -> Exactness {
            if self.declares_filters { op.intrinsic() } else { Exactness::Unsupported }
        }
        fn execute(&self, _: &RenderPlan, _: &ExecuteRequest, _: &dyn PlanSources, _: &CancellationToken, _: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
            Err(BackendError::Unavailable("retouch_test_execute"))
        }
    }

    #[test]
    fn filter_envelope_entries_validate_report_and_admission_refuses_undeclared() {
        let usm = LayerFilter::UnsharpMask { amount_pct: 120.0, radius_px: 2.0, threshold_levels: 0 };
        let noise = LayerFilter::AddNoise { amount_pct: 10.0, distribution: NoiseDistribution::Gaussian, monochromatic: true, seed: 42 };
        let entry = || LayerExtension::new(PlanNodeId(2), 1).with_filters(vec![usm, noise]);
        let ext = PlanExtensions::from_layers(vec![entry()]);
        assert_eq!(ext.validate(&plan()), Ok(vec![ExtOperator::Filter(FilterKind::UnsharpMask), ExtOperator::Filter(FilterKind::AddNoise)]));
        assert_eq!(ExtOperator::Filter(FilterKind::Median).key(), "filter_median");
        assert_eq!(ExtOperator::Filter(FilterKind::HighPass).intrinsic(), Exactness::Approximate);
        // Combination and parameter refusals.
        let with_blur = PlanExtensions::from_layers(vec![entry().with_blur(LayerBlur::new(BlurScope::Layer, 2.0))]);
        assert_eq!(with_blur.validate(&plan()), Err(PlanError::FilterCombination { node: 2 }));
        assert_eq!(PlanError::FilterCombination { node: 2 }.code(), "plan_filter_combination");
        let bad = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_filters(vec![LayerFilter::Median { radius_px: 0 }])]);
        assert_eq!(bad.validate(&plan()), Err(PlanError::FilterParameter { node: 2 }));
        let many = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_filters(vec![usm; MAX_FILTERS_PER_LAYER + 1])]);
        assert_eq!(many.validate(&plan()), Err(PlanError::FilterParameter { node: 2 }));
        // Admission: a backend that does not declare filters is refused under the filter key
        // before any sink call; one that declares them admits with the Approximate class.
        let cancel = CancellationToken::default();
        let req = ExecuteRequest { expected_revision: 1, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 20 };
        assert_eq!(
            admit_extended(&plan(), &ext, &Backend { declares_filters: false }, &req, &cancel),
            Err(BackendError::Unsupported { operator: "filter_unsharp_mask", exactness: Exactness::Unsupported })
        );
        let ledger = admit_extended(&plan(), &ext, &Backend { declares_filters: true }, &req, &cancel).unwrap();
        assert_eq!(ledger.lowest, Exactness::Approximate);
        assert!(ledger.operators.iter().any(|o| o.key == "filter_add_noise"));
        // Entries without filters serialise without the member (existing digests unchanged).
        let plain = serde_json::to_string(&LayerExtension::new(PlanNodeId(2), 1)).unwrap();
        assert!(!plain.contains("filters"), "{plain}");
        assert!(serde_json::to_string(&entry()).unwrap().contains("\"filters\""));
    }
}
