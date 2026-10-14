//! Lane SMB-PIGMENT-CONTENT-AWARE-2 slice 2: STU-RAS-143 search adaptations of the native fill
//! (colour_adaptation high / very_high = per-channel gain and bias, rotation_adaptation, scale,
//! mirror). Each test restricts the sampling area to a source block whose texture matches the hole
//! surroundings only through the adaptation, so the unadapted fill is the control. Photoshop parity
//! is not claimed (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::content_aware::*;
use hsk_studio_pigment::retouch::{RetouchLimits, RgbaPlane};
use hsk_studio_pigment::selection::{CoveragePlane, RectF, SelectionContext, rect as sel_rect};
use std::f64::consts::PI;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

fn grey(v: f64) -> [f32; 4] {
    let v = v as f32;
    [v, v, v, 1.0]
}

/// Left block `x < 32` = `src`, right block = `dst`; 64 x 48.
fn scene(src: impl Fn(i64, i64) -> f64, dst: impl Fn(i64, i64) -> f64) -> RgbaPlane {
    let b = rect(0, 0, 64, 48);
    let mut data = Vec::new();
    for y in 0..48 {
        for x in 0..64 {
            data.push(grey(if x < 32 { src(x, y) } else { dst(x, y) }));
        }
    }
    RgbaPlane::new(b, data).unwrap()
}

fn hole() -> CoveragePlane {
    sel_rect(RectF { x: 40.0, y: 16.0, width: 12.0, height: 12.0 }, &SelectionContext::new(rect(0, 0, 64, 48))).unwrap()
}

/// Mean absolute grey error over the hole against `expect`.
fn hole_error(out: &RgbaPlane, h: &CoveragePlane, expect: impl Fn(i64, i64) -> f64) -> f64 {
    let (ex, ey) = h.bounds.end().unwrap();
    let (mut sum, mut n) = (0.0f64, 0.0f64);
    for y in h.bounds.y..ey {
        for x in h.bounds.x..ex {
            if h.at(x, y) > 0 {
                sum += (f64::from(out.at(x, y)[0]) - expect(x, y)).abs();
                n += 1.0;
            }
        }
    }
    sum / n
}

type Field = Box<dyn Fn(i64, i64) -> f64>;

const SOURCE: SamplingArea<'static> = SamplingArea::Rectangular(Rect { x: 0, y: 0, width: 32, height: 48 });

#[test]
fn colour_adaptation_fits_gain_and_bias() {
    // Source stripes 0.2 / 0.8 (contrast 0.6); hole surroundings 0.25 / 0.75 (contrast 0.5): the
    // match needs gain 5/6 and bias 1/12 per channel.
    let stripe = |lo: f64, hi: f64| move |x: i64, _y: i64| if x.rem_euclid(4) < 2 { lo } else { hi };
    let target = scene(stripe(0.2, 0.8), stripe(0.25, 0.75));
    let h = hole();
    let limits = RetouchLimits::default();
    let mut errs = Vec::new();
    for c in [ColourAdaptation::Default, ColourAdaptation::High, ColourAdaptation::VeryHigh] {
        let o = ContentAwareOptions { colour_adaptation: c, ..ContentAwareOptions::default() };
        let r = fill(&target, &h, SOURCE, &o, &limits).unwrap();
        let e = hole_error(&r.result.plane, &h, stripe(0.25, 0.75));
        println!("colour {c:?}: mean error {e:.4e}, final energy {:.3e}, work {}", r.receipt.final_energy, r.receipt.work);
        errs.push((e, r.receipt.work));
    }
    let (default, high, very_high) = (errs[0], errs[1], errs[2]);
    // Stated errors: very_high reaches the gain (5/6 >= 0.75) -> mean <= 0.01; high clamps the gain
    // at 0.9 (contrast 0.54 vs 0.5) -> between; default copies the 0.6 contrast.
    assert!(very_high.0 <= 0.01, "very_high {}", very_high.0);
    assert!(high.0 < default.0 && very_high.0 < high.0, "{errs:?}");
    assert!(default.0 >= 0.02, "the control misses the contrast: {}", default.0);
    // The adapted distance is charged (statistics pass).
    assert!(high.1 > default.1);
}

