//! SMB-PRISM-ENG-5, oracle tier T0: closed-form ICC.1:2022 math in f64 (no colour management module).
//! The fixture `oracle/t0_analytic.json` carries profile specifications (s15Fixed16 integers), input
//! patches and expected outputs computed by `prism-oracle/gen/t0_generate.py` from the DECODED profile
//! values, never from the engine. These tests build the profiles from the specs and compare WideEngine
//! output with the stated tolerance. Failures are findings; the tolerance is not tuned to the engine.
mod oracle_support;
mod wide_support;
use hsk_studio_prism::*;
use oracle_support::*;
use std::collections::BTreeMap;
use wide_support::*;

const FIXTURE: &str = include_str!("oracle/t0_analytic.json");

fn load() -> Json {
    Json::parse(FIXTURE).expect("t0 fixture parses")
}

/// Defects the oracle has already captured, as (case, intent, bpc) -> finding id. A listed combination must
/// STILL violate its expectation (the test fails if it starts passing: remove the entry and the finding is
/// closed); every other combination must pass. Thresholds are never touched.
// SMB-PRISM-ENG-6 closed every T0 finding (curv gamma zero, gray/RGB BPC black, negative PCS clamp, curv table
// interpolation and inverse): the registry is empty and every combination must pass.
const KNOWN_FINDINGS: &[(&str, &str, bool, &str)] = &[];

fn known_finding(id: &str, intent: &str, bpc: bool) -> Option<&'static str> {
    KNOWN_FINDINGS.iter().find(|(c, i, b, _)| *c == id && *i == intent && *b == bpc).map(|e| e.3)
}

