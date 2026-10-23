//! Keying and matte refinement (lane SMB-AE-KEY-TRACK slice 1). Synthetic planes with known key
//! colours; expected values are derived by hand from the formulas documented in `src/keying.rs`.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::keying::*;
use hsk_studio_pigment::retouch::RgbaPlane;

fn rect(width: u32, height: u32) -> Rect {
    Rect { x: 0, y: 0, width, height }
}

fn row(pixels: &[[f32; 4]]) -> RgbaPlane {
    RgbaPlane::new(rect(pixels.len() as u32, 1), pixels.to_vec()).unwrap()
}

fn plane(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.push(f(x, y));
        }
    }
    RgbaPlane::new(rect(w, h), data).unwrap()
}

fn matte_of(src: &RgbaPlane, stages: &[KeyStage], plate: Option<&RgbaPlane>) -> Vec<f32> {
    run_key_pipeline(src, stages, plate, &KeyLimits::DEFAULT).unwrap().matte.data
}

fn close(a: f32, b: f64, eps: f64) -> bool {
    (f64::from(a) - b).abs() <= eps
}

#[test]
fn extractors_on_known_key_colours() {
    // Deterministic cube root.
    assert!((det_cbrt(27.0) - 3.0).abs() < 1e-15);
    assert!((det_cbrt(0.125) - 0.5).abs() < 1e-15);
    assert_eq!(det_cbrt(-1.0), 0.0);

    // Chroma key, YCbCr: key + a grey offset has the key's chroma and a luma offset of 0.2.
    let key = [0.0, 0.8, 0.0];
    let src = row(&[[0.0, 0.8, 0.0, 1.0], [0.2, 1.0, 0.2, 1.0], [1.0, 0.0, 0.0, 1.0]]);
    let chroma = |space, luma_weight| ChromaKey { key, space, tolerance: 0.1, softness: 0.2, luma_weight };
    let m = matte_of(&src, &[KeyStage::Chroma(chroma(ChromaSpace::YCbCr, 0.0), MatteCombine::Replace)], None);
    assert_eq!(m, vec![0.0, 0.0, 1.0], "chroma-only distance ignores the luma offset");
    let m = matte_of(&src, &[KeyStage::Chroma(chroma(ChromaSpace::YCbCr, 1.0), MatteCombine::Replace)], None);
    assert_eq!(m[0], 0.0);
    assert!(close(m[1], 0.5, 1e-6), "luma distance 0.2 -> ramp (0.2 - 0.1) / 0.2, got {}", m[1]);
    assert_eq!(m[2], 1.0);
    // Chroma key, Lab.
    let m = matte_of(&src, &[KeyStage::Chroma(chroma(ChromaSpace::Lab, 1.0), MatteCombine::Replace)], None);
    assert_eq!(m[0], 0.0);
    assert_eq!(m[2], 1.0);

    // Luma key on greys (BT.709 weights sum to 1, so Y = the grey level).
    let greys = row(&[[0.2, 0.2, 0.2, 1.0], [0.6, 0.6, 0.6, 1.0], [0.9, 0.9, 0.9, 1.0]]);
    let luma = |kind| KeyStage::Luma(LumaKey { kind, threshold: 0.5, tolerance: 0.1, softness: 0.2 }, MatteCombine::Replace);
    let m = matte_of(&greys, &[luma(LumaKeyType::KeyOutDarker)], None);
    assert_eq!(m[0], 0.0);
    assert!(close(m[1], 0.5, 1e-6), "darker: (0.6 - 0.5) / 0.2, got {}", m[1]);
    assert_eq!(m[2], 1.0);
    let m = matte_of(&greys, &[luma(LumaKeyType::KeyOutBrighter)], None);
    assert_eq!(m, vec![1.0, 0.0, 0.0]);
    let m = matte_of(&greys, &[luma(LumaKeyType::KeyOutSimilar)], None);
    // |0.6 - 0.5| sits on the tolerance 0.1 (f32 0.6 rounds up by 2.4e-8): at most 1.2e-7 above 0.
    assert!(m[1] < 1e-6 && close(m[0], 1.0, 1e-6) && m[2] == 1.0, "{m:?}");

    // Difference matte: one pixel differs from the clean plate by 0.5 in red.
    let plate = plane(5, 5, |_, _| [0.3, 0.3, 0.3, 1.0]);
    let shot = plane(5, 5, |x, y| if (x, y) == (2, 2) { [0.8, 0.3, 0.3, 1.0] } else { [0.3, 0.3, 0.3, 1.0] });
    let diff = KeyStage::Difference(DifferenceMatte { tolerance: 0.1, softness: 0.1, blur_px: 0.0 }, MatteCombine::Replace);
    let m = matte_of(&shot, std::slice::from_ref(&diff), Some(&plate));
    assert_eq!(m.iter().filter(|v| **v == 1.0).count(), 1);
    assert_eq!(m[12], 1.0);
    assert_eq!(m.iter().filter(|v| **v == 0.0).count(), 24);

    // Colour range key in RGB: add box [0, 0.1] x [0.8, 1] x [0, 0.1], fuzziness 0.1.
    let range = KeyStage::ColourRange(
        ColourRangeKey {
            space: RangeSpace::Rgb,
            add: vec![[0.0, 0.8, 0.0], [0.1, 1.0, 0.1]],
            subtract: vec![[0.05, 0.95, 0.05]],
            fuzziness: 0.1,
        },
        MatteCombine::Replace,
    );
    let src = row(&[[0.02, 0.93, 0.08, 1.0], [0.0, 0.75, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [0.05, 0.95, 0.05, 1.0]]);
    let m = matte_of(&src, &[range], None);
    // Pixel 0 is inside the add box but 0.03 from the subtract point: 1 - ramp(0.03, 0, 0.1) = 0.7.
    assert!(close(m[0], 0.7, 1e-6), "got {}", m[0]);
    assert!(close(m[1], 0.5, 1e-6), "0.05 below the box -> 0.5, got {}", m[1]);
    assert_eq!(m[2], 1.0);
    assert_eq!(m[3], 1.0, "the subtract sample itself is kept");
}

#[test]
fn despill_limits_the_screen_channel_to_a_neutral() {
    let src = row(&[[0.5, 0.8, 0.5, 1.0], [0.4, 0.4, 0.4, 1.0], [0.2, 0.3, 0.9, 1.0]]);
    let green = |preserve_luminance| {
        KeyStage::Despill(Despill { screen: SpillScreen::Green, limit: SpillLimit::Average, amount: 1.0, preserve_luminance })
    };
    let out = run_key_pipeline(&src, &[green(false)], None, &KeyLimits::DEFAULT).unwrap().colour.data;
    assert_eq!(out[0], [0.5, 0.5, 0.5, 1.0], "green limited to the red/blue average: a neutral");
    assert_eq!(out[1], src.data[1], "a neutral grey is unchanged bit for bit");
    assert_eq!(out[2], src.data[2], "green below the limit is unchanged");
    let out = run_key_pipeline(&src, &[green(true)], None, &KeyLimits::DEFAULT).unwrap().colour.data;
    let lifted = 0.5 + 0.7152 * (f64::from(0.8f32) - 0.5);
    for c in 0..3 {
        assert!(close(out[0][c], lifted, 1e-6), "luminance restored on every channel: {:?}", out[0]);
    }
    // Blue screen, max limit, half amount: B' = 0.9 - 0.5 (0.9 - 0.3) = 0.6.
    let blue = KeyStage::Despill(Despill { screen: SpillScreen::Blue, limit: SpillLimit::Max, amount: 0.5, preserve_luminance: false });
    let out = run_key_pipeline(&src, &[blue], None, &KeyLimits::DEFAULT).unwrap().colour.data;
    assert!(close(out[2][2], 0.6, 1e-6), "got {:?}", out[2]);
    assert_eq!(&out[2][..2], &[0.2, 0.3]);
}

#[test]
fn refine_choke_spread_feather_and_edge_aware_smoothing() {
    // A white 3x3 square on black, keyed by luma (darker removed): matte 1 on the square only.
    let sq = plane(9, 9, |x, y| if (3..6).contains(&x) && (3..6).contains(&y) { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] });
    let key = KeyStage::Luma(
        LumaKey { kind: LumaKeyType::KeyOutDarker, threshold: 0.5, tolerance: 0.0, softness: 0.0 },
        MatteCombine::Replace,
    );
    let base = matte_of(&sq, std::slice::from_ref(&key), None);
    assert_eq!(base.iter().filter(|v| **v == 1.0).count(), 9);
    // Spread 1 (disc r=1 = plus): 9 + 4 * 3 = 21 ones, the diagonal corner (2, 2) stays 0.
    let spread = matte_of(&sq, &[key.clone(), KeyStage::ChokeSpread { radius_px: 1 }], None);
    assert_eq!(spread.iter().filter(|v| **v == 1.0).count(), 21);
    assert_eq!(spread[2 * 9 + 2], 0.0);
    // Choke 1: only the centre keeps all four neighbours.
    let choke = matte_of(&sq, &[key.clone(), KeyStage::ChokeSpread { radius_px: -1 }], None);
    assert_eq!(choke.iter().filter(|v| **v == 1.0).count(), 1);
    assert_eq!(choke[4 * 9 + 4], 1.0);
    // Feather 2: mass preserved away from the border, symmetric.
    let feather = matte_of(&sq, &[key.clone(), KeyStage::Feather { radius_px: 2.0 }], None);
    let sum: f64 = feather.iter().map(|v| f64::from(*v)).sum();
    assert!((sum - 9.0).abs() < 1e-4, "feather sum {sum}");
    assert_eq!(feather[4 * 9 + 3], feather[4 * 9 + 5]);
    assert!(feather[4 * 9 + 2] > 0.0 && feather[4 * 9 + 2] < 1.0);

    // Edge-aware smoothing: left half black (keyed out), right half white (kept) with one 0.9-grey
    // pixel keyed by a second (similar) key: a matte hole in a near-flat colour region.
    let img = plane(12, 9, |x, y| {
        if x < 6 {
            [0.0, 0.0, 0.0, 1.0]
        } else if (x, y) == (9, 4) {
            [0.9, 0.9, 0.9, 1.0]
        } else {
            [1.0; 4]
        }
    });
    let hole = KeyStage::Luma(
        LumaKey { kind: LumaKeyType::KeyOutSimilar, threshold: 0.9, tolerance: 0.01, softness: 0.0 },
        MatteCombine::Multiply,
    );
    let at = |x: usize, y: usize| y * 12 + x;
    let raw = matte_of(&img, &[key.clone(), hole.clone()], None);
    assert_eq!((raw[at(5, 4)], raw[at(6, 4)], raw[at(9, 4)]), (0.0, 1.0, 0.0));
    let smooth = matte_of(&img, &[key.clone(), hole.clone(), KeyStage::EdgeSmooth { radius_px: 2, colour_sigma: 0.2 }], None);
    assert!(smooth[at(9, 4)] > 0.9, "the hole is filled from its similar-colour neighbours: {}", smooth[at(9, 4)]);
    assert!(smooth[at(6, 4)] > 0.98 && smooth[at(5, 4)] < 0.02, "the colour edge stays sharp: {} {}", smooth[at(5, 4)], smooth[at(6, 4)]);
    let blurred = matte_of(&img, &[key, hole, KeyStage::Feather { radius_px: 2.0 }], None);
    assert!(blurred[at(5, 4)] > 0.1, "a plain feather softens the same edge: {}", blurred[at(5, 4)]);
}