#[test]
fn transformed_search_matches_rotated_scaled_and_mirrored_sources() {
    let h = hole();
    let limits = RetouchLimits::default();
    let none = ContentAwareOptions {
        colour_adaptation: ColourAdaptation::None,
        search_iterations: 6,
        ..ContentAwareOptions::default()
    };
    let wave = |period: f64, angle_deg: f64| {
        let (s, c) = (angle_deg * PI / 180.0).sin_cos();
        move |x: i64, y: i64| 0.5 + 0.4 * (2.0 * PI * (x as f64 * c + y as f64 * s) / period).sin()
    };
    // (name, source, hole surroundings, adapted options)
    let rotation = ContentAwareOptions { rotation_adaptation: RotationAdaptation::Medium, ..none };
    let scale = ContentAwareOptions { scale: true, ..none };
    let mirror = ContentAwareOptions { mirror: true, ..none };
    let saw = |x: i64| 0.2 + 0.6 * x.rem_euclid(8) as f64 / 7.0;
    let cases: [(&str, Field, Field, ContentAwareOptions, f64); 3] = [
        // Waves at 30 degrees vs vertical waves: rotation 30 = 6 steps (medium allows 9).
        ("rotation", Box::new(wave(8.0, 30.0)), Box::new(wave(8.0, 0.0)), rotation, 0.05),
        // Period 8 * 2^(3/8) vs 8: scale step 3.
        ("scale", Box::new(wave(8.0 * 2f64.powf(0.375), 0.0)), Box::new(wave(8.0, 0.0)), scale, 0.05),
        // Rising vs falling sawtooth: a horizontal mirror (integer taps, exact).
        ("mirror", Box::new(move |x, _| saw(x)), Box::new(move |x, _| saw(-x)), mirror, 0.01),
    ];
    for (name, src, dst, adapted, bound) in cases {
        let target = scene(&src, &dst);
        let plain = fill(&target, &h, SOURCE, &none, &limits).unwrap();
        let fitted = fill(&target, &h, SOURCE, &adapted, &limits).unwrap();
        let (e0, e1) = (hole_error(&plain.result.plane, &h, &dst), hole_error(&fitted.result.plane, &h, &dst));
        println!(
            "{name}: unadapted {e0:.4e} (energy {:.3e}), adapted {e1:.4e} (energy {:.3e}, work {} vs {})",
            plain.receipt.final_energy, fitted.receipt.final_energy, fitted.receipt.work, plain.receipt.work
        );
        assert!(e1 <= bound, "{name}: adapted error {e1}");
        assert!(e0 >= 2.0 * e1 && e0 >= 0.02, "{name}: the control must miss: {e0} vs {e1}");
        assert!(fitted.receipt.final_energy < plain.receipt.final_energy, "{name}: energy");
        assert!(fitted.receipt.work > plain.receipt.work, "{name}: transformed search is charged");
    }
}

#[test]
fn hostile_adaptation_inputs_are_typed_refusals() {
    let stripe = |x: i64, _y: i64| if x.rem_euclid(4) < 2 { 0.1 } else { 0.9 };
    let target = scene(stripe, stripe);
    let h = hole();
    let limits = RetouchLimits::default();
    let d = ContentAwareOptions::default();
    let all = ContentAwareOptions {
        colour_adaptation: ColourAdaptation::VeryHigh,
        rotation_adaptation: RotationAdaptation::Full,
        scale: true,
        mirror: true,
        ..d
    };
    assert!(!d.adapts_search() && all.adapts_search());
    // The adapted search is charged before synthesis: a budget that admits the plain fill refuses
    // the adapted one (budget_exceeded, never a silently cheaper search).
    let plain = fill(&target, &h, SamplingArea::Auto, &d, &limits).unwrap();
    let work = RetouchLimits { max_work: plain.receipt.work, ..limits };
    assert!(fill(&target, &h, SamplingArea::Auto, &d, &work).is_ok());
    assert_eq!(fill(&target, &h, SamplingArea::Auto, &all, &work).unwrap_err().code(), "budget_exceeded");
    // Every adaptation is refused by the healing patch (no search), never ignored.
    for o in [
        ContentAwareOptions { colour_adaptation: ColourAdaptation::High, ..d },
        ContentAwareOptions { rotation_adaptation: RotationAdaptation::Full, ..d },
        ContentAwareOptions { scale: true, ..d },
        all,
    ] {
        let r = patch(&target, &h, (-24, 0), PatchMode::Normal, SamplingArea::Auto, &o, &limits);
        assert_eq!(r.unwrap_err().code(), "content_aware_option_unsupported");
        // The guided patch and the move accept them.
        assert!(patch(&target, &h, (-24, 0), PatchMode::ContentAware { structure: 4 }, SamplingArea::Auto, &o, &limits).is_ok());
        assert!(content_aware_move(&target, &h, (-24, 0), MoveMode::Move, SamplingArea::Auto, &o, &limits).is_ok());
    }
    // A sampling area too small for any patch stays a typed refusal with every adaptation.
    let tiny = SamplingArea::Rectangular(rect(0, 0, 5, 5));
    assert_eq!(fill(&target, &h, tiny, &all, &limits).unwrap_err().code(), "content_aware_no_source");
}
