//! JPEG decode: own marker walk first (SOF process/precision/dims, scans, entropy data up to EOI, APP2 ICC,
//! APP1 Exif orientation, APP14 Adobe transform), budgets checked from those declared values, then
//! `zune-jpeg` (scalar build: features `std` only) for pixel data.
use crate::error::{cadd, check_cancel, cmul, to_usize, RasterError, Result};
use crate::limits::Limits;
use crate::model::{Colour, JpegInfo, RasterFormat, RasterImage, Samples};
use hsk_studio_accord::CancellationToken;
use std::io::Cursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const CORRUPT: RasterError = RasterError::Corrupt("corrupt_jpeg");

struct Frame {
    process: &'static str,
    width: u16,
    height: u16,
    /// (horizontal, vertical) sampling factor per component.
    sampling: Vec<(u8, u8)>,
}

struct IccChunk {
    seq: u8,
    count: u8,
    data: std::ops::Range<usize>,
}

#[derive(Default)]
struct Scan {
    frame: Option<Frame>,
    scans: u32,
    jfif: bool,
    adobe_transform: Option<u8>,
    icc: Vec<IccChunk>,
    icc_bytes: u64,
    exif: Option<std::ops::Range<usize>>,
}

fn be16(b: &[u8], at: usize) -> Result<u16> {
    let end = at.checked_add(2).ok_or(RasterError::DimsOverflow)?;
    let s = b.get(at..end).ok_or(RasterError::Truncated)?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

fn sof(seg: &[u8], marker: u8, limits: &Limits) -> Result<Frame> {
    let process = match marker {
        0xC0 => "baseline",
        0xC1 => "extended_sequential",
        0xC2 => "progressive",
        0xC3 => return Err(RasterError::Unsupported("jpeg_lossless_unsupported")),
        0xC5..=0xC7 => return Err(RasterError::Unsupported("jpeg_hierarchical_unsupported")),
        _ => return Err(RasterError::Unsupported("arithmetic_coding_unsupported")),
    };
    let head = seg.get(..6).ok_or(CORRUPT)?;
    if head[0] != 8 {
        return Err(RasterError::Unsupported("jpeg_precision_unsupported"));
    }
    let height = u16::from_be_bytes([head[1], head[2]]);
    let width = u16::from_be_bytes([head[3], head[4]]);
    let count = head[5];
    if height == 0 {
        return Err(RasterError::Unsupported("jpeg_dnl_unsupported"));
    }
    if width == 0 {
        return Err(CORRUPT);
    }
    if u32::from(width) > limits.max_dimension || u32::from(height) > limits.max_dimension {
        return Err(RasterError::Limit("dims_over_limit"));
    }
    if !matches!(count, 1 | 3 | 4) {
        return Err(RasterError::Unsupported("jpeg_components_unsupported"));
    }
    let need = 6usize.checked_add(usize::from(count).checked_mul(3).ok_or(RasterError::DimsOverflow)?).ok_or(RasterError::DimsOverflow)?;
    let comps = seg.get(6..need).ok_or(CORRUPT)?;
    let mut sampling = Vec::with_capacity(usize::from(count));
    for c in comps.chunks_exact(3) {
        let (h, v) = (c[1] >> 4, c[1] & 0x0F);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || c[2] > 3 {
            return Err(CORRUPT);
        }
        sampling.push((h, v));
    }
    Ok(Frame { process, width, height, sampling })
}

/// Walks every marker from SOI to EOI, including the entropy-coded data after each SOS.
fn scan(bytes: &[u8], limits: &Limits) -> Result<Scan> {
    if bytes.len() < 3 || bytes[..2] != [0xFF, 0xD8] {
        return Err(RasterError::UnknownFormat);
    }
    let mut s = Scan::default();
    let mut at = 2usize;
    let mut metadata = 0u64;
    loop {
        // Markers may be preceded by fill bytes 0xFF; anything else between segments is skipped.
        while at < bytes.len() && bytes[at] != 0xFF {
            at = at.checked_add(1).ok_or(RasterError::DimsOverflow)?;
        }
        while at < bytes.len() && bytes[at] == 0xFF {
            at = at.checked_add(1).ok_or(RasterError::DimsOverflow)?;
        }
        let marker = *bytes.get(at).ok_or(RasterError::Truncated)?;
        at = at.checked_add(1).ok_or(RasterError::DimsOverflow)?;
        match marker {
            0xD9 => break,
            0xD8 => return Err(CORRUPT),
            0xD0..=0xD7 | 0x01 => continue,
            _ => {}
        }
        let len = usize::from(be16(bytes, at)?);
        if len < 2 {
            return Err(CORRUPT);
        }
        let start = at.checked_add(2).ok_or(RasterError::DimsOverflow)?;
        let end = at.checked_add(len).ok_or(RasterError::DimsOverflow)?;
        let seg = bytes.get(start..end).ok_or(RasterError::Truncated)?;
        match marker {
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if s.frame.is_some() {
                    return Err(CORRUPT);
                }
                s.frame = Some(sof(seg, marker, limits)?);
            }
            0xC8 | 0xCC => return Err(RasterError::Unsupported("arithmetic_coding_unsupported")),
            0xE0 if seg.starts_with(b"JFIF\0") => s.jfif = true,
            0xE1 if seg.starts_with(b"Exif\0\0") => {
                metadata = cadd(metadata, seg.len() as u64)?;
                if metadata > limits.max_metadata_bytes {
                    return Err(RasterError::Limit("metadata_over_limit"));
                }
                if s.exif.is_none() {
                    s.exif = Some(start.checked_add(6).ok_or(RasterError::DimsOverflow)?..end);
                }
            }
            0xE2 if seg.starts_with(b"ICC_PROFILE\0") => {
                let head = seg.get(..14).ok_or(RasterError::Corrupt("icc_chunks_inconsistent"))?;
                let data = start.checked_add(14).ok_or(RasterError::DimsOverflow)?..end;
                s.icc_bytes = cadd(s.icc_bytes, data.len() as u64)?;
                if s.icc_bytes > limits.max_icc_bytes {
                    return Err(RasterError::Limit("icc_over_limit"));
                }
                if s.icc.len() >= 255 {
                    return Err(RasterError::Corrupt("icc_chunk_count"));
                }
                s.icc.push(IccChunk { seq: head[12], count: head[13], data });
            }
            0xEE if seg.starts_with(b"Adobe") => {
                s.adobe_transform = Some(*seg.get(11).ok_or(CORRUPT)?);
            }
            0xDA => {
                if s.frame.is_none() {
                    return Err(CORRUPT);
                }
                s.scans = s.scans.checked_add(1).ok_or(RasterError::DimsOverflow)?;
                if s.scans > limits.max_jpeg_scans {
                    return Err(RasterError::Limit("scans_over_limit"));
                }
                // Entropy-coded data: stuffed 0xFF00, restart markers and fill bytes stay inside it.
                let mut p = end;
                loop {
                    let ff = bytes.get(p..).and_then(|r| r.iter().position(|b| *b == 0xFF)).ok_or(RasterError::Truncated)?;
                    p = p.checked_add(ff).ok_or(RasterError::DimsOverflow)?;
                    let next = *bytes.get(p.checked_add(1).ok_or(RasterError::DimsOverflow)?).ok_or(RasterError::Truncated)?;
                    match next {
                        0x00 | 0xD0..=0xD7 => p = p.checked_add(2).ok_or(RasterError::DimsOverflow)?,
                        0xFF => p = p.checked_add(1).ok_or(RasterError::DimsOverflow)?,
                        _ => break,
                    }
                }
                at = p;
                continue;
            }
            _ => {}
        }
        at = end;
    }
    if s.frame.is_none() || s.scans == 0 {
        return Err(CORRUPT);
    }
    Ok(s)
}

