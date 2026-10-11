//! File header section (26 bytes, Adobe Photoshop File Format Specification).

use crate::error::{PsdError, Result};
use crate::limits::Limits;
use crate::reader::Reader;

pub const HEADER_BYTES: usize = 26;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Bitmap,
    Grayscale,
    Indexed,
    Rgb,
    Cmyk,
    Multichannel,
    Duotone,
    Lab,
}

impl ColorMode {
    pub fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            0 => Self::Bitmap,
            1 => Self::Grayscale,
            2 => Self::Indexed,
            3 => Self::Rgb,
            4 => Self::Cmyk,
            7 => Self::Multichannel,
            8 => Self::Duotone,
            9 => Self::Lab,
            _ => return None,
        })
    }

    pub fn code(self) -> u16 {
        match self {
            Self::Bitmap => 0,
            Self::Grayscale => 1,
            Self::Indexed => 2,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Multichannel => 7,
            Self::Duotone => 8,
            Self::Lab => 9,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bitmap => "bitmap",
            Self::Grayscale => "grayscale",
            Self::Indexed => "indexed",
            Self::Rgb => "rgb",
            Self::Cmyk => "cmyk",
            Self::Multichannel => "multichannel",
            Self::Duotone => "duotone",
            Self::Lab => "lab",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// 1 = PSD, 2 = PSB.
    pub version: u16,
    pub channels: u16,
    pub height: u32,
    pub width: u32,
    pub depth: u16,
    pub color_mode: ColorMode,
}

impl Header {
    pub fn is_psb(&self) -> bool {
        self.version == 2
    }

    pub(crate) fn read(r: &mut Reader<'_>, limits: &Limits) -> Result<Self> {
        let sig = r.bytes(4, "header")?;
        if sig != b"8BPS" {
            return Err(PsdError::BadSignature);
        }
        let version = r.u16("header")?;
        if version != 1 && version != 2 {
            return Err(PsdError::UnsupportedVersion(version));
        }
        if r.bytes(6, "header")?.iter().any(|&b| b != 0) {
            return Err(PsdError::BadReserved);
        }
        let channels = r.u16("header")?;
        if !(1..=56).contains(&channels) {
            return Err(PsdError::BadChannelCount);
        }
        let height = r.u32("header")?;
        let width = r.u32("header")?;
        let max_dim = if version == 2 { 300_000 } else { 30_000 };
        if height == 0 || width == 0 || height > max_dim || width > max_dim {
            return Err(PsdError::BadDimensions);
        }
        if u64::from(width) * u64::from(height) > limits.max_pixels {
            return Err(PsdError::PixelLimit);
        }
        let depth = r.u16("header")?;
        if !matches!(depth, 1 | 8 | 16 | 32) {
            return Err(PsdError::BadDepth);
        }
        let color_mode = ColorMode::from_code(r.u16("header")?).ok_or(PsdError::BadColorMode)?;
        Ok(Self { version, channels, height, width, depth, color_mode })
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&self.channels.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.depth.to_be_bytes());
        out.extend_from_slice(&self.color_mode.code().to_be_bytes());
    }
}
