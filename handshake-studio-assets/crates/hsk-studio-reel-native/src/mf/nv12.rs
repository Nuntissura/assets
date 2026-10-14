//! Pure NV12 -> tight planar 4:2:0 (Y, U, V) conversion of a decoder output buffer, with every layout
//! fact checked before any byte is read (hostile COM output, V4-01/V4-02). Compiled with feature `mf` so
//! it is unit-tested on any OS.
//!
//! Layout (Learn "Uncompressed video buffers", "Image stride"): `alloc_height` luma rows of `stride`
//! bytes, then `alloc_height / 2` rows of interleaved U,V pairs of the same stride; top-down only (a
//! negative stride is refused). The picture is the top-left `width x height` crop.
#![deny(clippy::arithmetic_side_effects)]

/// Why a buffer cannot be converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nv12Error {
    /// Stride zero, negative (bottom-up), or narrower than the crop.
    Stride,
    /// Crop zero, odd-sized chroma beyond the allocation, or taller/wider than the allocation.
    Size,
    /// The buffer holds fewer bytes than the layout needs.
    Bytes,
    /// The output planes would exceed the caller's byte budget.
    Budget,
}

/// Bytes the three tight planes of a `width x height` 4:2:0 picture need, `None` on overflow.
pub(crate) fn i420_bytes(width: u32, height: u32) -> Option<u64> {
    let luma = u64::from(width).checked_mul(u64::from(height))?;
    let chroma = u64::from(width.div_ceil(2)).checked_mul(u64::from(height.div_ceil(2)))?;
    luma.checked_add(chroma.checked_mul(2)?)
}

// Used by the worker (`mf-worker`) and the unit tests; the host build only uses `i420_bytes`.
#[cfg_attr(not(feature = "mf-worker"), allow(dead_code))]
/// Converts the NV12 buffer into `[Y, U, V]` tight planes of the `width x height` crop. `stride` is the
/// row pitch in bytes (as reported, may be negative), `alloc_height` the luma rows of the allocation.
pub(crate) fn nv12_to_i420(
    buffer: &[u8],
    stride: i64,
    alloc_height: u32,
    width: u32,
    height: u32,
    max_bytes: u64,
) -> Result<[Vec<u8>; 3], Nv12Error> {
    if width == 0 || height == 0 || height > alloc_height {
        return Err(Nv12Error::Size);
    }
    let stride = u64::try_from(stride)
        .ok()
        .filter(|stride| *stride >= u64::from(width) && *stride > 0)
        .ok_or(Nv12Error::Stride)?;
    let total = i420_bytes(width, height).ok_or(Nv12Error::Budget)?;
    if total > max_bytes {
        return Err(Nv12Error::Budget);
    }
    let (chroma_w, chroma_h) = (u64::from(width.div_ceil(2)), u64::from(height.div_ceil(2)));
    // Chroma rows needed must exist in the allocation's chroma plane.
    if chroma_h > u64::from(alloc_height.div_ceil(2)) {
        return Err(Nv12Error::Size);
    }
    let uv_offset = stride
        .checked_mul(u64::from(alloc_height))
        .ok_or(Nv12Error::Bytes)?;
    // Last byte read: the last chroma row starts at uv_offset + (chroma_h - 1) * stride and spans
    // 2 * chroma_w bytes; the last luma row ends before uv_offset because height <= alloc_height.
    let needed = chroma_h
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(stride))
        .and_then(|rows| rows.checked_add(uv_offset))
        .and_then(|start| start.checked_add(chroma_w.checked_mul(2)?))
        .ok_or(Nv12Error::Bytes)?;
    if needed > buffer.len() as u64 {
        return Err(Nv12Error::Bytes);
    }
    // Every index below is < `needed` <= buffer.len(), so the usize conversions and slices cannot fail;
    // they still use checked forms.
    let to_usize = |value: u64| usize::try_from(value).map_err(|_| Nv12Error::Bytes);
    let (stride, width_u, height_u) = (to_usize(stride)?, to_usize(u64::from(width))?, to_usize(u64::from(height))?);
    let (chroma_w, chroma_h, uv_offset) = (to_usize(chroma_w)?, to_usize(chroma_h)?, to_usize(uv_offset)?);
    let row = |start: usize, len: usize| {
        start
            .checked_add(len)
            .and_then(|end| buffer.get(start..end))
            .ok_or(Nv12Error::Bytes)
    };
    let mut y = Vec::with_capacity(width_u.saturating_mul(height_u));
    for r in 0..height_u {
        y.extend_from_slice(row(r.checked_mul(stride).ok_or(Nv12Error::Bytes)?, width_u)?);
    }
    let plane = chroma_w.saturating_mul(chroma_h);
    let (mut u, mut v) = (Vec::with_capacity(plane), Vec::with_capacity(plane));
    for r in 0..chroma_h {
        let start = r
            .checked_mul(stride)
            .and_then(|offset| offset.checked_add(uv_offset))
            .ok_or(Nv12Error::Bytes)?;
        for pair in row(start, chroma_w.checked_mul(2).ok_or(Nv12Error::Bytes)?)?.chunks_exact(2) {
            u.push(pair[0]);
            v.push(pair[1]);
        }
    }
    Ok([y, u, v])
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // constant fixture indices
mod tests {
    use super::*;

