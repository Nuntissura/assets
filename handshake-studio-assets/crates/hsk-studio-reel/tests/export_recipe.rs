//! `StudioExportRecipe` and the export path that consumes it (lane SMB-PR-EXPORT-RECIPE-1 items 4 to 9;
//! STU-IO-101..108, STU-IO-141..147, STU-IO-175..185, STU-VID-062/063). Sources are synthetic.
use hsk_studio_reel::presets::EXPORT_PRESETS;
use hsk_studio_reel::*;

/// Item 6a, pinned before the presets resolve through `StudioExportRecipe`: the exact `ExportRecipe` (or
/// typed refusal) of every shipped preset, with and without the OS encoder.
#[test]
fn preset_recipes_are_pinned() {
    let pcm16 = Some(PcmChoice { bits: 16, float: false });
    let pcm24 = Some(PcmChoice { bits: 24, float: false });
    let os = |bitrate| OsVideoSettings { bitrate, profile: H264Profile::High };
    let ok = |id: &str, container, video, audio| Ok(ExportRecipe { recipe_id: id.to_owned(), container, video: Some(video), audio });
    let none = |codec: &str| Err(ReelError::CodecUnavailable { codec: codec.to_owned() });
    let av1 = |os_encoder: bool| {
        let _ = os_encoder;
        if cfg!(feature = "av1-encode") {
            ok("studio.av1.mp4", Container::Mp4, VideoCodecChoice::Av1(Av1Settings { speed: 6, quantizer: 100, bitrate: None, max_keyframe_interval: 240 }), pcm16)
        } else {
            none("av1")
        }
    };
    let want: Vec<(&str, bool, Result<ExportRecipe, ReelError>)> = vec![
        ("studio.h264.high-20mbps", true, ok("studio.h264.high-20mbps", Container::Mp4, VideoCodecChoice::H264(os(20_000_000)), pcm16)),
        ("studio.h264.high-20mbps", false, none("h264")),
        ("studio.h264.high-8mbps", true, ok("studio.h264.high-8mbps", Container::Mp4, VideoCodecChoice::H264(os(8_000_000)), pcm16)),
        ("studio.h264.high-8mbps", false, none("h264")),
        ("studio.hevc.main-15mbps", true, ok("studio.hevc.main-15mbps", Container::Mp4, VideoCodecChoice::Hevc(os(15_000_000)), pcm16)),
        ("studio.hevc.main-15mbps", false, none("hevc")),
        ("studio.dnxhr.hq.mov", true, ok("studio.dnxhr.hq.mov", Container::Mov, VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq), pcm24)),
        ("studio.dnxhr.hq.mov", false, ok("studio.dnxhr.hq.mov", Container::Mov, VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq), pcm24)),
        ("studio.dnxhr.lb.mov", true, ok("studio.dnxhr.lb.mov", Container::Mov, VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb), pcm24)),
        ("studio.dnxhr.lb.mov", false, ok("studio.dnxhr.lb.mov", Container::Mov, VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb), pcm24)),
        ("studio.av1.mp4", true, av1(true)),
        ("studio.av1.mp4", false, av1(false)),
    ];
    assert_eq!(EXPORT_PRESETS.len(), 6);
    let got: Vec<(&str, bool, Result<ExportRecipe, ReelError>)> =
        EXPORT_PRESETS.iter().flat_map(|p| [true, false].map(|os| (p.preset_id, os, p.recipe(os)))).collect();
    assert_eq!(got, want);
}
