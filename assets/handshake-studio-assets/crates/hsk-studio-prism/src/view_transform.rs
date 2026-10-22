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
//! Lane SMB-PRISM-VIEW-TRANSFORM-2 adds the scene inputs [`SceneInput`] (ACEScg through the
//! published ACEScg matrix), the Display P3 SDR view (`odt/displayp3/ODT.Academy.DisplayP3_dim.ctl`),
//! the PQ HDR views (`outputTransform/rec2020/RRTODT.Academy.Rec2020_{1000,2000,4000}nits_15nits_ST2084.ctl`
//! over `ACESlib.OutputTransforms` and `ACESlib.SSTS`), [`select_view`] for the STU-COL-313 display
//! targets with a typed refusal for the ones not built, and the binary32 WGSL form of the SDR views
//! ([`VIEW_WGSL`], [`view_gpu_params`]) that render-gpu's shared-texture output runs.
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

/// Scene-referred input of a view (STU-COL-311, STU-COL-312).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneInput {
    /// The Prism working space: linear-light sRGB (Rec.709 primaries, D65 white,
    /// [`crate::WORKING_PROFILE_LINEAR_SRGB`]); to ACES2065-1 through a Bradford adaptation D65 ->
    /// ACES white (CTL `calculate_rgb_to_rgb_matrix` default).
    LinearSrgb,
    /// ACEScg (AP1 primaries, ACES white, linear; the STU-COL-312 `scene_linear` binding); to
    /// ACES2065-1 through the published matrix [`ACESCG_TO_ACES2065_1`].
    AcesCg,
    /// ACES2065-1 (AP0 primaries, linear; the STU-COL-316 interchange anchor); no conversion.
    Aces2065_1,
}
impl SceneInput {
    pub const ALL: &'static [Self] = &[Self::LinearSrgb, Self::AcesCg, Self::Aces2065_1];
    pub const fn key(self) -> &'static str {
        match self {
            Self::LinearSrgb => "linear_srgb",
            Self::AcesCg => "acescg",
            Self::Aces2065_1 => "aces2065_1",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.key() == key)
    }
    fn to_ap0_matrix(self) -> M3 {
        match self {
            Self::LinearSrgb => tables().rec709_to_ap0,
            Self::AcesCg => ACESCG_TO_ACES2065_1,
            Self::Aces2065_1 => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
    fn to_ap0(self, x: [f64; 3]) -> [f64; 3] {
        match self {
            Self::Aces2065_1 => x,
            _ => mv(&self.to_ap0_matrix(), x),
        }
    }
}

/// ACEScg -> ACES2065-1 as published in the ACEScg specification (Academy S-2014-004; aces-docs
/// `docs/encodings/acescg.md`, matrix TRA_2, SMPTE RP 177 method), row-major for column vectors.
pub const ACESCG_TO_ACES2065_1: [[f64; 3]; 3] = [
    [0.695_452_241_4, 0.140_678_696_5, 0.163_869_062_2],
    [0.044_794_563_4, 0.859_671_118_5, 0.095_534_318_2],
    [-0.005_525_882_6, 0.004_025_210_3, 1.001_500_672_3],
];
/// ACES2065-1 -> ACEScg as published in the ACEScg specification (matrix TRA_1).
pub const ACES2065_1_TO_ACESCG: [[f64; 3]; 3] = [
    [1.451_439_316_1, -0.236_510_746_9, -0.214_928_569_3],
    [-0.076_553_773_4, 1.176_229_699_8, -0.099_675_926_4],
    [0.008_316_148_4, -0.006_032_449_8, 0.997_716_301_4],
];
/// ACEScg (AP1, ACES white) -> Prism working space (linear sRGB, D65): AP1 -> XYZ, Bradford ACES
/// white -> D65, XYZ -> Rec.709 (primaries from ACESlib.Utilities_Color.ctl, SMPTE RP 177 method).
pub const ACESCG_TO_WORKING: [[f64; 3]; 3] = mul(XYZ_TO_REC709, mul(cat(AP1[3], REC709[3]), AP1_TO_XYZ));
/// Prism working space -> ACEScg (the inverse construction of [`ACESCG_TO_WORKING`]).
pub const WORKING_TO_ACESCG: [[f64; 3]; 3] = mul(XYZ_TO_AP1, mul(cat(REC709[3], AP1[3]), REC709_TO_XYZ));

/// A Prism view transform (stage 6a). `non_exhaustive`: more views can be added without breaking
/// callers. SDR views output display-referred linear RGB in `[0, 1]` (the ODT's clamped linear code
/// values); HDR views output display-referred linear RGB in units of 100 cd/m2 (1.0 = 100 nits,
/// `[0, peak / 100]`), the stretched-black linear values of the CTL before its PQ encoding.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewTransformId {
    /// ACES 1.x RRT + ODT, SDR video, 100 nits, dim surround, Rec.709 / sRGB primaries
    /// (ODT.Academy.sRGB_100nits_dim / Rec709_100nits_dim, linear part).
    Aces1SdrVideo,
    /// ACES 1.x RRT + ODT.Academy.DisplayP3_dim: SDR video, 100 nits, dim surround, P3 primaries
    /// on D65 (linear part).
    Aces1SdrVideoDisplayP3,
    /// ACES 1.x RRTODT.Academy.Rec2020_1000nits_15nits_ST2084: SSTS tone scale Y_MIN 0.0001,
    /// Y_MID 15, Y_MAX 1000 nits, dark surround, Rec.2020 limiting and encoding primaries,
    /// stretched black (linear part, before ST 2084).
    Aces1HdrVideo1000Nits,
    /// As [`ViewTransformId::Aces1HdrVideo1000Nits`] with Y_MAX 2000 nits
    /// (RRTODT.Academy.Rec2020_2000nits_15nits_ST2084).
    Aces1HdrVideo2000Nits,
    /// As [`ViewTransformId::Aces1HdrVideo1000Nits`] with Y_MAX 4000 nits
    /// (RRTODT.Academy.Rec2020_4000nits_15nits_ST2084).
    Aces1HdrVideo4000Nits,
}
impl ViewTransformId {
    /// Every view, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Aces1SdrVideo,
        Self::Aces1SdrVideoDisplayP3,
        Self::Aces1HdrVideo1000Nits,
        Self::Aces1HdrVideo2000Nits,
        Self::Aces1HdrVideo4000Nits,
    ];
    /// Stable key (plan digests and receipts).
    pub const fn key(self) -> &'static str {
        match self {
            Self::Aces1SdrVideo => "aces1_sdr_video_100nits_dim",
            Self::Aces1SdrVideoDisplayP3 => "aces1_sdr_video_100nits_dim_display_p3",
            Self::Aces1HdrVideo1000Nits => "aces1_hdr_video_1000nits_15nits_rec2020",
            Self::Aces1HdrVideo2000Nits => "aces1_hdr_video_2000nits_15nits_rec2020",
            Self::Aces1HdrVideo4000Nits => "aces1_hdr_video_4000nits_15nits_rec2020",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.key() == key)
    }
    /// Peak luminance of the view in cd/m2 (100 for the SDR views).
    pub const fn peak_nits(self) -> u32 {
        match self {
            Self::Aces1SdrVideo | Self::Aces1SdrVideoDisplayP3 => 100,
            Self::Aces1HdrVideo1000Nits => 1000,
            Self::Aces1HdrVideo2000Nits => 2000,
            Self::Aces1HdrVideo4000Nits => 4000,
        }
    }
    pub const fn is_hdr(self) -> bool {
        self.peak_nits() > 100
    }
    /// Primaries of the output values: `rec709`, `p3_d65` or `rec2020` (all D65 white).
    pub const fn output_primaries(self) -> &'static str {
        match self {
            Self::Aces1SdrVideo => "rec709",
            Self::Aces1SdrVideoDisplayP3 => "p3_d65",
            Self::Aces1HdrVideo1000Nits | Self::Aces1HdrVideo2000Nits | Self::Aces1HdrVideo4000Nits => {
                "rec2020"
            }
        }
    }
    /// Working-space linear RGB -> display-referred linear RGB (see the type docs for the range).
    /// Any finite channel, negative included, is admitted; NaN or an infinity is
    /// [`Error::NonfiniteChannel`]. Allocates nothing.
    pub fn apply(self, rgb: [f32; 3]) -> Result<[f32; 3], Error> {
        self.apply_from(SceneInput::LinearSrgb, rgb)
    }
    /// [`ViewTransformId::apply`] from the given scene input.
    pub fn apply_from(self, input: SceneInput, rgb: [f32; 3]) -> Result<[f32; 3], Error> {
        let ap0 = input.to_ap0(finite(rgb)?);
        Ok(to_f32(self.eval_ap0(ap0)))
    }
    fn eval_ap0(self, ap0: [f64; 3]) -> [f64; 3] {
        let t = tables();
        match self {
            Self::Aces1SdrVideo => aces1_sdr_video(ap0, &t.xyz_d60_to_rec709),
            Self::Aces1SdrVideoDisplayP3 => aces1_sdr_video(ap0, &t.xyz_d60_to_p3d65),
            Self::Aces1HdrVideo1000Nits => aces1_hdr_video(ap0, &t.ssts1000, 1000.0),
            Self::Aces1HdrVideo2000Nits => aces1_hdr_video(ap0, &t.ssts2000, 2000.0),
            Self::Aces1HdrVideo4000Nits => aces1_hdr_video(ap0, &t.ssts4000, 4000.0),
        }
    }
    /// [`ViewTransformId::apply`] over interleaved RGB triples in place; a length that is not a
    /// multiple of 3 is [`Error::ChannelRange`]. On error, triples before the failing one are
    /// already transformed.
    pub fn apply_in_place(self, rgb: &mut [f32]) -> Result<(), Error> {
        ViewTransform::new(self, SceneInput::LinearSrgb).apply_in_place(rgb)
    }
}

