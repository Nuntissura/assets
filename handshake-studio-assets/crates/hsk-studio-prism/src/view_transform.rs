//! Stage-6a SDR view transforms (lane SMB-PRISM-VIEW-TRANSFORM): scene-referred working-space RGB
//! to display-referred linear RGB, per pixel, deterministic and allocation-free.
//!
//! Spec basis (module 14): STU-COL-161 stage "6a. Display transform | ColorEngine | Working space ->
//! display"; STU-COL-313 a view transform "converts the scene-referred reference to a
//! display-referred reference, and the display colour space then converts that to the physical
//! display's own encoding", with an "SDR view at the 100 cd/m2 reference presentation" required for
//! `srgb_display` and `rec1886_rec709_display`; STU-COL-310 names ACES as the scene-linear default.
//! The spec takes the ACES views from an OCIO configuration; Prism implements the specific
//! transform here, not an OCIO runtime. The spec names no other SDR tone curve, so no other curve
//! (Reinhard, filmic, ...) is offered; the untone-mapped colorimetric view is pigment's
//! `DisplayTransform::Clip`.
//!
//! [`ViewTransformId::Aces1SdrVideo`] is the ACES 1.x Output Transform for SDR video at 100 nits,
//! dim surround: the Reference Rendering Transform (`rrt/RRT.ctl`) followed by the linear part of
//! `odt/sRGB/ODT.Academy.sRGB_100nits_dim.ctl` / `odt/rec709/ODT.Academy.Rec709_100nits_dim.ctl`
//! (the two ODTs are identical up to their final encoding), written clean-room from the Academy's
//! CTL reference at tag v1.3.1 of `github.com/ampas/aces-dev` (`transforms/ctl/rrt`, `odt`, `lib`:
//! `ACESlib.RRT_Common`, `ACESlib.ODT_Common`, `ACESlib.Tonescales`, `ACESlib.Transform_Common`,
//! `ACESlib.Utilities_Color`). The view outputs the ODT's clamped linear code values in Rec.709 /
//! sRGB primaries, `[0, 1]`, i.e. the display-referred reference; the display encoding (sRGB
//! piecewise curve or BT.1886 with L_W 1, L_B 0) is the separate second stage
//! [`DisplayEncoding`], so a sink that encodes linear values for its display keeps doing so.
//!
//! ACES licence: portions derived from the Academy Color Encoding System reference
//! implementation, Copyright (c) 2015 Academy of Motion Picture Arts and Sciences (A.M.P.A.S.),
//! used under the ACES licence terms reproduced with their conditions and Disclaimer of Warranty in
//! `crates/hsk-studio-prism/LICENSE-ACES.txt`.
//!
//! Numerics: binary64 throughout, IEEE basic operations and Prism's own table-driven `ln`/`exp`
//! (`icc_eval.rs`) plus a series `atan`, so the bits are the same on every host (no libm, no fused
//! multiply-add). Deviations from the CTL, all where the CTL would produce NaN or nothing differs:
//! the chroma term of `rgb_2_yc` is floored at 0 before its square root (rounding can make it a
//! tiny negative number for near-neutral colours), and CTL's binary32 evaluation becomes binary64.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math

use crate::Error;
use crate::icc_eval::{exp, ln};
use std::sync::OnceLock;

type M3 = [[f64; 3]; 3];

