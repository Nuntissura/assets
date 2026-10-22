//! TIFF export over the `tiff` encoder. Colour types are declared here, with every sample (alpha
//! included) in `BITS_PER_SAMPLE`, so the horizontal predictor strides over the full pixel; the
//! codec's own RGBA/Gray types would break that stride once ExtraSamples is declared through it.
use crate::error::{cmul, Result, TiffError};
use crate::limits::MAX_ENCODE_BYTES;
use crate::endian::to_big_endian;
use crate::model::{Alpha, ByteOrder, Resolution, Samples};
use std::io::Cursor;
use tiff::encoder::colortype::ColorType;
use tiff::encoder::{DeflateLevel, TiffEncoder, TiffKind, TiffKindBig, TiffKindStandard, TiffValue};
use tiff::tags::{PhotometricInterpretation as Pi, SampleFormat as Sf, Tag, Type};
use tiff::Directory;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorModel {
    Gray,
    GrayA,
    Rgb,
    Rgba,
    Cmyk,
}

impl ColorModel {
    pub const fn samples_per_pixel(self) -> u16 {
        match self {
            Self::Gray => 1,
            Self::GrayA => 2,
            Self::Rgb => 3,
            Self::Rgba | Self::Cmyk => 4,
        }
    }

    const fn has_alpha(self) -> bool {
        matches!(self, Self::GrayA | Self::Rgba)
    }
}

/// STU-IO-136 compression domain: None, LZW, ZIP (`Deflate`, tag 8); PackBits is the extra member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Lzw,
    Deflate,
    PackBits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predictor {
    None,
    /// Horizontal differencing; integer samples only.
    Horizontal,
}

pub struct EncodeRequest<'a> {
    pub width: u32,
    pub height: u32,
    pub color: ColorModel,
    /// `Associated` or `Unassociated` for `GrayA`/`Rgba`; `None` for the others.
    pub alpha: Alpha,
    /// Requested bit depth: 8, 16 or 32 (matching `samples`); 10 is a typed refusal.
    pub bit_depth: u8,
    /// Interleaved, `width * height * samples_per_pixel` values.
    pub samples: &'a Samples,
    pub icc: Option<&'a [u8]>,
    pub compression: Compression,
    pub predictor: Predictor,
    /// Write BigTIFF instead of classic TIFF.
    pub big_tiff: bool,
    pub resolution: Option<Resolution>,
    pub orientation: Option<u16>,
}

macro_rules! color_type {
    ($name:ident, $inner:ty, $pi:expr, $bits:expr, $sf:expr, int) => {
        struct $name;
        impl ColorType for $name {
            type Inner = $inner;
            const TIFF_VALUE: Pi = $pi;
            const BITS_PER_SAMPLE: &'static [u16] = $bits;
            const SAMPLE_FORMAT: &'static [Sf] = $sf;
            fn horizontal_predict(row: &[$inner], result: &mut Vec<$inner>) {
                let stride = Self::SAMPLE_FORMAT.len();
                if row.len() < stride {
                    result.extend_from_slice(row);
                    return;
                }
                let (start, rest) = row.split_at(stride);
                result.extend_from_slice(start);
                result.extend(row.iter().zip(rest).map(|(p, c)| c.wrapping_sub(*p)));
            }
        }
    };
    ($name:ident, $inner:ty, $pi:expr, $bits:expr, $sf:expr, float) => {
        struct $name;
        impl ColorType for $name {
            type Inner = $inner;
            const TIFF_VALUE: Pi = $pi;
            const BITS_PER_SAMPLE: &'static [u16] = $bits;
            const SAMPLE_FORMAT: &'static [Sf] = $sf;
            /// Never selected: the request check refuses a predictor on float samples.
            fn horizontal_predict(row: &[$inner], result: &mut Vec<$inner>) {
                result.extend_from_slice(row);
            }
        }
    };
}

