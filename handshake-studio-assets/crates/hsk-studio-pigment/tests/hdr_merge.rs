//! Lane SMB-HDR-MERGE-1: merge bracketed exposures to 32-bit HDR ([STU-RAS-033], [STU-RAS-149]).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::hdr_merge::{
    AlignOptions, Bracket, DepthRole, Exposure, HDR_CAPABILITIES, HDR_MERGE_CAPABILITY, HDR_MERGE_DESCRIPTOR,
    HDR_TONE_MAP_CAPABILITY, HdrError, HdrLimits, MergeOptions, ToneMapOptions, merge_brackets, tone_map,
};
use hsk_studio_prism::DisplayEncoding;
use hsk_studio_pigment::retouch::RgbaPlane;

fn rect(width: u32, height: u32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// Render scene radiance `scene(x, y)` (rgb) at relative exposure `scale`, clipped at 1.
fn render(width: u32, height: u32, scale: f32, scene: impl Fn(i64, i64) -> [f32; 3]) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..i64::from(height) {
        for x in 0..i64::from(width) {
            let l = scene(x, y);
            data.push([
                (l[0] * scale).min(1.0),
                (l[1] * scale).min(1.0),
                (l[2] * scale).min(1.0),
                1.0,
            ]);
        }
    }
    RgbaPlane {
        bounds: rect(width, height),
        data,
    }
}

fn ev(frame: &RgbaPlane, v: f64) -> Bracket<'_> {
    Bracket {
        frame,
        exposure: Some(Exposure::Ev(v)),
        depth: 16,
    }
}

fn merge(brackets: &[Bracket<'_>], options: &MergeOptions) -> Result<hsk_studio_pigment::hdr_merge::HdrMerged, HdrError> {
    merge_brackets(brackets, options, &HdrLimits::DEFAULT, &CancellationToken::default())
}

/// Log ramp from 1e-4 to 3.5 (below the -2 EV frame's clip) over the pixels.
fn ramp_scene(x: i64, y: i64) -> [f32; 3] {
    let i = (y * 32 + x) as f64 / 255.0;
    let l = (1e-4f64.ln() + i * (3.5f64.ln() - 1e-4f64.ln())).exp() as f32;
    [l, 0.7 * l, 0.4 * l]
}

#[test]
fn merge_recovers_known_radiance_from_synthetic_brackets() {
    let dark = render(32, 8, 0.25, ramp_scene);
    let mid = render(32, 8, 1.0, ramp_scene);
    let bright = render(32, 8, 4.0, ramp_scene);
    let merged = merge(&[ev(&dark, -2.0), ev(&mid, 0.0), ev(&bright, 2.0)], &MergeOptions::default()).unwrap();
    assert_eq!(merged.depth, 32);
    assert_eq!(merged.reference, 1, "median exposure is the reference");
    assert_eq!(merged.relative_exposure, vec![0.25, 1.0, 4.0]);
    let mut above_one = 0;
    for y in 0..8 {
        for x in 0..32 {
            let want = ramp_scene(x, y);
            let got = merged.frame.at(x, y);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() <= 1e-4,
                    "pixel ({x},{y}) channel {c}: got {} want {}",
                    got[c],
                    want[c]
                );
            }
            assert_eq!(got[3], 1.0);
            if got[0] > 1.0 {
                above_one += 1;
            }
        }
    }
    assert!(above_one > 0, "32-bit result stores radiance beyond display range");
    assert!(merged.fallback_pixels > 0, "the darkest ramp pixels use the longest-exposure fallback");
    println!("NOT_PROVEN: Photoshop Merge to HDR Pro / Lightroom HDR merge parity (no D5 oracle; weights and thresholds are Studio readings)");
    println!("NOT_PROVEN: real camera brackets (all brackets in tests/hdr_merge.rs are synthesised from one scene)");
}

