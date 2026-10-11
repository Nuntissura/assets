//! SMB-PRISM-ENG-3: wide profile admission. Synthetic profiles only; installed-profile checks skip
//! with a typed reason when the file is absent. Expected values are computed from the profiles'
//! own definitions (Lab ramp -> D50 neutral -> sRGB encoding), never from the engine.
mod wide_support;
use hsk_studio_accord::CancellationToken;
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
/// K ramp printer: L = 100 - 80 K, a = b = 0, so K = 1 is L 20 and C, M, Y do not matter.
fn k_ramp_cmyk(grid: u8, intents: usize) -> Vec<u8> {
    let a2b = cmyk_a2b(grid, |_, _, _, k| (100.0 - 80.0 * k, 0.0, 0.0));
    let b2a = cmyk_b2a(6, |l, _, _| [0.0, 0.0, 0.0, (100.0 - l) / 80.0]);
    cmyk_profile(a2b, Some(b2a), intents, D50)
}

#[test]
fn large_cmyk_admitted_wide_refused_legacy_and_typed_refusals() {
    bounded_case("large_cmyk_admitted_wide_refused_legacy_and_typed_refusals", || {
        let ctx = Ctx::new();
        let cmyk = k_ramp_cmyk(17, 3);
        assert!(cmyk.len() > MAX_PROFILE_BYTES * 7 && cmyk.len() < MAX_WIDE_PROFILE_BYTES);
        let eng = engine(&ctx);
        let token = CancellationToken::default();
        let d = eng.inspect_profile(ctx.input(&ctx.source, &cmyk), &token).unwrap();
        assert_eq!(
            (d.kind, d.device, d.pcs, d.version_major, d.bytes),
            (ProfileKind::LegacyLut, DeviceSpace::Cmyk, PcsKind::Lab, 2, cmyk.len())
        );
        assert!(d.device_to_pcs.perceptual && d.device_to_pcs.colorimetric && d.device_to_pcs.saturation);
        assert!(d.pcs_to_device.colorimetric);
        let mw = d.media_white.unwrap();
        assert!((0..3).all(|i| (mw[i] - D50[i]).abs() < 1e-4));
        // The legacy path keeps its 64 KiB cap and is unchanged.
        let prism = Prism::new(ctx.actor.clone(), 1).unwrap();
        assert_eq!(
            prism.inspect_profile(ctx.input(&ctx.source, &cmyk), &token).unwrap_err(),
            Error::ProfileLimit
        );
        assert_eq!(prism.inspect_wide_profile(ctx.input(&ctx.source, &cmyk), &token).unwrap().kind, ProfileKind::LegacyLut);
        // Typed refusals, one per rule.
        let refuse = |bytes: &[u8]| eng.inspect_profile(ctx.input(&ctx.source, bytes), &token).unwrap_err();
        let mut over = cmyk.clone();
        over.resize(MAX_WIDE_PROFILE_BYTES + 1, 0);
        assert_eq!(refuse(&over), WideError::ProfileLimit);
        let patched = |at: usize, v: &[u8]| {
            let mut b = cmyk.clone();
            b[at..at + v.len()].copy_from_slice(v);
            b
        };
        assert_eq!(refuse(&patched(8, &[5])), WideError::UnsupportedVersion);
        assert_eq!(refuse(&patched(12, b"link")), WideError::UnsupportedClass);
        assert_eq!(refuse(&patched(16, b"Lab ")), WideError::UnsupportedColorSpace);
        assert_eq!(refuse(&patched(20, b"HSV ")), WideError::UnsupportedPcs);
        assert_eq!(refuse(&patched(36, b"nope")), WideError::MalformedProfile);
        assert_eq!(refuse(&cmyk[..100]), WideError::MalformedProfile);
        let multi_process = assemble(
            0x0210_0000, b"prtr", b"CMYK", b"Lab ",
            &[xyz_tag(D50), cmyk_a2b(2, |_, _, _, k| (100.0 - 80.0 * k, 0.0, 0.0))],
            &[(b"wtpt", 0), (b"A2B0", 1), (b"D2B0", 1)],
        );
        assert_eq!(refuse(&multi_process), WideError::UnsupportedTagType);
        let no_table = assemble(0x0210_0000, b"prtr", b"CMYK", b"Lab ", &[xyz_tag(D50)], &[(b"wtpt", 0)]);
        assert_eq!(refuse(&no_table), WideError::UnsupportedProfile);
        let wrong_hash = ProfileInput { expected_sha256: [0; 32], ..ctx.input(&ctx.source, &cmyk) };
        assert_eq!(eng.inspect_profile(wrong_hash, &token).unwrap_err(), WideError::HashMismatch);
        token.cancel();
        assert_eq!(eng.inspect_profile(ctx.input(&ctx.source, &cmyk), &token).unwrap_err(), WideError::Canceled);
    });
}

