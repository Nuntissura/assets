//! H.264 / HEVC encode through the OPERATING SYSTEM's encoder (Operator decision D24-A, lane
//! SMB-D24A-CODEC-DEFAULTS): reel never encodes H.264 or HEVC itself and bundles no encoder. The encoder is
//! an external, isolated one (reel-native's Media Foundation encoder-MFT worker, its cargo feature
//! `os-encode`, default off) registered on the export queue (`ExportQueue::with_video_encoder`). Without one
//! an H.264 / HEVC recipe is the typed `MEDIA_CODEC_UNAVAILABLE` before the first frame; where the OS has no
//! encoder (HEVC needs the paid Windows HEVC Video Extensions) the encoder's own refusal is the result.
//!
//! This module holds the port (`ExternalVideoEncoder`, `VideoEncodeSession`) and the pure parts the export
//! needs around it: Annex B splitting, ISO BMFF sample conversion (4-byte NAL lengths, parameter sets and
//! access-unit delimiters moved out of the samples) and the `avcC` / `hvcC` records built from the first key
//! unit's parameter sets (ISO/IEC 14496-15 5.3.3.1 and 8.3.3.1; field layout cross-checked against FFmpeg's
//! `ff_isom_write_avcc` / `ff_isom_write_hvcc`, read for layout only, no code copied).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::ReelError;
use crate::video::DecoderIdentity;
use hsk_studio_accord::CancellationToken;

/// Codec an OS encoder produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsVideoCodec {
    H264,
    Hevc,
}

impl OsVideoCodec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
        }
    }
}

/// H.264 profile of an OS-encoded recipe (`MF_MT_MPEG2_PROFILE` eAVEncH264VProfile values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum H264Profile {
    Baseline,
    Main,
    #[default]
    High,
}

impl H264Profile {
    pub const fn profile_idc(self) -> u32 {
        match self {
            Self::Baseline => 66,
            Self::Main => 77,
            Self::High => 100,
        }
    }
}

/// Rate settings of an OS-encoded H.264 / HEVC recipe (STU-IO-143 rate-control facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsVideoSettings {
    /// Average bitrate in bits per second (1 ..= 400 Mbit/s).
    pub bitrate: u32,
    /// H.264 only; HEVC is always Main (8-bit 4:2:0).
    pub profile: H264Profile,
}

impl Default for OsVideoSettings {
    fn default() -> Self {
        Self {
            bitrate: 8_000_000,
            profile: H264Profile::High,
        }
    }
}

/// Highest average bitrate an OS-encode recipe may ask for.
pub const OS_MAX_BITRATE: u32 = 400_000_000;

/// What the external encoder is told before the first frame (all checked by reel first: even sides,
/// 8-bit 4:2:0 frames, a frame rate whose rational fits u32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsEncodeSpec {
    pub codec: OsVideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub bitrate: u32,
    /// `MF_MT_MPEG2_PROFILE` value (H.264 66/77/100; HEVC 1 = Main).
    pub profile: u32,
}

/// One coded access unit as the OS encoder releases it: Annex B bytes and the frame index it presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedUnit {
    pub annex_b: Vec<u8>,
    pub frame_index: u64,
    pub keyframe: bool,
}

/// One open encode. Frames arrive in presentation order (tight Y, U, V planes, 8-bit 4:2:0); units leave in
/// decode order with their presentation index.
pub trait VideoEncodeSession: Send {
    fn encode(
        &mut self,
        planes: &[&[u8]],
        frame_index: u64,
        cancel: &CancellationToken,
        out: &mut Vec<EncodedUnit>,
    ) -> Result<(), ReelError>;
    /// No more frames: release every unit still held.
    fn finish(&mut self, cancel: &CancellationToken, out: &mut Vec<EncodedUnit>) -> Result<(), ReelError>;
}

