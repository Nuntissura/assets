//! Pure (no decoder) H.264 facts: the `AVCDecoderConfigurationRecord` (`avcC`) parser, the conversion
//! of one length-prefixed ISO BMFF sample into an Annex B byte stream (start codes), and the capability
//! descriptor of the H.264 adapter. Always compiled: H.264 is decoded only by an external decoder (D24:
//! the OS-provided Windows Media Foundation decoder in the isolated `hsk-studio-reel-native` worker), so
//! this crate never links an H.264 decoder; without one registered the route answers the typed
//! `MEDIA_CODEC_UNAVAILABLE`.
//!
//! Provenance (D26, clean room; ISO/IEC 14496-15 was not opened): the record layout was taken as layout
//! FACTS (no code read for translation, none copied) from Chromium `media/formats/mp4/box_definitions.cc`
//! `AVCDecoderConfigurationRecord::ParseInternal` (BSD-3-Clause) and FFmpeg `libavcodec/h264_parse.c`
//! `ff_h264_decode_extradata` (LGPL; read for layout facts only). Layout: `configurationVersion` (u8, 1),
//! `AVCProfileIndication` (u8), `profile_compatibility` (u8), `AVCLevelIndication` (u8), one byte whose
//! low 2 bits are `lengthSizeMinusOne` (1, 2 or 4 byte NAL lengths; 3 refused), one byte whose low 5 bits
//! count the SPS, each SPS as u16 big-endian length + bytes, one byte PPS count, each PPS as u16 length +
//! bytes; for `AVCProfileIndication` 100, 110, 122 and 144 an optional extension of 4+ bytes: low 2 bits
//! `chroma_format`, low 3 bits `bit_depth_luma_minus8`, low 3 bits `bit_depth_chroma_minus8` (each at most
//! 4) and a count of SPS-extension NAL units (u16 length + bytes each). A missing or malformed extension
//! is ignored (Chromium's reading); trailing bytes are not checked.
//!
//! Hostile input (V4-01/V4-02): every length is checked against the bytes that remain before it is read,
//! the parameter-set aggregate is bounded by the record (itself capped by the container), and the Annex B
//! conversion checks every NAL length and the running output size against the caller's byte budget
//! before anything is appended.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::ReelError;
use crate::model::{Certification, CodecDescriptor, MediaKind};

/// Decoder id the H.264 adapter reports (the reel-native Media Foundation worker).
pub const H264_DECODER_ID: &str = "mf-h264";
/// User-facing name of the H.264 adapter (D24: decode only, OS-provided decoder).
pub const H264_DISPLAY_NAME: &str = "H.264 (Windows Media Foundation H.264 decoder, OS-licensed)";
/// Licence statement of the H.264 decoder: an operating-system component used through its public
/// interfaces, licensed with the OS; no decoder code is bundled.
pub const H264_LICENCE: &str = "Windows Media Foundation H.264 decoder, OS-licensed";

/// Annex B start code written in front of every NAL unit.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Enumerable capability descriptor (STU-VID-051 as enriched by D23) of H.264 decode by the
/// OS-provided Media Foundation decoder reporting `decoder_version`.
pub fn h264_descriptor(decoder_version: &'static str, can_decode: bool) -> CodecDescriptor {
    CodecDescriptor {
        id: "h264",
        name: H264_DISPLAY_NAME,
        kind: MediaKind::Video,
        container: "isobmff-mp4-mov",
        bit_depths: &[8],
        chroma: Some("420"),
        transfer: None,
        decoder_id: H264_DECODER_ID,
        decoder_version,
        licence: H264_LICENCE,
        certification: Certification::NotApplicable,
        can_decode,
        can_encode: false,
    }
}