/// Reassembles APP2 ICC_PROFILE chunks (ICC.1 Annex B.4): every chunk names the same count, sequence numbers
/// 1..=count each exactly once, concatenated in sequence order (file order does not matter).
fn assemble_icc(bytes: &[u8], chunks: &[IccChunk]) -> Result<Option<Vec<u8>>> {
    let Some(first) = chunks.first() else { return Ok(None) };
    let bad = RasterError::Corrupt("icc_chunks_inconsistent");
    if usize::from(first.count) != chunks.len() || chunks.iter().any(|c| c.count != first.count) {
        return Err(bad);
    }
    let mut out = Vec::new();
    for seq in 1..=first.count {
        let mut found = chunks.iter().filter(|c| c.seq == seq);
        let chunk = found.next().ok_or(bad.clone())?;
        if found.next().is_some() {
            return Err(bad);
        }
        out.extend_from_slice(bytes.get(chunk.data.clone()).ok_or(RasterError::Truncated)?);
    }
    Ok(Some(out))
}

/// Exif orientation (TIFF tag 0x0112 in IFD0). `Err(())` = Exif present but unreadable.
fn exif_orientation(tiff: &[u8]) -> std::result::Result<Option<u16>, ()> {
    let little = match tiff.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(()),
    };
    let u16_at = |at: usize| -> std::result::Result<u16, ()> {
        let s = tiff.get(at..at.checked_add(2).ok_or(())?).ok_or(())?;
        Ok(if little { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    };
    let u32_at = |at: usize| -> std::result::Result<u32, ()> {
        let s = tiff.get(at..at.checked_add(4).ok_or(())?).ok_or(())?;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    if u16_at(2)? != 42 {
        return Err(());
    }
    let ifd = usize::try_from(u32_at(4)?).map_err(|_| ())?;
    let count = usize::from(u16_at(ifd)?);
    for i in 0..count {
        let entry = i.checked_mul(12).and_then(|o| o.checked_add(2)).and_then(|o| o.checked_add(ifd)).ok_or(())?;
        if u16_at(entry)? == 0x0112 {
            let kind = u16_at(entry.checked_add(2).ok_or(())?)?;
            let value = u16_at(entry.checked_add(8).ok_or(())?)?;
            if kind != 3 || !(1..=8).contains(&value) {
                return Err(());
            }
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn map_codec(error: zune_jpeg::errors::DecodeErrors) -> RasterError {
    use zune_jpeg::errors::DecodeErrors as E;
    match error {
        E::ExhaustedData => RasterError::Truncated,
        E::LargeDimensions(_) => RasterError::Limit("dims_over_limit"),
        E::Unsupported(_) => RasterError::Unsupported("jpeg_process_unsupported"),
        _ => CORRUPT,
    }
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<RasterImage> {
    check_cancel(cancel)?;
    let s = scan(bytes, limits)?;
    let frame = s.frame.as_ref().ok_or(CORRUPT)?;
    let n = frame.sampling.len();
    // Output layout chosen from the frame and APP14 before the codec runs (the same rules zune-jpeg applies:
    // 1 component Luma; 3 components YCbCr or (APP14 transform 0) RGB; 4 components CMYK or (transform 2) YCCK).
    let (colour, out_cs, ycck) = match (n, s.adobe_transform) {
        (1, _) => (Colour::Gray, ColorSpace::Luma, false),
        (3, _) => (Colour::Rgb, ColorSpace::RGB, false),
        (4, Some(2)) => (Colour::Cmyk, ColorSpace::YCCK, true),
        (4, _) => (Colour::Cmyk, ColorSpace::CMYK, false),
        _ => return Err(RasterError::Unsupported("jpeg_components_unsupported")),
    };
    let (w, h) = (u64::from(frame.width), u64::from(frame.height));
    let output = cmul(cmul(w, h)?, colour.channels() as u64)?;
    // Codec-internal buffers scale with the MCU-padded image: progressive keeps i16 coefficients for every
    // component of the whole image; sequential keeps one MCU row of i16 samples per component (x2 for upsampling).
    let (hmax, vmax) = frame.sampling.iter().fold((1u8, 1u8), |(a, b), (h, v)| (a.max(*h), b.max(*v)));
    let mcu_w = cmul(u64::from(hmax), 8)?;
    let mcu_h = cmul(u64::from(vmax), 8)?;
    let padded_w = cmul(w.div_ceil(mcu_w), mcu_w)?;
    let padded_h = cmul(h.div_ceil(mcu_h), mcu_h)?;
    let internal = if frame.process == "progressive" {
        cmul(cmul(cmul(padded_w, padded_h)?, n as u64)?, 2)?
    } else {
        cmul(cmul(cmul(padded_w, mcu_h)?, n as u64)?, 4)?
    };
    if cadd(output, internal)? > limits.max_decoded_bytes {
        return Err(RasterError::Limit("decoded_bytes_over_limit"));
    }
    let icc = assemble_icc(bytes, &s.icc)?;
    let mut losses = Vec::new();
    let exif_orientation = match s.exif.as_ref().map(|r| exif_orientation(bytes.get(r.clone()).unwrap_or(&[]))) {
        None | Some(Ok(None)) => None,
        Some(Ok(v)) => v,
        Some(Err(())) => {
            losses.push("jpeg_exif_unreadable");
            None
        }
    };
    check_cancel(cancel)?;
    let dim = to_usize(u64::from(limits.max_dimension))?;
    let options = DecoderOptions::default()
        .set_max_width(dim)
        .set_max_height(dim)
        .jpeg_set_max_scans(to_usize(u64::from(limits.max_jpeg_scans))?)
        .jpeg_set_out_colorspace(out_cs);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(bytes), options);
    decoder.decode_headers().map_err(map_codec)?;
    let info = decoder.info().ok_or(CORRUPT)?;
    let input = decoder.input_colorspace().ok_or(CORRUPT)?;
    let expected_input = match n {
        1 => matches!(input, ColorSpace::Luma),
        3 => matches!(input, ColorSpace::YCbCr | ColorSpace::RGB),
        _ => input == if ycck { ColorSpace::YCCK } else { ColorSpace::CMYK },
    };
    if (info.width, info.height) != (frame.width, frame.height) || !expected_input {
        return Err(RasterError::Corrupt("jpeg_decoder_layout_mismatch"));
    }
    let size = decoder.output_buffer_size().ok_or(RasterError::DimsOverflow)?;
    if size as u64 != output {
        return Err(RasterError::Corrupt("jpeg_decoder_layout_mismatch"));
    }
    let mut buf = vec![0u8; size];
    decoder.decode_into(&mut buf).map_err(map_codec)?;
    check_cancel(cancel)?;
    if ycck {
        ycck_to_inverted_cmyk(&mut buf);
    }
    let meta = JpegInfo {
        process: frame.process,
        components: u8::try_from(n).map_err(|_| CORRUPT)?,
        sampling: frame.sampling.clone(),
        scans: s.scans,
        adobe_transform: s.adobe_transform,
        jfif: s.jfif,
        cmyk_inverted: colour == Colour::Cmyk && s.adobe_transform.is_some(),
        ycck_converted: ycck,
        exif_bytes: s.exif.as_ref().map(std::ops::Range::len),
    };
    Ok(RasterImage {
        format: RasterFormat::Jpeg,
        width: u32::from(frame.width),
        height: u32::from(frame.height),
        colour,
        samples: Samples::U8(buf),
        source_bit_depth: 8,
        icc,
        exif_orientation,
        png: None,
        jpeg: Some(meta),
        losses,
    })
}

/// Y, Cb, Cr, K (as stored) -> inverted C, M, Y plus K: full-range BT.601 (T.871) Y/Cb/Cr -> R/G/B, then
/// C = 255 - R (the Adobe convention: YCCK files carry inverted CMYK, so the result is stored-inverted CMYK).
fn ycck_to_inverted_cmyk(buf: &mut [u8]) {
    let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    for px in buf.chunks_exact_mut(4) {
        let y = f32::from(px[0]);
        let cb = f32::from(px[1]) - 128.0;
        let cr = f32::from(px[2]) - 128.0;
        let r = clamp(y + 1.402 * cr);
        let g = clamp(y - 0.344_136 * cb - 0.714_136 * cr);
        let b = clamp(y + 1.772 * cb);
        px[0] = u8::MAX.wrapping_sub(r);
        px[1] = u8::MAX.wrapping_sub(g);
        px[2] = u8::MAX.wrapping_sub(b);
    }
}
