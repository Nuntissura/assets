//! Content-aware fill / patch / move (lane SMB-PIGMENT-CONTENT-AWARE). Synthetic textures whose
//! continuation is known exactly (stripes, checkerboard) bound the fill error; noise bounds the
//! statistics; hostile inputs must be typed refusals. Photoshop parity is not claimed (D5).
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

fn grey(v: f32) -> [f32; 4] {
    [v, v, v, 1.0]
}

/// Vertical stripes of period 4 (two dark, two light columns).
fn stripes(x: i64, _y: i64) -> [f32; 4] {
    if x.rem_euclid(4) < 2 { grey(0.1) } else { grey(0.9) }
}

/// Checkerboard of 4 x 4 cells.
fn checker(x: i64, y: i64) -> [f32; 4] {
    if (x.div_euclid(4) + y.div_euclid(4)).rem_euclid(2) == 0 { grey(0.2) } else { grey(0.8) }
}

/// Deterministic white noise in [0.2, 0.8].
fn noise(x: i64, y: i64) -> [f32; 4] {
    let mut z = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 29;
    grey(0.2 + 0.6 * ((z >> 11) as f64 / (1u64 << 53) as f64) as f32)
}

fn hole(canvas: Rect, r: RectF) -> CoveragePlane {
    sel_rect(r, &SelectionContext::new(canvas)).unwrap()
}

/// Mean absolute error (RGB) and worst error over the hole pixels against `expect`.
fn hole_error(out: &RgbaPlane, h: &CoveragePlane, expect: impl Fn(i64, i64) -> [f32; 4]) -> (f64, f64) {
    let (mut sum, mut worst, mut n) = (0.0f64, 0.0f64, 0.0f64);
    let (ex, ey) = h.bounds.end().unwrap();
    for y in h.bounds.y..ey {
        for x in h.bounds.x..ex {
            if h.at(x, y) == 0 {
                continue;
            }
            let (a, b) = (out.at(x, y), expect(x, y));
            for k in 0..3 {
                let e = (f64::from(a[k]) - f64::from(b[k])).abs();
                sum += e;
                worst = worst.max(e);
                n += 1.0;
            }
        }
    }
    (sum / n, worst)
}

#[test]
fn fill_continues_stripes_and_checker_with_a_quality_receipt() {
    let canvas = rect(0, 0, 48, 48);
    let limits = RetouchLimits::default();
    for (name, tex) in [("stripes", stripes as fn(i64, i64) -> [f32; 4]), ("checker", checker)] {
        let target = plane(canvas, |x, y| {
            // The hole's content is garbage: it must not leak into the fill.
            if (18..30).contains(&x) && (18..30).contains(&y) { [1.0, 0.0, 0.0, 1.0] } else { tex(x, y) }
        });
        let h = hole(canvas, RectF { x: 18.0, y: 18.0, width: 12.0, height: 12.0 });
        for adaptation in [ColourAdaptation::None, ColourAdaptation::Default] {
            let opts = ContentAwareOptions { colour_adaptation: adaptation, ..ContentAwareOptions::default() };
            let r = fill(&target, &h, SamplingArea::Auto, &opts, &limits).unwrap();
            let (mean, worst) = hole_error(&r.result.plane, &h, tex);
            println!("{name} {:?}: mean {mean:.2e} worst {worst:.2e} receipt {:?}", adaptation, r.receipt);
            // Stated error: the pattern is continued (mean |error| <= 0.02, worst <= 0.1 of the
            // 0.6..0.8 contrast); outside the hole the bytes are untouched.
            assert!(mean <= 0.02 && worst <= 0.1, "{name} {adaptation:?}: mean {mean} worst {worst}");
            assert_eq!(r.result.plane.at(5, 5), target.at(5, 5));
            assert_eq!(r.result.plane.at(17, 18), target.at(17, 18));
            assert_eq!(r.result.damage, Some(rect(18, 18, 12, 12)));
            // Receipt: pyramid coarsest first, iterations = levels x em_iterations, energy ~ 0
            // for a perfectly repeating texture.
            let rc = &r.receipt;
            assert!(rc.levels.len() >= 2, "{name}: multi-scale expected");
            assert_eq!(rc.levels.last().map(|l| (l.width, l.height)), Some((48, 48)));
            assert_eq!(rc.iterations, rc.levels.len() as u32 * opts.em_iterations);
            assert!(rc.levels.iter().all(|l| l.energies.len() == opts.em_iterations as usize));
            assert!(rc.final_energy < 1e-4, "{name}: final energy {}", rc.final_energy);
            assert!(rc.work > 0 && rc.seed == 0 && rc.patch_radius == 3 && !rc.guided);
            assert_eq!(rc.blend.is_some(), adaptation == ColourAdaptation::Default);
            if let Some(b) = rc.blend {
                assert!(b.converged, "{name}: healing blend converged");
            }
        }
    }
}

