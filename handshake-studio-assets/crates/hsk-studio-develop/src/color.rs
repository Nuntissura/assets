//! DNG camera colour: camera RGB -> XYZ (D50) from ColorMatrix/ForwardMatrix tags, plus the small
//! binary64 matrix toolkit it needs. Method = Adobe DNG specification chapter "Mapping Camera
//! Color Space to CIE XYZ Space" (CCT interpolation of the two profile matrices in mired space,
//! iterative white-point search from the camera neutral, ForwardMatrix path
//! `FM * D * inverse(AB * CC)`, ColorMatrix path `inverse(AB * CC * CM)` + Bradford to D50).
//! CCT uses Robertson (1968) isotemperature lines (published data, Wyszecki & Stiles).
use crate::DevelopError;

/// Row-major 3x3.
pub type Mat3 = [f64; 9];

pub const IDENTITY: Mat3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
/// D50 reference white, XYZ for xy = (0.34567, 0.35850) (Lindbloom). Other D50 conventions (ICC
/// PCS 0.9642/1/0.8249, DNG xy 0.3457/0.3585) differ by at most ~3e-4 relative.
pub const D50_XYZ: [f64; 3] = [0.96422, 1.0, 0.82521];
const BRADFORD: Mat3 = [
    0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296,
];

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = (a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c]) + a[r * 3 + 2] * b[6 + c];
        }
    }
    out
}

pub fn mul_vec(a: &Mat3, v: &[f64; 3]) -> [f64; 3] {
    [
        (a[0] * v[0] + a[1] * v[1]) + a[2] * v[2],
        (a[3] * v[0] + a[4] * v[1]) + a[5] * v[2],
        (a[6] * v[0] + a[7] * v[1]) + a[8] * v[2],
    ]
}

pub fn diag(d: [f64; 3]) -> Mat3 {
    [d[0], 0.0, 0.0, 0.0, d[1], 0.0, 0.0, 0.0, d[2]]
}

pub fn invert(m: &Mat3) -> Result<Mat3, DevelopError> {
    let c00 = m[4] * m[8] - m[5] * m[7];
    let c01 = m[5] * m[6] - m[3] * m[8];
    let c02 = m[3] * m[7] - m[4] * m[6];
    let det = (m[0] * c00 + m[1] * c01) + m[2] * c02;
    if !det.is_finite() || det == 0.0 {
        return Err(DevelopError::NotInvertibleMatrix);
    }
    let inv = [
        c00 / det,
        (m[2] * m[7] - m[1] * m[8]) / det,
        (m[1] * m[5] - m[2] * m[4]) / det,
        c01 / det,
        (m[0] * m[8] - m[2] * m[6]) / det,
        (m[2] * m[3] - m[0] * m[5]) / det,
        c02 / det,
        (m[1] * m[6] - m[0] * m[7]) / det,
        (m[0] * m[4] - m[1] * m[3]) / det,
    ];
    if inv.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NotInvertibleMatrix);
    }
    Ok(inv)
}

pub fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

pub fn xyz_to_xy(xyz: &[f64; 3]) -> Result<[f64; 2], DevelopError> {
    let sum = (xyz[0] + xyz[1]) + xyz[2];
    if !sum.is_finite() || sum <= 0.0 {
        return Err(DevelopError::NonFinite);
    }
    Ok([xyz[0] / sum, xyz[1] / sum])
}

/// Bradford chromatic adaptation taking `src_white` to `dst_white` (both XYZ).
pub fn bradford(src_white: &[f64; 3], dst_white: &[f64; 3]) -> Result<Mat3, DevelopError> {
    let s = mul_vec(&BRADFORD, src_white);
    let d = mul_vec(&BRADFORD, dst_white);
    if s.contains(&0.0) {
        return Err(DevelopError::NonFinite);
    }
    let scale = diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]]);
    Ok(mul(&mul(&invert(&BRADFORD)?, &scale), &BRADFORD))
}

