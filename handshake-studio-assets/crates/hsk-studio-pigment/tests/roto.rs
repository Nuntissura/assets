//! Lane SMB-AE-ROTO-TIMEREMAP slice 2: assisted roto (base-frame strokes -> graph-cut matte ->
//! flow-guided propagation) on a synthetic sequence with a known matte. Bounds are measured here
//! and printed; After Effects Roto Brush parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::frame_blend::{BlendLimits, frame_rect};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::select_tools::roto::*;
use hsk_studio_pigment::selection::CoveragePlane;

const W: u32 = 48;
const H: u32 = 36;
const R: f64 = 7.0;

fn centre(k: usize) -> (f64, f64) {
    (14.0 + 2.0 * k as f64, 18.0)
}

/// Red shaded disc on a textured green-blue gradient; the disc moves 2 px per frame.
fn frame(k: usize) -> RgbaPlane {
    let (cx, cy) = centre(k);
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let (xf, yf) = (f64::from(x), f64::from(y));
            let d = ((xf - cx).powi(2) + (yf - cy).powi(2)).sqrt();
            let px = if d <= R {
                [0.9 - 0.04 * d, 0.12 + 0.01 * d, 0.1]
            } else {
                let t = if (x / 3 + y / 3) % 2 == 0 { 0.04 } else { 0.0 };
                [0.1 + 0.2 * xf / f64::from(W) + t, 0.55 + 0.2 * yf / f64::from(H), 0.45 + t]
            };
            data.push([px[0] as f32, px[1] as f32, px[2] as f32, 1.0]);
        }
    }
    RgbaPlane::new(frame_rect(W, H), data).unwrap()
}

fn disc_plane(cx: f64, cy: f64, r: f64) -> CoveragePlane {
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let inside = (f64::from(x) - cx).powi(2) + (f64::from(y) - cy).powi(2) <= r * r;
            data.push(if inside { u16::MAX } else { 0 });
        }
    }
    CoveragePlane { bounds: frame_rect(W, H), data }
}

fn iou(a: &CoveragePlane, b: &CoveragePlane) -> f64 {
    let (mut inter, mut union) = (0u32, 0u32);
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let (p, q) = (a.at(x, y) >= 32768, b.at(x, y) >= 32768);
            inter += u32::from(p && q);
            union += u32::from(p || q);
        }
    }
    f64::from(inter) / f64::from(union.max(1))
}

#[test]
fn roto_base_and_propagation_track_a_moving_disc() {
    let frames: Vec<RgbaPlane> = (0..5).map(frame).collect();
    let opts = RotoOptions::default();
    let limits = RotoLimits::default();
    let (cx, cy) = centre(2);
    let stroke = disc_plane(cx, cy, 3.0);
    let base = roto_base(&frames[2], &RotoStrokes { foreground: &stroke, background: None }, &opts, &limits).unwrap();
    let base_iou = iou(&base, &disc_plane(cx, cy, R));
    println!("base IoU {base_iou:.4}");
    assert!(base_iou >= 0.9, "base IoU {base_iou}");
    let out = roto_propagate(&frames, 2, &base, &opts, &limits, &CancellationToken::default()).unwrap();
    assert_eq!(out.len(), 5);
    for f in &out {
        let (x, y) = centre(f.index);
        let v = iou(&f.matte, &disc_plane(x, y, R));
        println!("frame {} status {:?} band {} IoU {v:.4}", f.index, f.status, f.band_pixels);
        assert!(v >= 0.9, "frame {} IoU {v}", f.index);
        assert_eq!(f.status, if f.index == 2 { RotoStatus::Base } else { RotoStatus::Propagated });
    }
    // Deterministic: a second run gives the same mattes.
    assert_eq!(out, roto_propagate(&frames, 2, &base, &opts, &limits, &CancellationToken::default()).unwrap());
    // A background stroke over the whole disc wins over the foreground stroke: nothing is left.
    let all = disc_plane(cx, cy, 20.0);
    let refused = roto_base(&frames[2], &RotoStrokes { foreground: &stroke, background: Some(&all) }, &opts, &limits);
    assert_eq!(refused.unwrap_err().code(), "invalid_input");
}