/// A view together with its scene input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewTransform {
    pub view: ViewTransformId,
    pub input: SceneInput,
}
impl ViewTransform {
    pub const fn new(view: ViewTransformId, input: SceneInput) -> Self {
        Self { view, input }
    }
    pub fn apply(self, rgb: [f32; 3]) -> Result<[f32; 3], Error> {
        self.view.apply_from(self.input, rgb)
    }
    /// Interleaved RGB triples in place (see [`ViewTransformId::apply_in_place`]).
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
    ViewTransformId::Aces1SdrVideo.apply_from(SceneInput::Aces2065_1, ap0)
}

/// Second stage of STU-COL-313: display-referred linear values -> display code values.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayEncoding {
    /// IEC 61966-2-1 piecewise curve (CTL `moncurve_r` gamma 2.4, offset 0.055).
    Srgb,
    /// ITU-R BT.1886 inverse EOTF with L_W 1, L_B 0 (CTL `bt1886_r`): `L^(1/2.4)`.
    Bt1886,
    /// Pure 2.2 power (STU-COL-313 `gamma22_rec709_display`): `L^(1/2.2)`.
    Gamma22,
    /// SMPTE ST 2084 (PQ) of linear values in units of 100 cd/m2 (CTL `Y_2_ST2084` of `L * 100`).
    Pq,
}
impl DisplayEncoding {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Srgb => "srgb_piecewise",
            Self::Bt1886 => "bt1886_gamma24",
            Self::Gamma22 => "gamma22",
            Self::Pq => "st2084_pq",
        }
    }
    /// Encode linear values. SDR encodings clamp into `[0, 1]` first (the ODT's own clamp); PQ
    /// clamps into `[0, 100]` (0 .. 10000 cd/m2).
    pub fn encode(self, linear: [f32; 3]) -> Result<[f32; 3], Error> {
        let x = finite(linear)?;
        let f = |v: f64| match self {
            Self::Srgb => moncurve_r(v.clamp(0.0, 1.0), 2.4, 0.055),
            Self::Bt1886 => pow(v.clamp(0.0, 1.0), 1.0 / 2.4),
            Self::Gamma22 => pow(v.clamp(0.0, 1.0), 1.0 / 2.2),
            Self::Pq => y_2_st2084(v.clamp(0.0, 100.0) * 100.0),
        };
        Ok(to_f32([f(x[0]), f(x[1]), f(x[2])]))
    }
}