/// RGB -> XYZ for given xy primaries and white, adapted to D50 (ICC style).
pub fn rgb_to_xyz_d50(
    primaries: [[f64; 2]; 3],
    white_xy: [f64; 2],
) -> Result<Mat3, DevelopError> {
    let cols: Vec<[f64; 3]> = primaries.iter().map(|p| xy_to_xyz(*p)).collect();
    let basis: Mat3 = [
        cols[0][0], cols[1][0], cols[2][0], cols[0][1], cols[1][1], cols[2][1], cols[0][2],
        cols[1][2], cols[2][2],
    ];
    let white = xy_to_xyz(white_xy);
    let s = mul_vec(&invert(&basis)?, &white);
    let native = mul(&basis, &diag(s));
    Ok(mul(&bradford(&white, &D50_XYZ)?, &native))
}

/// Published sRGB -> XYZ (D50) colorant matrix (ICC sRGB profile / Lindbloom table). Row sums are
/// exactly `D50_XYZ`. `rgb_to_xyz_d50` on the sRGB primaries agrees within ~6e-5 (white-point xy
/// rounding), so the published table is used where a standard profile must be matched exactly.
pub const SRGB_TO_XYZ_D50: Mat3 = [
    0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045,
    0.7141733,
];

/// XYZ (D50) -> linear sRGB: inverse of `SRGB_TO_XYZ_D50`.
pub fn xyz_d50_to_linear_srgb() -> Result<Mat3, DevelopError> {
    invert(&SRGB_TO_XYZ_D50)
}

/// Robertson (1968) isotemperature lines: (mired, u, v, slope t) in CIE 1960 UCS.
const ROBERTSON: [(f64, f64, f64, f64); 31] = [
    (0.0, 0.18006, 0.26352, -0.24341),
    (10.0, 0.18066, 0.26589, -0.25479),
    (20.0, 0.18133, 0.26846, -0.26876),
    (30.0, 0.18208, 0.27119, -0.28539),
    (40.0, 0.18293, 0.27407, -0.30470),
    (50.0, 0.18388, 0.27709, -0.32675),
    (60.0, 0.18494, 0.28021, -0.35156),
    (70.0, 0.18611, 0.28342, -0.37915),
    (80.0, 0.18740, 0.28668, -0.40955),
    (90.0, 0.18880, 0.28997, -0.44278),
    (100.0, 0.19032, 0.29326, -0.47888),
    (125.0, 0.19462, 0.30141, -0.58204),
    (150.0, 0.19962, 0.30921, -0.70471),
    (175.0, 0.20525, 0.31647, -0.84901),
    (200.0, 0.21142, 0.32312, -1.0182),
    (225.0, 0.21807, 0.32909, -1.2168),
    (250.0, 0.22511, 0.33439, -1.4512),
    (275.0, 0.23247, 0.33904, -1.7298),
    (300.0, 0.24010, 0.34308, -2.0637),
    (325.0, 0.24792, 0.34655, -2.4681),
    (350.0, 0.25591, 0.34951, -2.9641),
    (375.0, 0.26400, 0.35200, -3.5814),
    (400.0, 0.27218, 0.35407, -4.3633),
    (425.0, 0.28039, 0.35577, -5.3762),
    (450.0, 0.28863, 0.35714, -6.7262),
    (475.0, 0.29685, 0.35823, -8.5955),
    (500.0, 0.30505, 0.35907, -11.324),
    (525.0, 0.31320, 0.35968, -15.628),
    (550.0, 0.32129, 0.36011, -23.325),
    (575.0, 0.32931, 0.36038, -40.770),
    (600.0, 0.33724, 0.36051, -116.45),
];

/// Correlated colour temperature (kelvin) of an xy chromaticity; refuses outside 1667 K..infinity.
pub fn cct_from_xy(xy: [f64; 2]) -> Result<f64, DevelopError> {
    let denominator = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    if !denominator.is_finite() || denominator <= 0.0 {
        return Err(DevelopError::NonFinite);
    }
    let u = 4.0 * xy[0] / denominator;
    let v = 6.0 * xy[1] / denominator;
    let mut previous: Option<f64> = None;
    for (i, (mired, ui, vi, ti)) in ROBERTSON.iter().enumerate() {
        let d = ((v - vi) - ti * (u - ui)) / (1.0 + ti * ti).sqrt();
        if let Some(last) = previous
            && ((d <= 0.0 && last > 0.0) || (d >= 0.0 && last < 0.0) || d == 0.0)
        {
            let p = if d == last { 0.0 } else { last / (last - d) };
            let m = ROBERTSON[i - 1].0 + p * (mired - ROBERTSON[i - 1].0);
            if m <= 0.0 {
                return Err(DevelopError::ColorNoConvergence);
            }
            return Ok(1.0e6 / m);
        }
        previous = Some(d);
    }
    Err(DevelopError::ColorNoConvergence)
}

