//! Lane SMB-AE-ROTO-TIMEREMAP slice 1: frame blending pixels (STU-MOT-007a `frame_mix`,
//! `pixel_motion`) against hand values and a synthetic translating sequence. Bounds are measured
//! here; After Effects parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::frame_blend::*;
use hsk_studio_pigment::retouch::RgbaPlane;

fn plane(w: u32, h: u32, f: impl Fn(f64, f64) -> [f32; 4]) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.push(f(f64::from(x), f64::from(y)));
        }
    }
    RgbaPlane::new(frame_rect(w, h), data).unwrap()
}

/// A grey Gaussian blob (sigma 2.5) centred at `(cx, cy)` on black, opaque.
fn blob(cx: f64, cy: f64) -> RgbaPlane {
    plane(40, 24, |x, y| {
        let d2 = (x - cx).powi(2) + (y - cy).powi(2);
        let v = (-d2 / (2.0 * 2.5 * 2.5)).exp() as f32;
        [v, v, v, 1.0]
    })
}

fn rms(a: &RgbaPlane, b: &RgbaPlane) -> f64 {
    let s: f64 = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(p, q)| (0..3).map(|c| (f64::from(p[c]) - f64::from(q[c])).powi(2)).sum::<f64>())
        .sum();
    (s / (a.data.len() * 3) as f64).sqrt()
}

#[test]
fn frame_mix_hand_values_and_hostile_inputs() {
    let lim = BlendLimits::DEFAULT;
    let a = plane(2, 1, |x, _| if x == 0.0 { [1.0, 0.0, 0.0, 1.0] } else { [0.2, 0.4, 0.6, 0.0] });
    let b = plane(2, 1, |x, _| if x == 0.0 { [0.0, 0.0, 1.0, 0.5] } else { [0.0, 0.0, 0.0, 0.0] });
    // Pixel 0: premultiplied 0.75 (1,0,0,1) + 0.25 (0,0,0.5,0.5) = (0.75, 0, 0.125, 0.875).
    let m = frame_mix(&a, &b, 0.25, &lim).unwrap();
    assert_eq!(m.data[0], [(0.75f64 / 0.875) as f32, 0.0, (0.125f64 / 0.875) as f32, 0.875]);
    // Fully transparent result reads transparent black, never NaN.
    assert_eq!(m.data[1], [0.0; 4]);
    assert_eq!(frame_mix(&a, &b, 0.0, &lim).unwrap().data[0], a.data[0]);
    assert_eq!(frame_mix(&a, &b, 1.0, &lim).unwrap().data[0], b.data[0]);
    // Deterministic bits.
    assert_eq!(frame_mix(&a, &b, 0.3, &lim).unwrap(), frame_mix(&a, &b, 0.3, &lim).unwrap());
    // Hostile: weights, shapes, samples, budgets (refused before allocation).
    for w in [f64::NAN, f64::INFINITY, -0.1, 1.5] {
        assert_eq!(frame_mix(&a, &b, w, &lim).unwrap_err(), BlendError::Parameter, "{w}");
    }
    let other = plane(1, 1, |_, _| [0.0; 4]);
    assert_eq!(frame_mix(&a, &other, 0.5, &lim).unwrap_err(), BlendError::Shape);
    let mut bad = b.clone();
    bad.data[0][1] = f32::NAN;
    assert_eq!(frame_mix(&a, &bad, 0.5, &lim).unwrap_err(), BlendError::OutOfDomain);
    let mut short = b.clone();
    short.data.pop();
    assert_eq!(frame_mix(&a, &short, 0.5, &lim).unwrap_err(), BlendError::Shape);
    let tiny = BlendLimits { max_bytes: 8, max_work: 1 };
    assert!(matches!(frame_mix(&a, &b, 0.5, &tiny), Err(BlendError::OverBudget { .. })));
    // A frame claiming u32::MAX x u32::MAX pixels is refused before any allocation.
    let huge = RgbaPlane { bounds: frame_rect(u32::MAX, u32::MAX), data: Vec::new() };
    let cancel = CancellationToken::default();
    assert!(optical_flow(&huge, &huge, &FlowParams::DEFAULT, &lim, &cancel).is_err());
    assert!(pixel_motion(&huge, &huge, 0.5, &FlowParams::DEFAULT, &lim, &cancel).is_err());
    let p = FlowParams { iterations: 0, ..FlowParams::DEFAULT };
    assert_eq!(optical_flow(&a, &b, &p, &lim, &cancel).unwrap_err(), BlendError::Parameter);
    let p = FlowParams { alpha: f64::NAN, ..FlowParams::DEFAULT };
    assert_eq!(pixel_motion(&a, &b, 0.5, &p, &lim, &cancel).unwrap_err(), BlendError::Parameter);
}

#[test]
fn pixel_motion_follows_a_translating_blob() {
    let lim = BlendLimits::DEFAULT;
    let cancel = CancellationToken::default();
    let (a, b) = (blob(14.0, 12.0), blob(18.0, 12.0));
    // Flow at the blob centre is the 4 px shift (measured bound 0.75 px).
    let flow = optical_flow(&a, &b, &FlowParams::DEFAULT, &lim, &cancel).unwrap();
    let [u, v] = flow.at(14, 12);
    println!("flow at centre: ({u:.4}, {v:.4})");
    assert!((u - 4.0).abs() < 0.75 && v.abs() < 0.5, "flow ({u}, {v})");
    // Halfway frame: pixel motion is close to the blob at x = 16; frame mix ghosts two half blobs.
    let truth = blob(16.0, 12.0);
    let pm = pixel_motion(&a, &b, 0.5, &FlowParams::DEFAULT, &lim, &cancel).unwrap();
    let fm = frame_mix(&a, &b, 0.5, &lim).unwrap();
    let (e_pm, e_fm) = (rms(&pm, &truth), rms(&fm, &truth));
    println!("rms vs truth: pixel_motion {e_pm:.5}, frame_mix {e_fm:.5}");
    assert!(e_pm < 0.5 * e_fm, "pixel motion {e_pm} vs frame mix {e_fm}");
    // End weights return the source frames bit for bit; runs are deterministic.
    assert_eq!(pixel_motion(&a, &b, 0.0, &FlowParams::DEFAULT, &lim, &cancel).unwrap(), a);
    assert_eq!(pixel_motion(&a, &b, 1.0, &FlowParams::DEFAULT, &lim, &cancel).unwrap(), b);
    assert_eq!(pm, pixel_motion(&a, &b, 0.5, &FlowParams::DEFAULT, &lim, &cancel).unwrap());
    // Work budget refused before any work; cancellation is typed.
    let tight = BlendLimits { max_bytes: 1 << 30, max_work: 1000 };
    assert!(matches!(pixel_motion(&a, &b, 0.5, &FlowParams::DEFAULT, &tight, &cancel), Err(BlendError::OverBudget { .. })));
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(pixel_motion(&a, &b, 0.5, &FlowParams::DEFAULT, &lim, &stop).unwrap_err(), BlendError::Canceled);
}
