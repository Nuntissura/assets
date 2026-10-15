//! Lane SMB-PRISM-VIEW-TRANSFORM-2: ACEScg input, the Display P3 SDR view, the PQ HDR views and
//! view selection with typed refusal.
//!
//! Reference values (OpenColorIO v2.3.2, BSD-3-Clause, `tests/cpu/transforms/BuiltinTransform_tests.cpp`,
//! an independent implementation of the ACES CTL, all for the input (0.5, 0.4, 0.3)):
//! - "ACEScg_to_ACES2065-1" -> (0.453158317919, 0.394926024520, 0.299297344519)
//! - "UTILITY - ACES-AP1_to_LINEAR-REC709_BFD" -> (0.578830986466, 0.388029190156, 0.282302431033)
//! - "ACES-OUTPUT - ACES2065-1_to_CIE-XYZ-D65 - SDR-VIDEO_1.0" -> XYZ (0.34128153, 0.32533440, 0.24217427)
//! - "ACES-OUTPUT - ... - HDR-VIDEO-{1000,2000,4000}nit-15nit-REC2020lim_1.1" -> XYZ in units of
//!   100 cd/m2: (0.48334542, 0.45336276, 0.32364485), (0.50538367, 0.47084737, 0.32972121),
//!   (0.52311981, 0.48482567, 0.33447576)
//! - "DISPLAY - CIE-XYZ-D65_to_REC.2100-PQ": XYZ (0.5, 0.4, 0.3) -> (0.464008302136,
//!   0.398157119110, 0.384828370950)
//!
//! The published ACEScg matrices are those of the ACEScg specification (aces-docs
//! `docs/encodings/acescg.md`).
use hsk_studio_prism::{
    ACES2065_1_TO_ACESCG, ACESCG_TO_ACES2065_1, ACESCG_TO_WORKING, DisplayEncoding, Error, SceneInput,
    ViewSelectError, ViewTransform, ViewTransformId as V, WORKING_TO_ACESCG, select_view,
};

type M3 = [[f64; 3]; 3];
const IN: [f32; 3] = [0.5, 0.4, 0.3];

fn npm(p: [[f64; 2]; 4]) -> M3 {
    let col = |xy: [f64; 2]| [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]];
    let (r, g, b, w) = (col(p[0]), col(p[1]), col(p[2]), col(p[3]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let det = |a: M3| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det(m);
    let mut s = [0.0; 3];
    for (k, sk) in s.iter_mut().enumerate() {
        let mut mk = m;
        for row in 0..3 {
            mk[row][k] = w[row];
        }
        *sk = det(mk) / d;
    }
    let mut out = m;
    for row in out.iter_mut() {
        for k in 0..3 {
            row[k] *= s[k];
        }
    }
    out
}
fn mv(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}
fn inv(m: M3) -> M3 {
    let c = |r1: usize, r2: usize, k1: usize, k2: usize| m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1];
    let det = m[0][0] * c(1, 2, 1, 2) - m[0][1] * c(1, 2, 0, 2) + m[0][2] * c(1, 2, 0, 1);
    [
        [c(1, 2, 1, 2) / det, -c(0, 2, 1, 2) / det, c(0, 1, 1, 2) / det],
        [-c(1, 2, 0, 2) / det, c(0, 2, 0, 2) / det, -c(0, 1, 0, 2) / det],
        [c(1, 2, 0, 1) / det, -c(0, 2, 0, 1) / det, c(0, 1, 0, 1) / det],
    ]
}
const REC709: [[f64; 2]; 4] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], [0.3127, 0.3290]];
const P3D65: [[f64; 2]; 4] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], [0.3127, 0.3290]];
const REC2020: [[f64; 2]; 4] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], [0.3127, 0.3290]];
fn f64s(v: [f32; 3]) -> [f64; 3] {
    v.map(f64::from)
}
fn rel(got: [f64; 3], want: [f64; 3]) -> f64 {
    (0..3).map(|i| (got[i] - want[i]).abs() / want[i].abs().max(1e-12)).fold(0.0, f64::max)
}