#[test]
fn fill_hole_at_the_image_edge_and_noise_statistics() {
    let canvas = rect(0, 0, 48, 40);
    let limits = RetouchLimits::default();
    // Hole touching the left edge and the top-left corner.
    let target = plane(canvas, stripes);
    for hr in [
        RectF { x: 0.0, y: 14.0, width: 10.0, height: 12.0 },
        RectF { x: 0.0, y: 0.0, width: 9.0, height: 9.0 },
    ] {
        let h = hole(canvas, hr);
        let r = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions::default(), &limits).unwrap();
        let (mean, worst) = hole_error(&r.result.plane, &h, stripes);
        println!("edge {hr:?}: mean {mean:.2e} worst {worst:.2e}");
        assert!(mean <= 0.02 && worst <= 0.1, "edge hole {hr:?}: mean {mean} worst {worst}");
    }
    // Noise cannot be continued pixel for pixel: the fill must keep its statistics (not a blur).
    let canvas = rect(0, 0, 64, 64);
    let target = plane(canvas, noise);
    let h = hole(canvas, RectF { x: 24.0, y: 24.0, width: 14.0, height: 14.0 });
    let r = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions::default(), &limits).unwrap();
    let stats = |pred: &dyn Fn(i64, i64) -> bool, p: &RgbaPlane| {
        let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
        for y in 0..64 {
            for x in 0..64 {
                if pred(x, y) {
                    let v = f64::from(p.at(x, y)[0]);
                    s += v;
                    s2 += v * v;
                    n += 1.0;
                }
            }
        }
        let m = s / n;
        (m, (s2 / n - m * m).sqrt())
    };
    let inside = |x: i64, y: i64| (24..38).contains(&x) && (24..38).contains(&y);
    let (m_out, sd_out) = stats(&|x, y| !inside(x, y), &target);
    let (m_in, sd_in) = stats(&inside, &r.result.plane);
    println!("noise: outside mean {m_out:.4} sd {sd_out:.4}; fill mean {m_in:.4} sd {sd_in:.4}; energy {}", r.receipt.final_energy);
    // Stated bounds: mean within 0.05, standard deviation at least 60 % of the source's (a blurred
    // average would collapse it) and at most 140 %.
    assert!((m_in - m_out).abs() <= 0.05, "noise mean {m_in} vs {m_out}");
    assert!(sd_in >= 0.6 * sd_out && sd_in <= 1.4 * sd_out, "noise sd {sd_in} vs {sd_out}");
    assert!(r.receipt.final_energy > 0.0, "noise cannot match exactly");
}

#[test]
fn fill_is_thread_count_invariant() {
    let canvas = rect(-7, 3, 56, 44);
    let target = plane(canvas, |x, y| {
        let n = noise(x, y);
        let s = checker(x, y);
        [0.5 * (n[0] + s[0]), 0.3 * n[1] + 0.2, s[2], 1.0]
    });
    let h = hole(canvas, RectF { x: 10.5, y: 15.0, width: 15.0, height: 11.5 });
    let limits = RetouchLimits::default();
    let base = ContentAwareOptions { seed: 0x5EED, ..ContentAwareOptions::default() };
    let reference = fill(&target, &h, SamplingArea::Auto, &base, &limits).unwrap();
    let bits = |p: &RgbaPlane| p.data.iter().flat_map(|q| q.map(f32::to_bits)).collect::<Vec<_>>();
    for threads in [2u32, 3, 5, 8, 16] {
        let r = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions { threads, ..base }, &limits).unwrap();
        assert_eq!(bits(&r.result.plane), bits(&reference.result.plane), "threads {threads}");
        assert_eq!(r.receipt, reference.receipt, "threads {threads}");
    }
    // Same seed twice: same bytes; another seed is a different (still valid) synthesis.
    let again = fill(&target, &h, SamplingArea::Auto, &base, &limits).unwrap();
    assert_eq!(bits(&again.result.plane), bits(&reference.result.plane));
    let other = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions { seed: 1, ..base }, &limits).unwrap();
    assert_eq!(other.receipt.seed, 1);
    // SMB-PIGMENT-CONTENT-AWARE-2: with every search adaptation on (gain/bias very_high, full
    // rotation, scale, mirror) the bytes still do not depend on the thread count.
    let adapted = ContentAwareOptions {
        colour_adaptation: ColourAdaptation::VeryHigh,
        rotation_adaptation: RotationAdaptation::Full,
        scale: true,
        mirror: true,
        em_iterations: 2,
        search_iterations: 3,
        ..base
    };
    let reference = fill(&target, &h, SamplingArea::Auto, &adapted, &limits).unwrap();
    for threads in [3u32, 8] {
        let r = fill(&target, &h, SamplingArea::Auto, &ContentAwareOptions { threads, ..adapted }, &limits).unwrap();
        assert_eq!(bits(&r.result.plane), bits(&reference.result.plane), "adapted threads {threads}");
        assert_eq!(r.receipt, reference.receipt, "adapted threads {threads}");
    }
    assert_ne!(bits(&reference.result.plane), bits(&again.result.plane), "the adaptations change the synthesis");
}