/// An encoder outside the caller's process (reel-native `os-encode`).
pub trait ExternalVideoEncoder: Send + Sync {
    /// Encoder id and version (receipts) of `codec`.
    fn identity(&self, codec: OsVideoCodec) -> DecoderIdentity;
    fn supports(&self, codec: OsVideoCodec) -> bool;
    fn open(
        &self,
        spec: &OsEncodeSpec,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoEncodeSession>, ReelError>;
}

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

/// NAL units of an Annex B byte stream (3- or 4-byte start codes; trailing zero bytes of a unit dropped).
pub fn split_annex_b(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while let Some(window) = stream.get(i..i.saturating_add(3)) {
        if window == [0, 0, 1] {
            starts.push(i.saturating_add(3));
            i = i.saturating_add(3);
        } else {
            i = i.saturating_add(1);
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let end = starts
            .get(n.saturating_add(1))
            .map_or(stream.len(), |next| next.saturating_sub(3));
        let mut unit = stream.get(start..end).unwrap_or(&[]);
        while let [rest @ .., 0] = unit {
            unit = rest;
        }
        if !unit.is_empty() {
            units.push(unit);
        }
    }
    units
}

/// NAL unit type of a unit's first header byte(s).
fn nal_type(codec: OsVideoCodec, unit: &[u8]) -> u8 {
    let first = unit.first().copied().unwrap_or(0);
    match codec {
        OsVideoCodec::H264 => first & 0x1F,
        OsVideoCodec::Hevc => (first >> 1) & 0x3F,
    }
}

/// Parameter sets of a stream (first one of each kind) and the sample conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParameterSets {
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Converts one Annex B access unit into an ISO BMFF sample (4-byte big-endian NAL lengths) without its
/// parameter sets and access-unit delimiters, which go to `sets`; a parameter set that differs from the
/// first one of its kind is refused (`os-encode-parameter-set-change`: one sample entry per track).
pub fn annex_b_to_sample(
    codec: OsVideoCodec,
    unit: &[u8],
    sets: &mut ParameterSets,
) -> Result<Vec<u8>, ReelError> {
    let mut sample = Vec::with_capacity(unit.len().saturating_add(16));
    for nal in split_annex_b(unit) {
        let kind = nal_type(codec, nal);
        let slot = match (codec, kind) {
            (OsVideoCodec::H264, 7) | (OsVideoCodec::Hevc, 33) => Some(&mut sets.sps),
            (OsVideoCodec::H264, 8) | (OsVideoCodec::Hevc, 34) => Some(&mut sets.pps),
            (OsVideoCodec::Hevc, 32) => Some(&mut sets.vps),
            // Access-unit delimiters carry nothing the sample needs.
            (OsVideoCodec::H264, 9) | (OsVideoCodec::Hevc, 35) => continue,
            _ => None,
        };
        if let Some(slot) = slot {
            match slot {
                Some(known) if known.as_slice() != nal => {
                    return Err(ReelError::Unsupported {
                        what: "os-encode-parameter-set-change",
                    });
                }
                Some(_) => {}
                None => *slot = Some(nal.to_vec()),
            }
            continue;
        }
        let len = u32::try_from(nal.len()).map_err(|_| corrupt("os-encode-nal-size"))?;
        sample.extend_from_slice(&len.to_be_bytes());
        sample.extend_from_slice(nal);
    }
    if sample.is_empty() {
        return Err(corrupt("os-encode-empty-unit"));
    }
    Ok(sample)
}

/// `avcC` (version 1, 4-byte NAL lengths, one SPS, one PPS; the High-profile chroma / bit-depth extension
/// for 8-bit 4:2:0) from the stream's first parameter sets.
pub fn build_avcc(sets: &ParameterSets) -> Result<Vec<u8>, ReelError> {
    let (Some(sps), Some(pps)) = (&sets.sps, &sets.pps) else {
        return Err(corrupt("os-encode-missing-parameter-sets"));
    };
    let (profile, compat, level) = match sps.as_slice() {
        [_, p, c, l, ..] => (*p, *c, *l),
        _ => return Err(corrupt("os-encode-sps")),
    };
    let sps_len = u16::try_from(sps.len()).map_err(|_| corrupt("os-encode-sps"))?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| corrupt("os-encode-pps"))?;
    let mut out = vec![1, profile, compat, level, 0xFF, 0xE1];
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    if matches!(profile, 100 | 110 | 122 | 144) {
        // chroma_format 1 (4:2:0), bit depths 8, no SPS extensions.
        out.extend_from_slice(&[0xFD, 0xF8, 0xF8, 0x00]);
    }
    Ok(out)
}

/// Bits of an RBSP (emulation-prevention bytes removed).
struct Rbsp {
    bytes: Vec<u8>,
    bit: usize,
}

impl Rbsp {
    fn new(nal_payload: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(nal_payload.len());
        let mut zeros = 0usize;
        for &b in nal_payload {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros.saturating_add(1) } else { 0 };
            bytes.push(b);
        }
        Self { bytes, bit: 0 }
    }

