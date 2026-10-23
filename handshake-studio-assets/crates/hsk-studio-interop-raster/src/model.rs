//! Decoded model. Samples are interleaved, row-major, in channel order of [`Colour`].
use crate::error::{cmul, to_usize, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterFormat {
    Png,
    Jpeg,
    /// Recognised by [`crate::sniff`] only; decode refuses `webp_decoder_unavailable`.
    Webp,
}

impl RasterFormat {
    /// Short label (the `format` field of interop-psd's `RasterSource`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }
}

/// Channel layout of the samples handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Gray,
    /// Gray + straight (unassociated) alpha.
    GrayA,
    Rgb,
    /// RGB + straight (unassociated) alpha.
    Rgba,
    /// C, M, Y, K exactly as decoded (JPEG only); see [`JpegInfo::cmyk_inverted`] for the ink convention.
    Cmyk,
}

impl Colour {
    pub const fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayA => 2,
            Self::Rgb => 3,
            Self::Rgba | Self::Cmyk => 4,
        }
    }

    /// Colour channels without alpha.
    pub const fn colour_channels(self) -> usize {
        match self {
            Self::Gray | Self::GrayA => 1,
            Self::Rgb | Self::Rgba => 3,
            Self::Cmyk => 4,
        }
    }

    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::GrayA | Self::Rgba)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn depth(&self) -> u16 {
        match self {
            Self::U8(_) => 8,
            Self::U16(_) => 16,
        }
    }
}

/// PNG colour metadata recorded as stored, never applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PngInfo {
    /// IHDR colour type (0, 2, 3, 4, 6) and bit depth (1, 2, 4, 8, 16) of the file.
    pub source_colour_type: u8,
    pub source_bit_depth: u8,
    pub interlaced: bool,
    /// tRNS present (expanded into the alpha channel).
    pub transparency_chunk: bool,
    /// gAMA value x 100000, as stored.
    pub gamma: Option<u32>,
    /// sRGB rendering intent (0..=3), as stored.
    pub srgb_intent: Option<u8>,
    /// cHRM white x,y, red x,y, green x,y, blue x,y, each x 100000, as stored.
    pub chromaticities: Option<[u32; 8]>,
    /// acTL frame count when the file is an APNG (only the default image is decoded).
    pub animation_frames: Option<u32>,
}

/// JPEG coding facts recorded from the markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JpegInfo {
    /// `baseline` (SOF0), `extended_sequential` (SOF1) or `progressive` (SOF2).
    pub process: &'static str,
    pub components: u8,
    /// (horizontal, vertical) sampling factor per component, in frame order.
    pub sampling: Vec<(u8, u8)>,
    pub scans: u32,
    /// Adobe APP14 transform flag (0 none/CMYK/RGB, 1 YCbCr, 2 YCCK) when an Adobe segment is present.
    pub adobe_transform: Option<u8>,
    /// JFIF APP0 present.
    pub jfif: bool,
    /// True when CMYK samples follow the Adobe convention (APP14 present: stored inverted, 0 = full ink,
    /// the same convention as PSD CMYK); false for CMYK without APP14 (255 = full ink).
    pub cmyk_inverted: bool,
    /// YCCK was converted to inverted CMYK by this crate (BT.601 full-range Y/Cb/Cr -> R/G/B, C = 255 - R).
    pub ycck_converted: bool,
    /// Raw APP1 Exif payload length (the payload itself is not exposed in this slice).
    pub exif_bytes: Option<usize>,
}

/// One decoded still image. Nothing is applied (orientation, gamma, colour management).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterImage {
    pub format: RasterFormat,
    pub width: u32,
    pub height: u32,
    pub colour: Colour,
    /// Interleaved samples; `U16` for 16-bit PNG, `U8` otherwise. `depth()` is 8 or 16.
    pub samples: Samples,
    /// Bits per sample in the file before expansion (PNG 1/2/4/8/16; JPEG 8).
    pub source_bit_depth: u8,
    pub icc: Option<Vec<u8>>,
    /// Exif orientation (1..=8) when present; recorded, never applied.
    pub exif_orientation: Option<u16>,
    pub png: Option<PngInfo>,
    pub jpeg: Option<JpegInfo>,
    /// Stable loss/notice codes, e.g. `png_animation_not_imported`.
    pub losses: Vec<&'static str>,
}

impl RasterImage {
    pub const fn depth(&self) -> u16 {
        self.samples.depth()
    }

    /// De-interleaves into one plane per channel (colour channels in [`Colour`] order, then alpha), each
    /// `width x height` row-major, 8-bit as bytes and 16-bit as big-endian byte pairs: the sample layout of
    /// interop-psd's `Plane` (the harness wraps these into `RasterSource`). CMYK planes are returned as decoded;
    /// the caller applies the ink convention from [`JpegInfo::cmyk_inverted`].
    pub fn planes_be(&self) -> Result<Vec<Vec<u8>>> {
        let channels = self.colour.channels();
        let pixels = to_usize(cmul(u64::from(self.width), u64::from(self.height))?)?;
        let bytes_per = usize::from(self.depth() / 8);
        let plane_len = to_usize(cmul(pixels as u64, bytes_per as u64)?)?;
        let mut planes: Vec<Vec<u8>> = (0..channels).map(|_| Vec::with_capacity(plane_len)).collect();
        match &self.samples {
            Samples::U8(v) => {
                for px in v.chunks_exact(channels) {
                    for (plane, s) in planes.iter_mut().zip(px) {
                        plane.push(*s);
                    }
                }
            }
            Samples::U16(v) => {
                for px in v.chunks_exact(channels) {
                    for (plane, s) in planes.iter_mut().zip(px) {
                        plane.extend_from_slice(&s.to_be_bytes());
                    }
                }
            }
        }
        if planes.iter().any(|p| p.len() != plane_len) {
            return Err(crate::RasterError::Corrupt("sample_count_mismatch"));
        }
        Ok(planes)
    }
}
