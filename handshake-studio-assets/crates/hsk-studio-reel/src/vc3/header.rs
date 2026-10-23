//! VC-3 coding-unit header (SMPTE ST 2019-1 7.2, Table 8; Amd 1:2023 clause 5-6) and the Compression ID
//! table (Annex C, Tables C.1 and C.2, with Annex D/E references). Byte offsets are relative to the start
//! of the coding unit; bit positions were read from Figures 19, 20, 23 and 26 and cross-checked on the
//! bytes of ffmpeg-written DNxHR frames (tests/fixtures/dnx).
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use super::entropy::{AcCode, Vlc};
use super::tables::*;
use crate::error::ReelError;
use crate::model::ChromaFormat;

/// Smallest header (7.2: 640 bytes for heights up to 1088 lines).
pub(crate) const BASE_HEADER_SIZE: u32 = 0x280;
/// Largest raster side (6.2: 1..=16384 samples per line and lines per frame).
pub(crate) const MAX_SIDE: u32 = 16384;
/// First byte of the macroblock scan indices payload (7.2.11).
const SCAN_INDICES_AT: u32 = 0x170;
/// End-of-frame signature when CRCF = 0 (7.4, Figure 39).
pub(crate) const EOF_SIGNATURE: [u8; 4] = [0x60, 0x0D, 0xC0, 0xDE];

/// Entropy table set of Annex E (the number of its amplitude table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableSet {
    E1,
    E4,
    E7,
    E10,
    E13,
    E16,
}

impl TableSet {
    pub(crate) fn tables(self) -> (&'static [AcCode], &'static [Vlc], &'static [Vlc]) {
        match self {
            Self::E1 => (&AC_E1, &RUN_E2, &DC_E3),
            Self::E4 => (&AC_E4, &RUN_E5, &DC_E6),
            Self::E7 => (&AC_E7, &RUN_E8, &DC_E9),
            Self::E10 => (&AC_E10, &RUN_E11, &DC_E12),
            Self::E13 => (&AC_E13, &RUN_E14, &DC_E15),
            Self::E16 => (&AC_E16, &RUN_E17, &DC_E18),
        }
    }
}

/// What Annex C says about one Compression ID.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CidInfo {
    pub cid: u32,
    /// Bit depths the CID allows (column "Bit depth").
    pub bit_depths: &'static [u8],
    pub set: TableSet,
    pub luma: &'static [[u8; 8]; 8],
    pub chroma: &'static [[u8; 8]; 8],
    /// Inverse quantization parameter p (8.2.7).
    pub p: i64,
    /// HD profile (Table C.1) vs RI profile (Table C.2).
    pub hd: bool,
}

const fn cid(
    cid: u32,
    bit_depths: &'static [u8],
    set: TableSet,
    luma: &'static [[u8; 8]; 8],
    chroma: &'static [[u8; 8]; 8],
    p: i64,
    hd: bool,
) -> CidInfo {
    CidInfo {
        cid,
        bit_depths,
        set,
        luma,
        chroma,
        p,
        hd,
    }
}