/// A Prism view transform (stage 6a). `non_exhaustive`: more views (HDR, other displays) can be
/// added without breaking callers.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewTransformId {
    /// ACES 1.x RRT + ODT, SDR video, 100 nits, dim surround, Rec.709 / sRGB primaries, from the
    /// Prism working space (linear-light sRGB, [`crate::WORKING_PROFILE_LINEAR_SRGB`]).
    Aces1SdrVideo,
}
impl ViewTransformId {
    /// Every view, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Aces1SdrVideo];
    /// Stable key (plan digests and receipts).
    pub const fn key(self) -> &'static str {
        match self {
            Self::Aces1SdrVideo => "aces1_sdr_video_100nits_dim",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.key() == key)
    }
    /// Working-space linear RGB -> display-referred linear RGB in `[0, 1]` (display primaries).
    /// Any finite channel, negative included, is admitted; NaN or an infinity is
    /// [`Error::NonfiniteChannel`]. Allocates nothing.
    pub fn apply(self, rgb: [f32; 3]) -> Result<[f32; 3], Error> {
        match self {
            Self::Aces1SdrVideo => {
                let x = finite(rgb)?;
                Ok(to_f32(aces1_sdr_video(mv(&tables().rec709_to_ap0, x))))
            }
        }
    }
    /// [`ViewTransformId::apply`] over interleaved RGB triples in place; a length that is not a
    /// multiple of 3 is [`Error::ChannelRange`]. On error, triples before the failing one are
    /// already transformed.
    pub fn apply_in_place(self, rgb: &mut [f32]) -> Result<(), Error> {
        if !rgb.len().is_multiple_of(3) {
            return Err(Error::ChannelRange);
        }
        for px in rgb.chunks_exact_mut(3) {
            let out = self.apply([px[0], px[1], px[2]])?;
            px.copy_from_slice(&out);
        }
        Ok(())
    }
}

/// [`ViewTransformId::Aces1SdrVideo`] with ACES2065-1 (AP0) input instead of the working space:
/// exactly RRT + ODT Rec.709/sRGB 100 nits dim up to the clamped linear code values.
pub fn aces1_sdr_video_from_aces2065(ap0: [f32; 3]) -> Result<[f32; 3], Error> {
    Ok(to_f32(aces1_sdr_video(finite(ap0)?)))
}

/// Second stage of STU-COL-313: display-referred linear `[0, 1]` -> display code values.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayEncoding {
    /// IEC 61966-2-1 piecewise curve (CTL `moncurve_r` gamma 2.4, offset 0.055).
    Srgb,
    /// ITU-R BT.1886 inverse EOTF with L_W 1, L_B 0 (CTL `bt1886_r`): `L^(1/2.4)`.
    Bt1886,
}
impl DisplayEncoding {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Srgb => "srgb_piecewise",
            Self::Bt1886 => "bt1886_gamma24",
        }
    }
    /// Encode linear values; inputs are clamped into `[0, 1]` first (the ODT's own clamp).
    pub fn encode(self, linear: [f32; 3]) -> Result<[f32; 3], Error> {
        let x = finite(linear)?;
        let f = |v: f64| {
            let v = v.clamp(0.0, 1.0);
            match self {
                Self::Srgb => moncurve_r(v, 2.4, 0.055),
                Self::Bt1886 => pow(v, 1.0 / 2.4),
            }
        };
        Ok(to_f32([f(x[0]), f(x[1]), f(x[2])]))
    }
}

/// Machine-readable description of the view transforms (manual / model surface).
pub const VIEW_TRANSFORM_DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PRISM","path":"view_transform","lane":"SMB-PRISM-VIEW-TRANSFORM","stage":"STU-COL-161 6a display transform","api":"ViewTransformId::apply([f32;3]) -> Result<[f32;3], Error>, apply_in_place(&mut [f32]); aces1_sdr_video_from_aces2065; DisplayEncoding::encode","views":[{"key":"aces1_sdr_video_100nits_dim","input":"Prism working space, linear-light sRGB (Rec.709 primaries, D65), converted to ACES2065-1 with a Bradford adaptation D65 -> ACES white (CTL calculate_rgb_to_rgb_matrix default)","transform":"ACES 1.x RRT + ODT Rec709/sRGB 100 nits dim, aces-dev v1.3.1 CTL","output":"display-referred linear RGB, Rec.709 primaries, clamped to [0,1] (ODT linearCV)","proven_by":"tests/view_transform.rs"}],"encodings":["srgb_piecewise","bt1886_gamma24"],"hostile":"NaN or infinite channel -> nonfinite_channel; negative channels admitted (RRT clamps them)","determinism":"binary64, IEEE basic operations and Prism's own ln/exp/atan, no libm, no FMA; allocation-free","not_in_scope":["OCIO configuration runtime","HDR views","Reinhard or other curves the spec does not name"],"licence":"ACES (A.M.P.A.S.) permissive terms, LICENSE-ACES.txt"}"#;

