//! In-process VC-3 decoder (DNxHD / DNxHR) written from SMPTE ST 2019-1:2016 and Amd 1:2023 (D24: pure
//! Rust, compiled into every build). One packet = one compressed frame = one coding unit (progressive,
//! frame encoding). Decoded: YCbCr 4:2:2 and 4:2:0 at 8 and 10 bits, without alpha, for every Compression
//! ID of Annex C whose header passes `header::parse_header`; the rest is a typed refusal (see there).
//!
//! Pipeline per frame: header -> per macroblock scan line (located by its scan index, 7.2.11; the DC
//! predictors restart at each line, 8.2.4) -> per macroblock the 12-bit header with qsf (7.3.1.1) -> per
//! DCT block (Table 5 / Table 4 order) entropy decode, inverse zig-zag, inverse quantization
//! (`entropy`), the crate's reference IDCT (`crate::idct8`), Studio rounding (nearest, ties up), level
//! adjustment `+ 2^(b-1)` (8.2.8.3) and clamping to `[0, 2^b - 1]` (the standard states no clamp; samples
//! outside the range cannot be represented) -> written straight into the cropped output planes
//! (augmentation samples and lines are discarded, 6.3).
//!
//! Hostile-input bounds (V4-01/02): the header's size and dimensions are checked against the packet,
//! the container-declared size (0 = undeclared) and `Limits::max_chunk_bytes` BEFORE the output planes
//! are allocated; every scan line is read from its own bounded slice (`[index, next index)` inside the
//! payload), so a lying index or an overlong codeword is `Corrupt`, never a read past the line; no other
//! allocation depends on the bitstream. Tests: tests/vc3_decode.rs.
#![deny(clippy::arithmetic_side_effects)]
mod entropy;
mod header;
mod tables;

use crate::bitreader::BitReader;
use crate::codec_adapters::DNX_IDENTITY;
use crate::error::{ReelError, check_cancel};
use crate::idct8::{idct8x8, round_half_up};
use crate::isobmff::ColorTag;
use crate::model::{ChromaFormat, ColorInfo, ColorOrigin, Limits, VideoFormat};
use crate::video::{
    DecodedPicture, PictureSink, VideoDecodeError, VideoDecodeSession, VideoStreamSpec,
};
use entropy::{BlockTables, decode_block};
use header::{EOF_SIGNATURE, FrameHeader, parse_header, scan_index};
use hsk_studio_accord::CancellationToken;

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

/// Colour of a decoded frame: the bitstream's colour volume (7.2.5) when it names one, else the
/// container's `colr`, else untagged. VC-3 levels are video (limited) range (Table 1).
fn frame_color(clv: u8, container: ColorTag) -> ColorInfo {
    let tagged = |primaries, transfer, matrix| ColorInfo {
        primaries,
        transfer,
        matrix,
        full_range: false,
        origin: ColorOrigin::SequenceHeader,
    };
    match clv {
        0 => tagged(1, 1, 1),
        // BT.2020 primaries; matrix non-constant (9) or constant (10) luminance; the transfer function is
        // not part of the colour volume field: unspecified (2).
        1 => tagged(9, 2, 9),
        2 => tagged(9, 2, 10),
        _ => match container {
            ColorTag::Nclx {
                primaries,
                transfer,
                matrix,
                ..
            } => ColorInfo {
                primaries,
                transfer,
                matrix,
                full_range: false,
                origin: ColorOrigin::Container,
            },
            _ => ColorInfo {
                primaries: 2,
                transfer: 2,
                matrix: 2,
                full_range: false,
                origin: ColorOrigin::Untagged,
            },
        },
    }
}

