//! Colour lookup tables in the Cube format (lane SMB-DEVELOP-LENS-PRESETS; consumed by develop's
//! creative looks, STU-RAW-126, and available to the colour-lookup adjustment, STU-RAS-167): a
//! tokenizer for the public Adobe "Cube LUT Specification 1.0" (2013) text format as also written by
//! DaVinci Resolve, and tetrahedral interpolation.
//!
//! Format accepted (tokenised per line; keywords are case-sensitive as the specification writes them):
//! * `#` starts a comment line (a `#` inside the quoted `TITLE` string is text, not a comment);
//! * `TITLE "text"` (at most once, quoted, before the data);
//! * `LUT_1D_SIZE n` (2..=65536) and/or `LUT_3D_SIZE n` (2..=[`MAX_LUT_3D_SIZE`]); at least one;
//! * `DOMAIN_MIN r g b`, `DOMAIN_MAX r g b` (default 0 and 1, min < max per channel), and the
//!   Resolve forms `LUT_1D_INPUT_RANGE lo hi` / `LUT_3D_INPUT_RANGE lo hi` (same domain on all
//!   channels);
//! * then exactly `n` (1D) and/or `n^3` (3D) data lines of three finite numbers; with both tables the
//!   1D rows come first (Resolve shaper convention) and run before the 3D table. 3D rows are ordered
//!   with red changing fastest, then green, then blue.
//!
//! Evaluation: input clamped to the domain and normalised to 0..1; 1D tables interpolate linearly
//! per channel; the 3D table uses tetrahedral interpolation (the six tetrahedra of the unit cube
//! split along the main diagonal, as in LittleCMS 2 and Prism's ICC CLUT evaluator), exact f64
//! weights. A cube whose rows equal their own grid coordinates is the identity at every input
//! (tetrahedral interpolation reproduces any affine function exactly).
//!
//! Hostile input (V4-01/02/08): the byte size is checked first, then each declared size BEFORE the
//! first data row is stored (aggregate `size^3 * 3` floats against [`MAX_LUT_VALUES`]; storage then
//! grows only with the rows actually present), lines are
//! bounded, every number must be finite; every refusal is a typed [`LutError`].
#![deny(clippy::arithmetic_side_effects)]

use sha2::{Digest, Sha256};

/// Largest accepted `.cube` document (bytes).
pub const MAX_CUBE_BYTES: usize = 64 << 20;
/// Largest accepted `LUT_3D_SIZE` (the common sizes are 17, 33, 64 and 65).
pub const MAX_LUT_3D_SIZE: usize = 129;
/// Largest accepted `LUT_1D_SIZE` (the specification's own limit).
pub const MAX_LUT_1D_SIZE: usize = 65_536;
/// Aggregate number of stored values (1D plus 3D, three per row).
pub const MAX_LUT_VALUES: usize = 3 * 129 * 129 * 129 + 3 * 65_536;
const MAX_LINE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutError {
    TooLarge,
    NotText,
    LineTooLong,
    UnknownKeyword,
    DuplicateKeyword,
    BadNumber,
    BadSize,
    BadDomain,
    BadTitle,
    KeywordAfterData,
    MissingSize,
    WrongRowCount,
}

impl LutError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "lut_too_large",
            Self::NotText => "lut_not_text",
            Self::LineTooLong => "lut_line_too_long",
            Self::UnknownKeyword => "lut_unknown_keyword",
            Self::DuplicateKeyword => "lut_duplicate_keyword",
            Self::BadNumber => "lut_bad_number",
            Self::BadSize => "lut_bad_size",
            Self::BadDomain => "lut_bad_domain",
            Self::BadTitle => "lut_bad_title",
            Self::KeywordAfterData => "lut_keyword_after_data",
            Self::MissingSize => "lut_missing_size",
            Self::WrongRowCount => "lut_wrong_row_count",
        }
    }
}

impl std::fmt::Display for LutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LutError {}

/// A parsed `.cube` table.
#[derive(Clone, Debug, PartialEq)]
pub struct CubeLut {
    pub title: Option<String>,
    pub domain_min: [f64; 3],
    pub domain_max: [f64; 3],
    /// 1D shaper rows (`LUT_1D_SIZE`), if any.
    pub lut_1d: Option<Vec<[f32; 3]>>,
    /// 3D grid size and rows (red fastest), if any.
    pub size_3d: usize,
    pub lut_3d: Vec<[f32; 3]>,
    /// SHA-256 of the exact source bytes (content address, STU-RAW-126).
    pub sha256: [u8; 32],
}

fn number(t: &str) -> Result<f64, LutError> {
    let v: f64 = t.parse().map_err(|_| LutError::BadNumber)?;
    if v.is_finite() { Ok(v) } else { Err(LutError::BadNumber) }
}

fn triple(rest: &[&str]) -> Result<[f64; 3], LutError> {
    match rest {
        [a, b, c] => Ok([number(a)?, number(b)?, number(c)?]),
        _ => Err(LutError::BadNumber),
    }
}