// ------------------------------------------------------------------ ACES 1.x SDR video pipeline

/// RRT + ODT (100 nits dim, Rec.709 primaries) on finite ACES2065-1 input; output linear code
/// values in `[0, 1]`.
fn aces1_sdr_video(aces_in: [f64; 3]) -> [f64; 3] {
    let t = tables();
    // ---- RRT (rrt/RRT.ctl) ----
    let mut aces = aces_in;
    // Glow module.
    let saturation = rgb_2_saturation(aces);
    let yc_in = rgb_2_yc(aces);
    let s = sigmoid_shaper((saturation - 0.4) / 0.2);
    let added_glow = 1.0 + glow_fwd(yc_in, RRT_GLOW_GAIN * s, RRT_GLOW_MID);
    aces = aces.map(|v| added_glow * v);
    // Red modifier.
    let hue = rgb_2_hue(aces);
    let centered_hue = center_hue(hue, RRT_RED_HUE);
    let hue_weight = cubic_basis_shaper(centered_hue, RRT_RED_WIDTH);
    aces[0] += hue_weight * saturation * (RRT_RED_PIVOT - aces[0]) * (1.0 - RRT_RED_SCALE);
    // ACES to RGB rendering space.
    aces = aces.map(|v| v.max(0.0));
    let rgb_pre = mv(&t.ap0_to_ap1, aces).map(|v| v.clamp(0.0, HALF_MAX));
    // Global desaturation.
    let rgb_pre = mv(&t.rrt_sat, rgb_pre);
    // Tonescale per channel.
    let rgb_post = rgb_pre.map(|v| spline(v, &t.c5));
    // RGB rendering space to OCES.
    let oces = mv(&t.ap1_to_ap0, rgb_post);

    // ---- ODT (odt/rec709 and odt/sRGB ODT.Academy.*_100nits_dim.ctl, linear part) ----
    let rgb_pre = mv(&t.ap0_to_ap1, oces);
    let rgb_post = rgb_pre.map(|v| spline(v, &t.c9));
    // Scale luminance to linear code value.
    let lin = rgb_post.map(|y| (y - t.cinema_black) / (CINEMA_WHITE - t.cinema_black));
    // Dark to dim surround.
    let lin = dark_to_dim(lin);
    // Desaturation for the luminance difference.
    let lin = mv(&t.odt_sat, lin);
    // Rendering space -> XYZ -> D60 to D65 CAT -> display primaries.
    let xyz = mv(&t.d60_to_d65, mv(&AP1_TO_XYZ, lin));
    mv(&XYZ_TO_REC709, xyz).map(|v| v.clamp(0.0, 1.0))
}

// RRT parameters (ACESlib.RRT_Common.ctl).
const RRT_GLOW_GAIN: f64 = 0.05;
const RRT_GLOW_MID: f64 = 0.08;
const RRT_RED_SCALE: f64 = 0.82;
const RRT_RED_PIVOT: f64 = 0.03;
const RRT_RED_HUE: f64 = 0.0;
const RRT_RED_WIDTH: f64 = 135.0;
const RRT_SAT_FACTOR: f64 = 0.96;
// ODT parameters (ACESlib.ODT_Common.ctl).
const CINEMA_WHITE: f64 = 48.0;
const DIM_SURROUND_GAMMA: f64 = 0.9811;
const ODT_SAT_FACTOR: f64 = 0.93;
// CTL constants (ACESlib.Transform_Common.ctl, CTL half limits).
const TINY: f64 = 1e-10;
const HALF_MAX: f64 = 65504.0;
const HALF_MIN: f64 = 5.960_464_477_539_063e-8;

