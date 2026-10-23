//! Byte-plane shuffle prefilter (HDF5/Blosc style): for `n` elements of `elem` bytes the output is
//! all first bytes, then all second bytes, and so on; a trailing partial element is kept verbatim.
//! Grouping like-significance bytes makes smooth f32/f16/u16 sample data compress; the transform is
//! exactly invertible for every input length.

/// Shuffles `data` into byte planes of `elem`-byte elements (`elem < 2` is the identity).
pub fn byte_shuffle(data: &[u8], elem: usize) -> Vec<u8> {
    if elem < 2 {
        return data.to_vec();
    }
    let whole = data.len() / elem * elem;
    let mut out = Vec::with_capacity(data.len());
    for plane in 0..elem {
        out.extend(data[..whole].chunks_exact(elem).map(|e| e[plane]));
    }
    out.extend_from_slice(&data[whole..]);
    out
}

/// Exact inverse of [`byte_shuffle`].
pub fn byte_unshuffle(data: &[u8], elem: usize) -> Vec<u8> {
    if elem < 2 {
        return data.to_vec();
    }
    let count = data.len() / elem;
    let whole = count * elem;
    let mut out = vec![0u8; data.len()];
    for plane in 0..elem {
        let source = &data[plane * count..(plane + 1) * count];
        for (element, byte) in out[..whole].chunks_exact_mut(elem).zip(source) {
            element[plane] = *byte;
        }
    }
    out[whole..].copy_from_slice(&data[whole..]);
    out
}