    /// 4x2 picture in a 6-byte-stride allocation of 4 luma rows: luma bytes are 10*row+col, chroma pairs
    /// (100+i, 200+i); padding bytes are 0xEE and must never appear in the output.
    fn buffer() -> Vec<u8> {
        let mut b = vec![0xEE_u8; 6 * 4 + 6 * 2];
        for r in 0..4 {
            for c in 0..4 {
                b[r * 6 + c] = (10 * r + c) as u8;
            }
        }
        for r in 0..2 {
            for i in 0..2 {
                b[24 + r * 6 + 2 * i] = 100 + (r * 2 + i) as u8;
                b[24 + r * 6 + 2 * i + 1] = 200 + (r * 2 + i) as u8;
            }
        }
        b
    }

    #[test]
    fn crops_and_deinterleaves() {
        let [y, u, v] = nv12_to_i420(&buffer(), 6, 4, 4, 2, 1 << 20).unwrap();
        assert_eq!(y, [0, 1, 2, 3, 10, 11, 12, 13]);
        assert_eq!(u, [100, 101]);
        assert_eq!(v, [200, 201]);
        // Full allocation height.
        let [y, u, v] = nv12_to_i420(&buffer(), 6, 4, 4, 4, 1 << 20).unwrap();
        assert_eq!(y.len(), 16);
        assert_eq!((u, v), (vec![100, 101, 102, 103], vec![200, 201, 202, 203]));
        // Odd crop: 3x3 -> chroma 2x2.
        let [y, u, _] = nv12_to_i420(&buffer(), 6, 4, 3, 3, 1 << 20).unwrap();
        assert_eq!((y.len(), u.len()), (9, 4));
    }

    #[test]
    fn hostile_layouts_are_refused_before_reading() {
        let b = buffer();
        for (stride, alloc, w, h, expected) in [
            (0, 4, 4, 2, Nv12Error::Stride),
            (-6, 4, 4, 2, Nv12Error::Stride),
            (3, 4, 4, 2, Nv12Error::Stride),
            (6, 4, 0, 2, Nv12Error::Size),
            (6, 4, 4, 0, Nv12Error::Size),
            (6, 4, 4, 5, Nv12Error::Size),
            (6, 6, 4, 2, Nv12Error::Bytes),
            (i64::MAX, 4, 4, 2, Nv12Error::Bytes),
            (6, u32::MAX, 4, 2, Nv12Error::Bytes),
        ] {
            assert_eq!(
                nv12_to_i420(&b, stride, alloc, w, h, 1 << 20),
                Err(expected),
                "stride {stride} alloc {alloc} {w}x{h}"
            );
        }
        // dims_u32_max: the planes alone overflow u64, refused before a byte is read.
        assert_eq!(
            nv12_to_i420(&b, i64::MAX, u32::MAX, u32::MAX, u32::MAX, u64::MAX),
            Err(Nv12Error::Budget)
        );
        assert_eq!(
            nv12_to_i420(&b, 1 << 33, u32::MAX, u32::MAX, u32::MAX, 1 << 20),
            Err(Nv12Error::Budget)
        );
        // aggregate_over_budget: exact budget passes, one byte less refuses.
        assert!(nv12_to_i420(&b, 6, 4, 4, 2, 12).is_ok());
        assert_eq!(nv12_to_i420(&b, 6, 4, 4, 2, 11), Err(Nv12Error::Budget));
        // A buffer one byte short of the last chroma pair.
        assert_eq!(
            nv12_to_i420(&b[..b.len() - 3], 6, 4, 4, 4, 1 << 20),
            Err(Nv12Error::Bytes)
        );
        assert_eq!(i420_bytes(u32::MAX, u32::MAX), None);
    }
}