fn run_case(id: &'static str) {
    bounded(id, 60, move || {
        let doc = load();
        let mut profiles: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for name in doc.get("profiles").keys() {
            profiles.insert(name.to_string(), t0_profile_bytes(doc.get("profiles").get(name)));
        }
        let case = doc
            .get("cases")
            .arr()
            .iter()
            .find(|c| c.get("id").str() == id)
            .unwrap_or_else(|| panic!("case {id} missing from fixture"));
        let ctx = Ctx::new();
        let mut eng = WideEngine::new(ctx.actor.clone(), 8).unwrap();
        let input = to_f32(&case.get("input").f64s());
        let expected = case.get("expected").f64s();
        let (cin, cout) = (case.get("channels_in").i64() as usize, case.get("channels_out").i64() as usize);
        assert_eq!(input.len() % cin, 0);
        assert_eq!(expected.len(), input.len() / cin * cout);
        let tol = case.get("tolerance_abs").f64();
        let lab = case.get("input_kind").str() == "lab";
        let dst = &profiles[case.get("destination").str()];
        let mut worst_all = (0.0f64, String::new());
        let (mut combos, mut failures, mut expected_fails) = (0, Vec::<String>::new(), Vec::<String>::new());
        for combo in case.get("combos").arr() {
            let (intent_name, bpc) = (combo.at(0).str(), combo.at(1).bool());
            let intent = intent_of(intent_name);
            let result = if lab {
                run_lab(&mut eng, &ctx, dst, &input, intent, bpc)
            } else {
                run(&mut eng, &ctx, &profiles[case.get("source").str()], dst, &input, intent, bpc)
            };
            let out = result.unwrap_or_else(|f| panic!("{id} {intent_name} bpc={bpc}: engine refused {:?}", f.error));
            assert_eq!(out.pixels.len(), expected.len(), "{id}: output length");
            let (mut worst, mut at) = (0.0f64, 0usize);
            let mut bad_patches: Vec<usize> = Vec::new();
            let clamped = case.has("compare") && case.get("compare").str() == "clamped";
            for (i, (got, want)) in out.pixels.iter().zip(&expected).enumerate() {
                let (g, w) = if clamped { (f64::from(*got).clamp(0.0, 1.0), want.clamp(0.0, 1.0)) } else { (f64::from(*got), *want) };
                let e = if got.is_finite() { (g - w).abs() } else { f64::INFINITY };
                if e > worst {
                    (worst, at) = (e, i);
                }
                if e > tol && bad_patches.last() != Some(&(i / cout)) {
                    bad_patches.push(i / cout);
                }
            }
            let mut broken = worst > tol;
            let mut detail = format!(
                "{intent_name} bpc={bpc} patch {} ch {}: got {} want {} (err {worst:.3e}, tolerance {tol:.1e})",
                at / cout,
                at % cout,
                out.pixels[at],
                expected[at]
            ) + &if bad_patches.is_empty() {
                String::new()
            } else {
                format!("; {} of {} patches off, first {:?}", bad_patches.len(), input.len() / cin, &bad_patches[..bad_patches.len().min(8)])
            };
            // Black point compensation report. Zero-black matrix/gray profiles: BPC is the identity (ISO 18619:
            // scale = (1 - Yd) / (1 - Ys) with Ys = Yd = 0) and every reported black is L* 0. Cases with a
            // non-zero source black state the analytic scale and black L* instead.
            let b = out.receipt.bpc;
            if bpc && intent != Intent::AbsoluteColorimetric {
                let (want_scale, want_src, want_dst) = if case.has("expected_bpc") {
                    let e = case.get("expected_bpc");
                    (e.get("scale").f64(), e.get("source_black_lstar").f64(), e.get("destination_black_lstar").f64())
                } else {
                    (1.0, 0.0, 0.0)
                };
                let near = |got: Option<f64>, want: f64, tol: f64| got.is_none_or(|g| (g - want).abs() <= tol);
                let ok = (b.scale.is_none() && want_scale == 1.0 || b.scale.is_some_and(|s| (s - want_scale).abs() <= 1e-5))
                    && near(b.source_black_lstar, want_src, 0.1)
                    && near(b.destination_black_lstar, want_dst, 0.1);
                if !ok {
                    broken = true;
                    detail.push_str(&format!(
                        "; bpc report scale={:?} source_black_lstar={:?} destination_black_lstar={:?} (expected scale {want_scale:.6}, L* {want_src:.3} -> {want_dst:.3})",
                        b.scale, b.source_black_lstar, b.destination_black_lstar
                    ));
                }
            }
            if let Json::Obj(_) = case.get("expected_clip") {
                let clip = case.get("expected_clip");
                let c = out.receipt.clip;
                assert_eq!(c.method, ClipMethod::LinearDevice, "{id}: clip method");
                assert_eq!(c.pixels_clipped as i64, clip.get("pixels_clipped").i64(), "{id}: pixels_clipped");
                let want: Vec<i64> = clip.get("channels_clipped").arr().iter().map(Json::i64).collect();
                let got: Vec<i64> = c.channels_clipped.iter().map(|v| *v as i64).collect();
                assert_eq!(got, want, "{id}: channels_clipped");
                assert!(
                    (c.max_overshoot - clip.get("max_overshoot").f64()).abs() < 1e-3,
                    "{id}: max_overshoot {} vs {}",
                    c.max_overshoot,
                    clip.get("max_overshoot").f64()
                );
            }
            combos += 1;
            match (broken, known_finding(id, intent_name, bpc)) {
                (true, Some(f)) => expected_fails.push(format!("{f}: {detail}")),
                (true, None) => failures.push(detail),
                (false, Some(f)) => failures.push(format!(
                    "FINDING {f} no longer reproduces for {intent_name} bpc={bpc}: remove it from KNOWN_FINDINGS"
                )),
                (false, None) => {
                    if worst > worst_all.0 {
                        worst_all = (worst, detail);
                    }
                }
            }
        }
        eprintln!(
            "ORACLE_T0 {{\"case\":\"{id}\",\"combos\":{combos},\"patches\":{},\"max_abs_err\":{:.3e},\"tolerance\":{tol:.1e},\"worst\":\"{}\",\"expected_fail\":{}}}",
            input.len() / cin,
            worst_all.0,
            worst_all.1.replace('"', "'"),
            expected_fails.len()
        );
        for e in &expected_fails {
            eprintln!("ORACLE_T0_FINDING {id}: {e}");
        }
        assert!(failures.is_empty(), "{id}: {}", failures.join(" | "));
    });
}

