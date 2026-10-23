//! Export presets (lane SMB-PR-SOURCE-MONITOR slice 4; PR-24, STU-VID-063 "Export recipes are portable
//! data with stable identity ... A user recipe is the same record as a shipped one"): Studio's shipped
//! presets as [`ExportPreset`] records with stable ids, each resolving to an [`ExportRecipe`].
//!
//! H.264 / HEVC presets run only through the OS encoder (Operator decision D24-A: reel-native's `os-encode`
//! worker registered with [`crate::ExportQueue::with_video_encoder`]); [`ExportPreset::recipe`] takes
//! whether such an encoder is registered and refuses an OS-encode preset without one with the typed
//! `MEDIA_CODEC_UNAVAILABLE` naming the codec, before any job is built. AV1 presets need reel's cargo
//! feature `av1-encode` (same refusal otherwise). VC-3 (DNxHR) presets always resolve.
//!
//! The preset names and bitrates are Studio's; Premiere / Media Encoder preset parity (names, bitrates,
//! GOP, audio) is UNVERIFIED (D5): the 704 captured recipe identities of STU-VID-063 are not imported here.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ReelError;
use crate::export::{Av1Settings, ExportRecipe, PcmChoice, VideoCodecChoice};
use crate::mux::Container;
use crate::os_encode::{H264Profile, OsVideoSettings};
use crate::vc3::encode::Vc3Profile;

/// Video of a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetVideo {
    Vc3(Vc3Profile),
    Av1(Av1Settings),
    /// OS encoder only (D24-A).
    H264(OsVideoSettings),
    /// OS encoder only (D24-A).
    Hevc(OsVideoSettings),
}

/// One shipped preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportPreset {
    /// Stable identity (STU-VID-063): never a name or a path.
    pub preset_id: &'static str,
    pub name: &'static str,
    pub container: Container,
    pub video: PresetVideo,
    pub audio: Option<PcmChoice>,
}

const PCM24: Option<PcmChoice> = Some(PcmChoice { bits: 24, float: false });
const PCM16: Option<PcmChoice> = Some(PcmChoice { bits: 16, float: false });

/// Studio's shipped presets, in display order.
pub const EXPORT_PRESETS: &[ExportPreset] = &[
    ExportPreset {
        preset_id: "studio.h264.high-20mbps",
        name: "H.264 High 20 Mbit/s (OS encoder)",
        container: Container::Mp4,
        video: PresetVideo::H264(OsVideoSettings { bitrate: 20_000_000, profile: H264Profile::High }),
        audio: PCM16,
    },
    ExportPreset {
        preset_id: "studio.h264.high-8mbps",
        name: "H.264 High 8 Mbit/s (OS encoder)",
        container: Container::Mp4,
        video: PresetVideo::H264(OsVideoSettings { bitrate: 8_000_000, profile: H264Profile::High }),
        audio: PCM16,
    },
    ExportPreset {
        preset_id: "studio.hevc.main-15mbps",
        name: "HEVC Main 15 Mbit/s (OS encoder)",
        container: Container::Mp4,
        video: PresetVideo::Hevc(OsVideoSettings { bitrate: 15_000_000, profile: H264Profile::High }),
        audio: PCM16,
    },
    ExportPreset {
        preset_id: "studio.dnxhr.hq.mov",
        name: "DNxHR HQ (MOV, PCM 24-bit)",
        container: Container::Mov,
        video: PresetVideo::Vc3(Vc3Profile::DnxhrHq),
        audio: PCM24,
    },
    ExportPreset {
        preset_id: "studio.dnxhr.lb.mov",
        name: "DNxHR LB (MOV, PCM 24-bit)",
        container: Container::Mov,
        video: PresetVideo::Vc3(Vc3Profile::DnxhrLb),
        audio: PCM24,
    },
    ExportPreset {
        preset_id: "studio.av1.mp4",
        name: "AV1 (MP4, PCM 16-bit)",
        container: Container::Mp4,
        video: PresetVideo::Av1(Av1Settings { speed: 6, quantizer: 100, bitrate: None, max_keyframe_interval: 240 }),
        audio: PCM16,
    },
];

/// The preset with `preset_id`, if Studio ships it.
pub fn export_preset(preset_id: &str) -> Option<&'static ExportPreset> {
    EXPORT_PRESETS.iter().find(|p| p.preset_id == preset_id)
}

impl ExportPreset {
    /// Whether this preset needs the OS encoder (D24-A).
    pub const fn needs_os_encoder(&self) -> bool {
        matches!(self.video, PresetVideo::H264(_) | PresetVideo::Hevc(_))
    }

    /// The recipe of this preset (recipe id = preset id). `os_encoder`: an OS encoder is registered on
    /// the queue the job goes to. Refused: an OS-encode preset without one, an AV1 preset without the
    /// `av1-encode` feature (`MEDIA_CODEC_UNAVAILABLE` naming the codec).
    pub fn recipe(&self, os_encoder: bool) -> Result<ExportRecipe, ReelError> {
        let video = match self.video {
            PresetVideo::H264(s) if os_encoder => VideoCodecChoice::H264(s),
            PresetVideo::Hevc(s) if os_encoder => VideoCodecChoice::Hevc(s),
            PresetVideo::H264(_) => return Err(ReelError::CodecUnavailable { codec: "h264".into() }),
            PresetVideo::Hevc(_) => return Err(ReelError::CodecUnavailable { codec: "hevc".into() }),
            PresetVideo::Av1(s) if cfg!(feature = "av1-encode") => VideoCodecChoice::Av1(s),
            PresetVideo::Av1(_) => return Err(ReelError::CodecUnavailable { codec: "av1".into() }),
            PresetVideo::Vc3(p) => VideoCodecChoice::Vc3(p),
        };
        Ok(ExportRecipe { recipe_id: self.preset_id.to_owned(), container: self.container, video: Some(video), audio: self.audio })
    }
}