// Chromaticities (ACESlib.Utilities_Color.ctl): red, green, blue, white.
const AP0: [[f64; 2]; 4] = [[0.7347, 0.2653], [0.0, 1.0], [0.0001, -0.077], [0.32168, 0.33767]];
const AP1: [[f64; 2]; 4] = [[0.713, 0.293], [0.165, 0.830], [0.128, 0.044], [0.32168, 0.33767]];
const REC709: [[f64; 2]; 4] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], [0.3127, 0.3290]];
/// Bradford cone response (CTL `CONE_RESP_MAT_BRADFORD`, which stores it transposed for row
/// vectors; here column-vector form).
const BRADFORD: M3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

const AP0_TO_XYZ: M3 = npm(AP0);
const XYZ_TO_AP0: M3 = inv(AP0_TO_XYZ);
const AP1_TO_XYZ: M3 = npm(AP1);
const XYZ_TO_AP1: M3 = inv(AP1_TO_XYZ);
const REC709_TO_XYZ: M3 = npm(REC709);
const XYZ_TO_REC709: M3 = inv(REC709_TO_XYZ);

struct Spline {
    coefs_low: &'static [f64],
    coefs_high: &'static [f64],
    knots_low: f64,
    knots_high: f64,
    log_min_x: f64,
    log_mid_x: f64,
    log_max_x: f64,
    log_min_y: f64,
    log_max_y: f64,
    slope_low: f64,
    slope_high: f64,
}

struct Tables {
    rec709_to_ap0: M3,
    ap0_to_ap1: M3,
    ap1_to_ap0: M3,
    rrt_sat: M3,
    odt_sat: M3,
    d60_to_d65: M3,
    cinema_black: f64,
    c5: Spline,
    c9: Spline,
}

// ACESlib.Tonescales.ctl RRT_PARAMS (c5) and ODT_48nits (c9).
const C5_LOW: [f64; 6] = [-4.0, -4.0, -3.157_376_577_3, -0.485_249_995_8, 1.847_732_470_6, 1.847_732_470_6];
const C5_HIGH: [f64; 6] = [-0.718_548_242_5, 2.081_030_717_2, 3.668_124_123_7, 4.0, 4.0, 4.0];
const C9_LOW: [f64; 10] = [
    -1.698_970_004_3,
    -1.698_970_004_3,
    -1.4779,
    -1.2291,
    -0.8648,
    -0.4480,
    0.00518,
    0.451_108_033_4,
    0.911_374_441_4,
    0.911_374_441_4,
];
const C9_HIGH: [f64; 10] = [
    0.515_438_696_5,
    0.847_043_778_3,
    1.1358,
    1.3802,
    1.5197,
    1.5985,
    1.6467,
    1.674_609_135_7,
    1.687_873_339,
    1.687_873_339,
];

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let ap0_to_ap1 = mul(XYZ_TO_AP1, AP0_TO_XYZ);
        let ap1_y = AP1_TO_XYZ[1];
        let c5 = Spline {
            coefs_low: &C5_LOW,
            coefs_high: &C5_HIGH,
            knots_low: 4.0,
            knots_high: 4.0,
            log_min_x: log10(0.18 * pow(2.0, -15.0)),
            log_mid_x: log10(0.18),
            log_max_x: log10(0.18 * pow(2.0, 18.0)),
            log_min_y: log10(0.0001),
            log_max_y: log10(10000.0),
            slope_low: 0.0,
            slope_high: 0.0,
        };
        let c9 = Spline {
            coefs_low: &C9_LOW,
            coefs_high: &C9_HIGH,
            knots_low: 8.0,
            knots_high: 8.0,
            log_min_x: log10(spline(0.18 * pow(2.0, -6.5), &c5)),
            log_mid_x: log10(spline(0.18, &c5)),
            log_max_x: log10(spline(0.18 * pow(2.0, 6.5), &c5)),
            log_min_y: log10(0.02),
            log_max_y: log10(48.0),
            slope_low: 0.0,
            slope_high: 0.04,
        };
        Tables {
            rec709_to_ap0: mul(XYZ_TO_AP0, mul(cat(REC709[3], AP0[3]), REC709_TO_XYZ)),
            ap0_to_ap1,
            ap1_to_ap0: mul(XYZ_TO_AP0, AP1_TO_XYZ),
            rrt_sat: sat_matrix(RRT_SAT_FACTOR, ap1_y),
            odt_sat: sat_matrix(ODT_SAT_FACTOR, ap1_y),
            d60_to_d65: cat(AP0[3], REC709[3]),
            // CTL: pow10(log10(0.02)).
            cinema_black: pow10(log10(0.02)),
            c5,
            c9,
        }
    })
}