fn size(rest: &[&str], max: usize) -> Result<usize, LutError> {
    let [t] = rest else { return Err(LutError::BadSize) };
    let n: usize = t.parse().map_err(|_| LutError::BadSize)?;
    if (2..=max).contains(&n) { Ok(n) } else { Err(LutError::BadSize) }
}

fn once<T>(slot: &mut Option<T>, v: T) -> Result<(), LutError> {
    if slot.is_some() {
        return Err(LutError::DuplicateKeyword);
    }
    *slot = Some(v);
    Ok(())
}

/// Parse a `.cube` document (see the module documentation).
pub fn parse_cube(bytes: &[u8]) -> Result<CubeLut, LutError> {
    if bytes.len() > MAX_CUBE_BYTES {
        return Err(LutError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| LutError::NotText)?;
    let mut title: Option<String> = None;
    let (mut n1, mut n3): (Option<usize>, Option<usize>) = (None, None);
    let (mut dmin, mut dmax): (Option<[f64; 3]>, Option<[f64; 3]>) = (None, None);
    let mut rows: Vec<[f32; 3]> = Vec::new();
    let mut expected = 0usize;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.len() > MAX_LINE {
            return Err(LutError::LineTooLong);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let first = trimmed.split_ascii_whitespace().next().unwrap_or("");
        let is_data = first.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.');
        if is_data {
            if expected == 0 {
                // First data row: the header is complete; check and reserve the declared rows.
                let one = n1.unwrap_or(0);
                let cube = match n3 {
                    Some(n) => n.checked_mul(n).and_then(|v| v.checked_mul(n)).ok_or(LutError::BadSize)?,
                    None => 0,
                };
                expected = one.checked_add(cube).ok_or(LutError::BadSize)?;
                if expected == 0 {
                    return Err(LutError::MissingSize);
                }
                if expected.checked_mul(3).is_none_or(|v| v > MAX_LUT_VALUES) {
                    return Err(LutError::BadSize);
                }
                // No up-front reservation: storage grows with the rows actually present, so a
                // short file declaring a large table never allocates the declared size.
            }
            if rows.len() >= expected {
                return Err(LutError::WrongRowCount);
            }
            let parts: Vec<&str> = trimmed.split_ascii_whitespace().collect();
            let [r, g, b] = triple(&parts)?;
            rows.push([r as f32, g as f32, b as f32]);
            if !rows.last().is_some_and(|v| v.iter().all(|x| x.is_finite())) {
                return Err(LutError::BadNumber);
            }
            continue;
        }
        if expected != 0 {
            return Err(LutError::KeywordAfterData);
        }
        let rest: Vec<&str> = trimmed.split_ascii_whitespace().skip(1).collect();
        match first {
            "TITLE" => {
                // The title is the quoted string after the keyword; anything after the closing quote
                // is refused (a decoy keyword cannot hide there).
                let after = trimmed.get(5..).ok_or(LutError::BadTitle)?.trim();
                let inner = after.strip_prefix('"').and_then(|s| s.strip_suffix('"')).ok_or(LutError::BadTitle)?;
                if inner.contains('"') {
                    return Err(LutError::BadTitle);
                }
                once(&mut title, inner.to_owned())?;
            }
            "LUT_1D_SIZE" => once(&mut n1, size(&rest, MAX_LUT_1D_SIZE)?)?,
            "LUT_3D_SIZE" => once(&mut n3, size(&rest, MAX_LUT_3D_SIZE)?)?,
            "DOMAIN_MIN" => once(&mut dmin, triple(&rest)?)?,
            "DOMAIN_MAX" => once(&mut dmax, triple(&rest)?)?,
            "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                let [lo, hi] = match rest.as_slice() {
                    [a, b] => [number(a)?, number(b)?],
                    _ => return Err(LutError::BadNumber),
                };
                once(&mut dmin, [lo; 3])?;
                once(&mut dmax, [hi; 3])?;
            }
            _ => return Err(LutError::UnknownKeyword),
        }
    }
    if n1.is_none() && n3.is_none() {
        return Err(LutError::MissingSize);
    }
    if rows.len() != expected || expected == 0 {
        return Err(LutError::WrongRowCount);
    }
    let domain_min = dmin.unwrap_or([0.0; 3]);
    let domain_max = dmax.unwrap_or([1.0; 3]);
    if domain_min.iter().zip(&domain_max).any(|(a, b)| a >= b) {
        return Err(LutError::BadDomain);
    }
    let one = n1.unwrap_or(0);
    let lut_3d = rows.split_off(one);
    Ok(CubeLut {
        title,
        domain_min,
        domain_max,
        lut_1d: n1.map(|_| rows),
        size_3d: n3.unwrap_or(0),
        lut_3d,
        sha256: Sha256::digest(bytes).into(),
    })
}

/// Grid index and fraction of `t` in 0..=1 on `n` points.
fn locate(t: f64, n: usize) -> (usize, f64) {
    let last = n.saturating_sub(1);
    let x = t * last as f64;
    let i = (x.floor() as usize).min(last.saturating_sub(1));
    (i, x - i as f64)
}