#[test]
fn clipped_highlights_come_from_the_darker_frame() {
    // Bright frame at 0 EV (reference), dark frame at -3 EV (1/8 of the light).
    let scene = |x: i64, _y: i64| -> [f32; 3] {
        match x {
            0 => [2.0, 0.5, 0.2],   // red clipped in the bright frame only
            1 => [0.99, 0.99, 0.99], // near clip (above 0.98) in the bright frame
            2 => [6.0, 6.0, 6.0],   // far above display range
            _ => [0.3, 0.2, 0.1],   // unclipped in both
        }
    };
    let bright = render(4, 1, 1.0, scene);
    let dark = render(4, 1, 0.125, scene);
    // A wrong clipped value: if the bright frame contributed anywhere it would show.
    let mut poisoned = bright.clone();
    poisoned.data[2] = [1.0, 1.0, 1.0, 1.0];
    let options = MergeOptions {
        reference: Some(0),
        ..MergeOptions::default()
    };
    let merged = merge(&[ev(&poisoned, 0.0), ev(&dark, -3.0)], &options).unwrap();
    assert_eq!(merged.relative_exposure, vec![1.0, 0.125]);
    let from_dark = |x: usize| {
        let z = dark.data[x];
        [z[0] / 0.125, z[1] / 0.125, z[2] / 0.125]
    };
    for x in 0..3 {
        let got = merged.frame.data[x];
        assert_eq!([got[0], got[1], got[2]], from_dark(x), "pixel {x} comes from the darker frame only");
    }
    assert_eq!(&merged.frame.data[0][..3], &[2.0, 0.5, 0.2]);
    assert_eq!(&merged.frame.data[2][..3], &[6.0, 6.0, 6.0]);
    let got = merged.frame.data[3];
    for (c, want) in [0.3f32, 0.2, 0.1].into_iter().enumerate() {
        assert!((got[c] - want).abs() <= 1e-6);
    }
    // Every sample clipped: the shortest exposure supplies the lower bound.
    let all = render(1, 1, 1.0, |_, _| [9.0, 9.0, 9.0]);
    let all_dark = render(1, 1, 0.125, |_, _| [9.0, 9.0, 9.0]);
    let m = merge(&[ev(&all, 0.0), ev(&all_dark, -3.0)], &options).unwrap();
    assert_eq!(&m.frame.data[0][..3], &[8.0, 8.0, 8.0]);
    assert_eq!(m.fallback_pixels, 1);
}