#[test]
fn sampling_area_restricts_the_sources() {
    // Left half red, right half blue; the hole sits in the red half.
    let canvas = rect(0, 0, 48, 32);
    let target = plane(canvas, |x, _| if x < 24 { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] });
    let h = hole(canvas, RectF { x: 6.0, y: 10.0, width: 10.0, height: 10.0 });
    let limits = RetouchLimits::default();
    let opts = ContentAwareOptions { colour_adaptation: ColourAdaptation::None, ..ContentAwareOptions::default() };
    let red = |_: i64, _: i64| [1.0f32, 0.0, 0.0, 1.0];
    let blue = |_: i64, _: i64| [0.0f32, 0.0, 1.0, 1.0];
    let auto = fill(&target, &h, SamplingArea::Auto, &opts, &limits).unwrap();
    assert!(hole_error(&auto.result.plane, &h, red).1 <= 1e-6);
    // Rectangular sampling area = the blue half: every hole pixel comes from blue.
    let r = fill(&target, &h, SamplingArea::Rectangular(rect(24, 0, 24, 32)), &opts, &limits).unwrap();
    assert!(hole_error(&r.result.plane, &h, blue).1 <= 1e-6);
    // Custom sampling plane (coverage >= 1/2 samples): the same blue half as a selection.
    let custom = hole(canvas, RectF { x: 24.0, y: 0.0, width: 24.0, height: 32.0 });
    let c = fill(&target, &h, SamplingArea::Custom(&custom), &opts, &limits).unwrap();
    assert!(hole_error(&c.result.plane, &h, blue).1 <= 1e-6);
    // A sampling area too small for one patch: typed refusal.
    let tiny = fill(&target, &h, SamplingArea::Rectangular(rect(40, 0, 4, 4)), &opts, &limits);
    assert_eq!(tiny.unwrap_err().code(), "content_aware_no_source");
}