color_type!(Gray8, u8, Pi::BlackIsZero, &[8], &[Sf::Uint], int);
color_type!(GrayA8, u8, Pi::BlackIsZero, &[8, 8], &[Sf::Uint; 2], int);
color_type!(Rgb8, u8, Pi::RGB, &[8, 8, 8], &[Sf::Uint; 3], int);
color_type!(Rgba8, u8, Pi::RGB, &[8, 8, 8, 8], &[Sf::Uint; 4], int);
color_type!(Cmyk8, u8, Pi::CMYK, &[8, 8, 8, 8], &[Sf::Uint; 4], int);
color_type!(Gray16, u16, Pi::BlackIsZero, &[16], &[Sf::Uint], int);
color_type!(GrayA16, u16, Pi::BlackIsZero, &[16, 16], &[Sf::Uint; 2], int);
color_type!(Rgb16, u16, Pi::RGB, &[16, 16, 16], &[Sf::Uint; 3], int);
color_type!(Rgba16, u16, Pi::RGB, &[16, 16, 16, 16], &[Sf::Uint; 4], int);
color_type!(Cmyk16, u16, Pi::CMYK, &[16, 16, 16, 16], &[Sf::Uint; 4], int);
color_type!(Gray32, u32, Pi::BlackIsZero, &[32], &[Sf::Uint], int);
color_type!(GrayA32, u32, Pi::BlackIsZero, &[32, 32], &[Sf::Uint; 2], int);
color_type!(Rgb32, u32, Pi::RGB, &[32, 32, 32], &[Sf::Uint; 3], int);
color_type!(Rgba32, u32, Pi::RGB, &[32, 32, 32, 32], &[Sf::Uint; 4], int);
color_type!(Cmyk32, u32, Pi::CMYK, &[32, 32, 32, 32], &[Sf::Uint; 4], int);
color_type!(Gray32F, f32, Pi::BlackIsZero, &[32], &[Sf::IEEEFP], float);
color_type!(GrayA32F, f32, Pi::BlackIsZero, &[32, 32], &[Sf::IEEEFP; 2], float);
color_type!(Rgb32F, f32, Pi::RGB, &[32, 32, 32], &[Sf::IEEEFP; 3], float);
color_type!(Rgba32F, f32, Pi::RGB, &[32, 32, 32, 32], &[Sf::IEEEFP; 4], float);
color_type!(Cmyk32F, f32, Pi::CMYK, &[32, 32, 32, 32], &[Sf::IEEEFP; 4], float);

/// ASCII metadata tags written when set: Software (305), ImageDescription (270), Artist (315), Copyright (33432).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub software: Option<String>,
    pub description: Option<String>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
}

/// Largest total metadata text accepted (bytes).
pub const MAX_METADATA_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
    pub byte_order: ByteOrder,
    pub metadata: Metadata,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { byte_order: ByteOrder::Little, metadata: Metadata::default() }
    }
}

/// Encodes one page. The output is little-endian.
pub fn encode(req: &EncodeRequest<'_>) -> Result<Vec<u8>> {
    encode_with(req, &EncodeOptions::default())
}

/// Encodes one page in the requested byte order with optional ASCII metadata. Output is a pure function of
/// the inputs (no timestamps, no host data). Big-endian output stores the samples big-endian before
/// compression and swaps the container fields (`endian.rs`); the horizontal predictor is applied to the logical
/// sample values first, as a native big-endian writer would.
pub fn encode_with(req: &EncodeRequest<'_>, opts: &EncodeOptions) -> Result<Vec<u8>> {
    check(req)?;
    check_metadata(&opts.metadata)?;
    let out = Vec::new();
    if opts.byte_order == ByteOrder::Little {
        return if req.big_tiff {
            encode_kind::<TiffKindBig>(req, opts, false, out)
        } else {
            encode_kind::<TiffKindStandard>(req, opts, false, out)
        };
    }
    let prepared = prepare_big_endian(req)?;
    let be_req = EncodeRequest {
        width: req.width,
        height: req.height,
        color: req.color,
        alpha: req.alpha,
        bit_depth: req.bit_depth,
        samples: &prepared,
        icc: req.icc,
        compression: req.compression,
        predictor: Predictor::None,
        big_tiff: req.big_tiff,
        resolution: req.resolution,
        orientation: req.orientation,
    };
    let predictor_tag = req.predictor == Predictor::Horizontal;
    let le = if req.big_tiff {
        encode_kind::<TiffKindBig>(&be_req, opts, predictor_tag, out)?
    } else {
        encode_kind::<TiffKindStandard>(&be_req, opts, predictor_tag, out)?
    };
    to_big_endian(&le, req.big_tiff)
}