#[test]
fn pipeline_is_bit_deterministic_and_gpu_refuses_typed() {
    let src = plane(16, 12, |x, y| {
        let t = (x as f32 * 0.37 + y as f32 * 0.11).fract();
        if (x + y) % 3 == 0 { [0.1, 0.85 - 0.1 * t, 0.15, 1.0] } else { [t, 0.4 + 0.3 * t, 0.6 * t, 1.0] }
    });
    let stages = vec![
        KeyStage::Chroma(
            ChromaKey { key: [0.1, 0.8, 0.15], space: ChromaSpace::Lab, tolerance: 0.05, softness: 0.3, luma_weight: 0.5 },
            MatteCombine::Replace,
        ),
        KeyStage::Levels { black: 0.1, white: 0.9 },
        KeyStage::ChokeSpread { radius_px: -1 },
        KeyStage::EdgeSmooth { radius_px: 2, colour_sigma: 0.3 },
        KeyStage::Feather { radius_px: 1.5 },
        KeyStage::Despill(Despill { screen: SpillScreen::Green, limit: SpillLimit::Average, amount: 1.0, preserve_luminance: true }),
    ];
    let a = run_key_pipeline(&src, &stages, None, &KeyLimits::DEFAULT).unwrap();
    let b = run_key_pipeline(&src, &stages, None, &KeyLimits::DEFAULT).unwrap();
    let bits = |r: &KeyResult| -> Vec<u32> {
        r.matte.data.iter().map(|v| v.to_bits()).chain(r.colour.data.iter().flatten().map(|v| v.to_bits())).collect()
    };
    assert_eq!(bits(&a), bits(&b));
    let (lo, hi) = a.matte.data.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    assert!(lo < hi, "the key separates something: {lo} {hi}");
    for (p, m) in a.colour.data.iter().zip(&a.matte.data) {
        assert_eq!(p[3], *m, "opaque source: alpha equals the matte");
    }
    for s in &stages {
        let e = gpu_admission(s).unwrap_err();
        assert_eq!(e, KeyError::GpuUnsupported { stage: s.key() });
        assert_eq!(e.code(), KEY_GPU_REFUSAL);
    }
    assert_eq!(KEY_EXACTNESS, hsk_studio_pigment::blend_math::Exactness::Approximate);
}

