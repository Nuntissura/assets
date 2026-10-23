//! Reversible integer Le Gall 5/3 synthesis for Canon CRX subbands.
//!
//! Adapted from LightCraft `crates/raw/src/vendor/crx_wavelet.rs` (https://github.com/storytold/lightcraft
//! at af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's `NOTICE`),
//! an independent implementation of the lifting equations of ITU-T T.800 Annex F (also printed in
//! US20030031370A1, paragraph 0047). Each tile level has an even local origin; internal tile
//! boundaries keep the neighbouring low/high coefficients and only outer image edges are symmetric.
//! Changes: checked arithmetic with develop's typed errors (V4-01), bounded allocation (V4-02).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;

/// Largest sample count one wavelet buffer may hold (the CRX image cap).
pub(crate) const MAX_SAMPLES: usize = 1 << 28;

#[derive(Clone, Debug)]
pub(crate) struct Band {
    pub width: usize,
    pub height: usize,
    pub data: Vec<i32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TileEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

fn corrupt() -> DevelopError {
    DevelopError::InvalidContainer
}

fn add(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_add(b).ok_or(DevelopError::Overflow)
}

fn mul(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_mul(b).ok_or(DevelopError::Overflow)
}

fn sample(v: i64) -> Result<i32, DevelopError> {
    i32::try_from(v).map_err(|_| corrupt())
}

/// (low count, high count) of a `length` line with tile neighbours `before` / `after`.
pub(crate) fn sizes(length: usize, before: bool, after: bool) -> (usize, usize) {
    let low = length.div_ceil(2).saturating_add(usize::from(after && length.is_multiple_of(2)));
    let high = (length / 2).saturating_add(usize::from(before)).saturating_add(usize::from(after));
    (low, high)
}

fn inverse_line(low: &[i32], high: &[i32], out: &mut [i32], before: bool, after: bool) -> Result<(), DevelopError> {
    let (low_count, high_count) = sizes(out.len(), before, after);
    if out.is_empty() || low.len() != low_count || high.len() != high_count {
        return Err(corrupt());
    }
    if high.is_empty() {
        *out.first_mut().ok_or_else(corrupt)? = *low.first().ok_or_else(corrupt)?;
        return Ok(());
    }
    // The high-pass storage starts at -1 when there is a preceding tile; only absent external
    // neighbours are reflected at the image boundary.
    let last_high = high.len().saturating_sub(1);
    let high_at = |index: usize| -> Result<i32, DevelopError> { high.get(index.min(last_high)).copied().ok_or_else(corrupt) };
    let shift = usize::from(before);
    let even = |i: usize| -> Result<i32, DevelopError> {
        let left = high_at(add(i, shift)?.saturating_sub(1))?;
        let right = high_at(add(i, shift)?)?;
        let update = i64::from(left).saturating_add(i64::from(right)).saturating_add(2).div_euclid(4);
        sample(i64::from(*low.get(i).ok_or_else(corrupt)?).saturating_sub(update))
    };
    for i in 0..out.len().div_ceil(2) {
        *out.get_mut(mul(i, 2)?).ok_or_else(corrupt)? = even(i)?;
    }
    for i in 0..out.len() / 2 {
        let at = mul(i, 2)?;
        let left = out.get(at).copied().ok_or_else(corrupt)?;
        let right = match out.get(add(at, 2)?).copied() {
            Some(value) => value,
            None if after => even(add(i, 1)?)?,
            None => left,
        };
        let prediction = i64::from(left).saturating_add(i64::from(right)).div_euclid(2);
        let value = high_at(add(i, shift)?)?;
        *out.get_mut(add(at, 1)?).ok_or_else(corrupt)? = sample(i64::from(value).saturating_add(prediction))?;
    }
    Ok(())
}

fn valid_band(band: &Band, width: usize, height: usize) -> bool {
    band.width == width && band.height == height && width.checked_mul(height) == Some(band.data.len())
}

/// A zeroed buffer of at most [`MAX_SAMPLES`], or a typed refusal instead of an abort.
pub(crate) fn zeroed(count: usize) -> Result<Vec<i32>, DevelopError> {
    if count > MAX_SAMPLES {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| DevelopError::InvalidDimensions)?;
    v.resize(count, 0);
    Ok(v)
}

/// One inverse level. `HL` is horizontally high-pass, `LH` vertically high-pass.
fn inverse_level(ll: &Band, hl: &Band, lh: &Band, hh: &Band, width: usize, height: usize, edges: TileEdges) -> Result<Band, DevelopError> {
    let count = mul(width, height)?;
    if count == 0 || count > MAX_SAMPLES {
        return Err(DevelopError::InvalidDimensions);
    }
    let (lw, hw) = sizes(width, edges.left, edges.right);
    let (lhgt, hhgt) = sizes(height, edges.top, edges.bottom);
    if !valid_band(ll, lw, lhgt) || !valid_band(hl, hw, lhgt) || !valid_band(lh, lw, hhgt) || !valid_band(hh, hw, hhgt) {
        return Err(corrupt());
    }
    let rows = add(lhgt, hhgt)?;
    let mut temporary = zeroed(mul(width, rows)?)?;
    // CRX analysis is vertical then horizontal; synthesis reverses that order.
    for y in 0..rows {
        let (lo, hi, by) = if y < lhgt { (ll, hl, y) } else { (lh, hh, y.saturating_sub(lhgt)) };
        inverse_line(
            lo.data.get(mul(by, lw)?..mul(add(by, 1)?, lw)?).ok_or_else(corrupt)?,
            hi.data.get(mul(by, hw)?..mul(add(by, 1)?, hw)?).ok_or_else(corrupt)?,
            temporary.get_mut(mul(y, width)?..mul(add(y, 1)?, width)?).ok_or_else(corrupt)?,
            edges.left,
            edges.right,
        )?;
    }
    let mut data = zeroed(count)?;
    let mut low = zeroed(lhgt)?;
    let mut high = zeroed(hhgt)?;
    let mut column = zeroed(height)?;
    for x in 0..width {
        for (y, value) in low.iter_mut().enumerate() {
            *value = temporary.get(add(mul(y, width)?, x)?).copied().ok_or_else(corrupt)?;
        }
        for (y, value) in high.iter_mut().enumerate() {
            *value = temporary.get(add(mul(add(y, lhgt)?, width)?, x)?).copied().ok_or_else(corrupt)?;
        }
        inverse_line(&low, &high, &mut column, edges.top, edges.bottom)?;
        for (y, &value) in column.iter().enumerate() {
            *data.get_mut(add(mul(y, width)?, x)?).ok_or_else(corrupt)? = value;
        }
    }
    Ok(Band { width, height, data })
}

/// Subbands in CRX order: LLn, HLn, LHn, HHn, ... HL1, LH1, HH1.
pub(crate) fn reconstruct(bands: &[Band], width: usize, height: usize, levels: u8, edges: TileEdges) -> Result<Vec<i32>, DevelopError> {
    let levels = usize::from(levels);
    if levels > 3 || bands.len() != add(1, mul(levels, 3)?)? || width == 0 || height == 0 {
        return Err(corrupt());
    }
    if mul(width, height)? > MAX_SAMPLES {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut dimensions = vec![(width, height)];
    for _ in 0..levels {
        let &(w, h) = dimensions.last().ok_or_else(corrupt)?;
        dimensions.push((sizes(w, edges.left, edges.right).0, sizes(h, edges.top, edges.bottom).0));
    }
    let &(low_width, low_height) = dimensions.last().ok_or_else(corrupt)?;
    if !bands.first().is_some_and(|b| valid_band(b, low_width, low_height)) {
        return Err(corrupt());
    }
    let mut ll = bands.first().cloned().ok_or_else(corrupt)?;
    for step in 0..levels {
        let level = levels.checked_sub(step).and_then(|v| v.checked_sub(1)).ok_or_else(corrupt)?;
        let &(w, h) = dimensions.get(level).ok_or_else(corrupt)?;
        let index = add(1, mul(step, 3)?)?;
        ll = inverse_level(
            &ll,
            bands.get(index).ok_or_else(corrupt)?,
            bands.get(add(index, 1)?).ok_or_else(corrupt)?,
            bands.get(add(index, 2)?).ok_or_else(corrupt)?,
            w,
            h,
            edges,
        )?;
    }
    if !valid_band(&ll, width, height) {
        return Err(corrupt());
    }
    Ok(ll.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed vectors (LightCraft test data): signed rounding, symmetric ends, subband order.
    #[test]
    fn integer_lifting_known_answers() {
        let mut out = [0; 9];
        inverse_line(&[6, 11, 8, 6, 3], &[2, 2, -1, -2], &mut out, false, false).unwrap();
        assert_eq!(out, [5, 9, 10, 11, 8, 6, 7, 3, 4]);
        let mut out = [0; 2];
        inverse_line(&[25], &[20], &mut out, false, false).unwrap();
        assert_eq!(out, [15, 35]);
        let band = |value| Band { width: 1, height: 1, data: vec![value] };
        let out = reconstruct(&[band(25), band(10), band(20), band(0)], 2, 2, 1, TileEdges::default()).unwrap();
        assert_eq!(out, [10, 20, 30, 40]);
        let mut out = [0; 2];
        assert!(inverse_line(&[i32::MAX], &[i32::MIN], &mut out, false, false).is_err());
    }
}
