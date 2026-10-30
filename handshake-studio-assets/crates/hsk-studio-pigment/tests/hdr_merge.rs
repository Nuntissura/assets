//! Lane SMB-HDR-MERGE-1: merge bracketed exposures to 32-bit HDR ([STU-RAS-033], [STU-RAS-149]).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::hdr_merge::{
    Bracket, DepthRole, Exposure, HDR_MERGE_CAPABILITY, HdrError, HdrLimits, MergeOptions, merge_brackets,
};
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
            reference: None,
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
