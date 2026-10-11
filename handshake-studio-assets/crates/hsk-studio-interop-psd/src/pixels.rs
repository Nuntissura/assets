//! Decoded samples. Planes keep the file's big-endian sample bytes; conversion to numbers is
//! explicit so colour decisions stay with the caller (decode is a conversion boundary).

use hsk_studio_accord::CancellationToken;

use crate::compression::row_bytes;
use crate::error::{PsdError, Result};
use crate::header::Header;
use crate::layers::PsdLayer;
use crate::limits::Limits;

/// One decoded channel plane: `height` rows of `row_bytes(width, depth)` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub bytes: Vec<u8>,
}

impl Plane {
    /// Sample at (x, y) as an integer: 1-bit value, u8, u16 or the raw u32 bit pattern of a
    /// 32-bit float sample.
    pub fn sample(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let row = row_bytes(self.width, self.depth).ok()?;
        let base = y as usize * row;
        match self.depth {
            1 => {
                let byte = *self.bytes.get(base + (x / 8) as usize)?;
                Some(u32::from((byte >> (7 - x % 8)) & 1))
            }
            8 => self.bytes.get(base + x as usize).map(|&v| u32::from(v)),
            16 => {
                let i = base + x as usize * 2;
                let b = self.bytes.get(i..i + 2)?;
                Some(u32::from(u16::from_be_bytes([b[0], b[1]])))
            }
            32 => {
                let i = base + x as usize * 4;
                let b = self.bytes.get(i..i + 4)?;
                Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            }
            _ => None,
        }
    }

    /// Samples as numbers in 0..=1 (8-bit /255, 16-bit /65535, 32-bit float as stored, 1-bit
    /// raw 0/1). The 16-bit scale is UNVERIFIED against Photoshop output.
    pub fn to_unit_f32(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity((self.width as usize).saturating_mul(self.height as usize));
        for y in 0..self.height {
            for x in 0..self.width {
                let Some(v) = self.sample(x, y) else {
                    continue;
                };
                out.push(match self.depth {
                    8 => v as f32 / 255.0,
                    16 => v as f32 / 65535.0,
                    32 => f32::from_bits(v),
                    _ => v as f32,
                });
            }
        }
        out
    }
}

impl PsdLayer {
    /// Decodes one channel by id over the rectangle it covers (mask channels use the mask
    /// rectangle). `Ok(None)` when the layer has no such channel.
    pub fn decode_channel(
        &self,
        channel_id: i16,
        header: &Header,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Option<Plane>> {
        let Some(channel) = self.channels.iter().find(|c| c.id == channel_id) else {
            return Ok(None);
        };
        let rect = self.channel_rect(channel_id);
        if rect.area() > limits.max_pixels {
            return Err(PsdError::PixelLimit);
        }
        let bytes = channel.decode(rect.width(), rect.height(), header, limits, cancel)?;
        Ok(Some(Plane { width: rect.width(), height: rect.height(), depth: header.depth, bytes }))
    }
}
