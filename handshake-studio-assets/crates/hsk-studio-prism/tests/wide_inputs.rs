//! SMB-PRISM-ENG-3 (input kinds, cache): gray TRC and Lab sources to a working RGB profile, the
//! pixel limit, and the parse-free cache hit. Expected values come from the published formulas
//! (gray gamma, CIELAB D50 -> XYZ -> Bruce Lindbloom's sRGB D50 inverse matrix -> sRGB encoding).
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
/// Lab (D50) -> encoded sRGB, f64 (Lindbloom D50 inverse).
fn lab_to_srgb(l: f64, a: f64, b: f64) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let xyz = [D50[0] * inv(fx), D50[1] * inv(fy), D50[2] * inv(fz)];
    let m = [
        [3.1338561, -1.6168667, -0.4906146],
        [-0.9787684, 1.9161415, 0.0334540],
        [0.0719453, -0.2289914, 1.4052427],
    ];
    m.map(|row| srgb_encode((row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]).clamp(0.0, 1.0)))
}
fn gray_profile(gamma: f64) -> Vec<u8> {
    assemble(
        0x0210_0000, b"mntr", b"GRAY", b"XYZ ",
        &[xyz_tag(D50), curv(Some(gamma))],
        &[(b"wtpt", 0), (b"kTRC", 1)],
    )
}

#[test]
fn gray_trc_and_lab_inputs_reach_the_working_rgb_space() {
    bounded_case("gray_trc_and_lab_inputs_reach_the_working_rgb_space", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        // Gray: one channel, gamma 2.2 (stored u8Fixed8 563/256) -> neutral D50 -> sRGB gray.
        let gray = gray_profile(2.2);
        assert_eq!(eng.inspect_profile(ctx.input(&ctx.source, &gray), &Default::default()).unwrap().kind, ProfileKind::GrayTrc);
        let g = [0.0f32, 0.25, 0.5, 1.0];
        let out = run(&mut eng, &ctx, &gray, SRGB, &g, Intent::RelativeColorimetric, false).unwrap();
        assert_eq!(out.receipt.source_space, SourceSpace::Device(DeviceSpace::Gray));
        for (i, v) in g.iter().enumerate() {
            let e = srgb_encode(f64::from(*v).powf(563.0 / 256.0));
            for c in 0..3 {
                assert!((f64::from(out.pixels[i * 3 + c]) - e).abs() < 1e-3, "gray {v} ch {c}: {} vs {e}", out.pixels[i * 3 + c]);
            }
        }
        // Lab: PCS-native input, no profile; neutral, chromatic and white points.
        let lab = [100.0f32, 0.0, 0.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 60.0, 20.0, -25.0, 40.0, -15.0, 18.0];
        let out = run_lab(&mut eng, &ctx, SRGB, &lab, Intent::RelativeColorimetric, false).unwrap();
        assert_eq!((out.receipt.source_space, out.receipt.source_profile_id.is_none()), (SourceSpace::Lab, true));
        for (i, p) in lab.chunks_exact(3).enumerate() {
            let e = lab_to_srgb(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
            for c in 0..3 {
                assert!((f64::from(out.pixels[i * 3 + c]) - e[c]).abs() < 1.5e-3, "lab {p:?} ch {c}: {} vs {}", out.pixels[i * 3 + c], e[c]);
            }
        }
        // Lab source black is L* 0 (ISO 18619): compensation toward a CMYK destination of black L* 20.
        let cmyk = cmyk_k_profile(2, 3, 20.0);
        let on = run_lab(&mut eng, &ctx, &cmyk, &[50.0, 0.0, 0.0], Intent::RelativeColorimetric, true).unwrap();
        assert!(on.receipt.bpc.source_black_lstar.unwrap().abs() < 1e-9 && (on.receipt.bpc.destination_black_lstar.unwrap() - 20.0).abs() < 0.1);
        // Typed refusals for Lab input.
        assert_eq!(run_lab(&mut eng, &ctx, SRGB, &[101.0, 0.0, 0.0], Intent::RelativeColorimetric, false).unwrap_err().error, WideError::ChannelRange);
        assert_eq!(run_lab(&mut eng, &ctx, SRGB, &[f32::NAN, 0.0, 0.0], Intent::RelativeColorimetric, false).unwrap_err().error, WideError::NonfiniteChannel);
        assert_eq!(run_lab(&mut eng, &ctx, SRGB, &[50.0, 0.0], Intent::RelativeColorimetric, false).unwrap_err().error, WideError::ChannelCount);
    });
}

#[test]
fn second_conversion_with_the_same_key_reuses_the_plan_without_parsing() {
    bounded_case("second_conversion_with_the_same_key_reuses_the_plan_without_parsing", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        let cmyk = cmyk_k_profile(5, 3, 20.0);
        // 100000 pixels in one request: far beyond the legacy 4096-pixel limit.
        let n = 100_000usize;
        let pixels: Vec<f32> = (0..n * 4).map(|i| ((i * 2654435761) % 1000) as f32 / 1000.0).collect();
        let first = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        assert_eq!((first.receipt.pixel_count, first.receipt.cache_hit), (n, false));
        assert_eq!((eng.cache_stats().parses, eng.cache_stats().misses), (2, 1));
        let second = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        assert!(second.receipt.cache_hit);
        assert_eq!(eng.cache_stats().parses, 2, "a cache hit must parse nothing");
        assert_eq!(eng.cache_stats().hits, 1);
        assert_eq!(second.pixels, first.pixels);
        assert_eq!(second.receipt.output_sha256, first.receipt.output_sha256);
        // A different key (BPC flag) is a new plan and parses again; the first key is still cached.
        run(&mut eng, &ctx, &cmyk, SRGB, &pixels[..400], Intent::RelativeColorimetric, false).unwrap();
        assert_eq!(eng.cache_stats().parses, 4);
        run(&mut eng, &ctx, &cmyk, SRGB, &pixels[..400], Intent::RelativeColorimetric, true).unwrap();
        assert_eq!(eng.cache_stats().parses, 4);
        // A hit still re-proves the bytes: a wrong hash is refused without parsing.
        let wrong = ProfileInput { expected_sha256: [1; 32], ..ctx.input(&ctx.source, &cmyk) };
        let mut request = ctx.request(&cmyk, SRGB, &pixels[..400], Intent::RelativeColorimetric, true);
        request.source = WideSource::Profile(wrong);
        let failure = eng.transform(&request, &Default::default(), &mut Collect(Vec::new())).unwrap_err();
        assert_eq!(failure.error, WideError::HashMismatch);
        // The per-request bound is explicit.
        let over = vec![0.0f32; (MAX_WIDE_PIXELS + 1) * 4];
        assert_eq!(run(&mut eng, &ctx, &cmyk, SRGB, &over, Intent::RelativeColorimetric, true).unwrap_err().error, WideError::PixelLimit);
    });
}
