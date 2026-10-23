//! PNG: own chunk pre-scan (dims, decoded budget, chunk bounds, IEND) first, then the `png` codec with its
//! allocation limit always set. 16-bit stays 16-bit; palette, tRNS and sub-byte gray are expanded by the codec.
use crate::error::{cadd, check_cancel, cmul, to_usize, RasterError, Result};
use crate::limits::{Limits, MAX_ENCODE_BYTES};
use crate::model::{Colour, PngInfo, RasterFormat, RasterImage, Samples};
use hsk_studio_accord::CancellationToken;
use std::io::Cursor;

pub(crate) const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Facts the pre-scan reads from the chunk list before the codec runs.
struct Scan {
    width: u32,
    height: u32,
    bit_depth: u8,
    colour_type: u8,
    interlaced: bool,
    trns: bool,
    iccp: bool,
    animation_frames: Option<u32>,
}

fn be32(b: &[u8], at: usize) -> Result<u32> {
    let end = at.checked_add(4).ok_or(RasterError::DimsOverflow)?;
    let s = b.get(at..end).ok_or(RasterError::Truncated)?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn scan(bytes: &[u8], limits: &Limits) -> Result<Scan> {
    if bytes.len() < 8 || &bytes[..8] != SIGNATURE {
        return Err(RasterError::UnknownFormat);
    }
    let mut at = 8usize;
    let mut first = true;
    let mut out: Option<Scan> = None;
    let mut metadata = 0u64;
    loop {
        let len = be32(bytes, at)?;
        if len > 0x7FFF_FFFF {
            return Err(RasterError::Corrupt("corrupt_png"));
        }
        let kind_at = at.checked_add(4).ok_or(RasterError::DimsOverflow)?;
        let data_at = kind_at.checked_add(4).ok_or(RasterError::DimsOverflow)?;
        let kind: [u8; 4] = bytes.get(kind_at..data_at).ok_or(RasterError::Truncated)?.try_into().map_err(|_| RasterError::Truncated)?;
        let data_end = data_at.checked_add(to_usize(u64::from(len))?).ok_or(RasterError::DimsOverflow)?;
        let next = data_end.checked_add(4).ok_or(RasterError::DimsOverflow)?;
        if next > bytes.len() {
            return Err(RasterError::Truncated);
        }
        let data = &bytes[data_at..data_end];
        if first {
            if &kind != b"IHDR" || len != 13 {
                return Err(RasterError::Corrupt("corrupt_png"));
            }
            let width = be32(data, 0)?;
            let height = be32(data, 4)?;
            let (bit_depth, colour_type, interlace) = (data[8], data[9], data[12]);
            let valid = match colour_type {
                0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
                3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                2 | 4 | 6 => matches!(bit_depth, 8 | 16),
                _ => false,
            };
            if !valid || data[10] != 0 || data[11] != 0 || interlace > 1 {
                return Err(RasterError::Corrupt("corrupt_png"));
            }
            if width == 0 || height == 0 {
                return Err(RasterError::Corrupt("corrupt_png"));
            }
            if width > limits.max_dimension || height > limits.max_dimension {
                return Err(RasterError::Limit("dims_over_limit"));
            }
            out = Some(Scan {
                width,
                height,
                bit_depth,
                colour_type,
                interlaced: interlace == 1,
                trns: false,
                iccp: false,
                animation_frames: None,
            });
            first = false;
        } else {
            let s = out.as_mut().ok_or(RasterError::Corrupt("corrupt_png"))?;
            match &kind {
                b"IEND" => return out.ok_or(RasterError::Corrupt("corrupt_png")),
                b"tRNS" => s.trns = true,
                b"iCCP" => s.iccp = true,
                b"acTL" => s.animation_frames = Some(be32(data, 0)?),
                b"IDAT" | b"fdAT" => {}
                _ => {
                    // Ancillary chunks the codec may buffer count against the metadata budget.
                    metadata = cadd(metadata, u64::from(len))?;
                    if metadata > limits.max_metadata_bytes {
                        return Err(RasterError::Limit("metadata_over_limit"));
                    }
                }
            }
        }
        at = next;
    }
}

/// Output channels after the codec's EXPAND transformation.
fn expanded(scan: &Scan) -> Result<Colour> {
    Ok(match (scan.colour_type, scan.trns) {
        (0, false) => Colour::Gray,
        (0, true) | (4, _) => Colour::GrayA,
        (2, false) | (3, false) => Colour::Rgb,
        (2, true) | (3, true) | (6, _) => Colour::Rgba,
        _ => return Err(RasterError::Corrupt("corrupt_png")),
    })
}

fn map_codec(error: png::DecodingError) -> RasterError {
    match error {
        png::DecodingError::IoError(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => RasterError::Truncated,
        png::DecodingError::LimitsExceeded => RasterError::Limit("decoded_bytes_over_limit"),
        _ => RasterError::Corrupt("corrupt_png"),
    }
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<RasterImage> {
    check_cancel(cancel)?;
    let scan = scan(bytes, limits)?;
    let colour = expanded(&scan)?;
    let sample_bytes: u64 = if scan.bit_depth == 16 { 2 } else { 1 };
    let pixels = cmul(u64::from(scan.width), u64::from(scan.height))?;
    let output = cmul(cmul(pixels, colour.channels() as u64)?, sample_bytes)?;
    // Codec-internal buffers: two raw rows (filter byte + packed samples) plus the inflated ICC/ancillary data.
    let raw_row = cadd(cmul(cmul(u64::from(scan.width), colour.channels() as u64)?, sample_bytes)?, 1)?;
    // (The ICC and ancillary chunks have their own budgets, enforced by the codec limit below.)
    let aggregate = cadd(output, cmul(raw_row, 2)?)?;
    if aggregate > limits.max_decoded_bytes {
        return Err(RasterError::Limit("decoded_bytes_over_limit"));
    }
    let codec_bytes = to_usize(cadd(cadd(limits.max_icc_bytes, limits.max_metadata_bytes)?, cmul(raw_row, 4)?)?)?;
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: codec_bytes });
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.set_ignore_text_chunk(true);
    let mut reader = decoder.read_info().map_err(map_codec)?;
    let (ct, bd) = reader.output_color_type();
    let got = match ct {
        png::ColorType::Grayscale => Colour::Gray,
        png::ColorType::GrayscaleAlpha => Colour::GrayA,
        png::ColorType::Rgb => Colour::Rgb,
        png::ColorType::Rgba => Colour::Rgba,
        png::ColorType::Indexed => return Err(RasterError::Corrupt("png_palette_not_expanded")),
    };
    let want_bits = if scan.bit_depth == 16 { 16 } else { 8 };
    if got != colour || bd as u8 != want_bits {
        return Err(RasterError::Corrupt("png_decoder_layout_mismatch"));
    }
    let size = reader.output_buffer_size().ok_or(RasterError::DimsOverflow)?;
    if size as u64 != output {
        return Err(RasterError::Corrupt("png_decoder_layout_mismatch"));
    }
    check_cancel(cancel)?;
    let mut buf = vec![0u8; size];
    reader.next_frame(&mut buf).map_err(map_codec)?;
    check_cancel(cancel)?;
    let info = reader.info();
    let icc = info.icc_profile.as_ref().map(|c| c.to_vec());
    if scan.iccp && icc.is_none() {
        // The codec drops an iCCP it cannot inflate (corrupt or over its byte limit); fail closed rather than
        // let the image be read as the sRGB assumption.
        return Err(RasterError::Corrupt("icc_unreadable"));
    }
    if icc.as_ref().is_some_and(|p| p.len() as u64 > limits.max_icc_bytes) {
        return Err(RasterError::Limit("icc_over_limit"));
    }
    let meta = PngInfo {
        source_colour_type: scan.colour_type,
        source_bit_depth: scan.bit_depth,
        interlaced: scan.interlaced,
        transparency_chunk: scan.trns,
        gamma: info.gama_chunk.map(png::ScaledFloat::into_scaled),
        srgb_intent: info.srgb.map(|i| i as u8),
        chromaticities: info.chrm_chunk.map(|c| {
            [
                c.white.0.into_scaled(),
                c.white.1.into_scaled(),
                c.red.0.into_scaled(),
                c.red.1.into_scaled(),
                c.green.0.into_scaled(),
                c.green.1.into_scaled(),
                c.blue.0.into_scaled(),
                c.blue.1.into_scaled(),
            ]
        }),
        animation_frames: scan.animation_frames,
    };
    let mut losses = Vec::new();
    if scan.animation_frames.is_some() {
        losses.push("png_animation_not_imported");
    }
    let samples = if want_bits == 16 {
        Samples::U16(buf.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect())
    } else {
        Samples::U8(buf)
    };
    Ok(RasterImage {
        format: RasterFormat::Png,
        width: scan.width,
        height: scan.height,
        colour,
        samples,
        source_bit_depth: scan.bit_depth,
        icc,
        exif_orientation: None,
        png: Some(meta),
        jpeg: None,
        losses,
    })
}

/// Deflate effort. Every level writes the same filter (Paeth; `Stored` writes no filter), so output bytes
/// are a pure function of the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngCompression {
    Stored,
    Fast,
    Balanced,
    High,
}

