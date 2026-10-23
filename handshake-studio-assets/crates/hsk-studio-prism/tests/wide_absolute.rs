//! SMB-PRISM-ENG-2: absolute colorimetric media-white scaling and the clip report. Expected values
//! are computed from the ICC definitions (XYZ_dst = XYZ_src * MW_src / MW_dst; linear sRGB from the
//! Lindbloom D50 inverse matrix), never from the engine.
mod wide_support;
use hsk_studio_observe::Outcome;
use hsk_studio_prism::*;
use wide_support::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}
/// K-ramp CMYK printer (black L* 20) with an explicit media white, or none.
fn paper_cmyk(white: Option<[f64; 3]>) -> Vec<u8> {
    let a2b = cmyk_a2b(2, |_, _, _, k| (100.0 - 80.0 * k, 0.0, 0.0));
    let b2a = cmyk_b2a(6, |l, _, _| [0.0, 0.0, 0.0, (100.0 - l) / 80.0]);
    match white {
        Some(w) => cmyk_profile(a2b, Some(b2a), 3, w),
        None => assemble(
            0x0210_0000,
            b"prtr",
            b"CMYK",
            b"Lab ",
            &[a2b, b2a],
            &[
                (b"A2B0", 0),
                (b"A2B1", 0),
                (b"A2B2", 0),
                (b"B2A0", 1),
                (b"B2A1", 1),
                (b"B2A2", 1),
            ],
        ),
    }
}

#[test]
fn absolute_colorimetric_scales_by_media_white_and_ignores_bpc() {
    bounded_case("absolute_colorimetric_scales_by_media_white_and_ignores_bpc", || {
        let ctx = Ctx::new();
        // Each (profiles, intent, flag) key holds one plan; this case uses six.
        let mut eng = WideEngine::new(ctx.actor.clone(), 8).unwrap();
        let token = hsk_studio_accord::CancellationToken::default();
        let paper = [0.90, 0.93, 0.70];
        let cmyk = paper_cmyk(Some(paper));
        let dst_mw = eng
            .inspect_profile(ctx.input(&ctx.destination, SRGB), &token)
            .unwrap()
            .media_white
            .unwrap();
        let scale: Vec<f64> = (0..3).map(|i| paper[i] / dst_mw[i]).collect();
        let pixels = [0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let abs = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::AbsoluteColorimetric, true).unwrap();
        // Paper white (K 0) keeps its absolute colour; K 1 (L* 20) scales the same way.
        for (px, l) in [100.0, 20.0].into_iter().enumerate() {
            let mut xyz = lab_to_xyz(l, 0.0, 0.0);
            for (v, s) in xyz.iter_mut().zip(&scale) {
                *v *= s;
            }
            let want = xyz_to_srgb(xyz);
            for (c, w) in want.iter().enumerate() {
                let got = f64::from(abs.pixels[px * 3 + c]);
                assert!((got - w).abs() < 2e-3, "px {px} ch {c}: {got} vs {w}");
            }
        }
        let r = &abs.receipt;
        assert!(r.absolute.applied);
        // The tag stores s15Fixed16, so the media white is the paper value to within 2^-16.
        for (got, want) in r.absolute.source_media_white.unwrap().iter().zip(&paper) {
            assert!((got - want).abs() < 2e-5);
        }
        for (got, want) in r.absolute.scale.unwrap().iter().zip(&scale) {
            assert!((got - want).abs() < 1e-4);
        }
        assert!(r.bpc.requested && !r.bpc.applied && r.bpc.rule == "absolute_ignores_bpc");
        assert_eq!(abs.delivery.outcome, Outcome::Success);
        // The flag changes nothing for absolute colorimetric, and relative colorimetric differs.
        let off = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::AbsoluteColorimetric, false).unwrap();
        assert_eq!(off.pixels, abs.pixels);
        let rel = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, false).unwrap();
        assert!(rel.pixels[..3].iter().all(|v| (*v - 1.0).abs() < 2e-3));
        assert!(
            abs.pixels[2] < 0.95 && abs.pixels[2] < abs.pixels[0] - 0.05,
            "paper white must stay yellowish: {:?}",
            &abs.pixels[..3]
        );
        // A Lab source counts as D50: only the destination's media white scales it.
        let lab = run_lab(&mut eng, &ctx, &cmyk, &[100.0, 0.0, 0.0], Intent::AbsoluteColorimetric, false).unwrap();
        assert_eq!(lab.receipt.absolute.source_media_white, Some([0.9642, 1.0, 0.8249]));
        // No media white tag: absolute is refused (typed), relative still works.
        let bare = paper_cmyk(None);
        let refused = run(&mut eng, &ctx, &bare, SRGB, &pixels, Intent::AbsoluteColorimetric, false).unwrap_err();
        assert_eq!(refused.error, WideError::MissingMediaWhite);
        assert!(run(&mut eng, &ctx, &bare, SRGB, &pixels, Intent::RelativeColorimetric, false).is_ok());
    });
}

#[test]
fn clip_report_counts_linear_out_of_gamut_values_for_matrix_destinations() {
    bounded_case("clip_report_counts_linear_out_of_gamut_values_for_matrix_destinations", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        let lab = [
            50.0f32, 0.0, 0.0, 60.0, 20.0, -25.0, 50.0, 80.0, 60.0, 50.0, -80.0, 60.0, 100.0, 0.0, 0.0,
        ];
        let out = run_lab(&mut eng, &ctx, SRGB, &lab, Intent::RelativeColorimetric, false).unwrap();
        // Independent count: linear sRGB outside [-1e-3, 1 + 1e-3] per channel.
        let (mut pixels, mut channels, mut worst) = (0u64, [0u64; 3], 0.0f64);
        for p in lab.chunks_exact(3) {
            let lin = xyz_to_linear_srgb(lab_to_xyz(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])));
            let mut any = false;
            for (c, v) in lin.iter().enumerate() {
                let over = if *v < -1e-3 {
                    -v
                } else if *v > 1.001 {
                    v - 1.0
                } else {
                    continue;
                };
                any = true;
                channels[c] += 1;
                worst = worst.max(over);
            }
            pixels += u64::from(any);
        }
        assert!((2..5).contains(&pixels), "fixture must mix in-gamut and out-of-gamut pixels");
        let clip = out.receipt.clip;
        assert_eq!(clip.method, ClipMethod::LinearDevice);
        assert_eq!(
            (clip.pixels_clipped, clip.channels_clipped[..3].to_vec()),
            (pixels, channels.to_vec())
        );
        assert_eq!(clip.channels_clipped[3], 0);
        assert!((clip.max_overshoot - worst).abs() < 2e-3, "{} vs {worst}", clip.max_overshoot);
        // The values themselves stay unclamped (extended range), so callers can gamut-map.
        assert!(
            out.pixels[9] < -1e-3 || out.pixels[10] < -1e-3 || out.pixels[11] < -1e-3,
            "{:?}",
            &out.pixels[9..12]
        );
        // A table-based destination cannot reveal clipping from its clamped output: stated, not guessed.
        let cmyk = paper_cmyk(Some(D50));
        let table = run_lab(&mut eng, &ctx, &cmyk, &lab, Intent::RelativeColorimetric, false).unwrap();
        assert_eq!(table.receipt.clip.method, ClipMethod::Unavailable);
        assert_eq!(table.receipt.clip.pixels_clipped, 0);
    });
}