/// Validate a frame against the packet and the bounds; returns the header and the output format.
fn admit(
    frame: &[u8],
    declared: (u32, u32),
    limits: &Limits,
    container: ColorTag,
) -> Result<(FrameHeader, header::CidInfo, VideoFormat), ReelError> {
    let (head, info) = parse_header(frame)?;
    let (declared_w, declared_h) = declared;
    if (declared_w != 0 && head.width > declared_w) || (declared_h != 0 && head.height > declared_h)
    {
        return Err(corrupt("vc3-size-mismatch"));
    }
    let format = VideoFormat {
        width: head.width,
        height: head.height,
        bit_depth: head.bit_depth,
        chroma: head.chroma,
        color: frame_color(head.clv, container),
    };
    if format
        .frame_bytes()
        .is_none_or(|bytes| bytes > limits.max_chunk_bytes)
    {
        return Err(ReelError::LimitExceeded {
            what: "vc3-frame-bytes",
        });
    }
    // Aggregate budget (V4-02): every macroblock costs at least its 12-bit header plus, per DCT block,
    // the shortest DC codeword and the EOB codeword, so a payload too short for the macroblock count the
    // header declares is refused before the output planes are sized from that count.
    let (ac, _, dc) = info.set.tables();
    let min_dc = dc.iter().map(|c| u64::from(c.len)).min().unwrap_or(1);
    let eob = ac
        .iter()
        .find(|c| c.amp == 0)
        .map_or(1, |c| u64::from(c.len));
    let blocks: u64 = if head.chroma == ChromaFormat::Yuv420 {
        6
    } else {
        8
    };
    let min_bits_per_mb = blocks
        .saturating_mul(min_dc.saturating_add(eob))
        .saturating_add(12);
    let payload_bits = (frame.len() as u64)
        .saturating_sub(u64::from(head.header_size))
        .saturating_sub(4)
        .saturating_mul(8);
    let macroblocks = u64::from(head.mb_cols).saturating_mul(u64::from(head.mb_rows));
    if macroblocks.saturating_mul(min_bits_per_mb) > payload_bits {
        return Err(corrupt("vc3-payload-size"));
    }
    // End-of-frame signature (7.4). With CRCF = 1 it holds a CRC-32 that this decoder does not check (the
    // workspace's single CRC-32 lives in another crate; V4-05 forbids a second copy).
    let eof_at = frame.len().saturating_sub(4);
    if !head.crc && frame.get(eof_at..) != Some(&EOF_SIGNATURE[..]) {
        return Err(corrupt("vc3-eof-signature"));
    }
    Ok((head, info, format))
}

/// Output planes with the bytes per sample of `format`, written by `put`.
struct Planes {
    planes: Vec<Vec<u8>>,
    dims: Vec<(u32, u32)>,
    wide: bool,
    max: i32,
}

impl Planes {
    fn new(format: &VideoFormat) -> Result<Self, ReelError> {
        let dims = format.plane_dims();
        let bps: usize = format.bytes_per_sample();
        let mut planes = Vec::with_capacity(dims.len());
        for &(w, h) in &dims {
            let bytes = usize::try_from(u64::from(w).saturating_mul(u64::from(h)))
                .ok()
                .and_then(|n| n.checked_mul(bps))
                .ok_or(ReelError::LimitExceeded {
                    what: "vc3-frame-bytes",
                })?;
            planes.push(vec![0u8; bytes]);
        }
        let max = 1i32
            .checked_shl(u32::from(format.bit_depth))
            .and_then(|m| m.checked_sub(1))
            .ok_or_else(|| corrupt("vc3-bit-depth"))?;
        Ok(Self {
            planes,
            dims,
            wide: bps == 2,
            max,
        })
    }

    /// Write one 8x8 block whose top-left sample is `(x0, y0)` in plane `plane`; samples outside the
    /// plane (augmentation) are discarded.
    fn put(&mut self, plane: usize, x0: u32, y0: u32, block: &[[i32; 8]; 8]) {
        let Some(&(w, h)) = self.dims.get(plane) else {
            return;
        };
        let wide = self.wide;
        let max = self.max;
        let Some(data) = self.planes.get_mut(plane) else {
            return;
        };
        for (dy, row) in (0u32..).zip(block.iter()) {
            let Some(y) = y0.checked_add(dy).filter(|y| *y < h) else {
                break;
            };
            for (dx, value) in (0u32..).zip(row.iter()) {
                let Some(x) = x0.checked_add(dx).filter(|x| *x < w) else {
                    break;
                };
                let sample = (*value).clamp(0, max) as u16;
                let index = u64::from(y)
                    .saturating_mul(u64::from(w))
                    .saturating_add(u64::from(x));
                let Ok(index) = usize::try_from(index) else {
                    return;
                };
                if wide {
                    let at = index.saturating_mul(2);
                    if let Some(cell) = data.get_mut(at..at.saturating_add(2)) {
                        cell.copy_from_slice(&sample.to_le_bytes());
                    }
                } else if let Some(cell) = data.get_mut(index) {
                    *cell = sample as u8;
                }
            }
        }
    }
}

/// Block layout of one macroblock: per DCT index k, (component 0 Y / 1 Cb / 2 Cr, x offset, y offset in
/// that component's plane, relative to the macroblock origin in that plane). Table 5 (4:2:2) and Table 4
/// (4:2:0); Figure 41 maps k to components.
const LAYOUT_422: [(usize, u32, u32); 8] = [
    (0, 0, 0),
    (0, 8, 0),
    (1, 0, 0),
    (2, 0, 0),
    (0, 0, 8),
    (0, 8, 8),
    (1, 0, 8),
    (2, 0, 8),
];
const LAYOUT_420: [(usize, u32, u32); 6] = [
    (0, 0, 0),
    (0, 8, 0),
    (1, 0, 0),
    (2, 0, 0),
    (0, 0, 8),
    (0, 8, 8),
];