/// Typed refusal of [`select_view`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewSelectError {
    /// Not a STU-COL-313 display target key.
    UnknownDisplay,
    /// A STU-COL-313 display target (or peak) for which Prism carries no ACES transform.
    NotBuilt,
}
impl ViewSelectError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownDisplay => "view_unknown_display",
            Self::NotBuilt => "view_not_built",
        }
    }
}

/// The tone-mapped view and display encoding Prism uses for a STU-COL-313 display target at a peak
/// luminance (`aces_view.peak_luminance`, 100 for the SDR targets). Built: `srgb_display`,
/// `rec1886_rec709_display`, `gamma22_rec709_display` (one shared Rec.709 view, three encodings),
/// `display_p3_display` (ODT.Academy.DisplayP3_dim, sRGB curve) and `rec2100_pq_display` at
/// 1000, 2000 and 4000 nits. `p3_d65_display`, `rec2100_hlg_display`, other PQ peaks (500) and
/// SDR targets at a peak other than 100 are [`ViewSelectError::NotBuilt`]; any other key is
/// [`ViewSelectError::UnknownDisplay`].
pub fn select_view(display: &str, peak_nits: u32) -> Result<(ViewTransformId, DisplayEncoding), ViewSelectError> {
    use DisplayEncoding as E;
    use ViewTransformId as V;
    let sdr = |v: V, e: E| if peak_nits == 100 { Ok((v, e)) } else { Err(ViewSelectError::NotBuilt) };
    match display {
        "srgb_display" => sdr(V::Aces1SdrVideo, E::Srgb),
        "rec1886_rec709_display" => sdr(V::Aces1SdrVideo, E::Bt1886),
        "gamma22_rec709_display" => sdr(V::Aces1SdrVideo, E::Gamma22),
        "display_p3_display" => sdr(V::Aces1SdrVideoDisplayP3, E::Srgb),
        "rec2100_pq_display" => match peak_nits {
            1000 => Ok((V::Aces1HdrVideo1000Nits, E::Pq)),
            2000 => Ok((V::Aces1HdrVideo2000Nits, E::Pq)),
            4000 => Ok((V::Aces1HdrVideo4000Nits, E::Pq)),
            _ => Err(ViewSelectError::NotBuilt),
        },
        "p3_d65_display" | "rec2100_hlg_display" => Err(ViewSelectError::NotBuilt),
        _ => Err(ViewSelectError::UnknownDisplay),
    }
}