#[test]
fn hostile_fill_inputs_are_typed_refusals() {
    let canvas = rect(0, 0, 32, 32);
    let limits = RetouchLimits::default();
    let target = plane(canvas, stripes);
    let h = hole(canvas, RectF { x: 8.0, y: 8.0, width: 8.0, height: 8.0 });
    let d = ContentAwareOptions::default();
    let code = |r: Result<ContentAwareResult, hsk_studio_pigment::retouch::RetouchError>| r.unwrap_err().code();
    // Option ranges (never clamped) and unimplemented option values (never approximated).
    for bad in [
        ContentAwareOptions { patch_radius: 0, ..d },
        ContentAwareOptions { patch_radius: 9, ..d },
        ContentAwareOptions { em_iterations: 0, ..d },
        ContentAwareOptions { search_iterations: 33, ..d },
        ContentAwareOptions { max_levels: 0, ..d },
        ContentAwareOptions { threads: 0, ..d },
        ContentAwareOptions { threads: 65, ..d },
    ] {
        assert_eq!(code(fill(&target, &h, SamplingArea::Auto, &bad, &limits)), "retouch_out_of_range");
    }
    for bad in [
        ContentAwareOptions { colour_adaptation: ColourAdaptation::High, ..d },
        ContentAwareOptions { colour_adaptation: ColourAdaptation::VeryHigh, ..d },
        ContentAwareOptions { rotation_adaptation: RotationAdaptation::Low, ..d },
        ContentAwareOptions { scale: true, ..d },
        ContentAwareOptions { mirror: true, ..d },
    ] {
        // Implemented since SMB-PIGMENT-CONTENT-AWARE-2 (was content_aware_option_unsupported);
        // the healing patch (no search) refuses them instead of ignoring them.
        assert!(fill(&target, &h, SamplingArea::Auto, &bad, &limits).is_ok(), "{bad:?}");
        let normal = patch(&target, &h, (8, 0), PatchMode::Normal, SamplingArea::Auto, &bad, &limits);
        assert_eq!(code(normal), "content_aware_option_unsupported");
    }
    // Hole size bound.
    let few = ContentAwareOptions { max_hole_pixels: 63, ..d };
    assert_eq!(code(fill(&target, &h, SamplingArea::Auto, &few, &limits)), "content_aware_hole_too_large");
    // Memory budget (pyramid charged before allocation) and work budget (before synthesis).
    let small_bytes = RetouchLimits { max_bytes: 32 * 32 * 16, ..limits };
    assert_eq!(code(fill(&target, &h, SamplingArea::Auto, &d, &small_bytes)), "budget_exceeded");
    let small_work = RetouchLimits { max_work: 10_000, ..limits };
    assert_eq!(code(fill(&target, &h, SamplingArea::Auto, &d, &small_work)), "budget_exceeded");
    // Dims u32::MAX with no data: shape refusal before any allocation.
    let huge = RgbaPlane { bounds: rect(0, 0, u32::MAX, u32::MAX), data: Vec::new() };
    assert_eq!(code(fill(&huge, &h, SamplingArea::Auto, &d, &limits)), "retouch_shape");
    let huge_hole = CoveragePlane { bounds: rect(0, 0, u32::MAX, u32::MAX), data: Vec::new() };
    assert_eq!(code(fill(&target, &huge_hole, SamplingArea::Auto, &d, &limits)), "retouch_shape");
    let huge_sampling = CoveragePlane { bounds: rect(0, 0, 2, 2), data: vec![0; 3] };
    assert_eq!(code(fill(&target, &h, SamplingArea::Custom(&huge_sampling), &d, &limits)), "retouch_shape");
    // Bounds whose end overflows i64.
    let far = RgbaPlane { bounds: rect(i64::MAX - 1, 0, 4, 1), data: vec![[0.0; 4]; 4] };
    assert_eq!(code(fill(&far, &h, SamplingArea::Auto, &d, &limits)), "overflow");
    // Non-finite / out-of-domain samples.
    let mut nan = target.clone();
    nan.data[3] = [f32::NAN, 0.0, 0.0, 1.0];
    assert_eq!(code(fill(&nan, &h, SamplingArea::Auto, &d, &limits)), "retouch_out_of_domain");
    // A hole covering everything leaves no source.
    let all = hole(canvas, RectF { x: 0.0, y: 0.0, width: 32.0, height: 32.0 });
    assert_eq!(code(fill(&target, &all, SamplingArea::Auto, &d, &limits)), "content_aware_no_source");
    // An empty hole (or one outside the target) returns the target unchanged.
    let outside = hole(rect(100, 100, 8, 8), RectF { x: 100.0, y: 100.0, width: 4.0, height: 4.0 });
    let r = fill(&target, &outside, SamplingArea::Auto, &d, &limits).unwrap();
    assert_eq!(r.result.plane, target);
    assert_eq!(r.result.damage, None);
    assert!(r.receipt.levels.is_empty());
}

/// Horizontal stripes of period 4.
fn hstripes(_x: i64, y: i64) -> [f32; 4] {
    if y.rem_euclid(4) < 2 { grey(0.1) } else { grey(0.9) }
}

/// Mean absolute RGB error over the rectangle `r` against `expect`.
fn rect_error(out: &RgbaPlane, r: Rect, expect: impl Fn(i64, i64) -> [f32; 4]) -> f64 {
    let (ex, ey) = r.end().unwrap();
    let (mut sum, mut n) = (0.0f64, 0.0f64);
    for y in r.y..ey {
        for x in r.x..ex {
            let (a, b) = (out.at(x, y), expect(x, y));
            for k in 0..3 {
                sum += (f64::from(a[k]) - f64::from(b[k])).abs();
                n += 1.0;
            }
        }
    }
    sum / n
}