/// Decode one compressed frame (one coding unit) into planes.
pub(crate) fn decode_frame(
    frame: &[u8],
    declared: (u32, u32),
    limits: &Limits,
    container: ColorTag,
    cancel: &CancellationToken,
) -> Result<(VideoFormat, Vec<Vec<u8>>), ReelError> {
    let (head, info, format) = admit(frame, declared, limits, container)?;
    let (ac, run, dc) = info.set.tables();
    let tables = BlockTables {
        ac,
        run,
        dc,
        index_bits: if head.bit_depth == 8 { 4 } else { 6 },
        p: info.p,
    };
    let layout: &[(usize, u32, u32)] = match head.chroma {
        ChromaFormat::Yuv420 => &LAYOUT_420,
        _ => &LAYOUT_422,
    };
    let chroma_mb_h: u32 = if head.chroma == ChromaFormat::Yuv420 {
        8
    } else {
        16
    };
    let level = 1i32
        .checked_shl(u32::from(head.bit_depth).saturating_sub(1))
        .ok_or_else(|| corrupt("vc3-bit-depth"))?;
    let mut planes = Planes::new(&format)?;
    let payload_at = usize::try_from(head.header_size).map_err(|_| corrupt("vc3-header-size"))?;
    // The compressed payload ends where the EOF signature starts.
    let payload_end = frame.len().saturating_sub(4);
    let payload = frame
        .get(payload_at..payload_end)
        .ok_or_else(|| corrupt("vc3-header-size"))?;
    let payload_len = payload.len() as u64;
    let mut coeffs = [[0i32; 8]; 8];
    let mut spatial = [[0.0f64; 8]; 8];
    let mut samples = [[0i32; 8]; 8];
    for row in 0..head.mb_rows {
        check_cancel(cancel)?;
        let start = u64::from(scan_index(frame, row)?);
        let end = if row.saturating_add(1) < head.mb_rows {
            u64::from(scan_index(frame, row.saturating_add(1))?)
        } else {
            payload_len
        };
        if start > end || end > payload_len {
            return Err(corrupt("vc3-scan-indices"));
        }
        let line = usize::try_from(start)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(s, e)| payload.get(s..e))
            .ok_or_else(|| corrupt("vc3-scan-indices"))?;
        let mut reader = BitReader::new(line);
        // DC predictors per component type restart at each macroblock scan line (8.2.4).
        let mut pred = [0i32; 3];
        for col in 0..head.mb_cols {
            // Macroblock header (7.3.1.1, Figure 30): qsf in the 11 most significant of 12 bits.
            let mb_header = reader.read(12).ok_or_else(|| corrupt("vc3-entropy"))?;
            let qsf = mb_header >> 1;
            for &(component, dx, dy) in layout {
                let weights = if component == 0 {
                    info.luma
                } else {
                    info.chroma
                };
                let predictor = pred
                    .get_mut(component)
                    .ok_or_else(|| corrupt("vc3-entropy"))?;
                decode_block(&mut reader, &tables, weights, qsf, predictor, &mut coeffs)?;
                idct8x8(&coeffs, &mut spatial);
                for (out_row, in_row) in samples.iter_mut().zip(spatial.iter()) {
                    for (out, value) in out_row.iter_mut().zip(in_row.iter()) {
                        *out = round_half_up(*value).saturating_add(level);
                    }
                }
                let (mb_w, mb_h) = if component == 0 {
                    (16u32, 16u32)
                } else {
                    (8u32, chroma_mb_h)
                };
                let x0 = col.saturating_mul(mb_w).saturating_add(dx);
                let y0 = row.saturating_mul(mb_h).saturating_add(dy);
                planes.put(component, x0, y0, &samples);
            }
        }
    }
    Ok((format, planes.planes))
}

/// A decode session of the in-process VC-3 decoder: one picture per packet, released immediately.
pub(crate) struct Vc3Session {
    declared: (u32, u32),
    limits: Limits,
    color: ColorTag,
}

/// Open a session for a stream the container describes.
pub(crate) fn open_session(spec: &VideoStreamSpec<'_>) -> Vc3Session {
    Vc3Session {
        declared: (spec.width, spec.height),
        limits: spec.limits,
        color: spec.color,
    }
}

impl VideoDecodeSession for Vc3Session {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        let (format, planes) =
            decode_frame(packet, self.declared, &self.limits, self.color, cancel)?;
        out.accept(DecodedPicture {
            tag,
            format,
            film_grain_applied: false,
            planes,
        })?;
        Ok(())
    }

    fn finish(
        &mut self,
        _cancel: &CancellationToken,
        _out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        Ok(())
    }
}

/// Identity in every receipt of this decoder.
pub(crate) const IDENTITY: crate::video::DecoderIdentity = DNX_IDENTITY;

/// Test/diagnostic entry: decode one raw VC-3 frame (no container) into its format and planes, with the
/// same bounds the route applies. `declared` (0 = undeclared) bounds the picture size.
pub fn decode_vc3_frame(
    frame: &[u8],
    declared: (u32, u32),
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(VideoFormat, Vec<Vec<u8>>), ReelError> {
    decode_frame(frame, declared, limits, ColorTag::Untagged, cancel)
}
