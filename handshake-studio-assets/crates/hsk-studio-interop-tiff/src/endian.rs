//! Rewrites the little-endian file the `tiff` encoder produces into a big-endian one (TIFF 6.0 section 2:
//! byte-order mark `MM`, every multi-byte header field, IFD field and out-of-line value array swapped).
//! Pixel strips are NOT touched here: the caller already stored the samples in big-endian order
//! (see `encode.rs`, `prepare_big_endian`), so compression sees the bytes a big-endian file holds.
//! The input is this crate's own encoder output; anything off-shape is `encoder_failed`, never a panic.
use crate::error::{cadd, cmul, Result, TiffError};
use std::ops::Range;

const MAX_ENTRIES: u64 = 4096;
const MAX_IFDS: u32 = 8;

fn bad() -> TiffError {
    TiffError::Corrupt("encoder_failed")
}

fn bad_at(code: &'static str) -> TiffError {
    TiffError::Corrupt(code)
}

fn span(len: usize, off: u64, n: u64) -> Result<Range<usize>> {
    let end = cadd(off, n)?;
    if end > u64::try_from(len).map_err(|_| bad())? {
        return Err(bad_at("encoder_endian_bounds"));
    }
    Ok(usize::try_from(off).map_err(|_| bad())?..usize::try_from(end).map_err(|_| bad())?)
}

fn read_le(b: &[u8], off: u64, n: u64) -> Result<u64> {
    let r = span(b.len(), off, n)?;
    let mut v = 0_u64;
    for byte in b[r].iter().rev() {
        v = v.checked_shl(8).ok_or_else(bad)? | u64::from(*byte);
    }
    Ok(v)
}

fn swap(b: &mut [u8], off: u64, n: u64, unit: usize) -> Result<()> {
    let r = span(b.len(), off, n)?;
    for chunk in b[r].chunks_exact_mut(unit) {
        chunk.reverse();
    }
    Ok(())
}

/// (bytes per element, swap unit) of a TIFF field type.
fn field(ty: u64) -> Result<(u64, usize)> {
    Ok(match ty {
        1 | 2 | 6 | 7 => (1, 1),
        3 | 8 => (2, 2),
        4 | 9 | 11 | 13 => (4, 4),
        5 | 10 => (8, 4),
        12 | 16 | 17 | 18 => (8, 8),
        _ => return Err(bad_at("encoder_endian_field_type")),
    })
}

pub(crate) fn to_big_endian(le: &[u8], big: bool) -> Result<Vec<u8>> {
    let mut b = le.to_vec();
    let (ptr_w, entry_w, count_w, head): (u64, u64, u64, u64) = if big { (8, 20, 8, 8) } else { (4, 12, 2, 2) };
    let mut ifd = if big {
        if b.len() < 16 || b[..4] != [b'I', b'I', 43, 0] || read_le(&b, 4, 2)? != 8 {
            return Err(bad());
        }
        let off = read_le(&b, 8, 8)?;
        b[..8].copy_from_slice(&[b'M', b'M', 0, 43, 0, 8, 0, 0]);
        swap(&mut b, 8, 8, 8)?;
        off
    } else {
        if b.len() < 8 || b[..4] != [b'I', b'I', 42, 0] {
            return Err(bad());
        }
        let off = read_le(&b, 4, 4)?;
        b[..4].copy_from_slice(&[b'M', b'M', 0, 42]);
        swap(&mut b, 4, 4, 4)?;
        off
    };
    let mut seen = 0_u32;
    while ifd != 0 {
        seen = seen.checked_add(1).ok_or_else(bad)?;
        if seen > MAX_IFDS {
            return Err(bad());
        }
        let count = read_le(&b, ifd, count_w)?;
        if count > MAX_ENTRIES {
            return Err(bad());
        }
        swap(&mut b, ifd, count_w, usize::try_from(count_w).map_err(|_| bad())?)?;
        let first = cadd(ifd, head)?;
        for i in 0..count {
            let e = cadd(first, cmul(i, entry_w)?)?;
            let tag_ty = cadd(e, 2)?;
            let n_at = cadd(e, 4)?;
            let v_at = cadd(n_at, ptr_w)?;
            let ty = read_le(&b, tag_ty, 2)?;
            let n = read_le(&b, n_at, ptr_w)?;
            swap(&mut b, e, 2, 2)?;
            swap(&mut b, tag_ty, 2, 2)?;
            swap(&mut b, n_at, ptr_w, usize::try_from(ptr_w).map_err(|_| bad())?)?;
            let (size, unit) = field(ty)?;
            let total = cmul(size, n)?;
            if total <= ptr_w {
                swap(&mut b, v_at, total, unit)?;
            } else {
                let ptr = read_le(&b, v_at, ptr_w)?;
                swap(&mut b, v_at, ptr_w, usize::try_from(ptr_w).map_err(|_| bad())?)?;
                swap(&mut b, ptr, total, unit)?;
            }
        }
        let next_at = cadd(first, cmul(count, entry_w)?)?;
        let next = read_le(&b, next_at, ptr_w)?;
        swap(&mut b, next_at, ptr_w, usize::try_from(ptr_w).map_err(|_| bad())?)?;
        ifd = next;
    }
    Ok(b)
}
