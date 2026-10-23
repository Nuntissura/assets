//! VC-3 DCT block entropy decoding (SMPTE ST 2019-1 8.2.1-8.2.7, Annex A): DC differential codeword
//! (8.2.4, Figure 46), AC codewords with sign, index and zero run (8.2.5, Figure 47, Figure A.2),
//! inverse zig-zag (8.2.6, Figure 48) and inverse quantization (8.2.7, equation 8.1).
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use crate::bitreader::BitReader;
use crate::error::ReelError;

/// One amplitude codeword (Annex E tables E.1, E.4, ...): `amp == 0` is the EOB codeword.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AcCode {
    /// Codeword left-aligned to 16 bits.
    pub start: u16,
    pub len: u8,
    pub amp: u8,
    pub run: bool,
    pub index: bool,
}

/// One run-length or DC-size codeword.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Vlc {
    pub start: u16,
    pub len: u8,
    pub value: u8,
}

trait Code {
    fn start(&self) -> u16;
    fn len(&self) -> u8;
}

impl Code for AcCode {
    fn start(&self) -> u16 {
        self.start
    }
    fn len(&self) -> u8 {
        self.len
    }
}

impl Code for Vlc {
    fn start(&self) -> u16 {
        self.start
    }
    fn len(&self) -> u8 {
        self.len
    }
}

fn corrupt() -> ReelError {
    ReelError::Corrupt {
        what: "vc3-entropy",
    }
}

/// Decode one codeword of a table sorted by `start`: the entry whose 16-bit left-aligned range holds the
/// next 16 bits. The codeword is consumed only when all of its bits exist in the reader.
fn lookup<T: Code + Copy>(table: &[T], reader: &mut BitReader<'_>) -> Result<T, ReelError> {
    let peek = reader.peek16();
    let at = table.partition_point(|e| e.start() <= peek);
    let entry = at
        .checked_sub(1)
        .and_then(|i| table.get(i))
        .copied()
        .ok_or_else(corrupt)?;
    let len = u32::from(entry.len());
    // The prefix of `peek` must equal the codeword (the tables are complete, so this always holds for a
    // well-formed table; it is checked rather than assumed).
    let shift = 16u32.checked_sub(len).ok_or_else(corrupt)?;
    if (peek ^ entry.start()).checked_shr(shift).unwrap_or(0) != 0 {
        return Err(corrupt());
    }
    reader.skip(len).ok_or_else(corrupt)?;
    Ok(entry)
}

/// Zig-zag order (Figure 48): `ZIGZAG[r] = (u, v)`, bitstream index `r` -> horizontal `u`, vertical `v`.
pub(crate) const ZIGZAG: [(u8, u8); 64] = zigzag();

const fn zigzag() -> [(u8, u8); 64] {
    // Figure 48 printed row by row (row = v, column = u): the bitstream index at each position.
    const FIG48: [[u8; 8]; 8] = [
        [0, 1, 5, 6, 14, 15, 27, 28],
        [2, 4, 7, 13, 16, 26, 29, 42],
        [3, 8, 12, 17, 25, 30, 41, 43],
        [9, 11, 18, 24, 31, 40, 44, 53],
        [10, 19, 23, 32, 39, 45, 52, 54],
        [20, 22, 33, 38, 46, 51, 55, 60],
        [21, 34, 37, 47, 50, 56, 59, 61],
        [35, 36, 48, 49, 57, 58, 62, 63],
    ];
    let mut out = [(0u8, 0u8); 64];
    let mut v = 0;
    while v < 8 {
        let mut u = 0;
        while u < 8 {
            #[allow(clippy::indexing_slicing)] // const loop bounded by 8; r < 64 by the table
            {
                out[FIG48[v][u] as usize] = (u as u8, v as u8);
            }
            u = u.saturating_add(1);
        }
        v = v.saturating_add(1);
    }
    out
}

/// The code tables and quantization parameters of one Compression ID for one component class.
#[derive(Clone, Copy)]
pub(crate) struct BlockTables {
    pub ac: &'static [AcCode],
    pub run: &'static [Vlc],
    pub dc: &'static [Vlc],
    /// Bits of the amplitude index P (Annex A.1: 4 at 8-bit sampling, 6 at 10/12-bit).
    pub index_bits: u32,
    /// Inverse quantization parameter p (8.2.7: 8 for CIDs 1235, 1241, 1250, else 32).
    pub p: i64,
}