impl CubeLut {
    /// Identity table of `size` (rows equal their grid coordinates).
    pub fn identity(size: usize) -> Result<Self, LutError> {
        if !(2..=MAX_LUT_3D_SIZE).contains(&size) {
            return Err(LutError::BadSize);
        }
        let last = size.saturating_sub(1) as f32;
        let mut rows = Vec::with_capacity(size.saturating_mul(size).saturating_mul(size));
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    rows.push([r as f32 / last, g as f32 / last, b as f32 / last]);
                }
            }
        }
        Ok(Self { title: None, domain_min: [0.0; 3], domain_max: [1.0; 3], lut_1d: None, size_3d: size, lut_3d: rows, sha256: [0; 32] })
    }

    fn row(&self, r: usize, g: usize, b: usize) -> [f64; 3] {
        let n = self.size_3d;
        let i = b.saturating_mul(n).saturating_add(g).saturating_mul(n).saturating_add(r);
        let v = self.lut_3d.get(i).copied().unwrap_or([0.0; 3]);
        [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
    }

    /// Evaluate one RGB value (domain clamp, 1D shaper, tetrahedral 3D).
    pub fn apply(&self, rgb: [f64; 3]) -> [f64; 3] {
        let mut t = [0.0f64; 3];
        for c in 0..3 {
            let (lo, hi) = (self.domain_min[c], self.domain_max[c]);
            let v = if rgb[c].is_nan() { lo } else { rgb[c].clamp(lo, hi) };
            t[c] = (v - lo) / (hi - lo);
        }
        if let Some(rows) = &self.lut_1d {
            for (c, slot) in t.iter_mut().enumerate() {
                let (i, f) = locate(*slot, rows.len());
                let a = f64::from(rows.get(i).map_or(0.0, |r| r[c]));
                let b = f64::from(rows.get(i.saturating_add(1)).map_or(0.0, |r| r[c]));
                *slot = a + f * (b - a);
            }
            if self.size_3d == 0 {
                return t;
            }
            for slot in &mut t {
                *slot = slot.clamp(0.0, 1.0);
            }
        }
        let n = self.size_3d;
        let (r0, fr) = locate(t[0], n);
        let (g0, fg) = locate(t[1], n);
        let (b0, fb) = locate(t[2], n);
        let (r1, g1, b1) = (r0.saturating_add(1), g0.saturating_add(1), b0.saturating_add(1));
        let c000 = self.row(r0, g0, b0);
        let c111 = self.row(r1, g1, b1);
        // Six tetrahedra; (c1, c2, c3) are the corner differences along the ordered axes.
        let (w, p1, p2) = if fr >= fg && fg >= fb {
            ([fr, fg, fb], self.row(r1, g0, b0), self.row(r1, g1, b0))
        } else if fr >= fb && fb >= fg {
            ([fr, fb, fg], self.row(r1, g0, b0), self.row(r1, g0, b1))
        } else if fb >= fr && fr >= fg {
            ([fb, fr, fg], self.row(r0, g0, b1), self.row(r1, g0, b1))
        } else if fg >= fr && fr >= fb {
            ([fg, fr, fb], self.row(r0, g1, b0), self.row(r1, g1, b0))
        } else if fg >= fb && fb >= fr {
            ([fg, fb, fr], self.row(r0, g1, b0), self.row(r0, g1, b1))
        } else {
            ([fb, fg, fr], self.row(r0, g0, b1), self.row(r0, g1, b1))
        };
        std::array::from_fn(|c| c000[c] + w[0] * (p1[c] - c000[c]) + w[1] * (p2[c] - p1[c]) + w[2] * (c111[c] - p2[c]))
    }

    /// The text cube form of this table (STU-COL-159 "Studio MUST export the text cube format";
    /// lane SMB-PREMIERE-COLOR-TITLES, additive): `TITLE` when set, the size keyword(s), both
    /// domain lines, then the 1D rows before the 3D rows (the order [`parse_cube`] reads), every
    /// number in Rust's shortest round-trip text, so `parse_cube(write_cube())` returns the same
    /// table. A title holding a `"` or a control character is written without those characters.
    pub fn write_cube(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        if let Some(t) = &self.title {
            let clean: String = t.chars().filter(|c| *c != '"' && !c.is_control()).collect();
            let _ = writeln!(s, "TITLE \"{clean}\"");
        }
        if let Some(rows) = &self.lut_1d {
            let _ = writeln!(s, "LUT_1D_SIZE {}", rows.len());
        }
        if self.size_3d > 0 {
            let _ = writeln!(s, "LUT_3D_SIZE {}", self.size_3d);
        }
        let [a, b, c] = self.domain_min;
        let _ = writeln!(s, "DOMAIN_MIN {a:?} {b:?} {c:?}");
        let [a, b, c] = self.domain_max;
        let _ = writeln!(s, "DOMAIN_MAX {a:?} {b:?} {c:?}");
        for [r, g, b] in self.lut_1d.iter().flatten().chain(&self.lut_3d) {
            let _ = writeln!(s, "{r:?} {g:?} {b:?}");
        }
        s
    }
}