/// `AVCDecoderConfigurationRecord` (`avcC` payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H264Config {
    pub profile_idc: u8,
    pub profile_compatibility: u8,
    pub level_idc: u8,
    /// Bytes of the big-endian NAL length in front of every NAL unit of a sample: 1, 2 or 4.
    pub nal_length_size: u8,
    /// Sequence parameter set NAL units (header byte included), at least one.
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter set NAL units (header byte included).
    pub pps: Vec<Vec<u8>>,
    /// `chroma_format` of the high-profile extension (0 mono, 1 4:2:0, 2 4:2:2, 3 4:4:4); `None` when the
    /// record has no (valid) extension. Profiles below High imply 4:2:0.
    pub chroma_format: Option<u8>,
    pub bit_depth_luma: Option<u8>,
    pub bit_depth_chroma: Option<u8>,
}

/// NAL unit type of a NAL header byte.
pub const fn nal_unit_type(header: u8) -> u8 {
    header & 0x1F
}

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// Profiles whose record may carry the chroma/bit-depth extension.
const fn has_extension(profile_idc: u8) -> bool {
    matches!(profile_idc, 100 | 110 | 122 | 144)
}

/// Bounded reader over the record.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.at)?;
        self.at = self.at.checked_add(1)?;
        Some(value)
    }

    fn u16(&mut self) -> Option<u16> {
        let pair = self.bytes.get(self.at..)?.first_chunk::<2>()?;
        self.at = self.at.checked_add(2)?;
        Some(u16::from_be_bytes(*pair))
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }
}

fn truncated() -> ReelError {
    ReelError::Corrupt {
        what: "avcc-truncated",
    }
}

/// Reads `count` parameter sets (u16 length + bytes each); a set whose length runs past the record is
/// `avcc-truncated`.
fn parameter_sets(reader: &mut Reader<'_>, count: u8) -> Result<Vec<Vec<u8>>, ReelError> {
    let mut sets = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let len = reader.u16().ok_or_else(truncated)?;
        let bytes = reader.take(usize::from(len)).ok_or_else(truncated)?;
        sets.push(bytes.to_vec());
    }
    Ok(sets)
}

/// Parses an `avcC` payload. Refusals: shorter than the fixed fields or a parameter set running past the
/// record -> `Corrupt avcc-truncated`; version other than 1 -> `Unsupported avcc-version`; NAL length size
/// 3 -> `Corrupt avcc-length-size`; an empty SPS or PPS, or a set whose NAL header names another NAL type
/// -> `Corrupt avcc-parameter-set`. A record without SPS/PPS is legal here (an `avc3` sample entry carries
/// them in band); `require_parameter_sets` is the `avc1` rule.
pub fn parse_avcc(record: &[u8]) -> Result<H264Config, ReelError> {
    let mut reader = Reader {
        bytes: record,
        at: 0,
    };
    let version = reader.u8().ok_or_else(truncated)?;
    if version != 1 {
        return Err(ReelError::Unsupported {
            what: "avcc-version",
        });
    }
    let profile_idc = reader.u8().ok_or_else(truncated)?;
    let profile_compatibility = reader.u8().ok_or_else(truncated)?;
    let level_idc = reader.u8().ok_or_else(truncated)?;
    // `& 3` keeps 0..=3, so `+ 1` is 1..=4.
    let nal_length_size = (reader.u8().ok_or_else(truncated)? & 0x03).saturating_add(1);
    if nal_length_size == 3 {
        return Err(ReelError::Corrupt {
            what: "avcc-length-size",
        });
    }
    let sps_count = reader.u8().ok_or_else(truncated)? & 0x1F;
    let sps = parameter_sets(&mut reader, sps_count)?;
    let pps_count = reader.u8().ok_or_else(truncated)?;
    let pps = parameter_sets(&mut reader, pps_count)?;
    let bad_set = |sets: &[Vec<u8>], kind: u8| {
        sets.iter()
            .any(|set| set.first().is_none_or(|header| nal_unit_type(*header) != kind))
    };
    if bad_set(&sps, NAL_SPS) || bad_set(&pps, NAL_PPS) {
        return Err(ReelError::Corrupt {
            what: "avcc-parameter-set",
        });
    }
    let (mut chroma_format, mut bit_depth_luma, mut bit_depth_chroma) = (None, None, None);
    if has_extension(profile_idc) && reader.remaining() >= 4 {
        let ext = (|| {
            let chroma = reader.u8()? & 0x03;
            let luma = reader.u8()? & 0x07;
            let chroma_depth = reader.u8()? & 0x07;
            (luma <= 4 && chroma_depth <= 4).then_some((chroma, luma, chroma_depth))
        })();
        if let Some((chroma, luma, chroma_depth)) = ext {
            // Both are at most 4, so `+ 8` cannot overflow.
            chroma_format = Some(chroma);
            bit_depth_luma = Some(luma.saturating_add(8));
            bit_depth_chroma = Some(chroma_depth.saturating_add(8));
        }
    }
    Ok(H264Config {
        profile_idc,
        profile_compatibility,
        level_idc,
        nal_length_size,
        sps,
        pps,
        chroma_format,
        bit_depth_luma,
        bit_depth_chroma,
    })
}