#[test]
fn patch_normal_heals_the_dragged_source_and_content_aware_follows_it() {
    let limits = RetouchLimits::default();
    // Normal: a blemish on stripes, dragged over stripes 12 px to the right (same phase).
    let canvas = rect(0, 0, 48, 32);
    let target = plane(canvas, |x, y| {
        if (18..26).contains(&x) && (10..18).contains(&y) { [0.0, 1.0, 0.0, 1.0] } else { stripes(x, y) }
    });
    let region = hole(canvas, RectF { x: 18.0, y: 10.0, width: 8.0, height: 8.0 });
    let d = ContentAwareOptions::default();
    let r = patch(&target, &region, (12, 0), PatchMode::Normal, SamplingArea::Auto, &d, &limits).unwrap();
    let (mean, worst) = hole_error(&r.result.plane, &region, stripes);
    assert!(worst <= 1e-5, "normal patch: mean {mean} worst {worst}");
    let blend = r.receipt.blend.expect("healing receipt");
    assert!(blend.converged && blend.source_offset == Some((12, 0)));
    assert!(r.receipt.guided && r.receipt.levels.is_empty());
    assert_eq!(r.result.plane.at(30, 12), target.at(30, 12), "the source is not modified");

    // Content-aware: vertical stripes left, horizontal stripes right; the hole sits in the
    // vertical half. Unguided, the fill continues the vertical stripes; dragged onto the
    // horizontal block with structure 7, its interior follows the dragged source.
    let canvas = rect(0, 0, 64, 40);
    let target = plane(canvas, |x, y| if x < 32 { stripes(x, y) } else { hstripes(x, y) });
    let region = hole(canvas, RectF { x: 8.0, y: 14.0, width: 10.0, height: 10.0 });
    let inner = rect(11, 17, 4, 4);
    let none = ContentAwareOptions { colour_adaptation: ColourAdaptation::None, ..d };
    let plain = fill(&target, &region, SamplingArea::Auto, &none, &limits).unwrap();
    let e_plain = rect_error(&plain.result.plane, inner, stripes);
    let guided = patch(
        &target,
        &region,
        (32, 0),
        PatchMode::ContentAware { structure: 7 },
        SamplingArea::Auto,
        &none,
        &limits,
    )
    .unwrap();
    let e_guided = rect_error(&guided.result.plane, inner, hstripes);
    println!("plain vs vertical {e_plain:.3e}; guided vs horizontal {e_guided:.3e}; receipt {:?}", guided.receipt);
    assert!(e_plain <= 0.05, "unguided fill continues the surroundings: {e_plain}");
    assert!(e_guided <= 0.05, "content-aware patch follows the dragged source: {e_guided}");
    assert!(guided.receipt.guided && !guided.receipt.levels.is_empty());
    assert_eq!(guided.result.damage, Some(rect(8, 14, 10, 10)));
}

#[test]
fn move_fills_the_vacated_area_and_places_the_object() {
    let limits = RetouchLimits::default();
    let canvas = rect(0, 0, 48, 32);
    let red = [1.0f32, 0.0, 0.0, 1.0];
    let object = |x: i64, y: i64| (8..16).contains(&x) && (12..20).contains(&y);
    let target = plane(canvas, |x, y| if object(x, y) { red } else { stripes(x, y) });
    // The selection keeps a one-pixel margin of background around the object.
    let sel = hole(canvas, RectF { x: 7.0, y: 11.0, width: 10.0, height: 10.0 });
    let moved = |x: i64, y: i64| if object(x - 24, y) { red } else { stripes(x, y) };
    for adaptation in [ColourAdaptation::None, ColourAdaptation::Default] {
        let o = ContentAwareOptions { colour_adaptation: adaptation, ..ContentAwareOptions::default() };
        let r = content_aware_move(&target, &sel, (24, 0), MoveMode::Move, SamplingArea::Auto, &o, &limits).unwrap();
        let (mean, worst) = hole_error(&r.result.plane, &sel, stripes);
        println!("move {adaptation:?}: vacated mean {mean:.2e} worst {worst:.2e}; paste {:?}", r.paste);
        assert!(mean <= 0.02 && worst <= 0.1, "vacated area continues the stripes: {mean} {worst}");
        assert!(rect_error(&r.result.plane, rect(31, 11, 10, 10), moved) <= 1e-5, "object placed");
        assert_eq!(r.result.damage, Some(rect(7, 11, 34, 10)));
        assert!(r.fill.as_ref().is_some_and(|f| !f.levels.is_empty()));
        assert_eq!(r.paste.is_some(), adaptation == ColourAdaptation::Default);
        if let Some(p) = r.paste {
            assert!(p.converged && p.source_offset == Some((-24, 0)));
        }
        // Extend keeps the original.
        let e = content_aware_move(&target, &sel, (24, 0), MoveMode::Extend, SamplingArea::Auto, &o, &limits).unwrap();
        assert!(rect_error(&e.result.plane, rect(8, 12, 8, 8), |_, _| red) <= 1e-5);
        assert!(rect_error(&e.result.plane, rect(32, 12, 8, 8), |_, _| red) <= 1e-5);
        assert!(e.fill.is_none());
        assert_eq!(e.result.damage, Some(rect(31, 11, 10, 10)));
        // Thread count does not change the bytes.
        let t = content_aware_move(
            &target,
            &sel,
            (24, 0),
            MoveMode::Move,
            SamplingArea::Auto,
            &ContentAwareOptions { threads: 4, ..o },
            &limits,
        )
        .unwrap();
        assert_eq!(t, r);
    }
}

