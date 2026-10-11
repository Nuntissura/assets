//! SMB-PRISM-ENG-6: matrix/TRC and Gray pairs run as one fused affine 3x3 between the curves. The fused
//! plan must reproduce the staged plan (source stage -> PCS map -> destination stage) up to f64 rounding,
//! with the same clip report, for linear, parametric, `curv` gamma and Gray profiles, with BPC and with
//! absolute media-white scaling. `-- --nocapture` prints the per-pixel time of both plans (meaningful in
//! a `--release` run only).
mod wide_support;
use hsk_studio_prism::*;
use std::time::Instant;
use wide_support::*;

const SRGB_LINEAR: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");
const P3_LINEAR: &[u8] = include_bytes!("profiles/DisplayP3-linear-v4.icc");
const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");

/// Deterministic channel values in 0..1 (64-bit LCG) plus the exact corners and mixed-zero pixels.
fn pixels(channels: usize, count: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut out: Vec<f32> = (0..count * channels)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32) / 16_777_216.0
        })
        .collect();
    for (i, v) in [0.0f32, 1.0, 0.0, 0.5, 1.0, 0.0].iter().cycle().take(channels * 6).enumerate() {
        out[i] = *v;
    }
    out
}

fn gray_profile(gamma: f64, white: [f64; 3]) -> Vec<u8> {
    assemble(0x0210_0000, b"mntr", b"GRAY", b"XYZ ", &[xyz_tag(white), curv(Some(gamma))], &[(b"wtpt", 0), (b"kTRC", 1)])
}

#[test]
fn fused_matrix_pairs_match_the_staged_path() {
    let ctx = Ctx::new();
    let gamma18 = matrix_v2(1.8);
    let gray = gray_profile(2.2, [0.9505, 1.0, 1.089]);
    let cases: [(&str, &[u8], &[u8], usize, Intent, bool); 7] = [
        ("linear_srgb_to_linear_p3", SRGB_LINEAR, P3_LINEAR, 3, Intent::RelativeColorimetric, false),
        ("srgb_to_p3", SRGB, P3, 3, Intent::RelativeColorimetric, false),
        ("srgb_to_p3_bpc", SRGB, P3, 3, Intent::Perceptual, true),
        ("curv18_to_srgb_bpc", &gamma18, SRGB, 3, Intent::RelativeColorimetric, true),
        ("p3_to_curv18_absolute", P3, &gamma18, 3, Intent::AbsoluteColorimetric, false),
        ("gray_d65_to_srgb_absolute", &gray, SRGB, 1, Intent::AbsoluteColorimetric, false),
        ("srgb_to_gray", SRGB, &gray, 3, Intent::RelativeColorimetric, false),
    ];
    for (name, src, dst, channels, intent, bpc) in cases {
        let px = pixels(channels, 20_000);
        let mut fused = WideEngine::new(ctx.actor.clone(), 4).unwrap();
        let mut staged = WideEngine::new(ctx.actor.clone(), 4).unwrap();
        staged.set_matrix_fusion(false);
        assert!(fused.matrix_fusion() && !staged.matrix_fusion());
        // Warm both plans, then time a second call each.
        let _ = run(&mut fused, &ctx, src, dst, &px[..channels], intent, bpc).unwrap();
        let _ = run(&mut staged, &ctx, src, dst, &px[..channels], intent, bpc).unwrap();
        let t = Instant::now();
        let a = run(&mut fused, &ctx, src, dst, &px, intent, bpc).unwrap();
        let fused_ns = t.elapsed().as_nanos() as f64 / 20_000.0;
        let t = Instant::now();
        let b = run(&mut staged, &ctx, src, dst, &px, intent, bpc).unwrap();
        let staged_ns = t.elapsed().as_nanos() as f64 / 20_000.0;
        assert!(a.receipt.cache_hit && b.receipt.cache_hit);
        assert_eq!(a.pixels.len(), b.pixels.len());
        let (mut worst, mut identical) = (0.0f32, 0usize);
        for (x, y) in a.pixels.iter().zip(&b.pixels) {
            assert!(x.is_finite() && y.is_finite(), "{name}: non-finite output");
            worst = worst.max((x - y).abs());
            identical += usize::from(x.to_bits() == y.to_bits());
        }
        let c = (a.receipt.clip, b.receipt.clip);
        eprintln!(
            "WIDE_FUSION {{\"case\":\"{name}\",\"values\":{},\"bit_identical\":{identical},\"max_abs_diff\":{worst:.3e},\"fused_ns_per_px\":{fused_ns:.1},\"staged_ns_per_px\":{staged_ns:.1},\"pixels_clipped\":{},\"bpc_scale\":{:?}}}",
            a.pixels.len(),
            c.0.pixels_clipped,
            a.receipt.bpc.scale
        );
        assert!(worst <= 2e-6, "{name}: fused differs from staged by {worst}");
        assert!(identical * 100 >= a.pixels.len() * 99, "{name}: only {identical} of {} values bit-identical", a.pixels.len());
        assert_eq!((c.0.method, c.0.pixels_clipped, c.0.channels_clipped), (c.1.method, c.1.pixels_clipped, c.1.channels_clipped), "{name}: clip report");
        assert!((c.0.max_overshoot - c.1.max_overshoot).abs() < 1e-9, "{name}: overshoot");
        assert_eq!((a.receipt.bpc, a.receipt.absolute), (b.receipt.bpc, b.receipt.absolute), "{name}: receipts");
    }
}
