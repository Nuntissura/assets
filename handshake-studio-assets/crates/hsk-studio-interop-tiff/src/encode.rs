//! TIFF export over the `tiff` encoder. Colour types are declared here, with every sample (alpha
//! included) in `BITS_PER_SAMPLE`, so the horizontal predictor strides over the full pixel; the
//! codec's own RGBA/Gray types would break that stride once ExtraSamples is declared through it.
use crate::error::{cmul, Result, TiffError};
use crate::limits::MAX_ENCODE_BYTES;
use crate::model::{Alpha, Resolution, Samples};
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

/// Encodes one page. The output is little-endian.
pub fn encode(req: &EncodeRequest<'_>) -> Result<Vec<u8>> {
    check(req)?;
    let out = Vec::new();
    if req.big_tiff {
        encode_kind::<TiffKindBig>(req, out)
    } else {
        encode_kind::<TiffKindStandard>(req, out)
    }
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

fn encode_kind<K: TiffKind>(req: &EncodeRequest<'_>, out: Vec<u8>) -> Result<Vec<u8>> {
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
        (Samples::U8(s), Gray) => write::<K, Gray8>(&mut enc, req, s)?,
        (Samples::U8(s), GrayA) => write::<K, GrayA8>(&mut enc, req, s)?,
        (Samples::U8(s), Rgb) => write::<K, Rgb8>(&mut enc, req, s)?,
        (Samples::U8(s), Rgba) => write::<K, Rgba8>(&mut enc, req, s)?,
        (Samples::U8(s), Cmyk) => write::<K, Cmyk8>(&mut enc, req, s)?,
        (Samples::U16(s), Gray) => write::<K, Gray16>(&mut enc, req, s)?,
        (Samples::U16(s), GrayA) => write::<K, GrayA16>(&mut enc, req, s)?,
        (Samples::U16(s), Rgb) => write::<K, Rgb16>(&mut enc, req, s)?,
        (Samples::U16(s), Rgba) => write::<K, Rgba16>(&mut enc, req, s)?,
        (Samples::U16(s), Cmyk) => write::<K, Cmyk16>(&mut enc, req, s)?,
        (Samples::U32(s), Gray) => write::<K, Gray32>(&mut enc, req, s)?,
        (Samples::U32(s), GrayA) => write::<K, GrayA32>(&mut enc, req, s)?,
        (Samples::U32(s), Rgb) => write::<K, Rgb32>(&mut enc, req, s)?,
        (Samples::U32(s), Rgba) => write::<K, Rgba32>(&mut enc, req, s)?,
        (Samples::U32(s), Cmyk) => write::<K, Cmyk32>(&mut enc, req, s)?,
        (Samples::F32(s), Gray) => write::<K, Gray32F>(&mut enc, req, s)?,
        (Samples::F32(s), GrayA) => write::<K, GrayA32F>(&mut enc, req, s)?,
        (Samples::F32(s), Rgb) => write::<K, Rgb32F>(&mut enc, req, s)?,
        (Samples::F32(s), Rgba) => write::<K, Rgba32F>(&mut enc, req, s)?,
        (Samples::F32(s), Cmyk) => write::<K, Cmyk32F>(&mut enc, req, s)?,
        (Samples::F64(_), _) => return Err(TiffError::unsupported("sample_type_unsupported")),
    }
    Ok(cursor.into_inner())
}

fn write<K: TiffKind, C: ColorType>(
    enc: &mut TiffEncoder<&mut Cursor<Vec<u8>>, K>,
    req: &EncodeRequest<'_>,
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
    if let Some(icc) = req.icc {
        let dir = img.encoder();
        let entry = dir.write_entry_bytes(Type::UNDEFINED, icc).map_err(map_enc)?;
        let extra = Directory::from_iter([(Tag::Unknown(34675), entry)]);
        dir.extend_from(&extra);
    }
    img.write_data(samples).map_err(map_enc)
}