#[test]
fn roto_hostile_inputs_and_model_unavailable() {
    let frames: Vec<RgbaPlane> = (0..3).map(frame).collect();
    let limits = RotoLimits::default();
    let cancel = CancellationToken::default();
    let (cx, cy) = centre(0);
    let base = disc_plane(cx, cy, R);
    let code = |r: Result<Vec<RotoFrame>, hsk_studio_pigment::select_tools::ToolError>| r.unwrap_err().code();
    // No learned model is installed; nothing is downloaded.
    let learned = RotoOptions { engine: RotoEngine::Learned, ..RotoOptions::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &learned, &limits, &cancel)), "model_unavailable");
    let stroke = disc_plane(cx, cy, 3.0);
    let strokes = RotoStrokes { foreground: &stroke, background: None };
    assert_eq!(roto_base(&frames[0], &strokes, &learned, &limits).unwrap_err().code(), "model_unavailable");
    let opts = RotoOptions::default();
    // Shapes, indices, empty mattes and strokes.
    let small = RgbaPlane::new(frame_rect(4, 4), vec![[0.0, 0.0, 0.0, 1.0]; 16]).unwrap();
    assert_eq!(code(roto_propagate(&[frames[0].clone(), small], 0, &base, &opts, &limits, &cancel)), "invalid_input");
    assert_eq!(code(roto_propagate(&frames, 3, &base, &opts, &limits, &cancel)), "invalid_input");
    assert_eq!(code(roto_propagate(&[], 0, &base, &opts, &limits, &cancel)), "invalid_input");
    let empty = CoveragePlane { bounds: frame_rect(W, H), data: vec![0; (W * H) as usize] };
    assert_eq!(code(roto_propagate(&frames, 0, &empty, &opts, &limits, &cancel)), "invalid_input");
    let none = RotoStrokes { foreground: &empty, background: None };
    assert_eq!(roto_base(&frames[0], &none, &opts, &limits).unwrap_err().code(), "invalid_input");
    // NaN sample and out-of-domain options (never clamped).
    let mut nan = frames[1].clone();
    nan.data[5][0] = f32::NAN;
    assert_eq!(code(roto_propagate(&[frames[0].clone(), nan], 0, &base, &opts, &limits, &cancel)), "invalid_input");
    let bad = RotoOptions { band_px: f64::NAN, ..RotoOptions::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &bad, &limits, &cancel)), "nonfinite");
    let bad = RotoOptions { band_px: -1.0, ..RotoOptions::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &bad, &limits, &cancel)), "tool_out_of_range");
    // Budgets refused before any work: frame count, aggregate output bytes, aggregate flow work.
    let few = RotoLimits { max_frames: 2, ..RotoLimits::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &opts, &few, &cancel)), "budget_exceeded");
    let bytes = RotoLimits { max_output_bytes: u64::from(W * H) * 2 * 2, ..RotoLimits::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &opts, &bytes, &cancel)), "budget_exceeded");
    let work = RotoLimits { blend: BlendLimits { max_bytes: 1 << 30, max_work: 1_000_000 }, ..RotoLimits::default() };
    assert_eq!(code(roto_propagate(&frames, 0, &base, &opts, &work, &cancel)), "budget_exceeded");
    // A frame header claiming u32::MAX x u32::MAX pixels is refused before allocation.
    let huge = RgbaPlane { bounds: frame_rect(u32::MAX, u32::MAX), data: Vec::new() };
    let c = code(roto_propagate(&[huge.clone(), huge], 0, &base, &opts, &limits, &cancel));
    assert!(matches!(c, "budget_exceeded" | "overflow"), "{c}");
    // Cancellation is typed.
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(code(roto_propagate(&frames, 0, &base, &opts, &limits, &stop)), "canceled");
}
