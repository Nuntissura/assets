//! Decoded model. Pixel samples are always interleaved, in file sample order.
use std::ops::Deref;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

/// PhotometricInterpretation of the pixels as handed out. `WhiteIsZero` pages are inverted by the
/// decoder, so their samples read as `BlackIsZero`; `source_photometric` keeps the file value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Photometric {
    BlackIsZero,
    Rgb,
    Cmyk,
    /// ICC L*a*b* (PI 9): L, a+128, b+128 as unsigned samples.
    IccLab,
    /// CIE L*a*b* (PI 8) exactly as stored: L unsigned, a and b two-complement signed values held in the
    /// unsigned sample type (8 or 16 bits); no scaling or offset is applied.
    CieLab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    Uint,
    Float,
}

/// ExtraSamples (tag 338) first-sample kind. Unspecified extra samples read as `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alpha {
    None,
    Associated,
    Unassociated,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::U32(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn bits(&self) -> u16 {
        match self {
            Self::U8(_) => 8,
            Self::U16(_) => 16,
            Self::U32(_) | Self::F32(_) => 32,
            Self::F64(_) => 64,
        }
    }
}

/// Rational as stored in the file (numerator, denominator); never normalized.
pub type Rational = (u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub x: Rational,
    pub y: Rational,
    /// ResolutionUnit: 1 none, 2 inch, 3 centimetre (default 2 when the tag is absent).
    pub unit: u16,
}

/// Raw tag bytes plus the file byte order, for the consumer that parses them (interop-psd for 37724).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBlock {
    pub byte_order: ByteOrder,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub width: u32,
    pub height: u32,
    pub photometric: Photometric,
    /// PhotometricInterpretation value in the file (0 WhiteIsZero, 1 BlackIsZero, 2 RGB, 3 palette, 5 CMYK, 6 YCbCr, 8 CIELab, 9 ICCLab).
    pub source_photometric: u16,
    /// Bits per sample of `pixels()`. Palette pages are expanded to 16-bit RGB.
    pub bits_per_sample: u16,
    /// Bits per sample stored in the file (8 or 16 index bits for a palette page).
    pub source_bits_per_sample: u16,
    pub sample_format: SampleFormat,
    pub samples_per_pixel: u16,
    /// File PlanarConfiguration was 2; the samples are interleaved regardless.
    pub planar_in_file: bool,
    pub alpha: Alpha,
    /// ExtraSamples values as stored.
    pub extra_samples: Vec<u16>,
    /// Compression id as stored.
    pub compression: u16,
    /// Predictor as stored (1 none, 2 horizontal, 3 floating point).
    pub predictor: u16,
    /// Tag 274, recorded and never applied.
    pub orientation: Option<u16>,
    pub resolution: Option<Resolution>,
    /// Tag 34675 bytes.
    pub icc: Option<Vec<u8>>,
    /// Tag 37724 (Photoshop ImageSourceData) raw block.
    pub image_source_data: Option<RawBlock>,
    /// Tag 34377 (Photoshop ImageResources) raw block.
    pub image_resources: Option<RawBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TiffPage {
    pub info: PageInfo,
    pub(crate) samples: Samples,
}

/// One extra sample of a page as its own plane (lane SMB-TIFF-CHANNELS-1): `extra_sample` is the
/// ExtraSamples (tag 338) value as stored (0 unspecified, 1 associated alpha, 2 unassociated alpha;
/// 0 when the tag lists no value for it) and `samples` the `width * height` row-major values, in the
/// page's sample type. Format-only: what a plane means to a document is the caller's mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraPlane {
    pub extra_sample: u16,
    pub samples: Samples,
}

impl PageInfo {
    /// Samples per pixel the photometric interpretation itself uses (1 gray, 3 RGB and Lab, 4 CMYK);
    /// the rest of `samples_per_pixel` are extra samples.
    pub const fn colour_samples_per_pixel(&self) -> u16 {
        match self.photometric {
            Photometric::BlackIsZero => 1,
            Photometric::Rgb | Photometric::IccLab | Photometric::CieLab => 3,
            Photometric::Cmyk => 4,
        }
    }
}

impl TiffPage {
    /// Interleaved samples, `width * height * samples_per_pixel` of them.
    pub fn pixels(&self) -> &Samples {
        &self.samples
    }

    /// Every extra sample of the page (the alpha first when ExtraSamples declares one) as its own
    /// plane, in file sample order. Bounded by the page budget the decode already enforced (the
    /// planes together hold at most the page's decoded samples).
    pub fn extra_planes(&self) -> Vec<ExtraPlane> {
        let spp = usize::from(self.samples_per_pixel);
        let colour = usize::from(self.colour_samples_per_pixel());
        (colour..spp)
            .enumerate()
            .map(|(k, at)| {
                let pick = |n: usize| (at..n).step_by(spp);
                let samples = match &self.samples {
                    Samples::U8(v) => Samples::U8(pick(v.len()).filter_map(|i| v.get(i).copied()).collect()),
                    Samples::U16(v) => Samples::U16(pick(v.len()).filter_map(|i| v.get(i).copied()).collect()),
                    Samples::U32(v) => Samples::U32(pick(v.len()).filter_map(|i| v.get(i).copied()).collect()),
                    Samples::F32(v) => Samples::F32(pick(v.len()).filter_map(|i| v.get(i).copied()).collect()),
                    Samples::F64(v) => Samples::F64(pick(v.len()).filter_map(|i| v.get(i).copied()).collect()),
                };
                ExtraPlane { extra_sample: self.extra_samples.get(k).copied().unwrap_or(0), samples }
            })
            .collect()
    }
}

impl Deref for TiffPage {
    type Target = PageInfo;
    fn deref(&self) -> &PageInfo {
        &self.info
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TiffFile {
    pub byte_order: ByteOrder,
    pub big_tiff: bool,
    pub pages: Vec<TiffPage>,
}
