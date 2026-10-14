//! Bounded pre-scan of the top IFD chain over `&[u8]` (classic TIFF and BigTIFF).
//!
//! Everything the third-party decoder could be hurt by is checked here first: header, IFD offsets,
//! entry counts, cycles, value ranges, strip/tile ranges, JPEG frame sizes, declared sizes and the
//! running aggregate. Tag values this crate hands out (ICC, tags 37724 and 34377) are copied here,
//! not through the decoder, so the decoder's own tag-size limit never applies to them.
use crate::error::{cadd, cmul, to_usize, Result, TiffError};
use crate::limits::Limits;
use crate::model::{Alpha, ByteOrder, PageInfo, Photometric, RawBlock, Resolution, SampleFormat};
use std::collections::HashSet;

pub(crate) const TAG_WIDTH: u16 = 256;
pub(crate) const TAG_LENGTH: u16 = 257;
pub(crate) const TAG_BITS: u16 = 258;
pub(crate) const TAG_COMPRESSION: u16 = 259;
pub(crate) const TAG_PHOTOMETRIC: u16 = 262;
pub(crate) const TAG_STRIP_OFFSETS: u16 = 273;
pub(crate) const TAG_ORIENTATION: u16 = 274;
pub(crate) const TAG_SAMPLES: u16 = 277;
pub(crate) const TAG_ROWS_PER_STRIP: u16 = 278;
pub(crate) const TAG_STRIP_COUNTS: u16 = 279;
pub(crate) const TAG_XRES: u16 = 282;
pub(crate) const TAG_YRES: u16 = 283;
pub(crate) const TAG_PLANAR: u16 = 284;
pub(crate) const TAG_RES_UNIT: u16 = 296;
pub(crate) const TAG_PREDICTOR: u16 = 317;
pub(crate) const TAG_TILE_WIDTH: u16 = 322;
pub(crate) const TAG_TILE_LENGTH: u16 = 323;
pub(crate) const TAG_TILE_OFFSETS: u16 = 324;
pub(crate) const TAG_TILE_COUNTS: u16 = 325;
pub(crate) const TAG_YCBCR_COEFFICIENTS: u16 = 529;
pub(crate) const TAG_EXTRA_SAMPLES: u16 = 338;
pub(crate) const TAG_SAMPLE_FORMAT: u16 = 339;
pub(crate) const TAG_ICC: u16 = 34675;
pub(crate) const TAG_IMAGE_RESOURCES: u16 = 34377;
pub(crate) const TAG_IMAGE_SOURCE_DATA: u16 = 37724;

#[derive(Clone, Copy)]
pub(crate) struct Cur<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) le: bool,
}