/// `segmented_spline_c5_fwd` / `segmented_spline_c9_fwd`: quadratic B-spline in log-log space with
/// linear extensions.
fn spline(x: f64, c: &Spline) -> f64 {
    let logx = log10(x.max(HALF_MIN));
    let logy = if logx <= c.log_min_x {
        logx * c.slope_low + (c.log_min_y - c.slope_low * c.log_min_x)
    } else if logx < c.log_mid_x {
        let kc = (c.knots_low - 1.0) * (logx - c.log_min_x) / (c.log_mid_x - c.log_min_x);
        segment(c.coefs_low, kc)
    } else if logx < c.log_max_x {
        let kc = (c.knots_high - 1.0) * (logx - c.log_mid_x) / (c.log_max_x - c.log_mid_x);
        segment(c.coefs_high, kc)
    } else {
        logx * c.slope_high + (c.log_max_y - c.slope_high * c.log_max_x)
    };
    pow10(logy)
}

/// One B-spline segment: `[t^2, t, 1] . (cf * M)` with M = [[.5,-1,.5],[-1,1,.5],[.5,0,0]].
fn segment(coefs: &[f64], knot_coord: f64) -> f64 {
    let jf = knot_coord.trunc();
    let t = knot_coord - jf;
    // knot_coord is in [0, knots - 1) here, so j + 2 < coefs.len(); `get` keeps it panic-free.
    let j = jf as usize;
    let cf = |k: usize| coefs.get(j.saturating_add(k)).copied().unwrap_or(0.0);
    let (c0, c1, c2) = (cf(0), cf(1), cf(2));
    t * t * (0.5 * c0 - c1 + 0.5 * c2) + t * (c1 - c0) + (0.5 * c0 + 0.5 * c1)
}

fn rgb_2_saturation(rgb: [f64; 3]) -> f64 {
    let mx = rgb[0].max(rgb[1]).max(rgb[2]);
    let mn = rgb[0].min(rgb[1]).min(rgb[2]);
    (mx.max(TINY) - mn.max(TINY)) / mx.max(1e-2)
}

fn rgb_2_yc(rgb: [f64; 3]) -> f64 {
    let [r, g, b] = rgb;
    let chroma = (b * (b - g) + g * (g - r) + r * (r - b)).max(0.0).sqrt();
    (b + g + r + 1.75 * chroma) / 3.0
}

/// Degrees in `[0, 360)`; NaN for a neutral triple (CTL `rgb_2_hue`).
fn rgb_2_hue(rgb: [f64; 3]) -> f64 {
    if rgb[0] == rgb[1] && rgb[1] == rgb[2] {
        return f64::NAN;
    }
    let h = (180.0 / std::f64::consts::PI)
        * atan2(SQRT3 * (rgb[1] - rgb[2]), 2.0 * rgb[0] - rgb[1] - rgb[2]);
    if h < 0.0 { h + 360.0 } else { h }
}

fn center_hue(hue: f64, center: f64) -> f64 {
    let h = hue - center;
    if h < -180.0 {
        h + 360.0
    } else if h > 180.0 {
        h - 360.0
    } else {
        h
    }
}

fn sigmoid_shaper(x: f64) -> f64 {
    let t = (1.0 - (x / 2.0).abs()).max(0.0);
    let sign = if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    };
    (1.0 + sign * (1.0 - t * t)) / 2.0
}

