//! `StudioExportRecipe` and the export path that consumes it (lane SMB-PR-EXPORT-RECIPE-1 items 4 to 9;
//! STU-IO-101..108, STU-IO-141..147, STU-IO-175..185, STU-VID-062/063). Sources are synthetic.
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::presets::{EXPORT_PRESETS, export_preset};
use hsk_studio_reel::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const TPF_25: u64 = 10_160_640_000;

struct Video {
    width: u32,
    tpf: u64,
    frames: u64,
    calls: Arc<AtomicU64>,
}

impl Video {
    fn layout(&self) -> VideoFormat {
        VideoFormat {
            width: self.width,
            height: 120,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv422,
            color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
        }
    }
}

impl ExportVideoSource for Video {
    fn format(&self) -> VideoFormat {
        self.layout()
    }
    fn ticks_per_frame(&self) -> u64 {
        self.tpf
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn frame(&mut self, index: u64, _cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.layout()
            .plane_dims()
            .iter()
            .map(|&(w, h)| (0..u64::from(w) * u64::from(h)).map(|i| (64 + (i + index * 5) % 128) as u8).collect())
            .collect())
    }
}

struct Tone {
    rate: u32,
    channels: u16,
    frames: u64,
}

impl ExportAudioSource for Tone {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn render(&mut self, first: u64, frames: u32, out: &mut Vec<f32>, _cancel: &CancellationToken) -> Result<(), ReelError> {
        for f in first..first + u64::from(frames) {
            for c in 0..self.channels {
                out.push(((f as f32) * 0.01 + f32::from(c)).sin() * 0.5);
            }
        }
        Ok(())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("reel-recipe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A VC-3 LB MOV recipe with PCM 24 on reel's exporter.
fn mov_recipe(id: &str) -> StudioExportRecipe {
    StudioExportRecipe::new(id, "DNxHR LB")
        .unwrap()
        .with_target_format(ExportTargetFormat::reel(Container::Mov))
        .unwrap()
        .with_video(Some(RecipeVideoCodec::Vc3(Vc3Profile::DnxhrLb)))
        .with_audio(Some(PcmChoice { bits: 24, float: false }))
}

fn inputs(width: u32, tpf: u64, rate: u32, channels: u16, calls: &Arc<AtomicU64>, output: PathBuf) -> RecipeJobInputs {
    let frames = 3;
    RecipeJobInputs {
        video: Some(Box::new(Video { width, tpf, frames, calls: calls.clone() })),
        audio: Some(Box::new(Tone { rate, channels, frames: 3 * 1920 })),
        span: SourceSpan { start_ticks: 0, end_ticks: frames * tpf },
        output,
        overwrite: false,
        artifact: "program".to_owned(),
        limits: ExportLimits::default(),
    }
}

fn code(r: Result<(), ExportRecipeError>) -> &'static str {
    r.err().map_or("ok", |e| e.code())
}

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

/// Item 4: every value is typed and checked when set, never clamped; a refusal stores nothing.
#[test]
fn parameters_are_typed_checked_and_never_clamped() {
    let mut r = StudioExportRecipe::new("r1", "checks").unwrap();
    let mut set = |id: &str, v: ExportValue| code(r.set_parameter(id, v));
    use ExportValue::{Bool, Float, Integer, Text};
    let cases: Vec<(&str, ExportValue, &str)> = vec![
        // Declared usable hard bounds (STU-FX-104), i64 extremes included.
        ("ADBEVideoWidth", Integer(3), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoWidth", Integer(30_001), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoWidth", Integer(i64::MIN), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoWidth", Integer(i64::MAX), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoTargetBitrate", Float(0.18), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoTargetBitrate", Float(240.5), "EFFECT_PARAM_OUT_OF_RANGE"),
        // One-sided row: the known side is enforced, the other not (then the unit rule applies).
        ("ADBE10BitWhitePoint", Integer(1024), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBE10BitWhitePoint", Integer(-5), "export_parameter_missing_unit"),
        // No declared bound: no range refusal (unclamped); a numeric still needs a unit.
        ("13", Integer(i64::MAX), "export_parameter_missing_unit"),
        ("ADBEAACAudioMode", Integer(2), "export_parameter_missing_unit"),
        // Degenerate 0 / -1 is not range-enforced; a codec integer needs the exporter first (STU-IO-177).
        ("ADBEVideoCodec", Integer(5), "export_codec_integer_without_exporter"),
        // Kinds, non-finite numbers and -0.0.
        ("ADBEVideoWidth", Float(264.0), "export_parameter_wrong_kind"),
        ("ADBEVideoTargetBitrate", Integer(20), "export_parameter_wrong_kind"),
        ("ADBEVideoTargetBitrate", Float(f64::NAN), "export_parameter_non_finite"),
        ("ADBEVideoTargetBitrate", Float(f64::INFINITY), "export_parameter_non_finite"),
        ("ADBEVideoTargetBitrate", Float(f64::NEG_INFINITY), "export_parameter_non_finite"),
        ("ADBEVideoTargetBitrate", Float(-0.0), "export_parameter_negative_zero"),
        ("ADBEHiddenTicksPerFrame", Integer(1), "export_parameter_wrong_kind"),
        ("ADBEAudioChannelConfiguration", Integer(1), "export_parameter_wrong_kind"),
        ("ADBEAudioChannelConfiguration", Text("x".repeat(4097)), "export_recipe_limit"),
        // Structure rows store no value (codes 1, 7, 8, 11).
        ("ADBEAACAudioOversampled", Bool(true), "export_parameter_takes_no_value"),
        ("ADBECodecPrefs", Bool(true), "export_parameter_takes_no_value"),
        ("ADBEVideoCodecGroup", Integer(0), "export_parameter_takes_no_value"),
        ("ADBEVideoAspect", Text("0,0".into()), "export_parameter_takes_no_value"),
        // Frame rate is integer ticks (STU-IO-178).
        ("ADBEVideoFPS", Float(29.97), "export_frame_rate_stored_as_float"),
        ("ADBEVideoFPS", Integer(0), "EFFECT_PARAM_OUT_OF_RANGE"),
        ("ADBEVideoFPS", Integer(254_016_000_001), "EFFECT_PARAM_OUT_OF_RANGE"),
        // Credentials, by field (STU-IO-147, STU-IO-185), before any other rule.
        ("ADBEFTPPassword", Integer(1234), "export_credential_in_recipe_record"),
        ("ADBEStockRefreshToken", Text("secret-refresh-token".into()), "export_credential_in_recipe_record"),
        ("ADBEStockLogin", Bool(true), "export_credential_in_recipe_record"),
        ("ADBEYouTubeThumbnailPoster", Text("#fff".into()), "export_credential_in_recipe_record"),
        // Unknown identifiers.
        ("NoSuchParameter", Integer(1), "export_parameter_unknown"),
    ];
    for (id, value, want) in cases {
        assert_eq!(set(id, value.clone()), want, "{id} = {value:?}");
    }
    assert_eq!(set(&"A".repeat(129), Integer(1)), "export_parameter_unknown");
    assert!(r.parameters().is_empty(), "a refusal stores nothing");
    // Accepted: bounds inclusive, unbounded non-numeric rows unclamped, both 29.97 tick values distinct.
    let mut set = |id: &str, v: ExportValue| code(r.set_parameter(id, v));
    for (id, value) in [
        ("ADBEVideoTargetBitrate", Float(0.19)),
        ("ADBEVideoWidth", Integer(30_000)),
        ("ADBEAudioNumChannels", Integer(-51)),
        ("ADBEHiddenTicksPerFrame", Bool(true)),
        ("ADBEAudioChannelConfiguration", Text("x".repeat(4096))),
        ("0", Text("opaque".into())),
        ("ADBEVideoFPS", Integer(8_475_667_200)),
    ] {
        assert_eq!(set(id, value.clone()), "ok", "{id} = {value:?}");
    }
    assert_eq!(r.parameters().len(), 7);
    let mut other = r.clone();
    other.set_parameter("ADBEVideoFPS", Integer(8_475_675_675)).unwrap();
    assert_ne!(other.parameters()["ADBEVideoFPS"], r.parameters()["ADBEVideoFPS"]);
    assert_ne!(other.to_json(), r.to_json());
    // Messages: bounds as declared; credentials and unknown identifiers never echo a value.
    let e = r.set_parameter("ADBEVideoTargetBitrate", Float(240.5)).unwrap_err();
    assert_eq!(e.to_string(), "EFFECT_PARAM_OUT_OF_RANGE identifier=ADBEVideoTargetBitrate value=240.5 hard_min=0.19 hard_max=240");
    let e = r.set_parameter("ADBEStockRefreshToken", Text("secret-refresh-token".into())).unwrap_err();
    assert_eq!(e.to_string(), "export_credential_in_recipe_record recipe=r1 field=ADBEStockRefreshToken");
    let e = r.set_parameter("SecretLookingName", Integer(1)).unwrap_err();
    assert!(!e.to_string().contains("SecretLookingName"), "{e}");
    // With the exporter identity the codec integer passes the exporter rule and meets the unit rule.
    let mut with_exporter = mov_recipe("r2");
    assert_eq!(code(with_exporter.set_parameter("ADBEVideoCodec", Integer(5))), "export_parameter_missing_unit");
}

/// Item 7: record fields are bounded before they are stored.
#[test]
fn record_fields_are_bounded() {
    let invalid = |id: &str| StudioExportRecipe::new(id, "n").err().map(|e| e.code());
    let long_id = "a".repeat(129);
    for id in ["", "has space", "caf\u{e9}", "tab\t", long_id.as_str()] {
        assert_eq!(invalid(id), Some("export_recipe_field_invalid"), "{id:?}");
    }
    assert!(StudioExportRecipe::new(&"a".repeat(128), &"n".repeat(256)).is_ok());
    assert_eq!(StudioExportRecipe::new("r", &"n".repeat(257)).unwrap_err().code(), "export_recipe_limit");
    let foreign = ExportTargetFormat { exporter_class_id: "x".repeat(65), exporter_file_type: "mov".into() };
    assert_eq!(StudioExportRecipe::new("r", "n").unwrap().with_target_format(foreign).unwrap_err().code(), "export_recipe_field_invalid");
    let mut r = StudioExportRecipe::new("r", "n").unwrap();
    let long = "a".repeat(256);
    for bad in ["", ".", "..", "a..b", "a/b", "a\\b", "c:", "a\0b", long.as_str()] {
        assert_eq!(code(r.add_output_target(bad)), "export_recipe_field_invalid", "{bad:?}");
    }
    for i in 0..64 {
        r.add_output_target(&format!("out-{i}.mov")).unwrap();
        r.add_standard_filter(&format!("filter-{i}")).unwrap();
    }
    assert_eq!(code(r.add_output_target("one-more.mov")), "export_recipe_limit");
    assert_eq!(code(r.add_standard_filter("one-more")), "export_recipe_limit");
    assert_eq!((r.output_targets().len(), r.standard_filters().len()), (64, 64));
    let backwards = SourceScope::Span(SourceSpan { start_ticks: 10, end_ticks: 9 });
    assert_eq!(StudioExportRecipe::new("r", "n").unwrap().with_source_scope(backwards).unwrap_err().code(), "export_recipe_field_invalid");
}

/// Item 9: deterministic, hand-written JSON; parameters in identifier byte order.
#[test]
fn recipe_json_is_deterministic() {
    let mut r = mov_recipe("studio.user.mov@1");
    r.set_parameter("ADBEVideoWidth", ExportValue::Integer(264)).unwrap();
    r.set_parameter("ADBEAudioRatePerSecond", ExportValue::Integer(48_000)).unwrap();
    r.set_parameter("ADBEHiddenTicksPerFrame", ExportValue::Bool(false)).unwrap();
    r.set_parameter("0", ExportValue::Text("q\"\\\n".into())).unwrap();
    let json = r.to_json();
    assert_eq!(json, r.clone().to_json());
    assert_eq!(
        json,
        "{\"recipe_id\":\"studio.user.mov@1\",\"name\":\"DNxHR LB\",\"target_format\":{\"exporter_class_id\":\"reel-isobmff-mux\",\"exporter_file_type\":\"mov\"},\"parameters\":{\"0\":\"q\\\"\\\\\\u000a\",\"ADBEAudioRatePerSecond\":48000,\"ADBEHiddenTicksPerFrame\":false,\"ADBEVideoWidth\":264},\"source_scope\":{\"kind\":\"entire_source\"},\"output_targets\":[],\"does_video\":true,\"does_audio\":true,\"video\":{\"codec\":\"vc3\",\"profile\":\"DnxhrLb\"},\"audio\":{\"pcm_bits\":24,\"float\":false},\"metadata_export_option\":\"none\",\"standard_filters\":[]}"
    );
    let h264 = export_preset("studio.h264.high-20mbps").unwrap().studio_recipe().to_json();
    assert!(h264.contains("\"parameters\":{\"ADBEVideoTargetBitrate\":20.0}"), "{h264}");
}

/// Item 6a and STU-VID-063: each preset is a `StudioExportRecipe` that the public builder rebuilds, and its
/// resolution is the preset's recipe.
#[test]
fn presets_are_studio_recipes() {
    for p in EXPORT_PRESETS {
        let s = p.studio_recipe();
        let target = s.target_format().cloned().unwrap();
        assert_eq!(target, ExportTargetFormat::reel(p.container));
        let mut rebuilt = StudioExportRecipe::new(p.preset_id, p.name).unwrap().with_target_format(target).unwrap().with_video(s.video()).with_audio(s.audio());
        for (id, v) in s.parameters() {
            rebuilt.set_parameter(id, v.clone()).unwrap();
        }
        assert_eq!(rebuilt, s, "{}", p.preset_id);
        assert!(s.does_video() && s.does_audio());
        for os in [true, false] {
            assert_eq!(s.resolve(os).map_err(ExportRecipeError::into_reel_error), p.recipe(os), "{}", p.preset_id);
        }
    }
}

/// Item 6d / 6e: everything the writer does not read is refused before any job exists.
#[test]
fn consumption_refuses_what_the_writer_does_not_read() {
    let resolve = |r: &StudioExportRecipe| r.resolve(true).err().map(|e| e.to_string());
    let bare = StudioExportRecipe::new("r", "n").unwrap().with_video(Some(RecipeVideoCodec::Vc3(Vc3Profile::DnxhrLb)));
    assert_eq!(resolve(&bare).as_deref(), Some("export_exporter_unavailable exporter=none"));
    let foreign = bare.clone().with_target_format(ExportTargetFormat { exporter_class_id: "NICK".into(), exporter_file_type: "H264".into() }).unwrap();
    assert_eq!(resolve(&foreign).as_deref(), Some("export_exporter_unavailable exporter=NICK/H264"));
    let mut r = mov_recipe("r");
    r.set_parameter("ADBEHiddenTicksPerFrame", ExportValue::Bool(true)).unwrap();
    assert_eq!(resolve(&r).as_deref(), Some("export_parameter_not_consumed identifier=ADBEHiddenTicksPerFrame"));
    let mut r = mov_recipe("r");
    r.set_parameter("ADBEVideoTargetBitrate", ExportValue::Float(20.0)).unwrap();
    assert_eq!(resolve(&r).as_deref(), Some("export_parameter_not_consumed identifier=ADBEVideoTargetBitrate"), "no bitrate on VC-3");
    let mut r = mov_recipe("r").with_video(None);
    r.set_parameter("ADBEVideoWidth", ExportValue::Integer(264)).unwrap();
    assert_eq!(resolve(&r).as_deref(), Some("export_parameter_not_consumed identifier=ADBEVideoWidth"), "no video");
    let mut r = mov_recipe("r");
    r.add_output_target("program.mov").unwrap();
    assert_eq!(resolve(&r).as_deref(), Some("export_parameter_not_consumed identifier=output_targets"));
    let mut r = mov_recipe("r");
    r.add_standard_filter("loudness").unwrap();
    assert_eq!(resolve(&r).as_deref(), Some("export_parameter_not_consumed identifier=standard_filters"));
    let h264 = StudioExportRecipe::new("h", "n").unwrap().with_target_format(ExportTargetFormat::reel(Container::Mp4)).unwrap().with_video(Some(RecipeVideoCodec::H264(H264Profile::Main)));
    assert_eq!(resolve(&h264).as_deref(), Some("export_preset_incomplete recipe=h missing=ADBEVideoTargetBitrate"));
    let hevc = h264.clone().with_video(Some(RecipeVideoCodec::Hevc));
    assert_eq!(resolve(&hevc).as_deref(), Some("export_preset_incomplete recipe=h missing=ADBEVideoTargetBitrate"));
    let mut h264 = h264;
    h264.set_parameter("ADBEVideoTargetBitrate", ExportValue::Float(12.5)).unwrap();
    assert_eq!(h264.resolve(false).unwrap_err().code(), "MEDIA_CODEC_UNAVAILABLE");
    let resolved = h264.resolve(true).unwrap();
    assert_eq!(resolved.video, Some(VideoCodecChoice::H264(OsVideoSettings { bitrate: 12_500_000, profile: H264Profile::Main })));
    assert_eq!(hsk_studio_reel::export_recipe::megabits_to_bits(f64::MAX), None, "conversion overflow is typed");
    assert_eq!(hsk_studio_reel::export_recipe::megabits_to_bits(0.19), Some(190_000));
}

/// Item 6f: a VC-3 MOV through `job_from_recipe` publishes the same bytes as the same job built by hand.
#[test]
fn job_from_recipe_publishes_the_hand_built_bytes() {
    let d = dir("e2e");
    let mut recipe = mov_recipe("studio.user.dnxhr-lb@1");
    for (id, v) in [
        ("ADBEVideoWidth", 264),
        ("ADBEVideoHeight", 120),
        ("ADBEVideoFPS", TPF_25 as i64),
        ("ADBEAudioRatePerSecond", 48_000),
        ("ADBEAudioNumChannels", 2),
    ] {
        recipe.set_parameter(id, ExportValue::Integer(v)).unwrap();
    }
    let calls = Arc::new(AtomicU64::new(0));
    let from_recipe = job_from_recipe(&recipe, inputs(264, TPF_25, 48_000, 2, &calls, d.join("recipe.mov")), false).unwrap();
    let mut by_hand = inputs(264, TPF_25, 48_000, 2, &calls, d.join("hand.mov"));
    let hand = ExportJob {
        recipe: ExportRecipe {
            recipe_id: "studio.user.dnxhr-lb@1".into(),
            container: Container::Mov,
            video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb)),
            audio: Some(PcmChoice { bits: 24, float: false }),
        },
        video: by_hand.video.take(),
        audio: by_hand.audio.take(),
        span: by_hand.span,
        output: by_hand.output.clone(),
        overwrite: false,
        artifact: "program".into(),
        limits: ExportLimits::default(),
    };
    let mut q = ExportQueue::new();
    q.submit(from_recipe);
    q.submit(hand);
    let receipts = q.run();
    assert!(receipts.iter().all(|r| r.outcome == ExportOutcome::Complete), "{receipts:?}");
    let (a, b) = (std::fs::read(d.join("recipe.mov")).unwrap(), std::fs::read(d.join("hand.mov")).unwrap());
    assert!(a == b && receipts[0].output_sha256 == receipts[1].output_sha256, "identical bytes");
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 6b / 6c: consumed identifiers must equal the source facts, read before any frame; the writer keeps
/// its own bounds (width 20000 is inside the dictionary and refused by the muxer).
#[test]
fn job_from_recipe_checks_source_facts_before_any_frame() {
    let d = dir("facts");
    let calls = Arc::new(AtomicU64::new(0));
    let with = |id: &str, v: i64| {
        let mut r = mov_recipe("r");
        r.set_parameter(id, ExportValue::Integer(v)).unwrap();
        r
    };
    let refused = |r: &StudioExportRecipe, tpf: u64| job_from_recipe(r, inputs(264, tpf, 48_000, 2, &calls, d.join("x.mov")), false).err().map(|e| e.to_string());
    assert_eq!(refused(&with("ADBEVideoWidth", 265), TPF_25).as_deref(), Some("export_parameter_source_mismatch identifier=ADBEVideoWidth declared=265 source=264"));
    assert_eq!(refused(&with("ADBEVideoHeight", 121), TPF_25).as_deref(), Some("export_parameter_source_mismatch identifier=ADBEVideoHeight declared=121 source=120"));
    // The two 29.97 rates stay distinct (STU-IO-178).
    assert_eq!(
        refused(&with("ADBEVideoFPS", 8_475_675_675), 8_475_667_200).as_deref(),
        Some("export_parameter_source_mismatch identifier=ADBEVideoFPS declared=8475675675 source=8475667200")
    );
    assert_eq!(refused(&with("ADBEVideoFPS", 8_475_667_200), 8_475_667_200), None);
    assert_eq!(refused(&with("ADBEAudioRatePerSecond", 44_100), TPF_25).as_deref(), Some("export_parameter_source_mismatch identifier=ADBEAudioRatePerSecond declared=44100 source=48000"));
    assert_eq!(refused(&with("ADBEAudioNumChannels", 3), TPF_25).as_deref(), Some("export_parameter_source_mismatch identifier=ADBEAudioNumChannels declared=3 source=2"));
    assert_eq!(refused(&with("ADBEAudioNumChannels", -2), TPF_25).as_deref(), Some("export_parameter_meaning_unestablished identifier=ADBEAudioNumChannels value=-2"));
    let scoped = mov_recipe("r").with_source_scope(SourceScope::Span(SourceSpan { start_ticks: 0, end_ticks: TPF_25 })).unwrap();
    assert!(refused(&scoped, TPF_25).unwrap().starts_with("export_parameter_source_mismatch identifier=source_scope"));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "source facts only, no frame");
    // Inside the dictionary (4..30000), equal to the source, refused by the writer before the first frame.
    let wide = with("ADBEVideoWidth", 20_000);
    let job = job_from_recipe(&wide, inputs(20_000, TPF_25, 48_000, 2, &calls, d.join("wide.mov")), false).unwrap();
    let mut q = ExportQueue::new();
    q.submit(job);
    let r = q.run().remove(0);
    assert!(matches!(&r.outcome, ExportOutcome::Failed { detail, .. } if detail.contains("dimensions")), "{r:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "writer refusal before the first frame");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 5: the declaration of each container (facts of `check_combination`; the in-crate grid pins it).
#[test]
fn containers_declare_what_they_carry() {
    let mp4 = Container::Mp4.capability();
    let mov = Container::Mov.capability();
    assert_eq!((mp4.exporter_class_id, mp4.exporter_file_type, mov.exporter_file_type), (MUXER_ID, "mp4", "mov"));
    assert_eq!((mp4.video_codecs, mov.video_codecs), (&["av1", "h264", "hevc"][..], &["av1", "h264", "hevc", "vc3"][..]));
    assert_eq!((mp4.max_pcm_rate, mov.max_pcm_rate), (Some(65_535), None));
    assert_eq!((mp4.colour_metadata, mov.colour_metadata), ("nclx", "nclc"));
    assert_eq!((mp4.caption_streams, mp4.timecode_tracks, mov.timecode_tracks), (&["tx3g"][..], false, false));
    assert!(!mp4.carries(Some("vc3"), None) && mov.carries(Some("vc3"), Some((24, false, 48_000))));
}