macro_rules! t0_cases {
    ($($name:ident => $id:literal),* $(,)?) => {
        const COVERED: &[&str] = &[$($id),*];
        $(#[test] fn $name() { run_case($id); })*
    };
}
t0_cases! {
    srgb_encoded_to_linear => "srgb_encoded_to_linear",
    linear_to_srgb_encoded => "linear_to_srgb_encoded",
    srgb_to_xyz_d50 => "srgb_to_xyz_d50",
    adobergb_to_xyz_d50 => "adobergb_to_xyz_d50",
    adobergb_to_srgb => "adobergb_to_srgb",
    srgb_to_adobergb => "srgb_to_adobergb",
    gray_curv_gamma_to_gray_linear => "gray_curv_gamma_to_gray_linear",
    gray_para3_to_gray_linear => "gray_para3_to_gray_linear",
    gray_linear_to_gray_curv_gamma => "gray_linear_to_gray_curv_gamma",
    gray_curv_gamma_to_srgb => "gray_curv_gamma_to_srgb",
    para1_to_linear => "para1_to_linear",
    para2_to_linear => "para2_to_linear",
    para4_to_linear => "para4_to_linear",
    curv_table_to_linear => "curv_table_to_linear",
    curv_table_dense_gray_scan => "curv_table_dense_gray_scan",
    para2_bpc_iso18619 => "para2_bpc_iso18619",
    para4_bpc_iso18619 => "para4_bpc_iso18619",
    gray_v21_gamma2_to_srgb_linear => "gray_v21_gamma2_to_srgb_linear",
    gray_v21_gamma18_to_srgb_linear => "gray_v21_gamma18_to_srgb_linear",
    prophoto_curv18_to_xyz_d50 => "prophoto_curv18_to_xyz_d50",
    prophoto_curv22_to_xyz_d50 => "prophoto_curv22_to_xyz_d50",
    prophoto_para3_to_xyz_d50 => "prophoto_para3_to_xyz_d50",
    srgb_curv18_to_xyz_d50 => "srgb_curv18_to_xyz_d50",
    p3_para3_to_xyz_d50 => "p3_para3_to_xyz_d50",
    srgb_curv18_bpc_to_srgb_linear => "srgb_curv18_bpc_to_srgb_linear",
    lab_to_srgb_para3_clipped => "lab_to_srgb_para3_clipped",
    lab_to_srgb_curv_table1024_clipped => "lab_to_srgb_curv_table1024_clipped",
    curv_gamma_256_mixed_zero_to_xyz_d50 => "curv_gamma_256_mixed_zero_to_xyz_d50",
    curv_gamma_384_mixed_zero_to_xyz_d50 => "curv_gamma_384_mixed_zero_to_xyz_d50",
    curv_gamma_461_mixed_zero_to_xyz_d50 => "curv_gamma_461_mixed_zero_to_xyz_d50",
    curv_gamma_512_mixed_zero_to_xyz_d50 => "curv_gamma_512_mixed_zero_to_xyz_d50",
    curv_gamma_538_mixed_zero_to_xyz_d50 => "curv_gamma_538_mixed_zero_to_xyz_d50",
    curv_gamma_563_mixed_zero_to_xyz_d50 => "curv_gamma_563_mixed_zero_to_xyz_d50",
    curv_gamma_614_mixed_zero_to_xyz_d50 => "curv_gamma_614_mixed_zero_to_xyz_d50",
    lab_to_srgb => "lab_to_srgb",
    lab_to_linear_srgb_out_of_gamut => "lab_to_linear_srgb_out_of_gamut",
}

#[test]
fn every_fixture_case_has_a_test() {
    let doc = load();
    let mut listed: Vec<&str> = doc.get("cases").arr().iter().map(|c| c.get("id").str()).collect();
    let mut covered: Vec<&str> = COVERED.to_vec();
    covered.sort_unstable();
    listed.sort_unstable();
    assert_eq!(listed, covered, "fixture cases and test list differ");
    assert_eq!(doc.get("tier").str(), "T0");
}

// ---- Bradford derivation, re-implemented here in f64 and compared with the Python (exact rational) one.
type M3 = [[f64; 3]; 3];
#[allow(clippy::needless_range_loop)]
fn mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}
fn mv(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}
#[allow(clippy::needless_range_loop)]
fn inv(m: &M3) -> M3 {
    let c = |i: usize, j: usize| {
        m[(i + 1) % 3][(j + 1) % 3] * m[(i + 2) % 3][(j + 2) % 3]
            - m[(i + 1) % 3][(j + 2) % 3] * m[(i + 2) % 3][(j + 1) % 3]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * c(0, j)).sum();
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = c(j, i) / det;
        }
    }
    o
}
fn xy(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}
#[allow(clippy::needless_range_loop)]
#[test]
fn bradford_d50_colorants_agree_with_the_independent_python_derivation() {
    let doc = load();
    let bradford: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let d65 = xy(0.3127, 0.3290);
    let d50 = [63190.0 / 65536.0, 1.0, 54061.0 / 65536.0];
    let primaries: BTreeMap<&str, [[f64; 2]; 3]> = BTreeMap::from([
        ("srgb", [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]]),
        ("adobergb", [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]]),
    ]);
    for (name, p) in primaries {
        let cols = p.map(|q| xy(q[0], q[1]));
        let mp: M3 = [0, 1, 2].map(|i| [0, 1, 2].map(|j| cols[j][i]));
        let s = mv(&inv(&mp), d65);
        let native: M3 = [0, 1, 2].map(|i| [0, 1, 2].map(|j| mp[i][j] * s[j]));
        let (rs, rd) = (mv(&bradford, d65), mv(&bradford, d50));
        let mut diag = [[0.0; 3]; 3];
        for i in 0..3 {
            diag[i][i] = rd[i] / rs[i];
        }
        let adapted = mul(&inv(&bradford), &mul(&diag, &mul(&bradford, &native)));
        let want = doc.get("derived").get("bradford_d50_colorants_float").get(name);
        let rows = doc.get("derived").get("colorants_s15_rows_rgb_by_xyz").get(name);
        for i in 0..3 {
            for j in 0..3 {
                let w = want.at(i).at(j).f64();
                assert!((adapted[i][j] - w).abs() < 1e-12, "{name} [{i}][{j}] rust {} python {w}", adapted[i][j]);
                // fixture rows are colorant-major: rows[j] = [X,Y,Z] of channel j
                assert_eq!((adapted[i][j] * 65536.0).round() as i64, rows.at(j).at(i).i64(), "{name} s15 [{j}][{i}]");
            }
        }
        // white property: RGB (1,1,1) maps to the PCS D50 white (Bradford takes the D65 white to D50).
        let sum = mv(&adapted, [1.0, 1.0, 1.0]);
        for k in 0..3 {
            assert!((sum[k] - d50[k]).abs() < 1e-12, "{name} white sum {sum:?} vs {d50:?}");
        }
    }
}