#[derive(Clone, Debug)]
pub struct PngEncodeRequest<'a> {
    pub width: u32,
    pub height: u32,
    /// Gray, GrayA, Rgb or Rgba (CMYK is refused: `png_colour_unsupported`).
    pub colour: Colour,
    /// Interleaved samples; `U8` writes 8-bit, `U16` writes 16-bit.
    pub samples: &'a Samples,
    pub icc: Option<&'a [u8]>,
    pub compression: PngCompression,
}

pub fn encode_png(req: &PngEncodeRequest<'_>) -> Result<Vec<u8>> {
    let colour_type = match req.colour {
        Colour::Gray => png::ColorType::Grayscale,
        Colour::GrayA => png::ColorType::GrayscaleAlpha,
        Colour::Rgb => png::ColorType::Rgb,
        Colour::Rgba => png::ColorType::Rgba,
        Colour::Cmyk => return Err(RasterError::Usage("png_colour_unsupported")),
    };
    if req.width == 0 || req.height == 0 {
        return Err(RasterError::Usage("dims_zero"));
    }
    let pixels = cmul(u64::from(req.width), u64::from(req.height))?;
    let want = to_usize(cmul(pixels, req.colour.channels() as u64)?)?;
    if req.samples.len() != want {
        return Err(RasterError::Usage("sample_count_mismatch"));
    }
    if cmul(want as u64, u64::from(req.samples.depth() / 8))? > MAX_ENCODE_BYTES {
        return Err(RasterError::Limit("encode_over_limit"));
    }
    if req.icc.is_some_and(|p| p.is_empty()) {
        return Err(RasterError::Usage("icc_empty"));
    }
    let data: Vec<u8> = match req.samples {
        Samples::U8(v) => v.clone(),
        Samples::U16(v) => v.iter().flat_map(|s| s.to_be_bytes()).collect(),
    };
    let mut info = png::Info::with_size(req.width, req.height);
    info.color_type = colour_type;
    info.bit_depth = if req.samples.depth() == 16 { png::BitDepth::Sixteen } else { png::BitDepth::Eight };
    info.icc_profile = req.icc.map(|p| std::borrow::Cow::Owned(p.to_vec()));
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::with_info(&mut out, info).map_err(|_| RasterError::Usage("png_encoder_rejected"))?;
        let (compression, filter) = match req.compression {
            PngCompression::Stored => (png::Compression::NoCompression, png::Filter::NoFilter),
            PngCompression::Fast => (png::Compression::Fast, png::Filter::Paeth),
            PngCompression::Balanced => (png::Compression::Balanced, png::Filter::Paeth),
            PngCompression::High => (png::Compression::High, png::Filter::Paeth),
        };
        enc.set_compression(compression);
        enc.set_filter(filter);
        let mut writer = enc.write_header().map_err(|_| RasterError::Usage("png_encoder_rejected"))?;
        writer.write_image_data(&data).map_err(|_| RasterError::Usage("png_encoder_rejected"))?;
        writer.finish().map_err(|_| RasterError::Usage("png_encoder_rejected"))?;
    }
    Ok(out)
}