/// Machine-readable description of the view transforms (manual / model surface).
pub const VIEW_TRANSFORM_DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PRISM","path":"view_transform","lanes":["SMB-PRISM-VIEW-TRANSFORM","SMB-PRISM-VIEW-TRANSFORM-2"],"stage":"STU-COL-161 6a display transform","api":"ViewTransformId::apply([f32;3]) (working space) / apply_from(SceneInput, [f32;3]) -> Result<[f32;3], Error>; ViewTransform{view,input}::apply / apply_in_place; aces1_sdr_video_from_aces2065; DisplayEncoding::encode; select_view(display_target, peak_nits) -> (view, encoding) | ViewSelectError view_unknown_display | view_not_built; view_gpu_params + VIEW_WGSL (SDR views on the GPU)","inputs":{"linear_srgb":"Prism working space, Bradford D65 -> ACES white to ACES2065-1","acescg":"AP1, published ACEScg TRA_2 matrix to ACES2065-1 (S-2014-004)","aces2065_1":"AP0, unchanged"},"views":[{"key":"aces1_sdr_video_100nits_dim","ctl":"RRT + ODT.Academy.sRGB_100nits_dim / Rec709_100nits_dim","output":"linear Rec.709 [0,1]","displays":["srgb_display (srgb_piecewise)","rec1886_rec709_display (bt1886_gamma24)","gamma22_rec709_display (gamma22)"]},{"key":"aces1_sdr_video_100nits_dim_display_p3","ctl":"RRT + ODT.Academy.DisplayP3_dim","output":"linear P3-D65 [0,1]","displays":["display_p3_display (srgb_piecewise)"]},{"key":"aces1_hdr_video_{1000,2000,4000}nits_15nits_rec2020","ctl":"RRTODT.Academy.Rec2020_{1000,2000,4000}nits_15nits_ST2084 (ACESlib.OutputTransforms, SSTS)","output":"linear Rec.2020 in units of 100 cd/m2, stretched black","displays":["rec2100_pq_display at aces_view.peak_luminance 1000, 2000, 4000 (st2084_pq)"]}],"not_built":["p3_d65_display","rec2100_hlg_display","rec2100_pq_display at other peaks (500)","SDR targets at a peak other than 100","HDR views on the GPU (view_gpu_params None)","OCIO configuration runtime","curves the spec does not name"],"proven_by":["tests/view_transform.rs","tests/view_transform_2.rs"],"hostile":"NaN or infinite channel -> nonfinite_channel; negative channels admitted (RRT clamps them)","determinism":"binary64, IEEE basic operations and Prism's own ln/exp/atan, no libm, no FMA; allocation-free; the WGSL form is binary32","licence":"ACES (A.M.P.A.S.) permissive terms, LICENSE-ACES.txt"}"#;

// ------------------------------------------------------------------ ACES 1.x pipelines

/// RRT sweeteners (rrt/RRT.ctl up to the tone scale; ACESlib.RRT_Common.ctl `rrt_sweeteners`):
/// glow, red modifier, AP0 -> AP1 with the CTL clamps, global desaturation.
fn rrt_sweeteners(aces_in: [f64; 3]) -> [f64; 3] {
    let t = tables();
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
    mv(&t.rrt_sat, rgb_pre)
}

/// RRT + SDR video ODT (100 nits dim) on finite ACES2065-1 input; `xyz_d60_to_display` is the
/// display's `XYZ_2_DISPLAY_PRI_MAT * D60_2_D65_CAT`. Output linear code values in `[0, 1]`.
fn aces1_sdr_video(aces_in: [f64; 3], xyz_d60_to_display: &M3) -> [f64; 3] {
    let t = tables();
    // ---- RRT (rrt/RRT.ctl) ----
    let rgb_pre = rrt_sweeteners(aces_in);
    let rgb_post = rgb_pre.map(|v| spline(v, &t.c5));
    let oces = mv(&t.ap1_to_ap0, rgb_post);
    // ---- ODT (ODT.Academy.{sRGB,Rec709}_100nits_dim, DisplayP3_dim; linear part) ----
    let rgb_pre = mv(&t.ap0_to_ap1, oces);
    let rgb_post = rgb_pre.map(|v| spline(v, &t.c9));
    // Scale luminance to linear code value.
    let lin = rgb_post.map(|y| (y - t.cinema_black) / (CINEMA_WHITE - t.cinema_black));
    // Dark to dim surround.
    let lin = dark_to_dim(lin);
    // Desaturation for the luminance difference.
    let lin = mv(&t.odt_sat, lin);
    // Rendering space -> XYZ -> D60 to D65 CAT -> display primaries.
    mv(xyz_d60_to_display, mv(&AP1_TO_XYZ, lin)).map(|v| v.clamp(0.0, 1.0))
}