/// DNG `LightSource` code -> CCT (kelvin) for the calibration illuminants.
pub fn illuminant_cct(code: u16) -> Result<f64, DevelopError> {
    Ok(match code {
        1 | 4 | 9 => 5500.0,
        20 => 5503.0,
        2 | 14 => 4200.0,
        3 => 2850.0,
        10 => 6500.0,
        11 => 7500.0,
        12 => 6500.0,
        13 => 5000.0,
        15 => 3500.0,
        16 => 3000.0,
        17 => 2856.0,
        18 => 4874.0,
        19 => 6774.0,
        21 => 6504.0,
        22 => 7504.0,
        23 => 5003.0,
        24 => 3200.0,
        _ => return Err(DevelopError::UnsupportedIlluminant),
    })
}

/// Parsed DNG colour tags (see Adobe DNG spec tag list). Matrices are row-major 3x3.
#[derive(Clone, Debug, PartialEq)]
pub struct DngColor {
    /// XYZ -> camera, per calibration illuminant.
    pub color_matrix: [Option<Mat3>; 2],
    /// White-balanced camera -> XYZ D50, per calibration illuminant.
    pub forward_matrix: [Option<Mat3>; 2],
    pub camera_calibration: [Option<Mat3>; 2],
    pub calibration_illuminant: [Option<u16>; 2],
    pub analog_balance: [f64; 3],
    pub as_shot_neutral: Option<[f64; 3]>,
    pub as_shot_white_xy: Option<[f64; 2]>,
    /// EV; informational (BaselineExposure), never applied silently.
    pub baseline_exposure: f64,
}

