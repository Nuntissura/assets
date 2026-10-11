//! SMB-PRISM-ENG-1: black point compensation per ISO 18619, owned by Prism. Expected values are
//! computed here from the published formulas (CIE Y(L*), scale = (1-Yd)/(1-Ys), white fixed), never
//! from the engine.
mod wide_support;
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
fn close(a: f32, b: f64, tol: f64, what: &str) {
    assert!((f64::from(a) - b).abs() < tol, "{what}: {a} vs {b}");
}

#[test]
fn matrix_destination_black_maps_exactly_once() {
    bounded_case("matrix_destination_black_maps_exactly_once", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        let cmyk = cmyk_k_profile(5, 3, 20.0);
        let pixels = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 1.0];
        // Source black L* 20 (B2A0 of Lab 0 is K = 1, A2B gives 20); sRGB destination black is 0.
        let on = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        let scale = 1.0 / (1.0 - y_of_l(20.0));
        let b = &on.receipt.bpc;
        assert!(b.requested && b.applied && b.rule == "iso18619" && !b.engine_builtin_suppressed);
        assert!((b.source_black_lstar.unwrap() - 20.0).abs() < 0.05, "{b:?}");
        assert!(b.destination_black_lstar.unwrap().abs() < 1e-3);
        assert!((b.scale.unwrap() - scale).abs() < 2e-3, "{b:?}");
        // White is fixed, black lands on black, K = 0.5 (L* 60) shifts by the affine map.
        for (px, k) in [0.0, 0.5, 1.0].into_iter().enumerate() {
            let y = scale * y_of_l(100.0 - 80.0 * k) + (1.0 - scale);
            for c in 0..3 {
                close(on.pixels[px * 3 + c], srgb_encode(y), 3e-3, "bpc on");
            }
        }
        // Flag off: the same pixels are the plain colorimetric gray (K = 1 -> 0.189376).
        let off = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, false).unwrap();
        assert!(!off.receipt.bpc.applied && off.receipt.bpc.rule == "not_requested");
        close(off.pixels[6], srgb_encode(y_of_l(20.0)), 3e-3, "bpc off black");
        close(on.pixels[6], 0.0, 3e-3, "bpc on black");
    });
}

#[test]
fn table_destination_black_point_from_the_iso_ramp() {
    bounded_case("table_destination_black_point_from_the_iso_ramp", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        // CMYK destination whose darkest reproducible neutral is L* 20 (B2A clips K at 1). Its
        // round-trip ramp is straight above the black, so the detected black stays L* 20.
        let dest = cmyk_k_profile(2, 3, 20.0);
        let pixels = [0.5f32, 0.5, 0.5];
        let on = run(&mut eng, &ctx, SRGB, &dest, &pixels, Intent::RelativeColorimetric, true).unwrap();
        let b = &on.receipt.bpc;
        assert!(b.applied && b.rule == "iso18619", "{b:?}");
        assert!(b.source_black_lstar.unwrap().abs() < 1e-3);
        assert!((b.destination_black_lstar.unwrap() - 20.0).abs() < 0.1, "{b:?}");
        let scale = 1.0 - y_of_l(20.0);
        assert!((b.scale.unwrap() - scale).abs() < 2e-3, "{b:?}");
        // sRGB 0.5 gray -> Y -> compensated Y' -> L' -> K = (100 - L') / 80, C = M = Y = 0.
        let y = srgb_decode(0.5);
        let k_on = (100.0 - l_of_y(scale * y + (1.0 - scale))) / 80.0;
        let k_off = (100.0 - l_of_y(y)) / 80.0;
        let off = run(&mut eng, &ctx, SRGB, &dest, &pixels, Intent::RelativeColorimetric, false).unwrap();
        for c in 0..3 {
            close(on.pixels[c], 0.0, 5e-3, "cmy");
        }
        close(on.pixels[3], k_on, 5e-3, "k with bpc");
        close(off.pixels[3], k_off, 5e-3, "k without bpc");
        assert!(k_on < k_off - 0.01, "compensation must lighten the K plate");
    });
}

#[test]
fn engine_builtin_black_scaling_is_suppressed_never_doubled() {
    bounded_case("engine_builtin_black_scaling_is_suppressed_never_doubled", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        // RGB v4 table profile: gray (t, t, t) -> t * D50, so sRGB output must be enc(t) exactly
        // when nothing but the table acts. moxcms's own constant-black scaling would shift shadows.
        let mab = mab_rgb_profile(2, |r, g, b| {
            let t = (r + g + b) / 3.0;
            [D50[0] * t, D50[1] * t, D50[2] * t]
        });
        let pixels = [0.05f32, 0.05, 0.05, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0];
        let off = run(&mut eng, &ctx, &mab, SRGB, &pixels, Intent::Perceptual, false).unwrap();
        assert!(off.receipt.bpc.engine_builtin_suppressed && !off.receipt.bpc.applied);
        for (i, t) in [0.05, 0.5, 1.0].into_iter().enumerate() {
            for c in 0..3 {
                close(off.pixels[i * 3 + c], srgb_encode(t), 5e-4, "perceptual, flag off");
            }
        }
        // Flag on: both black points are L* 0, so one compensation is the identity and the output
        // is bit-identical to the flag-off run (no second application anywhere).
        let on = run(&mut eng, &ctx, &mab, SRGB, &pixels, Intent::Perceptual, true).unwrap();
        assert_eq!(on.receipt.bpc.rule, "iso18619_equal_black_points");
        assert!(!on.receipt.bpc.applied && on.receipt.bpc.engine_builtin_suppressed);
        assert_eq!(on.pixels, off.pixels);
        // Relative colorimetric never involves the engine's scaling.
        let rel = run(&mut eng, &ctx, &mab, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        assert!(!rel.receipt.bpc.engine_builtin_suppressed);
    });
}