/// Every Compression ID of ST 2019-1:2016 (Tables C.1, C.2). 8.2.7: p = 8 for CIDs 1235, 1241, 1250.
pub(crate) const CIDS: [CidInfo; 20] = [
    cid(1235, &[10], TableSet::E1, &W_D1_LUMA, &W_D1_CHROMA, 8, true),
    cid(1237, &[8], TableSet::E4, &W_D2_LUMA, &W_D2_CHROMA, 32, true),
    cid(1238, &[8], TableSet::E7, &W_D3_LUMA, &W_D3_CHROMA, 32, true),
    cid(1241, &[10], TableSet::E1, &W_D4_LUMA, &W_D4_CHROMA, 8, true),
    cid(1242, &[8], TableSet::E4, &W_D5_LUMA, &W_D5_CHROMA, 32, true),
    cid(1243, &[8], TableSet::E7, &W_D6_LUMA, &W_D6_CHROMA, 32, true),
    cid(1244, &[8], TableSet::E4, &W_D7_LUMA, &W_D7_CHROMA, 32, true),
    cid(
        1250,
        &[10],
        TableSet::E10,
        &W_D8_LUMA,
        &W_D8_CHROMA,
        8,
        true,
    ),
    cid(
        1251,
        &[8],
        TableSet::E13,
        &W_D9_LUMA,
        &W_D9_CHROMA,
        32,
        true,
    ),
    cid(
        1252,
        &[8],
        TableSet::E16,
        &W_D10_LUMA,
        &W_D10_CHROMA,
        32,
        true,
    ),
    cid(1253, &[8], TableSet::E4, &W_D2_LUMA, &W_D2_CHROMA, 32, true),
    cid(1256, &[10], TableSet::E1, &W_D11, &W_D11, 32, true),
    cid(
        1258,
        &[8],
        TableSet::E16,
        &W_D10_LUMA,
        &W_D10_CHROMA,
        32,
        true,
    ),
    cid(1259, &[8], TableSet::E4, &W_D2_LUMA, &W_D2_CHROMA, 32, true),
    cid(1260, &[8], TableSet::E4, &W_D7_LUMA, &W_D7_CHROMA, 32, true),
    cid(1270, &[10, 12], TableSet::E1, &W_D11, &W_D11, 32, false),
    cid(
        1271,
        &[10, 12],
        TableSet::E1,
        &W_D1_LUMA,
        &W_D1_CHROMA,
        32,
        false,
    ),
    cid(
        1272,
        &[8],
        TableSet::E7,
        &W_D3_LUMA,
        &W_D3_CHROMA,
        32,
        false,
    ),
    cid(
        1273,
        &[8],
        TableSet::E4,
        &W_D2_LUMA,
        &W_D2_CHROMA,
        32,
        false,
    ),
    cid(
        1274,
        &[8],
        TableSet::E4,
        &W_D2_LUMA,
        &W_D2_CHROMA,
        32,
        false,
    ),
];

/// The header fields the decoder uses. Reserved bits are not interpreted (7.2: decoders should not
/// process reserved regions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub header_size: u32,
    pub hvn: u8,
    pub vbr: bool,
    pub crc: bool,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub cid: u32,
    pub chroma: ChromaFormat,
    /// Colour volume CLV (7.2.5): 0 BT.709, 1 BT.2020 NCL, 2 BT.2020 CL, 3 out-of-band.
    pub clv: u8,
    pub mb_cols: u32,
    pub mb_rows: u32,
}

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn byte(frame: &[u8], at: u32) -> Result<u8, ReelError> {
    usize::try_from(at)
        .ok()
        .and_then(|i| frame.get(i))
        .copied()
        .ok_or_else(|| corrupt("vc3-header-size"))
}

fn be16(frame: &[u8], at: u32) -> Result<u32, ReelError> {
    let hi = u32::from(byte(frame, at)?);
    let lo = u32::from(byte(
        frame,
        at.checked_add(1)
            .ok_or_else(|| corrupt("vc3-header-size"))?,
    )?);
    Ok((hi << 8) | lo)
}

fn be32(frame: &[u8], at: u32) -> Result<u32, ReelError> {
    let next = at
        .checked_add(2)
        .ok_or_else(|| corrupt("vc3-header-size"))?;
    Ok((be16(frame, at)? << 16) | be16(frame, next)?)
}

/// Header size the RI profile requires for `height` lines (7.2: 640 bytes up to 1088 lines, 4 more for
/// each further 16 lines, whole or partial).
pub(crate) fn ri_header_size(height: u32) -> u32 {
    let extra_lines = height.saturating_sub(1088);
    BASE_HEADER_SIZE.saturating_add(extra_lines.div_ceil(16).saturating_mul(4))
}

