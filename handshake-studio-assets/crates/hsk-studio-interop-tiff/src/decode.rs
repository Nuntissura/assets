//! Bounded decode: this crate's own pre-scan first, then the `tiff` codec for pixel data only.
use crate::error::{cmul, to_usize, Result, TiffError};
use crate::limits::Limits;
use crate::model::{Samples, TiffFile, TiffPage};
use crate::scan::{scan, PageScan};
use hsk_studio_accord::CancellationToken;
use std::io::{Cursor, Read, Seek, SeekFrom};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::{TiffError as Codec, TiffFormatError, TiffUnsupportedError};

/// Decodes every page of the top IFD chain. Nothing is applied (orientation, colour management).
pub fn decode(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<TiffFile> {
    cancel.check().map_err(|_| TiffError::Canceled)?;
    let scanned = scan(bytes, limits)?;
    let mut codec_limits = tiff::decoder::Limits::default();
    let page_cap = to_usize(limits.max_page_decoded_bytes)?;
    codec_limits.decoding_buffer_size = page_cap;
    codec_limits.intermediate_buffer_size = page_cap.min(codec_limits.intermediate_buffer_size);
    codec_limits.ifd_value_size = to_usize(limits.max_tag_bytes)?;
    let patch: Vec<(u64, u8)> = scanned.pages.iter().flat_map(|p| p.patch.iter().copied()).collect();
    let mut decoder = Decoder::new(Overlay { inner: Cursor::new(bytes), patches: patch })
        .map_err(map_codec)?
        .with_limits(codec_limits);

    let mut pages = Vec::with_capacity(scanned.pages.len());
    for (index, page) in scanned.pages.into_iter().enumerate() {
        cancel.check().map_err(|_| TiffError::Canceled)?;
        if index > 0 {
            decoder.next_image().map_err(map_codec)?;
        }
        pages.push(decode_page(&mut decoder, page)?);
    }
    Ok(TiffFile { byte_order: scanned.header.order, big_tiff: scanned.header.big, pages })
}

fn decode_page(decoder: &mut Decoder<Overlay<'_>>, page: PageScan) -> Result<TiffPage> {
    let info = page.info;
    let (w, h) = decoder.dimensions().map_err(map_codec)?;
    if (w, h) != (info.width, info.height) {
        return Err(TiffError::Corrupt("decoder_dimension_mismatch"));
    }
    let mut result = DecodingResult::U8(Vec::new());
    let layout = decoder.read_image_to_buffer(&mut result).map_err(map_codec)?;
    let got = u64::try_from(result.as_buffer(0).byte_len()).map_err(|_| TiffError::DimsOverflow)?;
    let want = u64::try_from(layout.complete_len).map_err(|_| TiffError::DimsOverflow)?;
    if got != want || want != page.decoded_bytes {
        return Err(TiffError::Corrupt("decoder_size_mismatch"));
    }
    let planes = layout.planes.max(1);
    let spp = usize::from(page.file_spp);
    let pixel_count = to_usize(cmul(u64::from(info.width), u64::from(info.height))?)?;
    let samples = match result {
        DecodingResult::U8(v) => Samples::U8(interleave(v, planes, spp, pixel_count)?),
        DecodingResult::U16(v) => Samples::U16(interleave(v, planes, spp, pixel_count)?),
        DecodingResult::U32(v) => Samples::U32(interleave(v, planes, spp, pixel_count)?),
        DecodingResult::F32(v) => Samples::F32(interleave(v, planes, spp, pixel_count)?),
        DecodingResult::F64(v) => Samples::F64(interleave(v, planes, spp, pixel_count)?),
        _ => return Err(TiffError::unsupported("sample_type_unsupported")),
    };
    let samples = match &page.palette {
        Some(p) => expand_palette(samples, p, info.source_bits_per_sample)?,
        None => samples,
    };
    let samples = if info.source_photometric == 6 { ycbcr_to_rgb(samples, spp)? } else { samples };
    if u64::try_from(samples.len()).map_err(|_| TiffError::DimsOverflow)?
        != cmul(u64::try_from(pixel_count).map_err(|_| TiffError::DimsOverflow)?, u64::from(info.samples_per_pixel))?
        || samples.bits() != info.bits_per_sample
    {
        return Err(TiffError::Corrupt("decoder_sample_mismatch"));
    }
    Ok(TiffPage { info, samples })
}

/// Read + Seek over the input with a few bytes presented differently to the codec.
struct Overlay<'a> {
    inner: Cursor<&'a [u8]>,
    patches: Vec<(u64, u8)>,
}

