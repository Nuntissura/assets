//! SMB-PRISM-ENG-4: scalar-determinism harness. The wide path must be the same arithmetic on every
//! host: no SIMD feature compiled into moxcms, no fused multiply-add contraction, a declared
//! interpolation, and the engine identity plus an output digest in every receipt. Cross-host proof
//! compares `output_sha256` of the pinned fixtures below between hosts (V3); this host pins the
//! x86_64 Windows values.
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
fn hex(d: &[u8; 32]) -> String {
    hash_hex(d)
}
/// Deterministic pseudo-random channel values in 0..1 (64-bit LCG, high bits).
fn lcg(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32) / 16_777_216.0
        })
        .collect()
}

#[test]
fn build_has_no_simd_no_fma_contraction_and_receipts_name_the_engine() {
    bounded_case("build_has_no_simd_no_fma_contraction_and_receipts_name_the_engine", || {
        // The moxcms dependency line is the only place SIMD features could be switched on.
        let manifest = include_str!("../Cargo.toml").replace("\r\n", "\n");
        let line = manifest.lines().find(|l| l.starts_with("moxcms =")).expect("moxcms dependency");
        assert!(line.contains("default-features = false"), "{line}");
        let features = line.split("features = [").nth(1).and_then(|f| f.split(']').next()).unwrap();
        let declared: Vec<&str> = features.split(',').map(|f| f.trim().trim_matches('"')).collect();
        for f in &declared {
            assert!(["extended_range", "lut", "options"].contains(f), "unreviewed moxcms feature {f}");
            assert!(!["avx", "sse", "neon", "sve"].iter().any(|s| f.contains(s)), "SIMD feature {f}");
        }
        let lock = include_str!("../../../Cargo.lock").replace("\r\n", "\n");
        assert!(lock.contains("name = \"moxcms\"\nversion = \"0.9.1\""), "engine version drifted from the receipt");
        // SMB-PRISM-ENG-6: the wide path is Prism's own f64 arithmetic from IEEE basic operations (no libm, no
        // `mul_add`); Rust never contracts it to fused multiply-add, whatever the target features.
        let build = engine_build();
        assert!(!build.fma_contraction, "the wide path must not depend on fused multiply-add");
        assert!(!build.simd && !build.fixed_point && build.interpolation == "tetrahedral_lab_output_trilinear" && build.barycentric_weights == "exact_f64");
        // The receipt of a real conversion carries the identity and a digest.
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        let cmyk = cmyk_k_profile(5, 3, 20.0);
        let r = run(&mut eng, &ctx, &cmyk, SRGB, &[0.1, 0.2, 0.3, 0.4], Intent::RelativeColorimetric, false).unwrap().receipt;
        assert_eq!((r.engine_name, r.engine_version, r.engine_build), (WIDE_ENGINE_NAME, WIDE_ENGINE_VERSION, build));
        assert_ne!(r.output_sha256, [0; 32]);
    });
}

#[test]
fn output_is_chunk_repeat_and_cold_start_invariant_with_pinned_digests() {
    bounded_case("output_is_chunk_repeat_and_cold_start_invariant_with_pinned_digests", || {
        let ctx = Ctx::new();
        let cmyk = cmyk_k_profile(9, 3, 20.0);
        let mab = mab_rgb_profile(3, |r, g, b| [0.4360 * r + 0.3851 * g + 0.1431 * b, 0.2225 * r + 0.7169 * g + 0.0606 * b, 0.0139 * r + 0.0971 * g + 0.7141 * b]);
        let pixels = lcg(3000 * 4, 7);
        let mut eng = engine(&ctx);
        let whole = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        // One call, three calls, and per-pixel calls produce the same bits.
        let mut stitched = Vec::new();
        for part in pixels.chunks(1000 * 4) {
            stitched.extend(run(&mut eng, &ctx, &cmyk, SRGB, part, Intent::RelativeColorimetric, true).unwrap().pixels);
        }
        assert_eq!(stitched, whole.pixels);
        for (i, px) in pixels.chunks(4).take(40).enumerate() {
            let one = run(&mut eng, &ctx, &cmyk, SRGB, px, Intent::RelativeColorimetric, true).unwrap();
            assert_eq!(one.pixels[..], whole.pixels[i * 3..i * 3 + 3], "pixel {i}");
        }
        // A cold engine (fresh plan, fresh bake) reproduces the digest exactly.
        let cold = run(&mut engine(&ctx), &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true).unwrap();
        assert_eq!(cold.receipt.output_sha256, whole.receipt.output_sha256);
        // Pinned digests for cross-host comparison: CMYK table, RGB mAB table, Lab source.
        let rgb = lcg(3000 * 3, 11);
        let table = run(&mut eng, &ctx, &mab, SRGB, &rgb, Intent::Perceptual, true).unwrap();
        let labs: Vec<f32> = rgb.chunks(3).flat_map(|p| [p[0] * 100.0, p[1] * 200.0 - 100.0, p[2] * 200.0 - 100.0]).collect();
        let lab = run_lab(&mut eng, &ctx, SRGB, &labs, Intent::AbsoluteColorimetric, false).unwrap();
        let got = [hex(&whole.receipt.output_sha256), hex(&table.receipt.output_sha256), hex(&lab.receipt.output_sha256)];
        eprintln!("DIGESTS cmyk={} mab={} lab={}", got[0], got[1], got[2]);
        // Re-pinned by SMB-PRISM-ENG-6 (WIDE_ENGINE_VERSION 1: exact f64 evaluation replaced the moxcms legs).
        const PINNED: [&str; 3] = [
            "0ec04bd4fd940e90e9fcd83e39afda261847371633dc99953aa1034553d1ae67",
            "61bc54cee68d2855db62e857b0f38c2f0f3ad19c1b6824b71377b354b2f2e611",
            "6c122339644d025b68c23d473aab5132a6a8547351e71c8fe4388a180a58b114",
        ];
        if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
            assert_eq!(got, PINNED, "pinned x86_64-windows digests changed");
        } else {
            eprintln!("UNPINNED target: compare the DIGESTS line with the x86_64-windows values");
        }
    });
}
