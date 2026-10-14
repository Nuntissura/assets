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
    /// PhotometricInterpretation value in the file (0 WhiteIsZero, 1 BlackIsZero, 2 RGB, 5 CMYK, 6 YCbCr, 9 ICCLab).
    pub source_photometric: u16,
    pub bits_per_sample: u16,
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

impl TiffPage {
    /// Interleaved samples, `width * height * samples_per_pixel` of them.
    pub fn pixels(&self) -> &Samples {
        &self.samples
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