fn check_metadata(m: &Metadata) -> Result<()> {
    let mut total = 0_usize;
    for text in [&m.software, &m.description, &m.artist, &m.copyright].into_iter().flatten() {
        if !text.is_ascii() || text.contains('\0') {
            return Err(TiffError::Usage("metadata_not_ascii"));
        }
        total = total.checked_add(text.len()).ok_or(TiffError::DimsOverflow)?;
        if total > MAX_METADATA_BYTES {
            return Err(TiffError::Limit("limit_metadata_bytes"));
        }
    }
    Ok(())
}

/// Row-wise horizontal differencing on the logical values (when requested), then each sample's bytes reversed,
/// so the little-endian `tiff` writer emits big-endian sample bytes.
fn prepare_big_endian(req: &EncodeRequest<'_>) -> Result<Samples> {
    let spp = usize::from(req.color.samples_per_pixel());
    let row = usize::try_from(req.width)
        .map_err(|_| TiffError::DimsOverflow)?
        .checked_mul(spp)
        .ok_or(TiffError::DimsOverflow)?;
    let diff = req.predictor == Predictor::Horizontal;
    macro_rules! conv {
        ($v:expr, $variant:ident, $swap:expr) => {{
            let mut out: Vec<_> = $v.to_vec();
            if diff {
                for (src, dst) in $v.chunks(row).zip(out.chunks_mut(row)) {
                    for i in spp..src.len() {
                        dst[i] = src[i].wrapping_sub(src[i.wrapping_sub(spp)]);
                    }
                }
            }
            for x in &mut out {
                *x = $swap(*x);
            }
            Samples::$variant(out)
        }};
    }
    Ok(match req.samples {
        Samples::U8(v) => conv!(v, U8, |x: u8| x),
        Samples::U16(v) => conv!(v, U16, u16::swap_bytes),
        Samples::U32(v) => conv!(v, U32, u32::swap_bytes),
        Samples::F32(v) => Samples::F32(v.iter().map(|x| f32::from_bits(x.to_bits().swap_bytes())).collect()),
        Samples::F64(_) => return Err(TiffError::unsupported("sample_type_unsupported")),
    })
}

fn check(req: &EncodeRequest<'_>) -> Result<()> {
    if req.width == 0 || req.height == 0 {
        return Err(TiffError::Usage("zero_dimension"));
    }
    match req.bit_depth {
        8 | 16 | 32 => {}
        10 => return Err(TiffError::unsupported("bit_depth_10_unsupported")),
        _ => return Err(TiffError::unsupported("bit_depth_unsupported")),
    }
    let depth_ok = matches!(
        (req.samples, req.bit_depth),
        (Samples::U8(_), 8) | (Samples::U16(_), 16) | (Samples::U32(_) | Samples::F32(_), 32)
    );
    if !depth_ok {
        return Err(if matches!(req.samples, Samples::F64(_)) {
            TiffError::unsupported("sample_type_unsupported")
        } else {
            TiffError::Usage("bit_depth_sample_mismatch")
        });
    }
    match (req.color.has_alpha(), req.alpha) {
        (true, Alpha::None) => return Err(TiffError::Usage("alpha_required")),
        (false, Alpha::Associated | Alpha::Unassociated) => {
            return Err(TiffError::Usage("alpha_not_representable"))
        }
        _ => {}
    }
    if req.predictor == Predictor::Horizontal && matches!(req.samples, Samples::F32(_)) {
        return Err(TiffError::unsupported("predictor_unsupported"));
    }
    let spp = u64::from(req.color.samples_per_pixel());
    let expected = cmul(cmul(u64::from(req.width), u64::from(req.height))?, spp)?;
    if u64::try_from(req.samples.len()).map_err(|_| TiffError::DimsOverflow)? != expected {
        return Err(TiffError::Usage("sample_count_mismatch"));
    }
    if cmul(expected, u64::from(req.bit_depth / 8))? > MAX_ENCODE_BYTES {
        return Err(TiffError::Limit("limit_encode_bytes"));
    }
    Ok(())
}

fn map_enc(e: tiff::TiffError) -> TiffError {
    match e {
        tiff::TiffError::IoError(_) => TiffError::Corrupt("encoder_io"),
        tiff::TiffError::IntSizeError => TiffError::DimsOverflow,
        tiff::TiffError::UsageError(_) => TiffError::Usage("encoder_usage"),
        _ => TiffError::Corrupt("encoder_failed"),
    }
}