/// ACEScg matrices against the published specification and OCIO; the Display P3 SDR view and the
/// three PQ HDR views against OCIO's SDR-VIDEO / HDR-VIDEO values in XYZ D65; the PQ encoding
/// against OCIO's REC.2100-PQ display; ACEScg input = ACES2065-1 input of the published matrix.
#[test]
fn extended_views_match_ocio_and_the_published_acescg_matrices() {
    let mut report = Vec::new();
    // Published matrices are mutual inverses to their 10 printed decimals.
    let id = mv(&ACES2065_1_TO_ACESCG, mv(&ACESCG_TO_ACES2065_1, [0.3, 0.7, 0.11]));
    let d_pub = rel(id, [0.3, 0.7, 0.11]);
    let d_cg_ap0 = rel(mv(&ACESCG_TO_ACES2065_1, f64s(IN)), [0.453_158_317_919, 0.394_926_024_520, 0.299_297_344_519]);
    let d_cg_work = rel(mv(&ACESCG_TO_WORKING, f64s(IN)), [0.578_830_986_466, 0.388_029_190_156, 0.282_302_431_033]);
    let back = mv(&WORKING_TO_ACESCG, mv(&ACESCG_TO_WORKING, f64s(IN)));
    let d_round = rel(back, f64s(IN));
    report.push(format!("published inverse {d_pub:e}; acescg->ap0 vs OCIO {d_cg_ap0:e}; acescg->working vs OCIO {d_cg_work:e}; working round trip {d_round:e}"));

    // ACEScg input path = ACES2065-1 input of the published matrix.
    let ap0 = mv(&ACESCG_TO_ACES2065_1, f64s(IN)).map(|v| v as f32);
    let via_cg = V::Aces1SdrVideo.apply_from(SceneInput::AcesCg, IN).unwrap();
    let via_ap0 = V::Aces1SdrVideo.apply_from(SceneInput::Aces2065_1, ap0).unwrap();
    let d_cg_view = rel(f64s(via_cg), f64s(via_ap0));

    // Display P3 SDR view: same XYZ as the SDR-VIDEO reference.
    let p3 = V::Aces1SdrVideoDisplayP3.apply_from(SceneInput::Aces2065_1, IN).unwrap();
    let d_p3 = rel(mv(&npm(P3D65), f64s(p3)), [0.341_281_53, 0.325_334_40, 0.242_174_27]);
    let rec709 = V::Aces1SdrVideo.apply_from(SceneInput::Aces2065_1, IN).unwrap();
    let d_709 = rel(mv(&npm(REC709), f64s(rec709)), [0.341_281_53, 0.325_334_40, 0.242_174_27]);

    // HDR PQ views.
    let hdr_refs = [
        (V::Aces1HdrVideo1000Nits, [0.483_345_42, 0.453_362_76, 0.323_644_85]),
        (V::Aces1HdrVideo2000Nits, [0.505_383_67, 0.470_847_37, 0.329_721_21]),
        (V::Aces1HdrVideo4000Nits, [0.523_119_81, 0.484_825_67, 0.334_475_76]),
    ];
    let mut d_hdr = 0.0f64;
    for (view, want) in hdr_refs {
        let out = view.apply_from(SceneInput::Aces2065_1, IN).unwrap();
        let d = rel(mv(&npm(REC2020), f64s(out)), want);
        report.push(format!("{}: {out:?} -> delta {d:e}", view.key()));
        d_hdr = d_hdr.max(d);
    }
    // PQ encoding of XYZ (0.5, 0.4, 0.3) on Rec.2020.
    let lin2020 = mv(&inv(npm(REC2020)), [0.5, 0.4, 0.3]).map(|v| v as f32);
    let pq = DisplayEncoding::Pq.encode(lin2020).unwrap();
    let d_pq = rel(f64s(pq), [0.464_008_302_136, 0.398_157_119_110, 0.384_828_370_950]);
    report.push(format!("acescg view {d_cg_view:e}; display p3 {d_p3:e}; rec709 {d_709:e}; hdr max {d_hdr:e}; pq {d_pq:e}"));
    for line in &report {
        println!("{line}");
    }
    // Tolerances from the first run (2026-10-15, x86_64-windows-msvc): published inverse 2.0e-10;
    // OCIO matrix values 4.0e-8 and 4.6e-8 (OCIO evaluates in binary32); round trip 7.4e-16;
    // ACEScg view 0; Display P3 5.6e-7 and Rec.709 5.9e-7 (OCIO refits the CTL splines); HDR
    // 5.3e-7 / 6.7e-7 / 7.7e-7; PQ 3.1e-8.
    assert!(d_pub <= 1e-9 && d_cg_ap0 <= 1e-7, "{report:?}");
    assert!(d_cg_work <= 1e-7 && d_round <= 1e-12, "{report:?}");
    assert!(d_cg_view <= 1e-6, "{report:?}");
    assert!(d_p3 <= 2e-6 && d_709 <= 2e-6, "{report:?}");
    assert!(d_hdr <= 2e-6, "{report:?}");
    assert!(d_pq <= 1e-7, "{report:?}");
    // Keys round-trip.
    for v in V::ALL {
        assert_eq!(V::from_key(v.key()), Some(*v));
    }
    for s in SceneInput::ALL {
        assert_eq!(SceneInput::from_key(s.key()), Some(*s));
    }
}

