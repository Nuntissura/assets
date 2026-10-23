//! Creative looks (lane SMB-DEVELOP-LENS-PRESETS slice 4; STU-RAW-126 creative profiles and look
//! tables, STU-RAW-013 profile stage before the slider edits): a `.cube` 3D/1D table parsed and
//! evaluated by Prism ([`hsk_studio_prism::CubeLut`], tetrahedral interpolation) with an amount.
//!
//! Placement: per pixel at the start of stage C, after calibration and before the white-balance
//! shift and tone controls (the profile stage precedes slider edits). Domain: `srgb_encoded` (default;
//! the common creative-LUT convention) evaluates the table on the signed sRGB-encoded working value
//! and decodes the result, `linear` evaluates on linear values. Inputs outside the table's domain are
//! clamped by the table (HDR values above the domain lose their excess: documented, not hidden).
//! Amount 0..=2 (`look_amount`, default 1; spec observed 0.3..1.7, so above-full strength exists):
//! `x + amount (lut(x) - x)` in the table's domain.
use crate::DevelopError;
use crate::adjust::{dec, enc};
use hsk_studio_prism::CubeLut;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LookDomain {
    #[default]
    SrgbEncoded,
    Linear,
}

impl LookDomain {
    pub const fn key(self) -> &'static str {
        match self {
            Self::SrgbEncoded => "srgb_encoded",
            Self::Linear => "linear",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::SrgbEncoded, Self::Linear].into_iter().find(|d| d.key() == key)
    }
}

/// A creative look bound to a recipe. The table is content-addressed by `lut.sha256`.
#[derive(Clone, Debug, PartialEq)]
pub struct CreativeLook {
    pub name: String,
    pub lut: Arc<CubeLut>,
    /// 0..=2, default 1.
    pub amount: f64,
    pub domain: LookDomain,
}

impl CreativeLook {
    /// Parse `.cube` bytes into a look (Prism refusals become `invalid_lut`).
    pub fn from_cube(name: &str, bytes: &[u8], domain: LookDomain) -> Result<Self, DevelopError> {
        let lut = hsk_studio_prism::parse_cube(bytes).map_err(|_| DevelopError::InvalidLut)?;
        Ok(Self { name: name.to_owned(), lut: Arc::new(lut), amount: 1.0, domain })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedLook {
    lut: Arc<CubeLut>,
    amount: f64,
    domain: LookDomain,
}

impl PreparedLook {
    pub(crate) fn prepare(look: &Option<CreativeLook>) -> Result<Option<Self>, DevelopError> {
        let Some(l) = look else { return Ok(None) };
        if !l.amount.is_finite() {
            return Err(DevelopError::NonFinite);
        }
        if !(0.0..=2.0).contains(&l.amount) {
            return Err(DevelopError::InvalidAdjustment);
        }
        let lut = &l.lut;
        let rows_ok = lut.lut_3d.len() == lut.size_3d.saturating_mul(lut.size_3d).saturating_mul(lut.size_3d)
            && (lut.size_3d >= 2 || lut.lut_1d.is_some())
            && lut.lut_1d.as_ref().is_none_or(|r| r.len() >= 2);
        if !rows_ok || lut.domain_min.iter().zip(&lut.domain_max).any(|(a, b)| !a.is_finite() || !b.is_finite() || a >= b) {
            return Err(DevelopError::InvalidLut);
        }
        if l.amount == 0.0 {
            return Ok(None);
        }
        Ok(Some(Self { lut: Arc::clone(&l.lut), amount: l.amount, domain: l.domain }))
    }

    pub(crate) fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let x = match self.domain {
            LookDomain::SrgbEncoded => [enc(v[0]), enc(v[1]), enc(v[2])],
            LookDomain::Linear => v,
        };
        let y = self.lut.apply(x);
        let mixed: [f64; 3] = std::array::from_fn(|c| x[c] + self.amount * (y[c] - x[c]));
        match self.domain {
            LookDomain::SrgbEncoded => [dec(mixed[0]), dec(mixed[1]), dec(mixed[2])],
            LookDomain::Linear => mixed,
        }
    }
}