/// RRTODT Rec.2020 ST 2084 (ACESlib.OutputTransforms.ctl `outputTransform` with Y_MIN 0.0001,
/// Y_MID 15, DISPLAY_PRI = LIMITING_PRI = Rec.2020, dark surround, stretched black, no D60 sim):
/// output linear Rec.2020 in units of 100 cd/m2.
fn aces1_hdr_video(aces_in: [f64; 3], ssts: &Spline, y_max: f64) -> [f64; 3] {
    let t = tables();
    let rgb_pre = rrt_sweeteners(aces_in);
    let rgb_post = rgb_pre.map(|v| spline(v, ssts));
    let lin = rgb_post.map(|y| (y - HDR_Y_MIN) / (y_max - HDR_Y_MIN));
    let xyz = mv(&AP1_TO_XYZ, lin);
    // limit_to_primaries(Rec.2020): to the limiting primaries, clamp [0, 1], back to XYZ.
    let limited = mv(&XYZ_TO_REC2020, xyz).map(|v| v.clamp(0.0, 1.0));
    let xyz = mv(&t.d60_to_d65, mv(&REC2020_TO_XYZ, limited));
    // Display primaries, clamp at 0, stretched black: Y = linearCV * Y_MAX, here / 100.
    mv(&XYZ_TO_REC2020, xyz).map(|v| v.max(0.0) * (y_max / 100.0))
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
// HDR output transform parameters (RRTODT.Academy.Rec2020_*nits_15nits_ST2084.ctl).
const HDR_Y_MIN: f64 = 0.0001;
const HDR_Y_MID: f64 = 15.0;
// CTL constants (ACESlib.Transform_Common.ctl, CTL half limits).
const TINY: f64 = 1e-10;
const HALF_MAX: f64 = 65504.0;
const HALF_MIN: f64 = 5.960_464_477_539_063e-8;

// Chromaticities (ACESlib.Utilities_Color.ctl): red, green, blue, white.
const AP0: [[f64; 2]; 4] = [[0.7347, 0.2653], [0.0, 1.0], [0.0001, -0.077], [0.32168, 0.33767]];
const AP1: [[f64; 2]; 4] = [[0.713, 0.293], [0.165, 0.830], [0.128, 0.044], [0.32168, 0.33767]];
const REC709: [[f64; 2]; 4] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], [0.3127, 0.3290]];
const P3D65: [[f64; 2]; 4] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], [0.3127, 0.3290]];
const REC2020: [[f64; 2]; 4] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], [0.3127, 0.3290]];
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
const XYZ_TO_P3D65: M3 = inv(npm(P3D65));
const REC2020_TO_XYZ: M3 = npm(REC2020);
const XYZ_TO_REC2020: M3 = inv(REC2020_TO_XYZ);

struct Spline {
    coefs_low: [f64; 10],
    coefs_high: [f64; 10],
    knots_low: f64,
    knots_high: f64,
    log_min_x: f64,
    log_mid_x: f64,
    log_max_x: f64,
    log_min_y: f64,
    log_mid_y: f64,
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
    xyz_d60_to_rec709: M3,
    xyz_d60_to_p3d65: M3,
    cinema_black: f64,
    c5: Spline,
    c9: Spline,
    ssts1000: Spline,
    ssts2000: Spline,
    ssts4000: Spline,
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

fn pad10(c: [f64; 6]) -> [f64; 10] {
    [c[0], c[1], c[2], c[3], c[4], c[5], 0.0, 0.0, 0.0, 0.0]
}

/// CTL builtin `interpolate1D` over a two-row table: linear inside, clamped outside.
fn interpolate1d(t: [[f64; 2]; 2], p: f64) -> f64 {
    if p <= t[0][0] {
        t[0][1]
    } else if p >= t[1][0] {
        t[1][1]
    } else {
        t[0][1] + (p - t[0][0]) / (t[1][0] - t[0][0]) * (t[1][1] - t[0][1])
    }
}

/// ACESlib.SSTS.ctl `init_TsParams(minLum, maxLum, expShift)` as a [`Spline`] (`ssts` has the
/// same quadratic B-spline form as the c5 spline: 4 knots per half, slopes 0 at both ends).
fn init_ts(min_lum: f64, max_lum: f64, exp_shift: f64) -> Spline {
    let ln2 = std::f64::consts::LN_2;
    let log2 = |x: f64| ln(x) / ln2;
    let pow2 = |v: f64| exp(v * ln2);
    let min_x0 = 0.18 * pow2(interpolate1d([[log10(0.0001), -15.0], [log10(0.02), -6.5]], log10(min_lum)));
    let max_x0 = 0.18 * pow2(interpolate1d([[log10(48.0), 6.5], [log10(10000.0), 18.0]], log10(max_lum)));
    let (mid_x0, mid_y, mid_slope) = (0.18, 4.8, 1.55);
    // init_coefsLow (MIN slope 0).
    let k_low = (log10(mid_x0) - log10(min_x0)) / 3.0;
    let mid_line = |x: f64| mid_slope * x + (log10(mid_y) - mid_slope * log10(mid_x0));
    let pct_low = interpolate1d([[-15.0, 0.18], [-6.5, 0.35]], log2(min_x0 / 0.18));
    let low = [
        log10(min_lum),
        log10(min_lum),
        log10(min_lum) + pct_low * (log10(mid_y) - log10(min_lum)),
        mid_line(log10(mid_x0) - 0.5 * k_low),
        mid_line(log10(mid_x0) + 0.5 * k_low),
    ];
    // init_coefsHigh (MAX slope 0).
    let k_high = (log10(max_x0) - log10(mid_x0)) / 3.0;
    let pct_high = interpolate1d([[6.5, 0.89], [18.0, 0.90]], log2(max_x0 / 0.18));
    let high = [
        mid_line(log10(mid_x0) - 0.5 * k_high),
        mid_line(log10(mid_x0) + 0.5 * k_high),
        log10(mid_y) + pct_high * (log10(max_lum) - log10(mid_y)),
        log10(max_lum),
        log10(max_lum),
    ];
    let shift = |x: f64| pow2(log2(x) - exp_shift);
    Spline {
        coefs_low: pad10([low[0], low[1], low[2], low[3], low[4], low[4]]),
        coefs_high: pad10([high[0], high[1], high[2], high[3], high[4], high[4]]),
        knots_low: 4.0,
        knots_high: 4.0,
        log_min_x: log10(shift(min_x0)),
        log_mid_x: log10(shift(mid_x0)),
        log_max_x: log10(shift(max_x0)),
        log_min_y: log10(min_lum),
        log_mid_y: log10(mid_y),
        log_max_y: log10(max_lum),
        slope_low: 0.0,
        slope_high: 0.0,
    }
}

/// `outputTransform`'s tone scale for Y_MAX: `expShift = log2(inv_ssts(Y_MID)) - log2(0.18)` on the
/// unshifted parameters, then the shifted parameters.
fn hdr_ssts(y_max: f64) -> Spline {
    let ln2 = std::f64::consts::LN_2;
    let default = init_ts(HDR_Y_MIN, y_max, 0.0);
    let exp_shift = ln(inv_spline(HDR_Y_MID, &default)) / ln2 - ln(0.18) / ln2;
    init_ts(HDR_Y_MIN, y_max, exp_shift)
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let ap0_to_ap1 = mul(XYZ_TO_AP1, AP0_TO_XYZ);
        let ap1_y = AP1_TO_XYZ[1];
        let c5 = Spline {
            coefs_low: pad10(C5_LOW),
            coefs_high: pad10(C5_HIGH),
            knots_low: 4.0,
            knots_high: 4.0,
            log_min_x: log10(0.18 * pow(2.0, -15.0)),
            log_mid_x: log10(0.18),
            log_max_x: log10(0.18 * pow(2.0, 18.0)),
            log_min_y: log10(0.0001),
            log_mid_y: log10(4.8),
            log_max_y: log10(10000.0),
            slope_low: 0.0,
            slope_high: 0.0,
        };
        let c9 = Spline {
            coefs_low: C9_LOW,
            coefs_high: C9_HIGH,
            knots_low: 8.0,
            knots_high: 8.0,
            log_min_x: log10(spline(0.18 * pow(2.0, -6.5), &c5)),
            log_mid_x: log10(spline(0.18, &c5)),
            log_max_x: log10(spline(0.18 * pow(2.0, 6.5), &c5)),
            log_min_y: log10(0.02),
            log_mid_y: log10(4.8),
            log_max_y: log10(48.0),
            slope_low: 0.0,
            slope_high: 0.04,
        };
        let d60_to_d65 = cat(AP0[3], REC709[3]);
        Tables {
            rec709_to_ap0: mul(XYZ_TO_AP0, mul(cat(REC709[3], AP0[3]), REC709_TO_XYZ)),
            ap0_to_ap1,
            ap1_to_ap0: mul(XYZ_TO_AP0, AP1_TO_XYZ),
            rrt_sat: sat_matrix(RRT_SAT_FACTOR, ap1_y),
            odt_sat: sat_matrix(ODT_SAT_FACTOR, ap1_y),
            d60_to_d65,
            xyz_d60_to_rec709: mul(XYZ_TO_REC709, d60_to_d65),
            xyz_d60_to_p3d65: mul(XYZ_TO_P3D65, d60_to_d65),
            // CTL: pow10(log10(0.02)).
            cinema_black: pow10(log10(0.02)),
            c5,
            c9,
            ssts1000: hdr_ssts(1000.0),
            ssts2000: hdr_ssts(2000.0),
            ssts4000: hdr_ssts(4000.0),
        }
    })
}