/// Parse and validate one coding unit header. Only what this decoder can decode passes: progressive
/// frame-encoded YCbCr 4:2:2 or 4:2:0 at 8 or 10 bits without alpha. Everything else is a typed refusal:
/// `Unsupported` for valid features not implemented (field encoding, adaptive macroblocks, alpha, 4:4:4,
/// RGB, 12-bit, unknown HVN/CID per clause 8), `Corrupt` for values the standard forbids.
pub(crate) fn parse_header(frame: &[u8]) -> Result<(FrameHeader, CidInfo), ReelError> {
    if (frame.len() as u64) < u64::from(BASE_HEADER_SIZE).saturating_add(4) {
        return Err(corrupt("vc3-header-size"));
    }
    let header_size = be32(frame, 0)?;
    let hvn = byte(frame, 4)?;
    match hvn {
        1..=3 => {}
        // HD profile with alpha (Amd 1:2023) and anything newer: valid VC-3, not decoded here.
        4 => return Err(unsupported("vc3-alpha")),
        _ => return Err(unsupported("vc3-header-version")),
    }
    let cc_a0 = byte(frame, 5)?;
    let cc_a1 = byte(frame, 6)?;
    let cc_a2 = byte(frame, 7)?;
    let vbr = cc_a0 & 0x10 != 0;
    let ffc = cc_a0 & 0x03;
    let macf = cc_a1 & 0x20 != 0;
    let crc = cc_a1 & 0x10 != 0;
    let alp = cc_a2 & 0x01 != 0;
    let height = be16(frame, 0x18)?;
    let width = be16(frame, 0x1A)?;
    let nal = be16(frame, 0x1D)?;
    let sbd = byte(frame, 0x21)? >> 5;
    let sst = byte(frame, 0x22)? & 0x04 != 0;
    let cid_value = be32(frame, 0x28)?;
    let cc_b = byte(frame, 0x2C)?;
    let ffe = cc_b & 0x80 != 0;
    let ssc = (cc_b >> 5) & 0x03;
    let clv = (cc_b >> 1) & 0x03;
    let clf = cc_b & 0x01 != 0;

    // Clause 8: proceed only for a Compression ID the decoder knows.
    let info = CIDS
        .iter()
        .find(|c| c.cid == cid_value)
        .copied()
        .ok_or_else(|| unsupported("vc3-compression-id"))?;
    if (hvn == 3) == info.hd {
        return Err(corrupt("vc3-header-version"));
    }
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) || nal != height {
        return Err(corrupt("vc3-dimensions"));
    }
    let expected_size = if info.hd {
        BASE_HEADER_SIZE
    } else {
        ri_header_size(height)
    };
    if header_size != expected_size {
        return Err(corrupt("vc3-header-size"));
    }
    if (frame.len() as u64) < u64::from(header_size).saturating_add(4) {
        return Err(corrupt("vc3-header-size"));
    }
    let bit_depth = match sbd {
        1 => 8,
        2 => 10,
        3 => 12,
        _ => return Err(corrupt("vc3-bit-depth")),
    };
    if !info.bit_depths.contains(&bit_depth) {
        return Err(corrupt("vc3-bit-depth"));
    }
    if vbr && info.hd {
        return Err(corrupt("vc3-vbr"));
    }
    // Feature refusals (valid streams this decoder does not decode yet).
    if alp {
        return Err(unsupported("vc3-alpha"));
    }
    if macf || cid_value == 1260 {
        return Err(unsupported("vc3-adaptive-macroblock"));
    }
    if sst || ffc != 0b01 || !ffe {
        return Err(if sst || ffc == 0b10 || ffc == 0b11 {
            unsupported("vc3-interlaced")
        } else {
            corrupt("vc3-field-frame")
        });
    }
    if clf {
        return Err(unsupported("vc3-rgb"));
    }
    let chroma = match ssc {
        0b00 => ChromaFormat::Yuv422,
        0b01 if hvn > 2 => ChromaFormat::Yuv420,
        0b10 => return Err(unsupported("vc3-444")),
        _ => return Err(corrupt("vc3-subsampling")),
    };
    if bit_depth == 12 {
        // The level scaling of 12-bit samples (Table 14) is not specified clearly enough to pin.
        return Err(unsupported("vc3-12-bit"));
    }
    if (chroma == ChromaFormat::Yuv422 && width % 2 != 0)
        || (chroma == ChromaFormat::Yuv420 && (width % 2 != 0 || height % 2 != 0))
    {
        return Err(corrupt("vc3-dimensions"));
    }
    let mb_cols = width.div_ceil(16);
    let mb_rows = height.div_ceil(16);
    // 7.2.10-7.2.11: one 4-byte scan index per macroblock scan line, inside the header.
    let ns = be16(frame, 0x16C)?;
    let indices_end = u64::from(mb_rows)
        .saturating_mul(4)
        .saturating_add(u64::from(SCAN_INDICES_AT));
    if ns != mb_rows || indices_end > u64::from(header_size) {
        return Err(corrupt("vc3-scan-indices"));
    }
    Ok((
        FrameHeader {
            header_size,
            hvn,
            vbr,
            crc,
            width,
            height,
            bit_depth,
            cid: cid_value,
            chroma,
            clv,
            mb_cols,
            mb_rows,
        },
        info,
    ))
}

/// Byte offset of macroblock scan line `row`, relative to the start of the compressed payload (7.2.11).
pub(crate) fn scan_index(frame: &[u8], row: u32) -> Result<u32, ReelError> {
    let at = row
        .checked_mul(4)
        .and_then(|o| o.checked_add(SCAN_INDICES_AT))
        .ok_or_else(|| corrupt("vc3-scan-indices"))?;
    be32(frame, at)
}