impl Read for Overlay<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = self.inner.position();
        let n = self.inner.read(buf)?;
        for (pos, value) in &self.patches {
            if let Some(i) = pos.checked_sub(start).and_then(|d| usize::try_from(d).ok())
                && i < n
            {
                buf[i] = *value;
            }
        }
        Ok(n)
    }
}

impl Seek for Overlay<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Planar pages arrive as `planes` consecutive planes; interleave them into pixel order.
fn interleave<T: Copy>(v: Vec<T>, planes: usize, spp: usize, pixels: usize) -> Result<Vec<T>> {
    if planes <= 1 {
        return Ok(v);
    }
    if planes != spp || v.len() != pixels.checked_mul(planes).ok_or(TiffError::DimsOverflow)? {
        return Err(TiffError::Corrupt("decoder_plane_mismatch"));
    }
    let mut out = Vec::with_capacity(v.len());
    for i in 0..pixels {
        for p in 0..planes {
            let idx = p.checked_mul(pixels).and_then(|b| b.checked_add(i)).ok_or(TiffError::DimsOverflow)?;
            out.push(*v.get(idx).ok_or(TiffError::Corrupt("decoder_plane_mismatch"))?);
        }
    }
    Ok(out)
}

pub(crate) fn map_codec(e: Codec) -> TiffError {
    match e {
        Codec::FormatError(f) => match f {
            TiffFormatError::TiffSignatureNotFound | TiffFormatError::TiffSignatureInvalid => TiffError::NotTiff,
            _ => TiffError::Corrupt("decoder_format"),
        },
        Codec::UnsupportedError(u) => match u {
            TiffUnsupportedError::UnsupportedCompressionMethod(m) => {
                TiffError::unsupported_value("compression_unsupported", u32::from(m.to_u16()))
            }
            _ => TiffError::unsupported("decoder_unsupported"),
        },
        Codec::IoError(_) => TiffError::Truncated,
        Codec::LimitsExceeded => TiffError::Limit("limit_decoder"),
        Codec::IntSizeError => TiffError::DimsOverflow,
        _ => TiffError::Corrupt("decoder_usage"),
    }
}

/// JPEG YCbCr (full range, ITU-R BT.601 / JFIF) to RGB in 16.16 fixed point; samples after the third stay.
fn ycbcr_to_rgb(samples: Samples, spp: usize) -> Result<Samples> {
    let Samples::U8(mut v) = samples else {
        return Err(TiffError::unsupported("ycbcr_requires_8bit"));
    };
    let sat = |x: i32| u8::try_from(x.clamp(0, 255)).unwrap_or(0);
    for px in v.chunks_exact_mut(spp) {
        if let [c0, c1, c2, ..] = px {
            let y = i32::from(*c0);
            let cb = i32::from(*c1).saturating_sub(128);
            let cr = i32::from(*c2).saturating_sub(128);
            let r = y.saturating_add(cr.saturating_mul(91881).saturating_add(32768) >> 16);
            let g_delta = cb.saturating_mul(22554).saturating_add(cr.saturating_mul(46802)).saturating_add(32768) >> 16;
            let b = y.saturating_add(cb.saturating_mul(116130).saturating_add(32768) >> 16);
            (*c0, *c1, *c2) = (sat(r), sat(y.saturating_sub(g_delta)), sat(b));
        }
    }
    Ok(Samples::U8(v))
}

/// Palette indices to 16-bit RGB through the ColorMap (reds, then greens, then blues; entries kept exactly).
fn expand_palette(indices: Samples, map: &[u16], bits: u16) -> Result<Samples> {
    let n = 1_usize.checked_shl(u32::from(bits)).ok_or(TiffError::DimsOverflow)?;
    let idx: Vec<usize> = match indices {
        Samples::U8(v) => v.into_iter().map(usize::from).collect(),
        Samples::U16(v) => v.into_iter().map(usize::from).collect(),
        _ => return Err(TiffError::unsupported("bits_unsupported")),
    };
    let mut out = Vec::with_capacity(idx.len().checked_mul(3).ok_or(TiffError::DimsOverflow)?);
    for i in idx {
        if i >= n {
            return Err(TiffError::Corrupt("palette_index_out_of_range"));
        }
        for plane in 0..3_usize {
            let at = plane.checked_mul(n).and_then(|b| b.checked_add(i)).ok_or(TiffError::DimsOverflow)?;
            out.push(*map.get(at).ok_or(TiffError::Corrupt("bad_color_map"))?);
        }
    }
    Ok(Samples::U16(out))
}