    fn u(&mut self, n: u32) -> Result<u64, ReelError> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self
                .bytes
                .get(self.bit.checked_div(8).unwrap_or(0))
                .ok_or_else(|| corrupt("os-encode-sps"))?;
            let shift = 7u32.saturating_sub(u32::try_from(self.bit & 7).unwrap_or(0));
            v = (v << 1) | u64::from(byte.checked_shr(shift).unwrap_or(0) & 1);
            self.bit = self.bit.saturating_add(1);
        }
        Ok(v)
    }

    fn ue(&mut self) -> Result<u64, ReelError> {
        let mut zeros = 0u32;
        while self.u(1)? == 0 {
            zeros = zeros.saturating_add(1);
            if zeros > 31 {
                return Err(corrupt("os-encode-sps"));
            }
        }
        let rest = self.u(zeros)?;
        Ok(1u64
            .checked_shl(zeros)
            .unwrap_or(0)
            .saturating_sub(1)
            .saturating_add(rest))
    }
}

/// `hvcC` (version 1, 4-byte NAL lengths, one VPS / SPS / PPS array each) from the stream's first
/// parameter sets; profile, tier, level, constraint flags, chroma format and bit depths are read from the SPS
/// (H.265 7.3.2.2 / 7.3.3).
pub fn build_hvcc(sets: &ParameterSets) -> Result<Vec<u8>, ReelError> {
    let (Some(vps), Some(sps), Some(pps)) = (&sets.vps, &sets.sps, &sets.pps) else {
        return Err(corrupt("os-encode-missing-parameter-sets"));
    };
    let mut r = Rbsp::new(sps.get(2..).ok_or_else(|| corrupt("os-encode-sps"))?);
    r.u(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u(3)?;
    let temporal_id_nested = r.u(1)?;
    // profile_tier_level(1, max_sub_layers_minus1): general part.
    let ptl_start = r.bit;
    r.u(2 + 1 + 5)?;
    r.u(32)?;
    r.u(48)?;
    r.u(8)?;
    let ptl_end = r.bit;
    let general: Vec<u8> = {
        let mut copy = Rbsp {
            bytes: r.bytes.clone(),
            bit: ptl_start,
        };
        let mut out = Vec::with_capacity(12);
        while copy.bit < ptl_end {
            out.push(u8::try_from(copy.u(8)?).unwrap_or(0));
        }
        out
    };
    let mut present = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        present.push((r.u(1)?, r.u(1)?));
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.u(2)?;
        }
    }
    for (profile, level) in present {
        if profile == 1 {
            r.u(88)?;
        }
        if level == 1 {
            r.u(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma = r.ue()?;
    if chroma == 3 {
        r.u(1)?;
    }
    r.ue()?;
    r.ue()?;
    if r.u(1)? == 1 {
        for _ in 0..4 {
            r.ue()?;
        }
    }
    let depth_luma = r.ue()?;
    let depth_chroma = r.ue()?;
    if chroma > 3 || depth_luma > 7 || depth_chroma > 7 || general.len() != 12 {
        return Err(corrupt("os-encode-sps"));
    }
    let as_u8 = |v: u64| u8::try_from(v).unwrap_or(0);
    let mut out = vec![1];
    out.extend_from_slice(&general); // profile space/tier/idc, compatibility, constraints, level
    out.extend_from_slice(&[0xF0, 0x00, 0xFC, 0xFC | as_u8(chroma)]);
    out.push(0xF8 | as_u8(depth_luma));
    out.push(0xF8 | as_u8(depth_chroma));
    out.extend_from_slice(&[0, 0]); // avgFrameRate
    let layers = as_u8(max_sub_layers_minus1.saturating_add(1)) & 7;
    out.push((layers << 3) | (as_u8(temporal_id_nested) << 2) | 3);
    out.push(3);
    for (kind, nal) in [(32u8, vps), (33, sps), (34, pps)] {
        let len = u16::try_from(nal.len()).map_err(|_| corrupt("os-encode-sps"))?;
        out.push(0x80 | kind);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    Ok(out)
}

/// Minimal `hvcC` check for the muxer: version 1, 4-byte NAL lengths, VPS, SPS and PPS arrays present and
/// every array and unit length inside the record.
pub fn parse_hvcc(record: &[u8]) -> Result<(), ReelError> {
    let bad = || corrupt("hvcc");
    if record.len() < 23 || record.first() != Some(&1) || record.get(21).map(|b| b & 3) != Some(3) {
        return Err(bad());
    }
    let arrays = *record.get(22).ok_or_else(bad)?;
    let mut at = 23usize;
    let mut seen = [false; 3];
    for _ in 0..arrays {
        let kind = record.get(at).ok_or_else(bad)? & 0x3F;
        let count = u16::from_be_bytes([*record.get(at.saturating_add(1)).ok_or_else(bad)?, *record.get(at.saturating_add(2)).ok_or_else(bad)?]);
        at = at.saturating_add(3);
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([*record.get(at).ok_or_else(bad)?, *record.get(at.saturating_add(1)).ok_or_else(bad)?]));
            at = at.saturating_add(2).checked_add(len).filter(|end| *end <= record.len()).ok_or_else(bad)?;
            if len > 0 && (32..=34).contains(&kind) {
                seen[usize::from(kind.saturating_sub(32))] = true;
            }
        }
    }
    if at != record.len() || seen != [true; 3] {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Annex B with 3- and 4-byte start codes and trailing zeros: parameter sets and the delimiter leave the
    /// sample, slices keep 4-byte lengths, a changed SPS is refused, an empty unit is corrupt, and the avcC
    /// built from the sets parses with reel's own avcC parser.
    #[test]
    fn annex_b_to_avc_sample_and_avcc() {
        let sps = [0x67, 100, 0, 30, 0xAC, 0xD9];
        let pps = [0x68, 0xEB, 0xE3];
        let mut unit = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1];
        unit.extend_from_slice(&sps);
        unit.extend_from_slice(&[0, 0, 0, 1]);
        unit.extend_from_slice(&pps);
        unit.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0, 0]);
        let mut sets = ParameterSets::default();
        let sample = annex_b_to_sample(OsVideoCodec::H264, &unit, &mut sets).unwrap();
        assert_eq!(sample, [0, 0, 0, 3, 0x65, 0x88, 0x84]);
        assert_eq!(sets.sps.as_deref(), Some(&sps[..]));
        let avcc = build_avcc(&sets).unwrap();
        let parsed = crate::h264_config::parse_avcc(&avcc).unwrap();
        assert_eq!((parsed.profile_idc, parsed.level_idc, parsed.nal_length_size), (100, 30, 4));
        let mut changed = unit.clone();
        changed[13] = 0xAD;
        assert_eq!(
            annex_b_to_sample(OsVideoCodec::H264, &changed, &mut sets),
            Err(ReelError::Unsupported {
                what: "os-encode-parameter-set-change"
            })
        );
        assert!(annex_b_to_sample(OsVideoCodec::H264, &[0, 0, 1, 0x09, 0xF0], &mut sets).is_err());
        assert!(build_avcc(&ParameterSets::default()).is_err());
        assert!(parse_hvcc(&[1; 22]).is_err());
    }
}