#[allow(clippy::needless_range_loop)]
#[test]
fn published_srgb_v4_fixture_colorants_versus_derived() {
    // Cross-check of the repo's CC0 sRGB-v4.icc colorants against the Bradford derivation (profile
    // fidelity, not engine behaviour): a different D50 definition or rounding shows up here.
    let doc = load();
    let derived = doc.get("derived").get("bradford_d50_colorants_float").get("srgb");
    let mut worst = 0.0f64;
    for (j, sig) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().enumerate() {
        let got = read_xyz_tag(SRGB, sig).expect("colorant tag");
        for i in 0..3 {
            worst = worst.max((got[i] - derived.at(i).at(j).f64()).abs());
        }
    }
    eprintln!("ORACLE_T0 {{\"case\":\"srgb_v4_fixture_colorants_vs_derived\",\"max_abs_colorant_diff\":{worst:.3e}}}");
    assert!(worst < 5e-4, "sRGB-v4.icc colorants differ from the Bradford derivation by {worst:.3e}");
}

#[test]
fn ciede2000_reproduces_published_reference_pairs() {
    // Sharma, Wu, Dalal 2005, "The CIEDE2000 colour-difference formula", supplementary test data.
    let rows: &[([f64; 3], [f64; 3], f64)] = &[
        ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
        ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
        ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
        ([50.0, -1.3802, -84.2814], [50.0, 0.0, -82.7485], 1.0000),
        ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
        ([50.0, 2.4900, -0.0010], [50.0, -2.4900, 0.0009], 7.1792),
        ([50.0, 2.4900, -0.0010], [50.0, -2.4900, 0.0011], 7.2195),
        ([50.0, -0.0010, 2.4900], [50.0, 0.0009, -2.4900], 4.8045),
        ([50.0, 2.5, 0.0], [50.0, 0.0, -2.5], 4.3065),
        ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
        ([50.0, 2.5, 0.0], [61.0, -5.0, 29.0], 22.8977),
        ([50.0, 2.5, 0.0], [56.0, -27.0, -3.0], 31.9030),
        ([50.0, 2.5, 0.0], [58.0, 24.0, 15.0], 19.4535),
        ([50.0, 2.5, 0.0], [50.0, 3.1736, 0.5854], 1.0000),
    ];
    for (a, b, want) in rows {
        let got = delta_e_2000(*a, *b);
        assert!((got - want).abs() < 6e-5, "dE00 {a:?} vs {b:?}: {got} want {want}");
        assert!((delta_e_2000(*b, *a) - got).abs() < 1e-12, "symmetry {a:?} {b:?}");
    }
}