#[test]
fn cmyk_to_srgb_k_ramp_matches_lab_definition_and_missing_table_is_typed() {
    bounded_case("cmyk_to_srgb_k_ramp_matches_lab_definition_and_missing_table_is_typed", || {
        let ctx = Ctx::new();
        let mut eng = engine(&ctx);
        let cmyk = k_ramp_cmyk(5, 3);
        #[rustfmt::skip]
        let pixels = [
            0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.25,
            0.0, 0.0, 0.0, 0.5,
            0.0, 0.0, 0.0, 0.75,
            0.0, 0.0, 0.0, 1.0,
            0.5, 0.5, 0.5, 0.5,
        ];
        let result = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, false).unwrap();
        // Expected: L = 100 - 80 K -> Y = ((L+16)/116)^3 -> sRGB gray (R = G = B).
        let expected = [0.0, 0.25, 0.5, 0.75, 1.0, 0.5].map(|k: f64| srgb_encode(y_of_l(100.0 - 80.0 * k)));
        assert_eq!(result.pixels.len(), 18);
        for (i, e) in expected.iter().enumerate() {
            for c in 0..3 {
                let v = f64::from(result.pixels[i * 3 + c]);
                assert!((v - e).abs() < 1e-3, "pixel {i} channel {c}: {v} vs {e}");
            }
        }
        let r = &result.receipt;
        assert_eq!((r.source_kind, r.destination_kind), (Some(ProfileKind::LegacyLut), ProfileKind::MatrixTrc));
        assert_eq!((r.source_space, r.destination_space), (SourceSpace::Device(DeviceSpace::Cmyk), DeviceSpace::Rgb));
        assert!(!r.engine_build.simd && r.engine_build.interpolation == "tetrahedral_lab_output_trilinear" && !r.options.prefer_fixed_point);
        assert_eq!(result.delivery.outcome, Outcome::Success);
        // Deterministic and cached: same bits, same digest, second call is a cache hit.
        let again = run(&mut eng, &ctx, &cmyk, SRGB, &pixels, Intent::RelativeColorimetric, false).unwrap();
        assert!(again.receipt.cache_hit && !r.cache_hit);
        assert_eq!(again.pixels, result.pixels);
        assert_eq!(again.receipt.output_sha256, r.output_sha256);
        assert_eq!(eng.cache_stats().hits, 1);
        // A profile with only the perceptual A2B0 table: ICC.1:2022 8.10.2 c) makes every intent use A2B0
        // (SMB-PRISM-ENG-6; the table inventory stays visible in WideProfileDescriptor), so relative equals
        // perceptual bit for bit.
        let only_perceptual = k_ramp_cmyk(5, 1);
        let desc = eng.inspect_profile(ctx.input(&ctx.source, &only_perceptual), &CancellationToken::default()).unwrap();
        assert!(desc.device_to_pcs.perceptual && !desc.device_to_pcs.colorimetric);
        let relative = run(&mut eng, &ctx, &only_perceptual, SRGB, &pixels, Intent::RelativeColorimetric, false).unwrap();
        let perceptual = run(&mut eng, &ctx, &only_perceptual, SRGB, &pixels, Intent::Perceptual, false).unwrap();
        assert_eq!(relative.pixels, perceptual.pixels);
        assert!((f64::from(perceptual.pixels[3 * 4 + 1]) - expected[4]).abs() < 2e-3);
        // Bad pixel buffers.
        assert_eq!(run(&mut eng, &ctx, &cmyk, SRGB, &pixels[..6], Intent::RelativeColorimetric, false).unwrap_err().error, WideError::ChannelCount);
        assert_eq!(run(&mut eng, &ctx, &cmyk, SRGB, &[0.0, 0.0, 0.0, 1.5], Intent::RelativeColorimetric, false).unwrap_err().error, WideError::ChannelRange);
    });
}

#[test]
fn admission_order_matrix_v4_matrix_v2_mab_and_installed_profiles() {
    bounded_case("admission_order_matrix_v4_matrix_v2_mab_and_installed_profiles", || {
        let ctx = Ctx::new();
        let eng = engine(&ctx);
        let token = CancellationToken::default();
        let inspect = |bytes: &[u8]| eng.inspect_profile(ctx.input(&ctx.source, bytes), &token).unwrap();
        let v4 = inspect(SRGB);
        assert_eq!((v4.kind, v4.trc, v4.version_major, v4.device), (ProfileKind::MatrixTrc, Some(TrcKind::Parametric), 4, DeviceSpace::Rgb));
        let v2 = inspect(&matrix_v2(2.2));
        assert_eq!((v2.kind, v2.trc, v2.version_major), (ProfileKind::MatrixTrc, Some(TrcKind::Curve), 2));
        assert!(v2.device_to_pcs.has_all() && v2.pcs_to_device.has_all());
        let mab = inspect(&mab_rgb_profile(2, |r, g, b| [0.4360 * r + 0.3851 * g + 0.1431 * b, 0.2225 * r + 0.7169 * g + 0.0606 * b, 0.0139 * r + 0.0971 * g + 0.7141 * b]));
        assert_eq!((mab.kind, mab.device, mab.pcs, mab.version_major), (ProfileKind::MultiDimensional, DeviceSpace::Rgb, PcsKind::Xyz, 4));
        assert!(mab.device_to_pcs.perceptual && mab.device_to_pcs.colorimetric && !mab.device_to_pcs.saturation && !mab.pcs_to_device.colorimetric);
        // Real installed profiles are optional evidence: absent files skip with a typed reason.
        let adobe = "C:/Program Files (x86)/Common Files/Adobe/Color";
        let installed = [
            (format!("{adobe}/Profiles/Recommended/USWebCoatedSWOP.icc"), ProfileKind::LegacyLut, DeviceSpace::Cmyk),
            (format!("{adobe}/Profiles/Recommended/CoatedFOGRA39.icc"), ProfileKind::LegacyLut, DeviceSpace::Cmyk),
            (format!("{adobe}/MPProfiles/Rec2100HLG203.icc"), ProfileKind::MultiDimensional, DeviceSpace::Rgb),
        ];
        for (path, kind, space) in installed {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let d = inspect(&bytes);
                    assert_eq!((d.kind, d.device), (kind, space), "{path}");
                    assert!(d.bytes > MAX_PROFILE_BYTES);
                }
                Err(e) => eprintln!("SKIP installed_profile_absent path={path} reason={:?}", e.kind()),
            }
        }
    });
}