/// `inv_ssts` / `segmented_spline_c*_rev` for the knot y of 4-knot splines (used for the HDR
/// exposure shift only).
fn inv_spline(y: f64, c: &Spline) -> f64 {
    let logy = log10(y.max(1e-10));
    let solve = |coefs: &[f64; 10], log_x0: f64, inc: f64, first_inclusive: bool| {
        let knot = |i: usize| (coefs[i] + coefs[i.saturating_add(1)]) / 2.0;
        let mut j = 2usize;
        for i in 0..3usize {
            let lo = knot(i);
            let hi = knot(i.saturating_add(1));
            let above = if first_inclusive && i == 0 { logy >= lo } else { logy > lo };
            if above && logy <= hi {
                j = i;
                break;
            }
        }
        let (c0, c1, c2) = (coefs[j], coefs[j.saturating_add(1)], coefs[j.saturating_add(2)]);
        let a = 0.5 * c0 - c1 + 0.5 * c2;
        let b = c1 - c0;
        let cc = 0.5 * c0 + 0.5 * c1 - logy;
        let d = (b * b - 4.0 * a * cc).max(0.0).sqrt();
        let t = (2.0 * cc) / (-d - b);
        log_x0 + (t + j as f64) * inc
    };
    let logx = if logy <= c.log_min_y {
        c.log_min_x
    } else if logy <= c.log_mid_y {
        solve(&c.coefs_low, c.log_min_x, (c.log_mid_x - c.log_min_x) / (c.knots_low - 1.0), false)
    } else if logy < c.log_max_y {
        solve(&c.coefs_high, c.log_mid_x, (c.log_max_x - c.log_mid_x) / (c.knots_high - 1.0), true)
    } else {
        c.log_max_x
    };
    pow10(logx)
}