#[test]
fn merge_refuses_mismatched_or_unexposed_inputs() {
    let a = render(4, 2, 1.0, |_, _| [0.5, 0.5, 0.5]);
    let b = render(4, 2, 0.5, |_, _| [0.5, 0.5, 0.5]);
    let small = render(3, 2, 0.5, |_, _| [0.5, 0.5, 0.5]);
    let opts = MergeOptions::default();
    assert_eq!(merge(&[ev(&a, 0.0)], &opts).unwrap_err(), HdrError::TooFewFrames { count: 1 });
    let many: Vec<Bracket<'_>> = (0..17).map(|i| ev(&a, f64::from(i))).collect();
    assert_eq!(merge(&many, &opts).unwrap_err(), HdrError::TooManyFrames { count: 17, limit: 16 });
    let missing = Bracket {
        frame: &b,
        exposure: None,
        depth: 16,
    };
    assert_eq!(merge(&[ev(&a, 0.0), missing], &opts).unwrap_err(), HdrError::MissingExposure { index: 1 });
    assert_eq!(merge(&[ev(&a, 0.0), ev(&small, -1.0)], &opts).unwrap_err(), HdrError::SizeMismatch { index: 1 });
    let seconds = Bracket {
        frame: &b,
        exposure: Some(Exposure::Seconds(0.01)),
        depth: 16,
    };
    assert_eq!(merge(&[ev(&a, 0.0), seconds], &opts).unwrap_err(), HdrError::MixedExposureUnits { index: 1 });
    for bad in [Exposure::Seconds(0.0), Exposure::Seconds(-1.0), Exposure::Seconds(f64::INFINITY)] {
        let first = Bracket {
            frame: &a,
            exposure: Some(Exposure::Seconds(0.02)),
            depth: 16,
        };
        let second = Bracket {
            frame: &b,
            exposure: Some(bad),
            depth: 16,
        };
        assert_eq!(merge(&[first, second], &opts).unwrap_err(), HdrError::InvalidExposure { index: 1 });
    }
    assert_eq!(merge(&[ev(&a, 0.0), ev(&b, f64::NAN)], &opts).unwrap_err(), HdrError::InvalidExposure { index: 1 });
    let first_is_reference = MergeOptions {
        reference: Some(0),
        ..MergeOptions::default()
    };
    assert_eq!(
        merge(&[ev(&a, 0.0), ev(&b, -31.0)], &first_is_reference).unwrap_err(),
        HdrError::InvalidExposure { index: 1 }
    );
    let mut nan = b.clone();
    nan.data[3][1] = f32::NAN;
    assert_eq!(merge(&[ev(&a, 0.0), ev(&nan, -1.0)], &opts).unwrap_err(), HdrError::NonFinite { index: 1 });
    let mut short = b.clone();
    short.data.pop();
    assert_eq!(merge(&[ev(&a, 0.0), ev(&short, -1.0)], &opts).unwrap_err(), HdrError::Shape { index: 1 });
    let mut alpha = b.clone();
    alpha.data[0][3] = 1.5;
    assert_eq!(merge(&[ev(&a, 0.0), ev(&alpha, -1.0)], &opts).unwrap_err(), HdrError::NonFinite { index: 1 });
    for bad in [
        MergeOptions {
            black: 0.5,
            clip: 0.4,
            ..MergeOptions::default()
        },
        MergeOptions {
            deghost: 4,
            ..MergeOptions::default()
        },
        MergeOptions {
            align: Some(AlignOptions { max_shift: 0 }),
            ..MergeOptions::default()
        },
        MergeOptions {
            align: Some(AlignOptions { max_shift: 65 }),
            ..MergeOptions::default()
        },
        MergeOptions {
            // 4x2 frames are below the 8-sample alignment minimum.
            align: Some(AlignOptions::default()),
            ..MergeOptions::default()
        },
        MergeOptions {
            black: f32::NAN,
            ..MergeOptions::default()
        },
        MergeOptions {
            reference: Some(2),
            ..MergeOptions::default()
        },
    ] {
        assert_eq!(merge(&[ev(&a, 0.0), ev(&b, -1.0)], &bad).unwrap_err(), HdrError::Parameter);
    }
    // [STU-RAS-149]: a 1-bit (bitmap mode) source is refused naming the depth and the set.
    let bitmap = Bracket {
        frame: &b,
        exposure: Some(Exposure::Ev(-1.0)),
        depth: 1,
    };
    assert_eq!(
        merge(&[ev(&a, 0.0), bitmap], &opts).unwrap_err(),
        HdrError::UnsupportedDepth {
            command: "hdr_merge",
            role: DepthRole::Source,
            depth: 1,
            supported: HDR_MERGE_CAPABILITY.source_depths,
        }
    );
    let tight = HdrLimits {
        max_bytes: 64,
        max_work: 1 << 20,
    };
    assert_eq!(
        merge_brackets(&[ev(&a, 0.0), ev(&b, -1.0)], &opts, &tight, &CancellationToken::default()).unwrap_err(),
        HdrError::OverBudget { needed: 128, limit: 64 }
    );
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert_eq!(
        merge_brackets(&[ev(&a, 0.0), ev(&b, -1.0)], &opts, &HdrLimits::DEFAULT, &cancel).unwrap_err(),
        HdrError::Canceled
    );
}

#[test]
fn merge_is_deterministic() {
    let scene = |x: i64, y: i64| -> [f32; 3] {
        let v = ((x * 7919 + y * 104_729) % 1000) as f32 / 250.0 + 0.001;
        [v, v * 0.5, v * 0.25]
    };
    let frames: Vec<RgbaPlane> = [0.125f32, 0.5, 2.0, 8.0].iter().map(|&s| render(24, 16, s, scene)).collect();
    let brackets: Vec<Bracket<'_>> = frames
        .iter()
        .zip([-3.0, -1.0, 1.0, 3.0])
        .map(|(f, v)| ev(f, v))
        .collect();
    let first = merge(&brackets, &MergeOptions::default()).unwrap();
    for _ in 0..3 {
        let again = merge(&brackets, &MergeOptions::default()).unwrap();
        assert_eq!(again.reference, first.reference);
        assert_eq!(again.fallback_pixels, first.fallback_pixels);
        for (a, b) in again.frame.data.iter().zip(&first.frame.data) {
            assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
        }
    }
    // Exposure times give the same relative exposures and the same bits as the equivalent EVs.
    let timed: Vec<Bracket<'_>> = frames
        .iter()
        .zip([1.0 / 64.0, 1.0 / 16.0, 0.25, 1.0])
        .map(|(f, t)| Bracket {
            frame: f,
            exposure: Some(Exposure::Seconds(t)),
            depth: 32,
        })
        .collect();
    let by_time = merge(&timed, &MergeOptions::default()).unwrap();
    assert_eq!(by_time.relative_exposure, first.relative_exposure);
    for (a, b) in by_time.frame.data.iter().zip(&first.frame.data) {
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
    }
}