const SRGB_LINEAR_V4: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");

/// Exact repro from the interop-psd report (PRISM-GRAY-BPC-001, closed by SMB-PRISM-ENG-6): v2.1 mntr GRAY/XYZ
/// profile, wtpt D50, kTRC curv count=1 gamma u8Fixed8 0x0200, destination sRGB-linear-v4, relative colorimetric,
/// pixels [0, 0.5, 1]. Both blacks are L* 0, so BPC is the identity: BPC on equals BPC off and gray 0.5 is Y 0.25.
#[test]
fn interop_psd_gray_bpc_repro_is_fixed() {
    bounded("interop_psd_gray_bpc_repro", 60, || {
        let gray = assemble(
            0x0210_0000,
            b"mntr",
            b"GRAY",
            b"XYZ ",
            &[xyz_tag(D50), curv(Some(2.0))],
            &[(b"wtpt", 0), (b"kTRC", 1)],
        );
        let ctx = Ctx::new();
        let mut eng = WideEngine::new(ctx.actor.clone(), 4).unwrap();
        let pixels = [0.0f32, 0.5, 1.0];
        let off = run(&mut eng, &ctx, &gray, SRGB_LINEAR_V4, &pixels, Intent::RelativeColorimetric, false).unwrap();
        let on = run(&mut eng, &ctx, &gray, SRGB_LINEAR_V4, &pixels, Intent::RelativeColorimetric, true).unwrap();
        let worst = off.pixels.iter().zip(&on.pixels).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        let b = on.receipt.bpc;
        assert!(worst <= 1e-6, "BPC on {:?} vs off {:?}", on.pixels, off.pixels);
        assert_eq!((b.source_black_lstar, b.destination_black_lstar, b.scale, b.applied), (Some(0.0), Some(0.0), Some(1.0), false), "{b:?}");
        for (i, want) in [0.0f64, 0.25, 1.0].into_iter().enumerate() {
            for c in 0..3 {
                let got = f64::from(on.pixels[i * 3 + c]);
                assert!((got - want).abs() < 1.6e-4, "pixel {i} ch {c}: {got} want {want}");
            }
        }
    });
}