/// View selection over the STU-COL-313 display targets (built ones resolve, named-but-unbuilt
/// ones are `view_not_built`, anything else `view_unknown_display`); HDR views: monotonic on a
/// neutral ramp, black 0, saturating at the peak; hostile input typed for every view and input.
#[test]
fn view_selection_refuses_unbuilt_targets_and_hdr_views_are_bounded() {
    assert_eq!(select_view("srgb_display", 100), Ok((V::Aces1SdrVideo, DisplayEncoding::Srgb)));
    assert_eq!(select_view("rec1886_rec709_display", 100), Ok((V::Aces1SdrVideo, DisplayEncoding::Bt1886)));
    assert_eq!(select_view("gamma22_rec709_display", 100), Ok((V::Aces1SdrVideo, DisplayEncoding::Gamma22)));
    assert_eq!(select_view("display_p3_display", 100), Ok((V::Aces1SdrVideoDisplayP3, DisplayEncoding::Srgb)));
    for (peak, view) in [(1000, V::Aces1HdrVideo1000Nits), (2000, V::Aces1HdrVideo2000Nits), (4000, V::Aces1HdrVideo4000Nits)] {
        assert_eq!(select_view("rec2100_pq_display", peak), Ok((view, DisplayEncoding::Pq)));
        assert_eq!(view.peak_nits(), peak);
        assert!(view.is_hdr());
    }
    for (display, peak) in [
        ("p3_d65_display", 100),
        ("rec2100_hlg_display", 1000),
        ("rec2100_pq_display", 500),
        ("rec2100_pq_display", 100),
        ("srgb_display", 1000),
    ] {
        assert_eq!(select_view(display, peak), Err(ViewSelectError::NotBuilt), "{display} {peak}");
    }
    for display in ["", "SRGB_DISPLAY", "srgb_display ", "aces1_sdr_video_100nits_dim", "acescg"] {
        assert_eq!(select_view(display, 100), Err(ViewSelectError::UnknownDisplay), "{display:?}");
    }
    assert_eq!(ViewSelectError::NotBuilt.code(), "view_not_built");

    // HDR views on a working-space neutral ramp: luminance (Rec.2020 Y) non-decreasing, every channel
    // in [0, peak]. Channels themselves are not monotonic near the peak: the CTL limits to Rec.2020
    // in D60-adapted XYZ (limit_to_primaries before the D60 -> D65 CAT), so the red channel of a
    // D60 neutral clips first and the CAT then mixes it (observed: R 9.764766 -> 9.764666 at 295
    // for 1000 nits); for the same reason a saturated neutral lands at about 97.7 % of the peak.
    let y2020 = npm(REC2020)[1];
    for view in [V::Aces1HdrVideo1000Nits, V::Aces1HdrVideo2000Nits, V::Aces1HdrVideo4000Nits] {
        let peak = view.peak_nits() as f32 / 100.0;
        assert_eq!(view.apply([0.0; 3]).unwrap(), [0.0; 3], "{}", view.key());
        let mut prev_y = 0.0f64;
        for i in 0..=1100 {
            let k = 10f32.powf(-6.0 + i as f32 / 100.0);
            let out = view.apply([k; 3]).unwrap();
            let y: f64 = (0..3).map(|c| y2020[c] * f64::from(out[c])).sum();
            assert!(y + 1e-6 >= prev_y, "{} luminance at {k}: {prev_y} -> {y}", view.key());
            assert!(out.iter().all(|v| (0.0..=peak).contains(v)), "{} at {k}: {out:?}", view.key());
            prev_y = y;
        }
        let top = view.apply([f32::MAX; 3]).unwrap();
        assert!(top.iter().all(|v| *v >= 0.95 * peak && *v <= peak), "{} f32::MAX {top:?}", view.key());
    }
    for view in V::ALL {
        for input in SceneInput::ALL {
            let vt = ViewTransform::new(*view, *input);
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(vt.apply([0.2, bad, 0.1]), Err(Error::NonfiniteChannel));
            }
            for px in [[-1e30, 5.0, 0.3], [f32::MAX, -f32::MAX, 0.0], [1e-45, 0.0, -0.0]] {
                let out = vt.apply(px).unwrap();
                let top = view.peak_nits() as f32 / 100.0;
                assert!(out.iter().all(|v| (0.0..=top).contains(v)), "{} {} {px:?} -> {out:?}", view.key(), input.key());
            }
            let mut odd = [0.1f32; 5];
            assert_eq!(vt.apply_in_place(&mut odd), Err(Error::ChannelRange));
        }
    }
    for enc in [DisplayEncoding::Gamma22, DisplayEncoding::Pq] {
        assert_eq!(enc.encode([f32::NAN, 0.0, 0.0]), Err(Error::NonfiniteChannel));
        // PQ of 0 cd/m2 is c1^m2 = 0.8359375^78.84375, about 7.3e-7 (ST 2084 has no exact zero).
        assert!(enc.encode([0.0; 3]).unwrap()[0] < 1e-6);
    }
    assert!((DisplayEncoding::Pq.encode([100.0, 1e9, -1.0]).unwrap()[0] - 1.0).abs() < 1e-6);
}