/// CTL `Y_2_ST2084` (cd/m2 -> PQ code value, full range).
fn y_2_st2084(c: f64) -> f64 {
    const M1: f64 = 0.159_301_757_812_5;
    const M2: f64 = 78.843_75;
    const C1: f64 = 0.835_937_5;
    const C2: f64 = 18.851_562_5;
    const C3: f64 = 18.6875;
    let lm = pow(c / 10000.0, M1);
    pow((C1 + C2 * lm) / (1.0 + C3 * lm), M2)
}

// ------------------------------------------------------------------ GPU form (SDR views)

/// Number of `f32` parameters of [`view_gpu_params`].
pub const VIEW_GPU_PARAMS: usize = 117;

/// The SDR view's parameters for [`VIEW_WGSL`], in binary32 (row-major 3x3 matrices for column
/// vectors): `[0..9)` input -> AP0, `[9..18)` AP0 -> AP1, `[18..27)` RRT_SAT, `[27..36)` AP1 -> AP0,
/// `[36..45)` ODT_SAT, `[45..54)` AP1 -> XYZ, `[54..63)` XYZ -> AP1, `[63..72)` XYZ (D60) ->
/// display (`XYZ_2_DISPLAY_PRI_MAT * D60_2_D65_CAT`), `[72..78)` c5 low, `[78..84)` c5 high,
/// `[84..94)` c9 low, `[94..104)` c9 high, `[104..110)` c5 log min x, log mid x, log max x, log min
/// y, log max y, high slope, `[110..116)` the same for c9, `[116]` cinema black. `None` for the HDR
/// views (no GPU form: the shared-texture target is 8-bit SDR).
pub fn view_gpu_params(view: ViewTransform) -> Option<[f32; VIEW_GPU_PARAMS]> {
    let t = tables();
    let display = match view.view {
        ViewTransformId::Aces1SdrVideo => t.xyz_d60_to_rec709,
        ViewTransformId::Aces1SdrVideoDisplayP3 => t.xyz_d60_to_p3d65,
        _ => return None,
    };
    let mut p = [0.0f32; VIEW_GPU_PARAMS];
    let mut at = 0usize;
    let mut put = |v: f64| {
        if let Some(slot) = p.get_mut(at) {
            *slot = v as f32;
        }
        at = at.saturating_add(1);
    };
    for m in [
        view.input.to_ap0_matrix(),
        t.ap0_to_ap1,
        t.rrt_sat,
        t.ap1_to_ap0,
        t.odt_sat,
        AP1_TO_XYZ,
        XYZ_TO_AP1,
        display,
    ] {
        m.iter().flatten().for_each(|v| put(*v));
    }
    C5_LOW.iter().chain(C5_HIGH.iter()).chain(C9_LOW.iter()).chain(C9_HIGH.iter()).for_each(|v| put(*v));
    for c in [&t.c5, &t.c9] {
        for v in [c.log_min_x, c.log_mid_x, c.log_max_x, c.log_min_y, c.log_max_y, c.slope_high] {
            put(v);
        }
    }
    put(t.cinema_black);
    Some(p)
}