/// Decode one DCT block into dequantized coefficients `out[v][u]` (DC unquantized, 8.2.7). `pred` is the
/// DC predictor of the block's component type (8.2.4), updated here. `weights[v][u]` is the component's
/// weight table; `qsf` the macroblock's quantization scale factor.
pub(crate) fn decode_block(
    reader: &mut BitReader<'_>,
    tables: &BlockTables,
    weights: &[[u8; 8]; 8],
    qsf: u32,
    pred: &mut i32,
    out: &mut [[i32; 8]; 8],
) -> Result<(), ReelError> {
    *out = [[0; 8]; 8];
    // DC (8.2.4): size epsilon, then kappa (epsilon bits), then delta per Figure 46.
    let eps = u32::from(lookup(tables.dc, reader)?.value);
    let delta: i32 = if eps == 0 {
        0
    } else {
        let kappa = i64::from(reader.read(eps).ok_or_else(corrupt)?);
        let half = 1i64
            .checked_shl(eps.saturating_sub(1))
            .ok_or_else(corrupt)?;
        let full = 1i64.checked_shl(eps).ok_or_else(corrupt)?;
        let d = if kappa >= half {
            kappa
        } else {
            kappa
                .checked_add(1)
                .and_then(|k| k.checked_sub(full))
                .ok_or_else(corrupt)?
        };
        i32::try_from(d).map_err(|_| corrupt())?
    };
    let dc = pred.checked_add(delta).ok_or_else(corrupt)?;
    *pred = dc;
    if let Some(cell) = out.get_mut(0).and_then(|row| row.get_mut(0)) {
        *cell = dc;
    }
    // AC (8.2.5, Figure 47). Every block ends with an EOB codeword (7.3.1.1), also when coefficient 63
    // was coded: a codeword other than EOB after index 63 is corrupt.
    let mut r: usize = 1;
    loop {
        let code = lookup(tables.ac, reader)?;
        if code.amp == 0 {
            return Ok(());
        }
        let negative = reader.read(1).ok_or_else(corrupt)? == 1;
        let mut amplitude = i64::from(code.amp);
        if code.index {
            let p = i64::from(reader.read(tables.index_bits).ok_or_else(corrupt)?);
            amplitude = p
                .checked_mul(64)
                .and_then(|o| o.checked_add(amplitude))
                .ok_or_else(corrupt)?;
        }
        if code.run {
            let run = usize::from(lookup(tables.run, reader)?.value);
            r = r.checked_add(run).ok_or_else(corrupt)?;
        }
        let &(u, v) = ZIGZAG.get(r).ok_or_else(corrupt)?;
        let (u, v) = (usize::from(u), usize::from(v));
        let weight = i64::from(
            *weights
                .get(v)
                .and_then(|row| row.get(u))
                .ok_or_else(corrupt)?,
        );
        let value = dequantize(amplitude, weight, i64::from(qsf), tables.p)?;
        let signed = if negative {
            value.checked_neg().ok_or_else(corrupt)?
        } else {
            value
        };
        let cell = out
            .get_mut(v)
            .and_then(|row| row.get_mut(u))
            .ok_or_else(corrupt)?;
        *cell = i32::try_from(signed).map_err(|_| corrupt())?;
        r = r.checked_add(1).ok_or_else(corrupt)?;
    }
}

/// Equation (8.1) for a non-zero quantized magnitude `q`:
/// `floor((q*W*qsf + floor(W*qsf/2) + (W != p ? p/2 : 0)) / p)`.
pub(crate) fn dequantize(q: i64, weight: i64, qsf: i64, p: i64) -> Result<i64, ReelError> {
    let wq = weight.checked_mul(qsf).ok_or_else(corrupt)?;
    let mut numerator = q
        .checked_mul(wq)
        .and_then(|x| x.checked_add(wq.checked_div(2)?))
        .ok_or_else(corrupt)?;
    if weight != p {
        numerator = numerator
            .checked_add(p.checked_div(2).ok_or_else(corrupt)?)
            .ok_or_else(corrupt)?;
    }
    numerator.checked_div(p).ok_or_else(corrupt)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_is_a_permutation_matching_figure_48() {
        let mut seen = [false; 64];
        for &(u, v) in &ZIGZAG {
            seen[usize::from(v) * 8 + usize::from(u)] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(ZIGZAG[0], (0, 0));
        assert_eq!(ZIGZAG[1], (1, 0));
        assert_eq!(ZIGZAG[2], (0, 1));
        assert_eq!(ZIGZAG[9], (0, 3));
        assert_eq!(ZIGZAG[63], (7, 7));
    }

    #[test]
    fn dequantize_follows_equation_8_1() {
        // q=1, W=32, qsf=1, p=32: (32 + 16) / 32 = 1 (no p/2 because W == p).
        assert_eq!(dequantize(1, 32, 1, 32), Ok(1));
        // q=3, W=33, qsf=4, p=32: (396 + 66 + 16) / 32 = 478 / 32 = 14.
        assert_eq!(dequantize(3, 33, 4, 32), Ok(14));
        // p=8 (CIDs 1235, 1241, 1250): q=2, W=32, qsf=2: (128 + 32 + 4) / 8 = 20.
        assert_eq!(dequantize(2, 32, 2, 8), Ok(20));
        assert!(dequantize(i64::MAX, 2, 2, 32).is_err());
    }
}
