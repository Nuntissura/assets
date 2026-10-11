//! Pure (no decoder) AV1 facts: the `AV1CodecConfigurationRecord` parser and the capability descriptor.
//! Always compiled, so a build that routes AV1 to an external decoder (see `video`) validates the same
//! record and describes the same capability without linking the in-process rav1d decoder (feature `av1`).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::ReelError;
use crate::model::{CodecDescriptor, MediaKind};

/// Enumerable capability descriptor (STU-VID-051) for AV1 decode by a decoder reporting `decoder_version`.
pub fn av1_descriptor(decoder_version: &'static str, can_decode: bool) -> CodecDescriptor {
    CodecDescriptor {
        id: "av1",
        kind: MediaKind::Video,
        container: "isobmff-mp4|matroska-webm",
        bit_depths: &[8, 10, 12],
        chroma: Some("mono|420|422|444"),
        transfer: None,
        decoder_version,
        can_decode,
        can_encode: false,
    }
}

/// `AV1CodecConfigurationRecord` (`av1C` box payload / Matroska CodecPrivate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av1Config {
    pub profile: u8,
    pub level: u8,
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub monochrome: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    /// Sequence header (and other) OBUs, fed to the decoder before the first packet.
    pub config_obus: Vec<u8>,
}

impl Av1Config {
    /// Largest picture (width x height) whose PACKED frame in this record's chroma layout and bit depth
    /// fits `max_frame_bytes` (planes tight, one byte per sample at 8 bit, two above; see `VideoFormat`).
    /// 4:2:0 8-bit is 1.5 bytes per pixel, so the default 8 MiB chunk holds 5,592,405 px (1080p is
    /// 2,073,600); the 4:4:4 16-bit worst case, used only when no record describes the stream, is 6.
    /// This sizes the decoder's pre-allocation guard; the exact packed size of every decoded picture is
    /// still checked against `max_frame_bytes` before it is copied (`video::Forward::accept`, test
    /// `oversize_decoded_picture_is_refused_before_it_is_copied`).
    pub fn pixel_limit(&self, max_frame_bytes: usize) -> u32 {
        // Samples per pixel, doubled to stay integral for the half-sample chroma planes.
        let samples_x2: u64 = if self.monochrome {
            2
        } else {
            match (self.subsampling_x, self.subsampling_y) {
                (true, true) => 3,
                (true, false) => 4,
                _ => 6,
            }
        };
        let bytes_x2 = if self.high_bitdepth {
            samples_x2.saturating_mul(2)
        } else {
            samples_x2
        };
        // `bytes_x2` is 2..=12, never zero.
        let pixels = (max_frame_bytes as u64)
            .saturating_mul(2)
            .checked_div(bytes_x2)
            .unwrap_or(0);
        u32::try_from(pixels).unwrap_or(u32::MAX)
    }
}

pub fn parse_av1c(record: &[u8]) -> Result<Av1Config, ReelError> {
    let [b0, b1, b2, _, ..] = *record else {
        return Err(ReelError::Corrupt {
            what: "av1c-truncated",
        });
    };
    if b0 & 0x80 == 0 {
        return Err(ReelError::Corrupt {
            what: "av1c-marker",
        });
    }
    if b0 & 0x7F != 1 {
        return Err(ReelError::Unsupported {
            what: "av1c-version",
        });
    }
    let profile = b1 >> 5;
    if profile > 2 {
        return Err(ReelError::Unsupported {
            what: "av1-profile",
        });
    }
    let (high_bitdepth, twelve_bit) = (b2 & 0x40 != 0, b2 & 0x20 != 0);
    // Profile 2 alone defines 12-bit samples; profiles 0 and 1 are 8 or 10 bit.
    if twelve_bit && (!high_bitdepth || profile != 2) {
        return Err(ReelError::Corrupt {
            what: "av1c-bit-depth",
        });
    }
    Ok(Av1Config {
        profile,
        level: b1 & 0x1F,
        high_bitdepth,
        twelve_bit,
        monochrome: b2 & 0x10 != 0,
        subsampling_x: b2 & 0x08 != 0,
        subsampling_y: b2 & 0x04 != 0,
        config_obus: record[4..].to_vec(),
    })
}