/// Blocky deterministic texture (8x8 blocks, values 0.03..0.2), defined on every integer.
fn texture(x: i64, y: i64) -> [f32; 3] {
    let (bx, by) = (x.div_euclid(8) as u64, y.div_euclid(8) as u64);
    let mut h = bx.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ by.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    let v = 0.03 + 0.17 * ((h >> 40) as f32 / 16_777_216.0);
    [v, 0.9 * v, 0.8 * v]
}

#[test]
fn misaligned_bracket_is_shifted_back() {
    let dark = render(128, 128, 0.25, texture);
    let mid = render(128, 128, 1.0, texture);
    // The +2 EV frame's content moved by (+3, -2): it shows texture(x - 3, y + 2).
    let bright = render(128, 128, 4.0, |x, y| texture(x - 3, y + 2));
    let brackets = [ev(&dark, -2.0), ev(&mid, 0.0), ev(&bright, 2.0)];
    let options = MergeOptions {
        align: Some(AlignOptions { max_shift: 8 }),
        ..MergeOptions::default()
    };
    let merged = merge(&brackets, &options).unwrap();
    assert_eq!(merged.shifts, vec![(0, 0), (0, 0), (-3, 2)], "known integer shift recovered");
    for y in 0..128 {
        for x in 0..128 {
            let want = texture(x, y);
            let got = merged.frame.at(x, y);
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() <= 1e-4, "pixel ({x},{y}) channel {c}: {} vs {}", got[c], want[c]);
            }
        }
    }
    // Without alignment the shifted frame blurs block edges.
    let unaligned = merge(&brackets, &MergeOptions::default()).unwrap();
    let worst = unaligned
        .frame
        .data
        .iter()
        .zip(&merged.frame.data)
        .map(|(a, b)| (a[0] - b[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst > 0.01, "unaligned merge differs (worst {worst})");
    // A shift beyond the bound is not searched: max_shift 2 cannot reach (-3, 2).
    let bounded = merge(
        &brackets,
        &MergeOptions {
            align: Some(AlignOptions { max_shift: 2 }),
            ..MergeOptions::default()
        },
    )
    .unwrap();
    assert!(bounded.shifts[2].0.abs() <= 2 && bounded.shifts[2].1.abs() <= 2);
}

/// Background 0.1 with a 4x4 object of radiance 0.4 whose left edge is at `ox`.
fn object_at(ox: i64, x: i64, y: i64) -> [f32; 3] {
    if (ox..ox + 4).contains(&x) && (6..10).contains(&y) {
        [0.4, 0.4, 0.4]
    } else {
        [0.1, 0.1, 0.1]
    }
}

#[test]
fn moving_object_is_not_doubled_with_deghost_on() {
    // The object sits at A in the -2 EV frame, at B in the reference (0 EV) and at C in the
    // +2 EV frame (clipped there).
    let (a, b, c) = (1i64, 6i64, 11i64);
    let dark = render(16, 16, 0.25, |x, y| object_at(a, x, y));
    let mid = render(16, 16, 1.0, |x, y| object_at(b, x, y));
    let bright = render(16, 16, 4.0, |x, y| object_at(c, x, y));
    let brackets = [ev(&dark, -2.0), ev(&mid, 0.0), ev(&bright, 2.0)];
    let deghosted = merge(
        &brackets,
        &MergeOptions {
            deghost: 2,
            ..MergeOptions::default()
        },
    )
    .unwrap();
    for y in 0..16 {
        for x in 0..16 {
            let want = object_at(b, x, y)[0];
            let got = deghosted.frame.at(x, y);
            for v in &got[..3] {
                assert!((v - want).abs() <= 1e-6, "pixel ({x},{y}): {v} vs {want}");
            }
        }
    }
    assert_eq!(deghosted.ghost_pixels, 32, "A and B squares are ghosts");
    // Off: the object leaks into A (doubled), so the test above is meaningful.
    let plain = merge(&brackets, &MergeOptions::default()).unwrap();
    assert_eq!(plain.ghost_pixels, 0);
    assert!((plain.frame.at(a + 1, 7)[0] - 0.1).abs() > 0.01, "without deghost the moving object doubles");
    // Every level detects a two-stop difference; levels above 3 are refused (refusal test).
    for level in 1..=3 {
        let m = merge(
            &brackets,
            &MergeOptions {
                deghost: level,
                ..MergeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(m.frame, deghosted.frame);
    }
}

#[test]
fn tone_map_keeps_order_and_range() {
    // Grey ramp 0 .. 1000 (log after the first sample) plus a fixed-hue colour ramp.
    let n: u32 = 64;
    let mut data = Vec::new();
    for i in 0..n {
        let l = if i == 0 {
            0.0
        } else {
            (1e-3f64 * 1e6f64.powf(f64::from(i) / f64::from(n - 1))) as f32
        };
        data.push([l, l, l, 1.0]);
    }
    for i in 0..n {
        let l = (1e-3f64 * 1e5f64.powf(f64::from(i) / f64::from(n - 1))) as f32;
        data.push([4.0 * l, 2.0 * l, l, 0.5]);
    }
    let frame = RgbaPlane {
        bounds: rect(n, 2),
        data,
    };
    let len = n as usize;
    let cancel = CancellationToken::default();
    for depth in [8u8, 16] {
        let top: u16 = if depth == 8 { 255 } else { 65535 };
        let options = ToneMapOptions {
            target_depth: depth,
            ..ToneMapOptions::default()
        };
        let toned = tone_map(&frame, 32, &options, &HdrLimits::DEFAULT, &cancel).unwrap();
        assert_eq!(toned.depth, depth);
        assert_eq!(toned.bounds, frame.bounds);
        let row = |r: usize| &toned.data[r * len..(r + 1) * len];
        assert_eq!(row(0)[0], [0, 0, 0, top]);
        for r in 0..2 {
            for pair in row(r).windows(2) {
                for c in 0..3 {
                    assert!(pair[0][c] <= pair[1][c], "order kept at depth {depth}: {:?} -> {:?}", pair[0], pair[1]);
                }
            }
            for px in row(r) {
                assert!(px.iter().all(|&v| v <= top));
            }
        }
        // Hue order kept (r >= g >= b); alpha 0.5 rounds half away from zero.
        for px in row(1) {
            assert!(px[0] >= px[1] && px[1] >= px[2], "{px:?}");
            assert_eq!(u32::from(px[3]), (u32::from(top) + 1) / 2);
        }
        if depth == 16 {
            let grey = row(0);
            let level_of = |v: f32| {
                let i = frame.data[..len].iter().position(|p| p[0] >= v).unwrap();
                grey[i][0]
            };
            assert!(level_of(1.0) < level_of(4.0) && level_of(4.0) < level_of(30.0) && level_of(30.0) < level_of(500.0));
            assert!(grey[len - 1][0] < 65535, "1000 compresses below white without Lwhite");
        }
    }
    // Lwhite maps exactly to display white; -1 EV of 2x equals 0 EV of x.
    let white = ToneMapOptions {
        white: Some(4.0),
        target_depth: 8,
        ..ToneMapOptions::default()
    };
    let four = RgbaPlane {
        bounds: rect(1, 1),
        data: vec![[4.0, 4.0, 4.0, 1.0]],
    };
    assert_eq!(
        tone_map(&four, 32, &white, &HdrLimits::DEFAULT, &cancel).unwrap().data,
        vec![[255, 255, 255, 255]]
    );
    let two = RgbaPlane {
        bounds: rect(1, 1),
        data: vec![[2.0, 1.0, 0.5, 1.0]],
    };
    let minus_one = ToneMapOptions {
        exposure_ev: -1.0,
        ..ToneMapOptions::default()
    };
    let one = RgbaPlane {
        bounds: rect(1, 1),
        data: vec![[1.0, 0.5, 0.25, 1.0]],
    };
    assert_eq!(
        tone_map(&two, 32, &minus_one, &HdrLimits::DEFAULT, &cancel).unwrap(),
        tone_map(&one, 32, &ToneMapOptions::default(), &HdrLimits::DEFAULT, &cancel).unwrap()
    );
    // Hostile options are refused, never clamped.
    for bad in [
        ToneMapOptions {
            exposure_ev: f64::NAN,
            ..ToneMapOptions::default()
        },
        ToneMapOptions {
            exposure_ev: 31.0,
            ..ToneMapOptions::default()
        },
        ToneMapOptions {
            white: Some(0.0),
            ..ToneMapOptions::default()
        },
        ToneMapOptions {
            encoding: DisplayEncoding::Pq,
            ..ToneMapOptions::default()
        },
    ] {
        assert_eq!(tone_map(&one, 32, &bad, &HdrLimits::DEFAULT, &cancel).unwrap_err(), HdrError::Parameter);
    }
    let mut inf = one.clone();
    inf.data[0][0] = f32::INFINITY;
    assert_eq!(
        tone_map(&inf, 32, &ToneMapOptions::default(), &HdrLimits::DEFAULT, &cancel).unwrap_err(),
        HdrError::NonFinite { index: 0 }
    );
    // The merge result feeds the tone-mapper directly (32-bit in, 16-bit out).
    let dark = render(32, 8, 0.25, ramp_scene);
    let mid = render(32, 8, 1.0, ramp_scene);
    let merged = merge(&[ev(&dark, -2.0), ev(&mid, 0.0)], &MergeOptions::default()).unwrap();
    let toned = tone_map(&merged.frame, merged.depth, &ToneMapOptions::default(), &HdrLimits::DEFAULT, &cancel).unwrap();
    assert_eq!(toned.data.len(), 256);
}

#[test]
fn unsupported_depth_is_refused_typed() {
    let cancel = CancellationToken::default();
    let frame = RgbaPlane {
        bounds: rect(2, 1),
        data: vec![[2.0, 1.0, 0.5, 1.0], [0.1, 0.1, 0.1, 1.0]],
    };
    for depth in [1u8, 8, 16, 12] {
        let err = tone_map(&frame, depth, &ToneMapOptions::default(), &HdrLimits::DEFAULT, &cancel).unwrap_err();
        assert_eq!(
            err,
            HdrError::UnsupportedDepth {
                command: "hdr_tone_map",
                role: DepthRole::Source,
                depth,
                supported: &[32],
            }
        );
        assert_eq!(err.code(), "unsupported_depth");
        let text = err.to_string();
        assert!(text.contains(&format!("depth {depth}")) && text.contains("[32]"), "{text}");
    }
    for target in [32u8, 1, 0] {
        let options = ToneMapOptions {
            target_depth: target,
            ..ToneMapOptions::default()
        };
        assert_eq!(
            tone_map(&frame, 32, &options, &HdrLimits::DEFAULT, &cancel).unwrap_err(),
            HdrError::UnsupportedDepth {
                command: "hdr_tone_map",
                role: DepthRole::Target,
                depth: target,
                supported: &[8, 16],
            },
            "no silent promotion or demotion to a supported depth"
        );
    }
    let a = render(4, 2, 1.0, |_, _| [0.5, 0.5, 0.5]);
    let odd = Bracket {
        frame: &a,
        exposure: Some(Exposure::Ev(-1.0)),
        depth: 12,
    };
    assert_eq!(
        merge(&[ev(&a, 0.0), odd], &MergeOptions::default()).unwrap_err(),
        HdrError::UnsupportedDepth {
            command: "hdr_merge",
            role: DepthRole::Source,
            depth: 12,
            supported: &[8, 16, 32],
        }
    );
    // Inspectable capability metadata: the table and the descriptor agree.
    assert_eq!(HDR_CAPABILITIES, [HDR_MERGE_CAPABILITY, HDR_TONE_MAP_CAPABILITY]);
    let descriptor: serde_json::Value = serde_json::from_str(HDR_MERGE_DESCRIPTOR).unwrap();
    let commands = descriptor["commands"].as_array().unwrap();
    assert_eq!(commands.len(), HDR_CAPABILITIES.len());
    for (cap, entry) in HDR_CAPABILITIES.iter().zip(commands) {
        assert_eq!(entry["command"], cap.command);
        let depths = |key: &str| -> Vec<u8> {
            entry[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
                .collect()
        };
        assert_eq!(depths("source_depths"), cap.source_depths);
        assert_eq!(depths("target_depths"), cap.target_depths);
    }
}