#[test]
fn hostile_inputs_are_refused_before_allocation() {
    let src = plane(100, 100, |_, _| [0.5, 0.5, 0.5, 1.0]);
    let run = |p: &RgbaPlane, s: &[KeyStage], plate: Option<&RgbaPlane>, l: KeyLimits| run_key_pipeline(p, s, plate, &l).unwrap_err();
    let ok = KeyLimits::DEFAULT;
    let chroma = |tolerance| {
        KeyStage::Chroma(ChromaKey { key: [0.0, 1.0, 0.0], space: ChromaSpace::YCbCr, tolerance, softness: 0.1, luma_weight: 0.0 }, MatteCombine::Replace)
    };
    assert_eq!(run(&src, &[chroma(f32::NAN)], None, ok), KeyError::Parameter);
    assert_eq!(run(&src, &[chroma(3.0)], None, ok), KeyError::Parameter);
    assert_eq!(run(&src, &[KeyStage::ChokeSpread { radius_px: i32::MIN }], None, ok), KeyError::Parameter);
    assert_eq!(run(&src, &[KeyStage::Feather { radius_px: f32::INFINITY }], None, ok), KeyError::Parameter);
    assert_eq!(run(&src, &[KeyStage::Levels { black: 0.5, white: 0.5 }], None, ok), KeyError::Parameter);
    assert_eq!(run(&src, &vec![KeyStage::Levels { black: 0.0, white: 1.0 }; MAX_KEY_STAGES + 1], None, ok), KeyError::Parameter);
    let empty_range = KeyStage::ColourRange(
        ColourRangeKey { space: RangeSpace::Lab, add: vec![], subtract: vec![], fuzziness: 0.1 },
        MatteCombine::Replace,
    );
    assert_eq!(run(&src, &[empty_range], None, ok), KeyError::Parameter);
    // Planes: NaN sample, wrong length, dims_u32_max with no data (no allocation of the area).
    let mut nan = src.clone();
    nan.data[7][1] = f32::NAN;
    assert_eq!(run(&nan, &[chroma(0.1)], None, ok), KeyError::OutOfDomain);
    let huge = RgbaPlane { bounds: Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX }, data: vec![] };
    assert_eq!(run(&huge, &[chroma(0.1)], None, ok), KeyError::Shape);
    let far = RgbaPlane { bounds: Rect { x: i64::MAX, y: 0, width: 2, height: 1 }, data: vec![[0.0; 4]; 2] };
    assert_eq!(run(&far, &[chroma(0.1)], None, ok), KeyError::Overflow);
    // Clean plate missing or misplaced.
    let diff = KeyStage::Difference(DifferenceMatte { tolerance: 0.1, softness: 0.1, blur_px: 0.0 }, MatteCombine::Replace);
    assert_eq!(run(&src, std::slice::from_ref(&diff), None, ok), KeyError::MissingCleanPlate);
    let small = plane(10, 10, |_, _| [0.5; 4]);
    assert_eq!(run(&src, std::slice::from_ref(&diff), Some(&small), ok), KeyError::Shape);
    // Aggregate bytes and work over budget.
    let tight = KeyLimits { max_bytes: 1_000, max_work: u64::MAX };
    assert!(matches!(run(&src, &[chroma(0.1)], None, tight), KeyError::OverBudget { needed: 280_000, limit: 1_000 }));
    let tight = KeyLimits { max_bytes: u64::MAX, max_work: 1_000_000 };
    let e = run(&src, &[KeyStage::ChokeSpread { radius_px: 256 }], None, tight);
    assert_eq!(e, KeyError::OverBudget { needed: 10_000 * 513 * 513, limit: 1_000_000 });
}