impl DngColor {
    pub fn empty() -> Self {
        Self {
            color_matrix: [None, None],
            forward_matrix: [None, None],
            camera_calibration: [None, None],
            calibration_illuminant: [None, None],
            analog_balance: [1.0; 3],
            as_shot_neutral: None,
            as_shot_white_xy: None,
            baseline_exposure: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraSolution {
    /// Raw camera RGB (no white balance applied) -> XYZ D50.
    pub camera_to_xyz_d50: Mat3,
    /// Camera neutral used (from AsShotNeutral, or derived from AsShotWhiteXY).
    pub neutral: [f64; 3],
    pub white_xy: [f64; 2],
    pub cct: f64,
    pub used_forward_matrix: bool,
    /// Interpolation weight toward illuminant 2 (0 = first profile, 1 = second).
    pub weight: f64,
}

struct Profiles<'a> {
    color: &'a DngColor,
    /// temperatures sorted low -> high, with the source index of each
    order: [usize; 2],
    cct: [f64; 2],
    two: bool,
}

impl<'a> Profiles<'a> {
    fn new(color: &'a DngColor) -> Result<Self, DevelopError> {
        let two = color.color_matrix[1].is_some() || color.forward_matrix[1].is_some();
        if color.color_matrix[0].is_none() && color.color_matrix[1].is_none() {
            return Err(DevelopError::MissingColourData);
        }
        let mut cct = [0.0; 2];
        let mut order = [0, 1];
        if two {
            for (i, slot) in cct.iter_mut().enumerate() {
                let code = color.calibration_illuminant[i].ok_or(DevelopError::MissingColourData)?;
                *slot = illuminant_cct(code)?;
            }
            if cct[0] > cct[1] {
                order = [1, 0];
                cct = [cct[1], cct[0]];
            }
        }
        Ok(Self {
            color,
            order,
            cct,
            two,
        })
    }

    /// Weight toward the higher-temperature profile, linear in mired, clamped to 0..=1.
    fn weight(&self, cct: f64) -> f64 {
        if !self.two || cct <= self.cct[0] {
            0.0
        } else if cct >= self.cct[1] {
            1.0
        } else {
            (1.0 / cct - 1.0 / self.cct[0]) / (1.0 / self.cct[1] - 1.0 / self.cct[0])
        }
    }

    fn blend(&self, set: &[Option<Mat3>; 2], default: Option<Mat3>, w: f64) -> Option<Mat3> {
        let low = set[self.order[0]].or(default);
        let high = set[self.order[1]].or(default);
        match (low, high) {
            (Some(a), Some(b)) if self.two => {
                let mut out = [0.0; 9];
                for i in 0..9 {
                    out[i] = (1.0 - w) * a[i] + w * b[i];
                }
                Some(out)
            }
            (Some(a), _) => Some(a),
            (None, Some(b)) => Some(b),
            _ => None,
        }
    }

    fn xyz_to_camera(&self, w: f64) -> Result<Mat3, DevelopError> {
        let cm = self
            .blend(&self.color.color_matrix, None, w)
            .ok_or(DevelopError::MissingColourData)?;
        let cc = self
            .blend(&self.color.camera_calibration, Some(IDENTITY), w)
            .unwrap_or(IDENTITY);
        Ok(mul(&mul(&diag(self.color.analog_balance), &cc), &cm))
    }

    fn ab_cc(&self, w: f64) -> Mat3 {
        let cc = self
            .blend(&self.color.camera_calibration, Some(IDENTITY), w)
            .unwrap_or(IDENTITY);
        mul(&diag(self.color.analog_balance), &cc)
    }
}

/// Solve camera -> XYZ D50 for the as-shot white. Refuses missing data and non-convergence.
pub fn solve(color: &DngColor) -> Result<CameraSolution, DevelopError> {
    let ab = color.analog_balance;
    if ab.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(DevelopError::NonFinite);
    }
    let profiles = Profiles::new(color)?;
    let (neutral, mut xy) = match (color.as_shot_neutral, color.as_shot_white_xy) {
        (Some(n), _) => {
            if n.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                return Err(DevelopError::NonFinite);
            }
            (Some(n), [1.0 / 3.0, 1.0 / 3.0])
        }
        (None, Some(xy)) => (None, xy),
        (None, None) => return Err(DevelopError::MissingColourData),
    };
    // Fixed point: xy -> CCT -> blended matrices -> XYZ of the neutral -> xy.
    let mut converged = neutral.is_none();
    if let Some(n) = neutral {
        for _ in 0..100 {
            let cct = cct_from_xy(xy)?;
            let w = profiles.weight(cct);
            let xyz = mul_vec(&invert(&profiles.xyz_to_camera(w)?)?, &n);
            let next = xyz_to_xy(&xyz)?;
            let delta = (next[0] - xy[0]).abs() + (next[1] - xy[1]).abs();
            xy = next;
            if delta <= 1.0e-9 {
                converged = true;
                break;
            }
        }
    }
    if !converged || !xy.iter().all(|v| v.is_finite()) {
        return Err(DevelopError::ColorNoConvergence);
    }
    let cct = cct_from_xy(xy)?;
    let w = profiles.weight(cct);
    let m_xyz_to_camera = profiles.xyz_to_camera(w)?;
    let neutral = match neutral {
        Some(n) => n,
        None => {
            let n = mul_vec(&m_xyz_to_camera, &xy_to_xyz(xy));
            [n[0] / n[1], 1.0, n[2] / n[1]]
        }
    };
    let forward = profiles.blend(&color.forward_matrix, None, w);
    let ab_cc = profiles.ab_cc(w);
    let (camera_to_xyz_d50, used_forward_matrix) = match forward {
        Some(fm) => {
            // ReferenceNeutral = inverse(AB*CC) * CameraNeutral; D = diag(1/ReferenceNeutral).
            let inv_abcc = invert(&ab_cc)?;
            let reference = mul_vec(&inv_abcc, &neutral);
            if reference.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                return Err(DevelopError::NonFinite);
            }
            let d = diag([1.0 / reference[0], 1.0 / reference[1], 1.0 / reference[2]]);
            (mul(&mul(&fm, &d), &inv_abcc), true)
        }
        None => {
            let to_xyz = invert(&m_xyz_to_camera)?;
            let adapt = bradford(&xy_to_xyz(xy), &D50_XYZ)?;
            (mul(&adapt, &to_xyz), false)
        }
    };
    Ok(CameraSolution {
        camera_to_xyz_d50,
        neutral,
        white_xy: xy,
        cct,
        used_forward_matrix,
        weight: w,
    })
}
