//! Lane SMB-AE-ROTO-TIMEREMAP slice 3: roto refine edge (trimap-band matting, decontaminate
//! colours, reduce chatter) on synthetic images with a known matte `I = a F + (1 - a) B`. Bounds
//! are measured here and printed; After Effects Refine Edge parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::frame_blend::frame_rect;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::select_tools::refine::{MatteMethod, RefineOptions};
use hsk_studio_pigment::select_tools::roto::RotoLimits;
use hsk_studio_pigment::select_tools::roto_refine::*;
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::selection::CoveragePlane;

const F: [f64; 3] = [0.9, 0.1, 0.1];
const B: [f64; 3] = [0.1, 0.2, 0.9];

/// True alpha: a linear ramp from x = 17 (0) to x = 23 (1).
fn alpha(x: u32) -> f64 {
    ((f64::from(x) - 17.0) / 6.0).clamp(0.0, 1.0)
}

fn ramp_frame(w: u32, h: u32) -> RgbaPlane {
    let mut data = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            let a = alpha(x);
            let c = [0, 1, 2].map(|k| (a * F[k] + (1.0 - a) * B[k]) as f32);
            data.push([c[0], c[1], c[2], 1.0]);
        }
    }
    RgbaPlane::new(frame_rect(w, h), data).unwrap()
}

fn plane(w: u32, h: u32, inside: impl Fn(u32, u32) -> bool) -> CoveragePlane {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.push(if inside(x, y) { u16::MAX } else { 0 });
        }
    }
    CoveragePlane { bounds: frame_rect(w, h), data }
}

#[test]
fn trimap_band_matting_recovers_the_soft_edge_and_decontaminates() {
    let (w, h) = (40u32, 20u32);
    let frame = ramp_frame(w, h);
    let binary = plane(w, h, |x, _| alpha(x) >= 0.5);
    let opts = RefineOptions {
        method: MatteMethod::ClosedForm,
        decontaminate_colours: true,
        ..RefineOptions::default()
    };
    let out = refine_roto_frame(&frame, &binary, &RotoBand::Radius(5.0), &opts, &ToolLimits::default()).unwrap();
    assert!(out.band_pixels > 0);
    // Mean absolute alpha error over the ramp columns: refined vs the binary matte.
    let (mut e_ref, mut e_bin, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for y in 2..h - 2 {
        for x in 15..26 {
            let truth = alpha(x);
            e_ref += (f64::from(out.coverage.at(i64::from(x), i64::from(y))) / 65535.0 - truth).abs();
            e_bin += (f64::from(binary.at(i64::from(x), i64::from(y))) / 65535.0 - truth).abs();
            n += 1.0;
        }
    }
    let (e_ref, e_bin) = (e_ref / n, e_bin / n);
    println!("mean |alpha error|: refined {e_ref:.4}, binary {e_bin:.4}");
    assert!(e_ref < 0.5 * e_bin, "refined {e_ref} vs binary {e_bin}");
    // Decontamination: the half-transparent mixture at x = 20 moves toward the foreground colour.
    let colours = out.colours.as_ref().expect("decontaminated colours");
    let cb = out.coverage.bounds;
    let i = ((10 - cb.y) as usize) * cb.width as usize + (20 - cb.x) as usize;
    let mixed = frame.data[(10 * w + 20) as usize];
    let dist = |c: [f64; 3]| (0..3).map(|k| (c[k] - F[k]).powi(2)).sum::<f64>().sqrt();
    let before = dist([0, 1, 2].map(|k| f64::from(mixed[k])));
    let after = dist([0, 1, 2].map(|k| f64::from(colours[i][k])));
    println!("distance to F at x=20: before {before:.4}, after {after:.4}");
    assert!(after < before, "after {after} before {before}");
    // A painted trimap band is the only unknown region: band = the painted pixels.
    let painted = plane(w, h, |x, _| (18..23).contains(&x));
    let t = refine_roto_frame(&frame, &binary, &RotoBand::Trimap(&painted), &opts, &ToolLimits::default()).unwrap();
    assert_eq!(t.band_pixels, 5 * u64::from(h));
    // Hostile: non-finite band, NaN sample.
    let e = refine_roto_frame(&frame, &binary, &RotoBand::Radius(f64::NAN), &opts, &ToolLimits::default()).unwrap_err();
    assert_eq!(e.code(), "nonfinite");
    let mut bad = frame.clone();
    bad.data[3][2] = f32::INFINITY;
    let e = refine_roto_frame(&bad, &binary, &RotoBand::Radius(3.0), &opts, &ToolLimits::default()).unwrap_err();
    assert_eq!(e.code(), "invalid_input");
}

#[test]
fn reduce_chatter_damps_a_flickering_matte_and_refuses_hostile_options() {
    let (w, h) = (40u32, 20u32);
    let frame = ramp_frame(w, h);
    let frames = vec![frame.clone(); 4];
    // Flicker: the matte boundary jumps between x = 18 and x = 22 every frame.
    let mattes: Vec<CoveragePlane> = (0..4).map(|k| plane(w, h, |x, _| x >= if k % 2 == 0 { 18 } else { 22 })).collect();
    let limits = RotoLimits::default();
    let cancel = CancellationToken::default();
    let base = RotoRefineOptions { band_px: 0.0, ..RotoRefineOptions::default() };
    let jitter = |out: &[RotoRefined]| -> f64 {
        out.windows(2)
            .map(|p| p[0].coverage.data.iter().zip(&p[1].coverage.data).map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs() / 65535.0).sum::<f64>())
            .sum()
    };
    let raw = refine_roto_sequence(&frames, &mattes, &base, &limits, &cancel).unwrap();
    let calm = refine_roto_sequence(&frames, &mattes, &RotoRefineOptions { reduce_chatter: 100.0, ..base }, &limits, &cancel).unwrap();
    let (j_raw, j_calm) = (jitter(&raw), jitter(&calm));
    println!("frame-to-frame matte change: raw {j_raw:.2}, reduce chatter 100 % {j_calm:.2}");
    assert!(j_calm < 0.5 * j_raw, "calm {j_calm} raw {j_raw}");
    assert_eq!(calm.len(), 4);
    // Hostile options and shapes (never clamped).
    let code = |o: RotoRefineOptions, m: &[CoveragePlane]| refine_roto_sequence(&frames, m, &o, &limits, &cancel).unwrap_err().code();
    assert_eq!(code(RotoRefineOptions { reduce_chatter: f64::NAN, ..base }, &mattes), "nonfinite");
    assert_eq!(code(RotoRefineOptions { reduce_chatter: 150.0, ..base }, &mattes), "tool_out_of_range");
    assert_eq!(code(RotoRefineOptions { band_px: -2.0, ..base }, &mattes), "tool_out_of_range");
    assert_eq!(code(base, &mattes[..3]), "invalid_input");
    let tight = RotoLimits { max_output_bytes: 100, ..RotoLimits::default() };
    let e = refine_roto_sequence(&frames, &mattes, &base, &tight, &cancel).unwrap_err();
    assert_eq!(e.code(), "budget_exceeded");
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(refine_roto_sequence(&frames, &mattes, &base, &limits, &stop).unwrap_err().code(), "canceled");
}