impl<'a> Cur<'a> {
    pub(crate) fn slice(&self, pos: u64, len: u64) -> Result<&'a [u8]> {
        let end = pos.checked_add(len).ok_or(TiffError::Truncated)?;
        let (p, e) = (
            usize::try_from(pos).map_err(|_| TiffError::Truncated)?,
            usize::try_from(end).map_err(|_| TiffError::Truncated)?,
        );
        self.b.get(p..e).ok_or(TiffError::Truncated)
    }

    fn u16(&self, pos: u64) -> Result<u16> {
        let a: [u8; 2] = self.slice(pos, 2)?.try_into().map_err(|_| TiffError::Truncated)?;
        Ok(if self.le { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
    }

    fn u32(&self, pos: u64) -> Result<u32> {
        let a: [u8; 4] = self.slice(pos, 4)?.try_into().map_err(|_| TiffError::Truncated)?;
        Ok(if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }

    fn u64(&self, pos: u64) -> Result<u64> {
        let a: [u8; 8] = self.slice(pos, 8)?.try_into().map_err(|_| TiffError::Truncated)?;
        Ok(if self.le { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub(crate) tag: u16,
    pub(crate) ty: u16,
    pub(crate) count: u64,
    /// Absolute position of the value bytes (validated when read).
    pub(crate) data_pos: u64,
    pub(crate) data_len: u64,
}

fn type_size(ty: u16) -> Option<u64> {
    match ty {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

pub(crate) struct Header {
    pub(crate) big: bool,
    pub(crate) order: ByteOrder,
    first_ifd: u64,
}

pub(crate) fn header(b: &[u8]) -> Result<Header> {
    let le = match b.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(TiffError::NotTiff),
    };
    let cur = Cur { b, le };
    let order = if le { ByteOrder::Little } else { ByteOrder::Big };
    match cur.u16(2).map_err(|_| TiffError::NotTiff)? {
        42 => Ok(Header { big: false, order, first_ifd: u64::from(cur.u32(4)?) }),
        43 => {
            if cur.u16(4)? != 8 || cur.u16(6)? != 0 {
                return Err(TiffError::NotTiff);
            }
            Ok(Header { big: true, order, first_ifd: cur.u64(8)? })
        }
        _ => Err(TiffError::NotTiff),
    }
}

fn read_ifd(cur: &Cur<'_>, big: bool, off: u64, limits: &Limits) -> Result<(Vec<Entry>, u64)> {
    let (count, count_len, entry_len, next_len) = if big {
        (cur.u64(off)?, 8_u64, 20_u64, 8_u64)
    } else {
        (u64::from(cur.u16(off)?), 2, 12, 4)
    };
    if count > u64::from(limits.max_ifd_entries) {
        return Err(TiffError::IfdEntryCount);
    }
    let table = cadd(off, count_len)?;
    let table_len = cmul(count, entry_len)?;
    cur.slice(table, cadd(table_len, next_len)?)?;
    let inline_cap: u64 = if big { 8 } else { 4 };
    let value_off: u64 = if big { 12 } else { 8 };
    let mut entries = Vec::with_capacity(to_usize(count)?);
    for i in 0..count {
        let epos = cadd(table, cmul(i, entry_len)?)?;
        let tag = cur.u16(epos)?;
        let ty = cur.u16(cadd(epos, 2)?)?;
        let count = if big { cur.u64(cadd(epos, 4)?)? } else { u64::from(cur.u32(cadd(epos, 4)?)?) };
        let Some(size) = type_size(ty) else {
            continue;
        };
        let data_len = cmul(count, size)?;
        let vpos = cadd(epos, value_off)?;
        let data_pos = if data_len <= inline_cap {
            vpos
        } else if big {
            cur.u64(vpos)?
        } else {
            u64::from(cur.u32(vpos)?)
        };
        entries.push(Entry { tag, ty, count, data_pos, data_len });
    }
    let next = if big {
        cur.u64(cadd(table, table_len)?)?
    } else {
        u64::from(cur.u32(cadd(table, table_len)?)?)
    };
    Ok((entries, next))
}

fn find(entries: &[Entry], tag: u16) -> Option<&Entry> {
    entries.iter().find(|e| e.tag == tag)
}

struct Ctx<'a> {
    cur: Cur<'a>,
    limits: &'a Limits,
}

impl<'a> Ctx<'a> {
    fn bytes(&self, e: &Entry) -> Result<&'a [u8]> {
        if e.data_len > self.limits.max_tag_bytes {
            return Err(TiffError::Limit("limit_tag_bytes"));
        }
        self.cur.slice(e.data_pos, e.data_len)
    }

    /// Unsigned integer array (BYTE, SHORT, LONG, LONG8, IFD, IFD8), at most `max_count` values.
    fn uints(&self, e: &Entry, max_count: u64) -> Result<Vec<u64>> {
        if e.count > max_count {
            return Err(TiffError::Limit("limit_tag_count"));
        }
        let raw = self.bytes(e)?;
        let size = match e.ty {
            1 | 7 => 1_usize,
            3 => 2,
            4 | 13 => 4,
            16 | 18 => 8,
            _ => return Err(TiffError::Corrupt("bad_field_type")),
        };
        let le = self.cur.le;
        Ok(raw
            .chunks_exact(size)
            .map(|c| {
                let mut buf = [0_u8; 8];
                if le {
                    buf[..size].copy_from_slice(c);
                    u64::from_le_bytes(buf)
                } else {
                    buf[8_usize.saturating_sub(size)..].copy_from_slice(c);
                    u64::from_be_bytes(buf)
                }
            })
            .collect())
    }

    fn uint1(&self, entries: &[Entry], tag: u16) -> Result<Option<u64>> {
        let Some(e) = find(entries, tag) else {
            return Ok(None);
        };
        let v = self.uints(e, 1)?;
        match v.as_slice() {
            [x] => Ok(Some(*x)),
            _ => Err(TiffError::Corrupt("bad_count")),
        }
    }

    fn rational(&self, entries: &[Entry], tag: u16) -> Result<Option<(u32, u32)>> {
        let Some(e) = find(entries, tag) else {
            return Ok(None);
        };
        if e.ty != 5 || e.count != 1 {
            return Err(TiffError::Corrupt("bad_rational"));
        }
        let raw = self.bytes(e)?;
        let c = Cur { b: raw, le: self.cur.le };
        Ok(Some((c.u32(0)?, c.u32(4)?)))
    }

    /// `count` rationals as f64 (denominator 0 is corrupt).
    fn rationals(&self, entries: &[Entry], tag: u16, count: u64) -> Result<Option<Vec<f64>>> {
        let Some(e) = find(entries, tag) else {
            return Ok(None);
        };
        if e.ty != 5 || e.count != count {
            return Err(TiffError::Corrupt("bad_rational"));
        }
        let c = Cur { b: self.bytes(e)?, le: self.cur.le };
        let mut out = Vec::new();
        for i in 0..count {
            let pos = cmul(i, 8)?;
            let (n, d) = (c.u32(pos)?, c.u32(cadd(pos, 4)?)?);
            if d == 0 {
                return Err(TiffError::Corrupt("bad_rational"));
            }
            out.push(f64::from(n) / f64::from(d));
        }
        Ok(Some(out))
    }

    fn block(&self, entries: &[Entry], tag: u16) -> Result<Option<Vec<u8>>> {
        match find(entries, tag) {
            Some(e) => Ok(Some(self.bytes(e)?.to_vec())),
            None => Ok(None),
        }
    }
}

/// One validated page: everything the decoder needs plus what is handed out.
pub(crate) struct PageScan {
    pub(crate) info: PageInfo,
    pub(crate) decoded_bytes: u64,
    /// Byte overlays presented to the codec only (never to the caller): see `extra_sample_patch`.
    pub(crate) patch: Vec<(u64, u8)>,
}

pub(crate) struct FileScan {
    pub(crate) header: Header,
    pub(crate) pages: Vec<PageScan>,
}

const COMPRESSIONS: &[u16] = &[1, 5, 7, 8, 32773, 32946];

pub(crate) fn scan(bytes: &[u8], limits: &Limits) -> Result<FileScan> {
    if u64::try_from(bytes.len()).map_err(|_| TiffError::DimsOverflow)? > limits.max_input_bytes {
        return Err(TiffError::Limit("limit_input_bytes"));
    }
    let hdr = header(bytes)?;
    let cur = Cur { b: bytes, le: hdr.order == ByteOrder::Little };
    let ctx = Ctx { cur, limits };
    let mut seen: HashSet<u64> = HashSet::new();
    let mut pages = Vec::new();
    let mut total: u64 = 0;
    let mut next = hdr.first_ifd;
    while next != 0 {
        if !seen.insert(next) {
            return Err(TiffError::IfdCycle);
        }
        if pages.len() >= to_usize(u64::from(limits.max_pages))? {
            return Err(TiffError::Limit("limit_pages"));
        }
        let (entries, nxt) = read_ifd(&cur, hdr.big, next, limits)?;
        let page = page(&ctx, &entries, hdr.order)?;
        total = cadd(total, page.decoded_bytes)?;
        if total > limits.max_total_decoded_bytes {
            return Err(TiffError::Limit("aggregate_over_budget"));
        }
        pages.push(page);
        next = nxt;
    }
    if pages.is_empty() {
        return Err(TiffError::NotTiff);
    }
    Ok(FileScan { header: hdr, pages })
}

fn dim(v: Option<u64>, tag: u16) -> Result<u32> {
    let v = v.ok_or(TiffError::MissingTag(tag))?;
    if v == 0 {
        return Err(TiffError::Corrupt("zero_dimension"));
    }
    u32::try_from(v).map_err(|_| TiffError::DimsOverflow)
}

fn page(ctx: &Ctx<'_>, entries: &[Entry], order: ByteOrder) -> Result<PageScan> {
    let limits = ctx.limits;
    let width = dim(ctx.uint1(entries, TAG_WIDTH)?, TAG_WIDTH)?;
    let height = dim(ctx.uint1(entries, TAG_LENGTH)?, TAG_LENGTH)?;

    let compression = u16::try_from(ctx.uint1(entries, TAG_COMPRESSION)?.unwrap_or(1))
        .map_err(|_| TiffError::Corrupt("bad_compression"))?;
    let pi = ctx.uint1(entries, TAG_PHOTOMETRIC)?.ok_or(TiffError::MissingTag(TAG_PHOTOMETRIC))?;
    let (photometric, source_photometric) = match pi {
        0 | 1 => (Photometric::BlackIsZero, pi as u16),
        2 => (Photometric::Rgb, 2),
        5 => (Photometric::Cmyk, 5),
        6 => (Photometric::Rgb, 6),
        9 => (Photometric::IccLab, 9),
        3 => return Err(TiffError::unsupported("photometric_palette")),
        4 => return Err(TiffError::unsupported("photometric_mask")),
        8 => return Err(TiffError::unsupported("photometric_cielab")),
        10 => return Err(TiffError::unsupported("photometric_itulab")),
        32844 | 32845 => return Err(TiffError::unsupported("photometric_logluv")),
        other => {
            return Err(TiffError::unsupported_value(
                "photometric_unsupported",
                u32::try_from(other).unwrap_or(u32::MAX),
            ))
        }
    };
    if !COMPRESSIONS.contains(&compression) {
        return Err(TiffError::unsupported_value("compression_unsupported", u32::from(compression)));
    }

    if pi == 6 {
        // Only JPEG-compressed YCbCr is decoded (the codec leaves it as Y/Cb/Cr; it is converted to RGB with
        // the default ITU-R BT.601 coefficients, so any other coefficients are refused).
        if compression != 7 {
            return Err(TiffError::unsupported("ycbcr_requires_jpeg"));
        }
        if let Some(c) = ctx.rationals(entries, TAG_YCBCR_COEFFICIENTS, 3)?
            && !c.iter().zip([0.299, 0.587, 0.114]).all(|(a, b)| (a - b).abs() < 0.0015)
        {
            return Err(TiffError::unsupported("ycbcr_coefficients_unsupported"));
        }
    }

    let spp = u16::try_from(ctx.uint1(entries, TAG_SAMPLES)?.unwrap_or(1))
        .map_err(|_| TiffError::Corrupt("bad_samples_per_pixel"))?;
    if spp == 0 || spp > limits.max_samples_per_pixel {
        return Err(TiffError::Limit("limit_samples_per_pixel"));
    }
    let spp_n = u64::from(spp);

    let bits_vec = match find(entries, TAG_BITS) {
        Some(e) => ctx.uints(e, spp_n)?,
        None => vec![1],
    };
    let bits = *bits_vec.first().ok_or(TiffError::Corrupt("bad_count"))?;
    if bits_vec.iter().any(|b| *b != bits) {
        return Err(TiffError::unsupported("bits_heterogeneous"));
    }
    let fmt_vec = match find(entries, TAG_SAMPLE_FORMAT) {
        Some(e) => ctx.uints(e, spp_n)?,
        None => vec![1],
    };
    let fmt = *fmt_vec.first().ok_or(TiffError::Corrupt("bad_count"))?;
    if fmt_vec.iter().any(|f| *f != fmt) {
        return Err(TiffError::unsupported("sample_format_heterogeneous"));
    }
    let bits = u16::try_from(bits).map_err(|_| TiffError::Corrupt("bad_bits_per_sample"))?;
    let sample_format = match (fmt, bits) {
        (1, 8 | 16 | 32) => SampleFormat::Uint,
        (3, 32 | 64) => SampleFormat::Float,
        (1, _) => return Err(TiffError::unsupported_value("bits_unsupported", u32::from(bits))),
        (3, _) => return Err(TiffError::unsupported_value("float_bits_unsupported", u32::from(bits))),
        (other, _) => {
            return Err(TiffError::unsupported_value(
                "sample_format_unsupported",
                u32::try_from(other).unwrap_or(u32::MAX),
            ))
        }
    };
    let container = u64::from(bits / 8);

    let planar = match ctx.uint1(entries, TAG_PLANAR)?.unwrap_or(1) {
        1 => false,
        2 => true,
        _ => return Err(TiffError::Corrupt("bad_planar_configuration")),
    };
    let predictor = u16::try_from(ctx.uint1(entries, TAG_PREDICTOR)?.unwrap_or(1))
        .map_err(|_| TiffError::Corrupt("bad_predictor"))?;
    if !matches!(predictor, 1..=3) {
        return Err(TiffError::unsupported_value("predictor_unsupported", u32::from(predictor)));
    }

    let extra_samples: Vec<u16> = match find(entries, TAG_EXTRA_SAMPLES) {
        Some(e) => ctx
            .uints(e, spp_n)?
            .into_iter()
            .map(|v| u16::try_from(v).map_err(|_| TiffError::Corrupt("bad_extra_samples")))
            .collect::<Result<_>>()?,
        None => Vec::new(),
    };
    let alpha = match extra_samples.first() {
        Some(1) => Alpha::Associated,
        Some(2) => Alpha::Unassociated,
        _ => Alpha::None,
    };

    // Declared sizes, before any allocation (V4-02).
    if width > limits.max_dimension || height > limits.max_dimension {
        return Err(TiffError::Limit("limit_dimension"));
    }
    let samples = cmul(cmul(u64::from(width), u64::from(height))?, spp_n)?;
    let decoded_bytes = cmul(samples, container)?;
    if decoded_bytes > limits.max_page_decoded_bytes {
        return Err(TiffError::Limit("limit_page_bytes"));
    }

    chunks(ctx, entries, width, height, spp_n, container, planar, compression)?;

    let orientation = ctx
        .uint1(entries, TAG_ORIENTATION)?
        .map(|v| u16::try_from(v).map_err(|_| TiffError::Corrupt("bad_orientation")))
        .transpose()?;
    let resolution = match (ctx.rational(entries, TAG_XRES)?, ctx.rational(entries, TAG_YRES)?) {
        (Some(x), Some(y)) => {
            let unit = u16::try_from(ctx.uint1(entries, TAG_RES_UNIT)?.unwrap_or(2))
                .map_err(|_| TiffError::Corrupt("bad_resolution_unit"))?;
            Some(Resolution { x, y, unit })
        }
        _ => None,
    };
    let raw = |tag| -> Result<Option<RawBlock>> {
        Ok(ctx.block(entries, tag)?.map(|bytes| RawBlock { byte_order: order, bytes }))
    };
    let info = PageInfo {
        width,
        height,
        photometric,
        source_photometric,
        bits_per_sample: bits,
        sample_format,
        samples_per_pixel: spp,
        planar_in_file: planar,
        alpha,
        extra_samples,
        compression,
        predictor,
        orientation,
        resolution,
        icc: ctx.block(entries, TAG_ICC)?,
        image_source_data: raw(TAG_IMAGE_SOURCE_DATA)?,
        image_resources: raw(TAG_IMAGE_RESOURCES)?,
    };
    let patch = extra_sample_patch(ctx, entries, &info)?;
    Ok(PageScan { info, decoded_bytes, patch })
}

/// The codec classifies an extra sample as data only when ExtraSamples says alpha; an Unspecified (0) extra
/// sample would be dropped from its output. Exactly one extra sample is decoded by presenting the codec an
/// alpha value in its place; what is handed out (`extra_samples`, `alpha`) always comes from the file.
/// More than one extra sample on a colour page is refused rather than silently dropped.
fn extra_sample_patch(ctx: &Ctx<'_>, entries: &[Entry], info: &PageInfo) -> Result<Vec<(u64, u8)>> {
    let base: u16 = match info.photometric {
        Photometric::BlackIsZero => return Ok(Vec::new()),
        Photometric::Rgb | Photometric::IccLab => 3,
        Photometric::Cmyk => 4,
    };
    let extra = info.samples_per_pixel.checked_sub(base).ok_or(TiffError::Corrupt("samples_below_photometric"))?;
    if extra > 1 {
        return Err(TiffError::unsupported_value("extra_samples_unsupported", u32::from(extra)));
    }
    let (Some(e), true) = (find(entries, TAG_EXTRA_SAMPLES), extra == 1 && info.alpha == Alpha::None) else {
        return Ok(Vec::new());
    };
    if info.extra_samples.len() != 1 {
        return Ok(Vec::new());
    }
    let alpha = |le: bool| -> Vec<(u64, u8)> {
        match (e.ty, le) {
            (3, true) => vec![(e.data_pos, 2), (e.data_pos.saturating_add(1), 0)],
            (3, false) => vec![(e.data_pos, 0), (e.data_pos.saturating_add(1), 2)],
            _ => vec![(e.data_pos, 2)],
        }
    };
    Ok(alpha(ctx.cur.le))
}

/// Strip/tile layout: counts, ranges inside the input, chunk byte bounds and JPEG frame sizes.
#[allow(clippy::too_many_arguments)]
fn chunks(
    ctx: &Ctx<'_>,
    entries: &[Entry],
    width: u32,
    height: u32,
    spp: u64,
    container: u64,
    planar: bool,
    compression: u16,
) -> Result<()> {
    let limits = ctx.limits;
    let tiled = find(entries, TAG_TILE_OFFSETS).is_some();
    let (off_tag, cnt_tag) = if tiled {
        (TAG_TILE_OFFSETS, TAG_TILE_COUNTS)
    } else {
        (TAG_STRIP_OFFSETS, TAG_STRIP_COUNTS)
    };
    let off_e = find(entries, off_tag).ok_or(TiffError::MissingTag(off_tag))?;
    let offsets = ctx.uints(off_e, limits.max_chunks)?;
    let counts = match find(entries, cnt_tag) {
        Some(e) => Some(ctx.uints(e, limits.max_chunks)?),
        None => None,
    };
    if let Some(c) = &counts
        && c.len() != offsets.len()
    {
        return Err(TiffError::Corrupt("chunk_count_mismatch"));
    }
    // Pixels one chunk covers (before the plane split).
    let (cw, ch) = if tiled {
        let tw = ctx.uint1(entries, TAG_TILE_WIDTH)?.ok_or(TiffError::MissingTag(TAG_TILE_WIDTH))?;
        let tl = ctx.uint1(entries, TAG_TILE_LENGTH)?.ok_or(TiffError::MissingTag(TAG_TILE_LENGTH))?;
        if tw == 0 || tl == 0 {
            return Err(TiffError::Corrupt("zero_tile_dimension"));
        }
        (tw, tl)
    } else {
        let rps = ctx.uint1(entries, TAG_ROWS_PER_STRIP)?.unwrap_or(u64::from(height));
        if rps == 0 {
            return Err(TiffError::Corrupt("zero_rows_per_strip"));
        }
        (u64::from(width), rps.min(u64::from(height)))
    };
    let chunk_samples = if planar { 1 } else { spp };
    let chunk_bytes = cmul(cmul(cmul(cw, ch)?, chunk_samples)?, container)?;
    if chunk_bytes > limits.max_page_decoded_bytes {
        return Err(TiffError::Limit("limit_page_bytes"));
    }
    for (i, off) in offsets.iter().enumerate() {
        let len = match &counts {
            Some(c) => *c.get(i).ok_or(TiffError::Corrupt("chunk_count_mismatch"))?,
            None => 0,
        };
        let data = ctx.cur.slice(*off, len)?;
        if compression == 7 {
            jpeg_guard(data, limits)?;
        }
    }
    Ok(())
}

/// Bounds the frame a JPEG chunk declares, so the codec cannot be asked for a huge buffer.
fn jpeg_guard(data: &[u8], limits: &Limits) -> Result<()> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return Ok(());
    }
    let mut pos: usize = 2;
    loop {
        // Find the next marker.
        while data.get(pos) != Some(&0xFF) {
            if pos >= data.len() {
                return Ok(());
            }
            pos = pos.saturating_add(1);
        }
        while data.get(pos) == Some(&0xFF) {
            pos = pos.saturating_add(1);
        }
        let Some(&m) = data.get(pos) else {
            return Ok(());
        };
        pos = pos.saturating_add(1);
        match m {
            0x00 | 0x01 | 0xD0..=0xD7 | 0xD8 => continue,
            0xD9 | 0xDA => return Ok(()),
            _ => {}
        }
        let len = match data.get(pos..pos.saturating_add(2)) {
            Some([a, b]) => usize::from(u16::from_be_bytes([*a, *b])),
            _ => return Ok(()),
        };
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let Some(f) = data.get(pos.saturating_add(2)..pos.saturating_add(8)) else {
                return Ok(());
            };
            let h = u64::from(u16::from_be_bytes([f[1], f[2]]));
            let w = u64::from(u16::from_be_bytes([f[3], f[4]]));
            let comps = u64::from(f[5]);
            // Worst case 4 bytes per sample after decode.
            let bytes = cmul(cmul(cmul(w, h)?, comps.max(1))?, 4)?;
            if bytes > limits.max_page_decoded_bytes {
                return Err(TiffError::Limit("limit_jpeg_frame"));
            }
            return Ok(());
        }
        pos = pos.saturating_add(len.max(2));
    }
}