fn glow_fwd(yc: f64, gain: f64, mid: f64) -> f64 {
    if yc <= 2.0 / 3.0 * mid {
        gain
    } else if yc >= 2.0 * mid {
        0.0
    } else {
        gain * (mid / yc - 0.5)
    }
}

/// Uniform cubic B-spline bump of full width `w` degrees, centred on 0, peak 1 (CTL
/// `cubic_basis_shaper`); NaN input gives 0.
fn cubic_basis_shaper(x: f64, w: f64) -> f64 {
    let k0 = -w / 2.0;
    let k4 = w / 2.0;
    if !(x > k0 && x < k4) {
        return 0.0;
    }
    let kc = (x - k0) * 4.0 / w;
    let jf = kc.trunc();
    let t = kc - jf;
    let (t2, t3) = (t * t, t * t * t);
    let y = match jf as i32 {
        3 => -t3 / 6.0 + t2 / 2.0 - t / 2.0 + 1.0 / 6.0,
        2 => t3 / 2.0 - t2 + 2.0 / 3.0,
        1 => -t3 / 2.0 + t2 / 2.0 + t / 2.0 + 1.0 / 6.0,
        0 => t3 / 6.0,
        _ => 0.0,
    };
    y * 1.5
}

/// `darkSurround_to_dimSurround` (ACESlib.ODT_Common.ctl).
fn dark_to_dim(lin: [f64; 3]) -> [f64; 3] {
    let xyz = mv(&AP1_TO_XYZ, lin);
    let mut divisor = xyz[0] + xyz[1] + xyz[2];
    if divisor == 0.0 {
        divisor = 1e-10;
    }
    let (x, y) = (xyz[0] / divisor, xyz[1] / divisor);
    let big_y = pow(xyz[1].max(0.0), DIM_SURROUND_GAMMA);
    let yd = y.max(1e-10);
    let xyz = [x * big_y / yd, big_y, (1.0 - x - y) * big_y / yd];
    mv(&XYZ_TO_AP1, xyz)
}

/// CTL `moncurve_r` (linear -> encoded).
fn moncurve_r(y: f64, gamma: f64, offs: f64) -> f64 {
    let yb = pow(offs * gamma / ((gamma - 1.0) * (1.0 + offs)), gamma);
    let rs = pow((gamma - 1.0) / offs, gamma - 1.0) * pow((1.0 + offs) / gamma, gamma);
    if y >= yb {
        (1.0 + offs) * pow(y, 1.0 / gamma) - offs
    } else {
        y * rs
    }
}

// ------------------------------------------------------------------ deterministic scalar helpers

const SQRT3: f64 = 1.732_050_807_568_877_2;
const LN10: f64 = std::f64::consts::LN_10;

fn pow(x: f64, y: f64) -> f64 {
    crate::icc_eval::pow(x, y)
}
fn log10(x: f64) -> f64 {
    ln(x) / LN10
}
fn pow10(y: f64) -> f64 {
    exp(y * LN10)
}

/// atan for |x| <= 1: two half-angle reductions (|x| <= tan(pi/16) < 0.2) then the Taylor series
/// (24 terms, truncation below 1e-35); IEEE basic operations and `sqrt` only.
fn atan_unit(x: f64) -> f64 {
    let r1 = x / (1.0 + (1.0 + x * x).sqrt());
    let r2 = r1 / (1.0 + (1.0 + r1 * r1).sqrt());
    let z2 = r2 * r2;
    let mut p = 0.0;
    let mut k: u32 = 24;
    while k > 0 {
        k = k.saturating_sub(1);
        let odd = f64::from(k.saturating_mul(2).saturating_add(1));
        let term = 1.0 / odd;
        p = if k & 1 == 0 { term } else { -term } + z2 * p;
    }
    4.0 * r2 * p
}

fn atan(x: f64) -> f64 {
    let a = x.abs();
    let r = if a <= 1.0 {
        atan_unit(a)
    } else {
        std::f64::consts::FRAC_PI_2 - atan_unit(1.0 / a)
    };
    if x < 0.0 { -r } else { r }
}

