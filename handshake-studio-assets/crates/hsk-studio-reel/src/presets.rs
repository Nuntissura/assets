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
//! Each preset is a [`StudioExportRecipe`] ([`ExportPreset::studio_recipe`]); it is complete against the six
//! identifiers reel's writer consumes (`export_recipe::REEL_WRITER_BINDING`), not against the 453 of the
//! export parameter dictionary.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ReelError;
use crate::export::{Av1Settings, ExportRecipe, PcmChoice};
use crate::export_recipe::{ExportRecipeError, StudioExportRecipe};
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

    /// The record of this preset (STU-VID-063: the same record as a user recipe; lane SMB-PR-EXPORT-RECIPE-1):
    /// reel's exporter for the container, the codec selection, and for H.264 / HEVC the bitrate as
    /// `ADBEVideoTargetBitrate` in megabits per second.
    pub fn studio_recipe(&self) -> StudioExportRecipe {
        crate::export_recipe::preset_recipe(self)
    }

    /// The recipe of this preset (recipe id = preset id), resolved through [`ExportPreset::studio_recipe`].
    /// `os_encoder`: an OS encoder is registered on the queue the job goes to. Refused: an OS-encode preset
    /// without one, an AV1 preset without the `av1-encode` feature (`MEDIA_CODEC_UNAVAILABLE` naming the
    /// codec; pinned by tests/export_recipe.rs `preset_recipes_are_pinned`).
    pub fn recipe(&self, os_encoder: bool) -> Result<ExportRecipe, ReelError> {
        self.studio_recipe().resolve(os_encoder).map_err(ExportRecipeError::into_reel_error)
    }
}
