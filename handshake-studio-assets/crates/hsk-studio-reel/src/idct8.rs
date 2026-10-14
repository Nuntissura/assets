//! The one 8x8 inverse DCT of the crate's Studio-owned intra codecs (VC-3 per SMPTE ST 2019-1 8.2.8.1,
//! ProRes per SMPTE RDD 36 7.4); both define the same transform
//! `x(i,j) = 1/4 * sum_u sum_v C(u) C(v) X(u,v) cos((2i+1)u pi/16) cos((2j+1)v pi/16)`, `C(0) = 1/sqrt(2)`,
//! `C(k) = 1` otherwise, with `u`, `i` horizontal and `v`, `j` vertical.
//!
//! Determinism contract: f64, separable (horizontal pass per row, then vertical pass per column),
//! summation in ascending frequency order, plain `a * b + c` (never `mul_add`), basis values from the
//! constant table `BASIS` (never `f64::cos` at run time, whose libm result can differ by host). The
//! output is therefore bit-identical on every IEEE-754 binary64 host (test
//! `idct_is_bit_identical_across_calls_and_matches_the_definition`). Neither standard makes an IDCT
//! implementation normative to the bit; this one is Studio's pinned reference, so a change that can alter
//! one output bit bumps the decoder versions that use it.
#![deny(clippy::arithmetic_side_effects)]

/// `BASIS[u][i] = C(u) / 2 * cos((2i + 1) u pi / 16)`; the two factors 1/2 make the 1/4 of the definition.
/// Values are the f64 nearest to the exact basis (test `basis_table_matches_the_definition`).
#[allow(clippy::excessive_precision, clippy::unreadable_literal)]
pub(crate) const BASIS: [[f64; 8]; 8] = [
    [
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
        0.35355339059327373,
    ],
    [
        0.4903926402016152,
        0.4157348061512726,
        0.27778511650980114,
        0.09754516100806417,
        -0.0975451610080641,
        -0.277785116509801,
        -0.4157348061512727,
        -0.4903926402016152,
    ],
    [
        0.46193976625564337,
        0.19134171618254492,
        -0.19134171618254486,
        -0.46193976625564337,
        -0.4619397662556434,
        -0.19134171618254517,
        0.191341716182545,
        0.46193976625564326,
    ],
    [
        0.4157348061512726,
        -0.0975451610080641,
        -0.4903926402016152,
        -0.2777851165098011,
        0.2777851165098009,
        0.4903926402016152,
        0.09754516100806439,
        -0.41573480615127256,
    ],
    [
        0.3535533905932738,
        -0.35355339059327373,
        -0.35355339059327384,
        0.3535533905932737,
        0.35355339059327384,
        -0.35355339059327334,
        -0.35355339059327356,
        0.3535533905932733,
    ],
    [
        0.27778511650980114,
        -0.4903926402016152,
        0.09754516100806415,
        0.4157348061512728,
        -0.41573480615127256,
        -0.09754516100806401,
        0.4903926402016153,
        -0.27778511650980076,
    ],
    [
        0.19134171618254492,
        -0.4619397662556434,
        0.46193976625564326,
        -0.19134171618254495,
        -0.19134171618254528,
        0.46193976625564337,
        -0.4619397662556432,
        0.19134171618254478,
    ],
    [
        0.09754516100806417,
        -0.2777851165098011,
        0.4157348061512728,
        -0.4903926402016153,
        0.49039264020161527,
        -0.4157348061512725,
        0.27778511650980076,
        -0.09754516100806429,
    ],
];

/// Inverse DCT of `coeffs[v][u]` into `out[j][i]` (row = vertical index). Floating-point arithmetic only
/// (no integer overflow is possible: `arithmetic_side_effects` does not apply to f64).
pub(crate) fn idct8x8(coeffs: &[[i32; 8]; 8], out: &mut [[f64; 8]; 8]) {
    // Horizontal pass: tmp[v][i] = sum_u BASIS[u][i] * X(u, v).
    let mut tmp = [[0.0f64; 8]; 8];
    for (row_in, row_out) in coeffs.iter().zip(tmp.iter_mut()) {
        for (i, cell) in row_out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (u, value) in row_in.iter().enumerate() {
                acc += BASIS[u][i] * f64::from(*value);
            }
            *cell = acc;
        }
    }
    // Vertical pass: out[j][i] = sum_v BASIS[v][j] * tmp[v][i].
    for (j, row_out) in out.iter_mut().enumerate() {
        for (i, cell) in row_out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (v, row) in tmp.iter().enumerate() {
                acc += BASIS[v][j] * row[i];
            }
            *cell = acc;
        }
    }
}

/// Studio's sample rounding for the IDCT output: nearest integer, ties toward +infinity
/// (`floor(x + 0.5)`), saturated to i32 (an f64 that does not fit cannot come from a bounded block and is
/// clamped by the caller anyway).
pub(crate) fn round_half_up(x: f64) -> i32 {
    let r = (x + 0.5).floor();
    if r >= f64::from(i32::MAX) {
        i32::MAX
    } else if r <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        r as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_table_matches_the_definition() {
        for (u, row) in BASIS.iter().enumerate() {
            let c = if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            } / 2.0;
            for (i, value) in row.iter().enumerate() {
                let exact =
                    c * ((2.0 * i as f64 + 1.0) * u as f64 * std::f64::consts::PI / 16.0).cos();
                assert!((value - exact).abs() <= 1e-15, "u {u} i {i}");
            }
        }
    }

    #[test]
    fn idct_is_bit_identical_across_calls_and_matches_the_definition() {
        let mut coeffs = [[0i32; 8]; 8];
        for (v, row) in coeffs.iter_mut().enumerate() {
            for (u, cell) in row.iter_mut().enumerate() {
                *cell = ((u * 37 + v * 11) % 41) as i32 - 20;
            }
        }
        let mut a = [[0.0; 8]; 8];
        let mut b = [[0.0; 8]; 8];
        idct8x8(&coeffs, &mut a);
        idct8x8(&coeffs, &mut b);
        assert_eq!(
            a.map(|r| r.map(f64::to_bits)),
            b.map(|r| r.map(f64::to_bits))
        );
        // Direct double sum of the definition agrees to well under one output step.
        for j in 0..8 {
            for i in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        s += BASIS[u][i] * BASIS[v][j] * f64::from(coeffs[v][u]);
                    }
                }
                assert!((s - a[j][i]).abs() < 1e-9);
            }
        }
        // DC only: x = X(0,0) / 8 everywhere.
        let mut dc = [[0i32; 8]; 8];
        dc[0][0] = 64;
        idct8x8(&dc, &mut a);
        for row in a {
            for x in row {
                assert!((x - 8.0).abs() < 1e-12);
            }
        }
        assert_eq!(round_half_up(2.5), 3);
        assert_eq!(round_half_up(-2.5), -2);
        assert_eq!(round_half_up(-2.6), -3);
        assert_eq!(round_half_up(1e300), i32::MAX);
    }
}