fn atan2(y: f64, x: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, PI};
    if x > 0.0 {
        atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 { atan(y / x) + PI } else { atan(y / x) - PI }
    } else if y > 0.0 {
        FRAC_PI_2
    } else if y < 0.0 {
        -FRAC_PI_2
    } else {
        0.0
    }
}

fn finite(rgb: [f32; 3]) -> Result<[f64; 3], Error> {
    if rgb.iter().all(|v| v.is_finite()) {
        Ok(rgb.map(f64::from))
    } else {
        Err(Error::NonfiniteChannel)
    }
}

fn to_f32(v: [f64; 3]) -> [f32; 3] {
    v.map(|c| c as f32)
}

fn mv(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

const fn mul(a: M3, b: M3) -> M3 {
    const fn d(a: &M3, b: &M3, i: usize, j: usize) -> f64 {
        a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]
    }
    [
        [d(&a, &b, 0, 0), d(&a, &b, 0, 1), d(&a, &b, 0, 2)],
        [d(&a, &b, 1, 0), d(&a, &b, 1, 1), d(&a, &b, 1, 2)],
        [d(&a, &b, 2, 0), d(&a, &b, 2, 1), d(&a, &b, 2, 2)],
    ]
}

/// Inverse by cofactors; every matrix inverted here is a non-singular primaries matrix.
const fn inv(m: M3) -> M3 {
    let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
    let c01 = m[1][2] * m[2][0] - m[1][0] * m[2][2];
    let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
    let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
    [
        [c00 / det, (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det, (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det],
        [c01 / det, (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det, (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det],
        [c02 / det, (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det, (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det],
    ]
}

const fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

/// Normalised primaries matrix (CTL `RGBtoXYZ(primaries, 1.0)`, column-vector form).
const fn npm(p: [[f64; 2]; 4]) -> M3 {
    let r = xy_to_xyz(p[0]);
    let g = xy_to_xyz(p[1]);
    let b = xy_to_xyz(p[2]);
    let w = xy_to_xyz(p[3]);
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let mi = inv(m);
    let s = [
        mi[0][0] * w[0] + mi[0][1] * w[1] + mi[0][2] * w[2],
        mi[1][0] * w[0] + mi[1][1] * w[1] + mi[1][2] * w[2],
        mi[2][0] * w[0] + mi[2][1] * w[1] + mi[2][2] * w[2],
    ];
    [
        [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
        [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
        [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
    ]
}

/// Bradford adaptation between two white chromaticities (CTL `calculate_cat_matrix`).
const fn cat(src: [f64; 2], dst: [f64; 2]) -> M3 {
    let s = xy_to_xyz(src);
    let d = xy_to_xyz(dst);
    let b = BRADFORD;
    let sc = [
        b[0][0] * s[0] + b[0][1] * s[1] + b[0][2] * s[2],
        b[1][0] * s[0] + b[1][1] * s[1] + b[1][2] * s[2],
        b[2][0] * s[0] + b[2][1] * s[1] + b[2][2] * s[2],
    ];
    let dc = [
        b[0][0] * d[0] + b[0][1] * d[1] + b[0][2] * d[2],
        b[1][0] * d[0] + b[1][1] * d[1] + b[1][2] * d[2],
        b[2][0] * d[0] + b[2][1] * d[1] + b[2][2] * d[2],
    ];
    let vk = [[dc[0] / sc[0], 0.0, 0.0], [0.0, dc[1] / sc[1], 0.0], [0.0, 0.0, dc[2] / sc[2]]];
    mul(inv(b), mul(vk, b))
}

/// `calc_sat_adjust_matrix` in column-vector form: out = sat * rgb + (1 - sat) * Y(rgb).
fn sat_matrix(sat: f64, y: [f64; 3]) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (1.0 - sat) * y[j] + if i == j { sat } else { 0.0 };
        }
    }
    m
}