fn encode_kind<K: TiffKind>(req: &EncodeRequest<'_>, opts: &EncodeOptions, predictor_tag: bool, out: Vec<u8>) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(out);
    let mut enc = TiffEncoder::<_, K>::new_generic(&mut cursor).map_err(map_enc)?;
    enc = match req.compression {
        Compression::None => enc,
        Compression::Lzw => enc.with_compression(tiff::encoder::Compression::Lzw),
        Compression::Deflate => {
            enc.with_compression(tiff::encoder::Compression::Deflate(DeflateLevel::Balanced))
        }
        Compression::PackBits => enc.with_compression(tiff::encoder::Compression::Packbits),
    };
    if req.predictor == Predictor::Horizontal {
        enc = enc.with_predictor(tiff::encoder::Predictor::Horizontal);
    }
    use ColorModel::{Cmyk, Gray, GrayA, Rgb, Rgba};
    match (req.samples, req.color) {
        (Samples::U8(s), Gray) => write::<K, Gray8>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U8(s), GrayA) => write::<K, GrayA8>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U8(s), Rgb) => write::<K, Rgb8>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U8(s), Rgba) => write::<K, Rgba8>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U8(s), Cmyk) => write::<K, Cmyk8>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U16(s), Gray) => write::<K, Gray16>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U16(s), GrayA) => write::<K, GrayA16>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U16(s), Rgb) => write::<K, Rgb16>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U16(s), Rgba) => write::<K, Rgba16>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U16(s), Cmyk) => write::<K, Cmyk16>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U32(s), Gray) => write::<K, Gray32>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U32(s), GrayA) => write::<K, GrayA32>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U32(s), Rgb) => write::<K, Rgb32>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U32(s), Rgba) => write::<K, Rgba32>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::U32(s), Cmyk) => write::<K, Cmyk32>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F32(s), Gray) => write::<K, Gray32F>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F32(s), GrayA) => write::<K, GrayA32F>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F32(s), Rgb) => write::<K, Rgb32F>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F32(s), Rgba) => write::<K, Rgba32F>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F32(s), Cmyk) => write::<K, Cmyk32F>(&mut enc, req, opts, predictor_tag, s)?,
        (Samples::F64(_), _) => return Err(TiffError::unsupported("sample_type_unsupported")),
    }
    Ok(cursor.into_inner())
}

fn write<K: TiffKind, C: ColorType>(
    enc: &mut TiffEncoder<&mut Cursor<Vec<u8>>, K>,
    req: &EncodeRequest<'_>,
    opts: &EncodeOptions,
    predictor_tag: bool,
    samples: &[C::Inner],
) -> Result<()>
where
    [C::Inner]: TiffValue,
{
    let mut img = enc.new_image::<C>(req.width, req.height).map_err(map_enc)?;
    if req.color.has_alpha() {
        let kind: u16 = if req.alpha == Alpha::Associated { 1 } else { 2 };
        img.encoder().write_tag(Tag::ExtraSamples, &[kind][..]).map_err(map_enc)?;
    }
    if let Some(r) = &req.resolution {
        let dir = img.encoder();
        dir.write_tag(Tag::XResolution, tiff::encoder::Rational { n: r.x.0, d: r.x.1 }).map_err(map_enc)?;
        dir.write_tag(Tag::YResolution, tiff::encoder::Rational { n: r.y.0, d: r.y.1 }).map_err(map_enc)?;
        dir.write_tag(Tag::ResolutionUnit, r.unit).map_err(map_enc)?;
    }
    if let Some(o) = req.orientation {
        img.encoder().write_tag(Tag::Orientation, o).map_err(map_enc)?;
    }
    if predictor_tag {
        img.encoder().write_tag(Tag::Predictor, 2_u16).map_err(map_enc)?;
    }
    let m = &opts.metadata;
    for (tag, text) in [
        (Tag::ImageDescription, &m.description),
        (Tag::Software, &m.software),
        (Tag::Artist, &m.artist),
        (Tag::Copyright, &m.copyright),
    ] {
        if let Some(text) = text {
            img.encoder().write_tag(tag, text.as_str()).map_err(map_enc)?;
        }
    }
    if let Some(icc) = req.icc {
        let dir = img.encoder();
        let entry = dir.write_entry_bytes(Type::UNDEFINED, icc).map_err(map_enc)?;
        let extra = Directory::from_iter([(Tag::Unknown(34675), entry)]);
        dir.extend_from(&extra);
    }
    img.write_data(samples).map_err(map_enc)
}