impl H264Config {
    /// `avc1` rule: the record carries at least one SPS and one PPS (they are not repeated in band), else
    /// `Corrupt avcc-parameter-set`.
    pub fn require_parameter_sets(&self) -> Result<(), ReelError> {
        if self.sps.is_empty() || self.pps.is_empty() {
            return Err(ReelError::Corrupt {
                what: "avcc-parameter-set",
            });
        }
        Ok(())
    }

    /// Chroma format the stream declares: the extension's value, else 4:2:0 (1), which every profile
    /// without the extension implies.
    pub fn chroma_format_or_420(&self) -> u8 {
        self.chroma_format.unwrap_or(1)
    }

    /// Luma bit depth the stream declares (8 when the record has no extension).
    pub fn bit_depth_luma_or_8(&self) -> u8 {
        self.bit_depth_luma.unwrap_or(8)
    }

    /// The SPS then PPS NAL units as an Annex B byte stream (start code in front of each), appended to
    /// `out` within `max_bytes` total.
    pub fn append_parameter_sets(&self, out: &mut Vec<u8>, max_bytes: usize) -> Result<(), ReelError> {
        for nal in self.sps.iter().chain(&self.pps) {
            append_nal(out, nal, max_bytes)?;
        }
        Ok(())
    }
}

fn over_budget() -> ReelError {
    ReelError::LimitExceeded {
        what: "h264-annexb-bytes",
    }
}

/// Appends start code + `nal` when the result stays within `max_bytes`; nothing is appended otherwise.
fn append_nal(out: &mut Vec<u8>, nal: &[u8], max_bytes: usize) -> Result<(), ReelError> {
    let total = out
        .len()
        .checked_add(START_CODE.len())
        .and_then(|len| len.checked_add(nal.len()))
        .filter(|len| *len <= max_bytes)
        .ok_or_else(over_budget)?;
    out.reserve(total.saturating_sub(out.len()));
    out.extend_from_slice(&START_CODE);
    out.extend_from_slice(nal);
    Ok(())
}