#[test]
fn hostile_patch_and_move_inputs_are_typed_refusals() {
    let limits = RetouchLimits::default();
    let canvas = rect(0, 0, 32, 32);
    let target = plane(canvas, stripes);
    let region = hole(canvas, RectF { x: 8.0, y: 8.0, width: 6.0, height: 6.0 });
    let d = ContentAwareOptions::default();
    let auto = SamplingArea::Auto;
    let pcode = |r: Result<ContentAwareResult, hsk_studio_pigment::retouch::RetouchError>| r.unwrap_err().code();
    let mcode = |r: Result<MoveResult, hsk_studio_pigment::retouch::RetouchError>| r.unwrap_err().code();
    for structure in [0u8, 8, 255] {
        let m = PatchMode::ContentAware { structure };
        assert_eq!(pcode(patch(&target, &region, (8, 0), m, auto, &d, &limits)), "retouch_out_of_range");
    }
    // Source outside the target, or an offset whose rectangle overflows.
    assert_eq!(pcode(patch(&target, &region, (20, 0), PatchMode::Normal, auto, &d, &limits)), "retouch_out_of_range");
    assert_eq!(pcode(patch(&target, &region, (i64::MAX, 0), PatchMode::Normal, auto, &d, &limits)), "overflow");
    assert_eq!(mcode(content_aware_move(&target, &region, (0, i64::MAX), MoveMode::Move, auto, &d, &limits)), "overflow");
    // Malformed selection planes and unsupported options.
    let bad = CoveragePlane { bounds: rect(0, 0, 4, 4), data: vec![65535; 15] };
    assert_eq!(pcode(patch(&target, &bad, (8, 0), PatchMode::Normal, auto, &d, &limits)), "retouch_shape");
    assert_eq!(mcode(content_aware_move(&target, &bad, (8, 0), MoveMode::Move, auto, &d, &limits)), "retouch_shape");
    // Mirror is implemented since SMB-PIGMENT-CONTENT-AWARE-2 (was refused); the healing patch
    // refuses it (no search to adapt).
    let mirror = ContentAwareOptions { mirror: true, ..d };
    assert!(content_aware_move(&target, &region, (8, 0), MoveMode::Move, auto, &mirror, &limits).is_ok());
    assert_eq!(pcode(patch(&target, &region, (8, 0), PatchMode::Normal, auto, &mirror, &limits)), "content_aware_option_unsupported");
    let tight = RetouchLimits { max_work: 1_000, ..limits };
    assert_eq!(mcode(content_aware_move(&target, &region, (8, 0), MoveMode::Move, auto, &d, &tight)), "budget_exceeded");
    // The destination footprint is excluded from the sampling area: a sampling rectangle whose
    // every patch overlaps the footprint has no source; the same rectangle with the destination
    // elsewhere does.
    let around = SamplingArea::Rectangular(rect(20, 4, 14, 14));
    assert_eq!(mcode(content_aware_move(&target, &region, (16, 0), MoveMode::Move, around, &d, &limits)), "content_aware_no_source");
    assert!(content_aware_move(&target, &region, (0, 16), MoveMode::Move, around, &d, &limits).is_ok());
    // A destination entirely outside the target: the vacated area is filled, nothing is placed.
    let gone = content_aware_move(&target, &region, (1000, 0), MoveMode::Move, auto, &d, &limits).unwrap();
    assert!(gone.paste.is_none() && gone.fill.is_some());
    assert_eq!(gone.result.damage, Some(rect(8, 8, 6, 6)));
    let (mean, _) = hole_error(&gone.result.plane, &region, stripes);
    assert!(mean <= 0.02, "vacated area continues the stripes: {mean}");
}