/// WGSL form of the SDR views (binary32), entry `vt_view(rgb: vec3<f32>) -> vec3<f32>` (scene
/// input -> display-referred linear `[0, 1]`), reading [`view_gpu_params`] through a host-provided
/// `fn vp(i: u32) -> f32`. Same steps as the binary64 path; the neutral case (undefined hue) skips
/// the red modifier explicitly instead of relying on NaN comparisons.
pub const VIEW_WGSL: &str = r"
fn vt_m3(o: u32, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        vp(o) * v.x + vp(o + 1u) * v.y + vp(o + 2u) * v.z,
        vp(o + 3u) * v.x + vp(o + 4u) * v.y + vp(o + 5u) * v.z,
        vp(o + 6u) * v.x + vp(o + 7u) * v.y + vp(o + 8u) * v.z);
}
fn vt_log10(x: f32) -> f32 {
    return log2(x) * 0.30102999566398120;
}
fn vt_pow10(y: f32) -> f32 {
    return exp2(y * 3.3219280948873623);
}
fn vt_seg(o: u32, kc: f32, last: f32) -> f32 {
    let jf = min(floor(kc), last);
    let t = kc - jf;
    let j = o + u32(max(jf, 0.0));
    let c0 = vp(j);
    let c1 = vp(j + 1u);
    let c2 = vp(j + 2u);
    return t * t * (0.5 * c0 - c1 + 0.5 * c2) + t * (c1 - c0) + 0.5 * (c0 + c1);
}
fn vt_spline(x: f32, lo: u32, hi: u32, n: f32, g: u32) -> f32 {
    let logx = vt_log10(max(x, 5.9604645e-8));
    var logy: f32;
    if logx <= vp(g) {
        logy = vp(g + 3u);
    } else if logx < vp(g + 1u) {
        logy = vt_seg(lo, (n - 1.0) * (logx - vp(g)) / (vp(g + 1u) - vp(g)), n - 2.0);
    } else if logx < vp(g + 2u) {
        logy = vt_seg(hi, (n - 1.0) * (logx - vp(g + 1u)) / (vp(g + 2u) - vp(g + 1u)), n - 2.0);
    } else {
        logy = logx * vp(g + 5u) + (vp(g + 4u) - vp(g + 5u) * vp(g + 2u));
    }
    return vt_pow10(logy);
}
fn vt_cubic(x: f32, w: f32) -> f32 {
    if !(x > -w / 2.0 && x < w / 2.0) {
        return 0.0;
    }
    let kc = (x + w / 2.0) * 4.0 / w;
    let jf = floor(kc);
    let t = kc - jf;
    let t2 = t * t;
    let t3 = t2 * t;
    var y = 0.0;
    if jf == 3.0 {
        y = -t3 / 6.0 + t2 / 2.0 - t / 2.0 + 1.0 / 6.0;
    } else if jf == 2.0 {
        y = t3 / 2.0 - t2 + 2.0 / 3.0;
    } else if jf == 1.0 {
        y = -t3 / 2.0 + t2 / 2.0 + t / 2.0 + 1.0 / 6.0;
    } else if jf == 0.0 {
        y = t3 / 6.0;
    }
    return y * 1.5;
}
fn vt_view(rgb: vec3<f32>) -> vec3<f32> {
    var a = vt_m3(0u, rgb);
    // Glow.
    let mx = max(a.r, max(a.g, a.b));
    let mn = min(a.r, min(a.g, a.b));
    let sat = (max(mx, 1e-10) - max(mn, 1e-10)) / max(mx, 0.01);
    let chroma = sqrt(max(a.b * (a.b - a.g) + a.g * (a.g - a.r) + a.r * (a.r - a.b), 0.0));
    let yc = (a.b + a.g + a.r + 1.75 * chroma) / 3.0;
    let sx = (sat - 0.4) / 0.2;
    let st = max(1.0 - abs(sx / 2.0), 0.0);
    let s = (1.0 + sign(sx) * (1.0 - st * st)) / 2.0;
    let gain = 0.05 * s;
    var glow = 0.0;
    if yc <= 2.0 / 3.0 * 0.08 {
        glow = gain;
    } else if yc < 0.16 {
        glow = gain * (0.08 / yc - 0.5);
    }
    a = a * (1.0 + glow);
    // Red modifier (hue undefined for a neutral: weight 0).
    var w = 0.0;
    if !(a.r == a.g && a.g == a.b) {
        var h = degrees(atan2(1.7320508075688772 * (a.g - a.b), 2.0 * a.r - a.g - a.b));
        if h < 0.0 {
            h = h + 360.0;
        }
        if h > 180.0 {
            h = h - 360.0;
        }
        w = vt_cubic(h, 135.0);
    }
    a.r = a.r + w * sat * (0.03 - a.r) * (1.0 - 0.82);
    a = max(a, vec3<f32>(0.0));
    var pre = clamp(vt_m3(9u, a), vec3<f32>(0.0), vec3<f32>(65504.0));
    pre = vt_m3(18u, pre);
    let post = vec3<f32>(
        vt_spline(pre.r, 72u, 78u, 4.0, 104u),
        vt_spline(pre.g, 72u, 78u, 4.0, 104u),
        vt_spline(pre.b, 72u, 78u, 4.0, 104u));
    // ODT.
    let pre9 = vt_m3(9u, vt_m3(27u, post));
    let post9 = vec3<f32>(
        vt_spline(pre9.r, 84u, 94u, 8.0, 110u),
        vt_spline(pre9.g, 84u, 94u, 8.0, 110u),
        vt_spline(pre9.b, 84u, 94u, 8.0, 110u));
    let cb = vp(116u);
    var lin = (post9 - vec3<f32>(cb)) / (48.0 - cb);
    let xyz = vt_m3(45u, lin);
    var d = xyz.x + xyz.y + xyz.z;
    if d == 0.0 {
        d = 1e-10;
    }
    let cx = xyz.x / d;
    let cy = xyz.y / d;
    var yy = 0.0;
    if xyz.y > 0.0 {
        yy = pow(xyz.y, 0.9811);
    }
    let yd = max(cy, 1e-10);
    lin = vt_m3(54u, vec3<f32>(cx * yy / yd, yy, (1.0 - cx - cy) * yy / yd));
    lin = vt_m3(36u, lin);
    return clamp(vt_m3(63u, vt_m3(45u, lin)), vec3<f32>(0.0), vec3<f32>(1.0));
}
";

/// `segmented_spline_c5_fwd` / `segmented_spline_c9_fwd`: quadratic B-spline in log-log space with
/// linear extensions.
fn spline(x: f64, c: &Spline) -> f64 {
    let logx = log10(x.max(HALF_MIN));
    let logy = if logx <= c.log_min_x {
        logx * c.slope_low + (c.log_min_y - c.slope_low * c.log_min_x)
    } else if logx < c.log_mid_x {
        let kc = (c.knots_low - 1.0) * (logx - c.log_min_x) / (c.log_mid_x - c.log_min_x);
        segment(&c.coefs_low, kc)
    } else if logx < c.log_max_x {
        let kc = (c.knots_high - 1.0) * (logx - c.log_mid_x) / (c.log_max_x - c.log_mid_x);
        segment(&c.coefs_high, kc)
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