/// Converts one ISO BMFF sample (NAL units, each behind a `nal_length_size`-byte big-endian length) into
/// Annex B: `out` is cleared, then (when `parameter_sets` is given, i.e. on a sync sample) the SPS/PPS
/// of the record, then every NAL unit of the sample, each behind a 4-byte start code. Refusals: a length
/// field cut short, a NAL length of zero or running past the sample -> `Corrupt h264-nal-length`; a
/// length size other than 1, 2 or 4 -> `Corrupt avcc-length-size`; output above `max_bytes` ->
/// `LimitExceeded h264-annexb-bytes` (checked before each append). On error `out` holds a prefix only and
/// must not be sent.
pub fn sample_to_annex_b(
    sample: &[u8],
    nal_length_size: u8,
    parameter_sets: Option<&H264Config>,
    max_bytes: usize,
    out: &mut Vec<u8>,
) -> Result<(), ReelError> {
    let size = usize::from(nal_length_size);
    if !matches!(size, 1 | 2 | 4) {
        return Err(ReelError::Corrupt {
            what: "avcc-length-size",
        });
    }
    out.clear();
    if let Some(config) = parameter_sets {
        config.append_parameter_sets(out, max_bytes)?;
    }
    let bad_length = || ReelError::Corrupt {
        what: "h264-nal-length",
    };
    let mut rest = sample;
    while !rest.is_empty() {
        let (field, tail) = rest.split_at_checked(size).ok_or_else(bad_length)?;
        // At most 4 bytes: a u32 holds the field exactly.
        let len = field
            .iter()
            .fold(0u32, |acc, byte| acc.wrapping_shl(8) | u32::from(*byte));
        let len = usize::try_from(len).map_err(|_| bad_length())?;
        if len == 0 {
            return Err(bad_length());
        }
        let (nal, after) = tail.split_at_checked(len).ok_or_else(bad_length)?;
        append_nal(out, nal, max_bytes)?;
        rest = after;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Baseline record: one 4-byte SPS, one 2-byte PPS, 4-byte NAL lengths.
    fn record() -> Vec<u8> {
        vec![
            1, 66, 0xC0, 10, 0xFF, 0xE1, 0, 4, 0x67, 66, 0xC0, 10, 1, 0, 2, 0x68, 0xCE,
        ]
    }

    #[test]
    fn avcc_fields_and_extension() {
        let config = parse_avcc(&record()).unwrap();
        assert_eq!(
            (config.profile_idc, config.level_idc, config.nal_length_size),
            (66, 10, 4)
        );
        assert_eq!(config.sps, vec![vec![0x67, 66, 0xC0, 10]]);
        assert_eq!(config.pps, vec![vec![0x68, 0xCE]]);
        assert_eq!(config.chroma_format, None);
        assert_eq!(config.chroma_format_or_420(), 1);
        // High 4:2:2 10-bit extension: chroma 2, luma/chroma depth 10.
        let mut high = record();
        high[1] = 122;
        high[8..10].copy_from_slice(&[0x67, 122]);
        high.extend_from_slice(&[0xFE, 0xFA, 0xFA, 0]);
        let config = parse_avcc(&high).unwrap();
        assert_eq!(
            (config.chroma_format, config.bit_depth_luma, config.bit_depth_chroma),
            (Some(2), Some(10), Some(10))
        );
        // A bit depth above 12 makes the extension invalid: ignored, not an error.
        let mut bad_ext = high.clone();
        let at = bad_ext.len() - 3;
        bad_ext[at] = 0x07;
        assert_eq!(parse_avcc(&bad_ext).unwrap().chroma_format, None);
    }

    #[test]
    fn avcc_hostile_records_are_typed() {
        // Every truncation of the fixed fields and parameter sets.
        let full = record();
        for len in 0..full.len() {
            let error = parse_avcc(&full[..len]).unwrap_err();
            assert!(
                matches!(
                    error,
                    ReelError::Corrupt {
                        what: "avcc-truncated"
                    }
                ),
                "len {len}: {error:?}"
            );
        }
        let mut version = full.clone();
        version[0] = 2;
        assert_eq!(
            parse_avcc(&version),
            Err(ReelError::Unsupported {
                what: "avcc-version"
            })
        );
        let mut three = full.clone();
        three[4] = 0xFE;
        assert_eq!(
            parse_avcc(&three),
            Err(ReelError::Corrupt {
                what: "avcc-length-size"
            })
        );
        // SPS length u16::MAX against a short record.
        let mut huge = full.clone();
        huge[6..8].copy_from_slice(&[0xFF, 0xFF]);
        assert_eq!(parse_avcc(&huge), Err(truncated()));
        // No SPS and no PPS: legal for avc3, refused by the avc1 rule.
        let no_sps = vec![1, 66, 0, 10, 0xFF, 0xE0, 0];
        let in_band = parse_avcc(&no_sps).unwrap();
        assert!(in_band.sps.is_empty() && in_band.pps.is_empty());
        assert_eq!(
            in_band.require_parameter_sets(),
            Err(ReelError::Corrupt {
                what: "avcc-parameter-set"
            })
        );
        assert_eq!(parse_avcc(&full).unwrap().require_parameter_sets(), Ok(()));
        // An SPS that is a PPS; an empty PPS.
        let wrong_type = {
            let mut r = full.clone();
            r[8] = 0x68;
            r
        };
        let empty_pps = vec![1, 66, 0, 10, 0xFF, 0xE1, 0, 1, 0x67, 1, 0, 0];
        for hostile in [wrong_type, empty_pps] {
            assert_eq!(
                parse_avcc(&hostile),
                Err(ReelError::Corrupt {
                    what: "avcc-parameter-set"
                }),
                "{hostile:?}"
            );
        }
    }

    #[test]
    fn sample_converts_to_annex_b_with_parameter_sets_on_sync() {
        let config = parse_avcc(&record()).unwrap();
        let sample = [0, 0, 0, 3, 0x65, 1, 2, 0, 0, 0, 1, 0x06];
        let mut out = Vec::new();
        sample_to_annex_b(&sample, 4, Some(&config), 1 << 20, &mut out).unwrap();
        assert_eq!(
            out,
            [
                0, 0, 0, 1, 0x67, 66, 0xC0, 10, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 1, 2, 0,
                0, 0, 1, 0x06
            ]
        );
        sample_to_annex_b(&sample, 4, None, 1 << 20, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 1, 2, 0, 0, 0, 1, 0x06]);
        // 1- and 2-byte lengths.
        sample_to_annex_b(&[2, 0x41, 9], 1, None, 64, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x41, 9]);
        sample_to_annex_b(&[0, 1, 0x41], 2, None, 64, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn hostile_samples_are_typed_and_bounded() {
        let length = Err(ReelError::Corrupt {
            what: "h264-nal-length",
        });
        let mut out = Vec::new();
        // Length field cut short, zero length, length past the sample, u32::MAX length.
        for sample in [
            &[0u8, 0, 0][..],
            &[0, 0, 0, 0],
            &[0, 0, 0, 5, 1, 2],
            &[0xFF, 0xFF, 0xFF, 0xFF, 1],
        ] {
            assert_eq!(
                sample_to_annex_b(sample, 4, None, usize::MAX, &mut out),
                length,
                "{sample:?}"
            );
        }
        assert_eq!(
            sample_to_annex_b(&[1, 0x41], 3, None, 64, &mut out),
            Err(ReelError::Corrupt {
                what: "avcc-length-size"
            })
        );
        // Aggregate budget: 1-byte lengths grow every NAL by 3 bytes; the budget is checked before append.
        let many: Vec<u8> = std::iter::repeat_n([1u8, 0x41], 100).flatten().collect();
        assert_eq!(
            sample_to_annex_b(&many, 1, None, 499, &mut out),
            Err(ReelError::LimitExceeded {
                what: "h264-annexb-bytes"
            })
        );
        assert!(out.len() <= 499);
        sample_to_annex_b(&many, 1, None, 500, &mut out).unwrap();
        assert_eq!(out.len(), 500);
        // The budget also bounds the parameter sets of a sync sample.
        let config = parse_avcc(&record()).unwrap();
        assert_eq!(
            sample_to_annex_b(&[], 4, Some(&config), 9, &mut out),
            Err(ReelError::LimitExceeded {
                what: "h264-annexb-bytes"
            })
        );
    }
}
