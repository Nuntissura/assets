//! R44 finding F-R44-1 (lane SMB-R44-FOLLOWUPS-1): `DisplayEncoding::decode`, the exact inverse
//! of `DisplayEncoding::encode`. Oracles: the round trip `decode(encode(x)) == x` within binary32
//! rounding over a grid that crosses the sRGB moncurve linear segment (below `yb ~ 0.00304`) and
//! its breakpoint; the closed-form segment constants of CTL `moncurve_r` (gamma 2.4, offset 0.055)
//! computed here in f64; typed refusals for PQ (no ST 2084 inverse carried) and non-finite input.
use hsk_studio_prism::{DisplayEncoding, Error};

/// CTL `moncurve_r` constants for gamma 2.4, offset 0.055: linear breakpoint `yb` and slope `rs`.
fn moncurve_segment() -> (f64, f64) {
    let (g, o) = (2.4f64, 0.055f64);
    let yb = (o * g / ((g - 1.0) * (1.0 + o))).powf(g);
    let rs = ((g - 1.0) / o).powf(g - 1.0) * ((1.0 + o) / g).powf(g);
    (yb, rs)
}

/// The grid: dense around the moncurve breakpoint and in the linear segment, then over (0, 1].
fn grid() -> Vec<f32> {
    let (yb, _) = moncurve_segment();
    let mut v: Vec<f32> = vec![0.0, 1.0e-6, 1.0e-5, 1.0e-4, 0.001, 0.002, 0.003, 0.0035, 0.004, 0.04045 / 12.92, 0.5, 0.999, 1.0];
    for k in -20..=20 {
        v.push((yb * (1.0 + f64::from(k) * 0.005)) as f32);
    }
    for k in 1..=200 {
        v.push(k as f32 / 200.0);
    }
    v
}

fn close(x: f32, back: f32) -> bool {
    // binary32 encode output then binary32 decode output: a few ulps relative, absolute floor at 0.
    (f64::from(back) - f64::from(x)).abs() <= 1.0e-6 * f64::from(x).abs() + 1.0e-12
}

#[test]
fn decode_inverts_encode_across_the_srgb_breakpoint_and_the_power_curves() {
    let (yb, _) = moncurve_segment();
    assert!((0.0030..0.0031).contains(&yb), "yb {yb}");
    for e in [DisplayEncoding::Srgb, DisplayEncoding::Bt1886, DisplayEncoding::Gamma22] {
        let mut below = 0;
        for x in grid() {
            let enc = e.encode([x, x, x]).unwrap();
            let back = e.decode(enc).unwrap();
            for c in back {
                assert!(close(x, c), "{} x {x:e} -> {enc:?} -> {c:e}", e.key());
            }
            below += usize::from(f64::from(x) < yb && x > 0.0);
        }
        assert!(below >= 10, "the grid crosses the linear segment");
    }
}

#[test]
fn srgb_decode_is_moncurve_f_not_the_iec_constants() {
    let (yb, rs) = moncurve_segment();
    // Linear segment: decode(v) = v / rs, so decode(encode(0.002)) is 0.002 (binary32 rounding) where the IEC
    // decode (v / 12.92 below 0.04045) would return 0.002 * rs / 12.92 (about 1.00023 x).
    let v = (0.002 * rs) as f32;
    let d = DisplayEncoding::Srgb.decode([v; 3]).unwrap()[0];
    assert!((f64::from(d) / (f64::from(v) / rs) - 1.0).abs() <= 1e-6, "{d:e}");
    let iec = f64::from(v) / 12.92;
    assert!((iec / f64::from(d) - 1.0).abs() > 1.0e-4, "the IEC curve differs measurably: {iec:e} vs {d:e}");
    // Both segments meet at the breakpoint code value offs / (gamma - 1).
    let xb = 0.055 / 1.4;
    let lo = DisplayEncoding::Srgb.decode([(xb - 1e-7) as f32; 3]).unwrap()[0];
    let hi = DisplayEncoding::Srgb.decode([(xb + 1e-7) as f32; 3]).unwrap()[0];
    assert!((f64::from(hi) - f64::from(lo)).abs() < 1e-6 && (f64::from(lo) - yb).abs() < 1e-6, "{lo:e} {hi:e} {yb:e}");
    // The encoding clamps into [0, 1]; decode clamps its input there too.
    assert_eq!(DisplayEncoding::Srgb.decode([1.5, -0.25, 1.0]).unwrap(), [1.0, 0.0, 1.0]);
    assert_eq!(DisplayEncoding::Srgb.decode(DisplayEncoding::Srgb.encode([1.5, 0.0, 1.0]).unwrap()).unwrap(), [1.0, 0.0, 1.0]);
}

#[test]
fn decode_refuses_pq_and_nonfinite_typed() {
    assert_eq!(DisplayEncoding::Pq.decode([0.5; 3]), Err(Error::DecodeUnsupported));
    assert_eq!(Error::DecodeUnsupported.code(), "decode_unsupported");
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for e in [DisplayEncoding::Srgb, DisplayEncoding::Bt1886, DisplayEncoding::Gamma22, DisplayEncoding::Pq] {
            assert_eq!(e.decode([0.5, bad, 0.5]), Err(Error::NonfiniteChannel), "{}", e.key());
        }
    }
}
