//! Lane SMB-PR-SEQUENCE-PRESETS-1: the STU-VID-011 sequence settings record (with STU-VID-018
//! immersive and STU-VID-019 preview settings), the STU-VID-012a time-display enumeration, the
//! STU-VID-016/016a sequence-preset section of the style registry with its shipped library and typed
//! commands, and the STU-VID-017/017a editing modes. Test names carry the MT they prove.
use hsk_studio_accord::{
    ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, Ticks,
};
use hsk_studio_folio::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const F25: u64 = 10_160_640_000;
const F2997: u64 = 8_475_667_200;

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn sequence(sequence_id: &str, ticks_per_frame: u64) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: sequence_id.into(),
        name: sequence_id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame },
        duration: Ticks::new(0),
        tracks: Vec::new(),
        markers: Vec::new(),
        work_area: None,
    }
}

/// A document with `seq-a` (25 fps) and `seq-b` (29.97 fps), both without settings entries.
fn document() -> StudioDocument {
    let json = json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    let mut d: StudioDocument = serde_json::from_value(json).unwrap();
    d.sequences.push(sequence("seq-a", F25));
    d.sequences.push(sequence("seq-b", F2997));
    d
}

/// A complete 25 fps HD settings record.
fn settings() -> StudioSequenceSettings {
    StudioSequenceSettings {
        frame_size: SequenceFrameSize { width: 1920, height: 1080 },
        pixel_aspect_ratio: SequencePixelAspectRatio::SQUARE,
        frame_rate: SequenceFrameRate { ticks_per_frame: F25 },
        field_order: SequenceFieldOrder::Progressive,
        video_time_display: StudioTimeDisplay::Timecode25,
        audio_time_display: StudioTimeDisplay::AudioSamples,
        audio_sample_rate_hz: 48_000,
        audio_master_channel_type: AudioMasterChannelType::Stereo,
        working_color_space: SequenceWorkingColorSpace::STUDIO_DEFAULT,
        auto_tone_map: false,
        use_maximum_bit_depth: false,
        use_maximum_render_quality: false,
        allow_linear_compositing: true,
        initial_video_track_count: 3,
        initial_audio_track_count: 4,
        preview_render_format: StudioPreviewRenderFormat { codec: Some("apcs".into()), frame_size: SequenceFrameSize { width: 1920, height: 1080 } },
        immersive: None,
    }
}

fn records_with(entries: Vec<StudioSequenceSettingsEntry>) -> StudioSequenceSettingsRecords {
    let mut r = StudioSequenceSettingsRecords::new(DOC);
    r.sequences = entries;
    r
}

fn entry(sequence_id: &str, s: StudioSequenceSettings) -> StudioSequenceSettingsEntry {
    StudioSequenceSettingsEntry { sequence_id: sequence_id.into(), editing_mode: CUSTOM_EDITING_MODE.into(), settings: s }
}

fn validate(r: &StudioSequenceSettingsRecords) -> Result<(), Diagnostic> {
    validate_sequence_settings_records(r, &document(), Budget::default(), &token())
}

fn code(r: Result<(), Diagnostic>) -> Option<(Code, String)> {
    r.err().map(|d| (d.code, d.target))
}

/// Parses the record set after `edit` changes its wire JSON.
fn parse_edited(edit: impl FnOnce(&mut Value)) -> Result<StudioSequenceSettingsRecords, Diagnostic> {
    let mut v = serde_json::to_value(records_with(vec![entry("seq-a", settings())])).unwrap();
    edit(&mut v);
    parse_sequence_settings_records(&serde_json::to_vec(&v).unwrap(), &document(), Budget::default(), &token())
}

#[test]
fn the_normative_sequence_settings_record_mt_33686() {
    // Every STU-VID-011 row, in table order, with the stated wire shape.
    let v = serde_json::to_value(settings()).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut table = vec![
        "frame_size", "pixel_aspect_ratio", "frame_rate", "field_order", "video_time_display", "audio_time_display",
        "audio_sample_rate_hz", "audio_master_channel_type", "working_color_space", "auto_tone_map", "use_maximum_bit_depth",
        "use_maximum_render_quality", "allow_linear_compositing", "initial_video_track_count", "initial_audio_track_count",
        "preview_render_format", "immersive",
    ];
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    table.sort_unstable();
    assert_eq!(sorted, table, "exactly the 17 rows, none added or renamed");
    assert_eq!(v["frame_size"], json!({"width":1920,"height":1080}));
    assert_eq!(v["pixel_aspect_ratio"], json!({"numerator":1,"denominator":1}));
    assert_eq!(v["frame_rate"], json!({"ticks_per_frame":F25}));
    assert_eq!(v["field_order"], json!("progressive"));
    assert_eq!(v["audio_time_display"], json!("audio_samples"));
    assert_eq!(v["audio_master_channel_type"], json!("stereo"));
    assert_eq!(v["working_color_space"], json!({"color_space":"BT.709 RGB Full","is_linearized":false}));
    assert_eq!(v["immersive"], Value::Null, "explicit null = a flat sequence");
    // Every row is required in the schema.
    let schema = serde_json::to_value(sequence_settings_records_schema()).unwrap();
    let required: Vec<&str> =
        schema["$defs"]["StudioSequenceSettings"]["required"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    let mut required = required;
    required.sort_unstable();
    assert_eq!(required, table);
    // A missing row and a float aspect ratio are refused (rational, never a float).
    assert_eq!(parse_edited(|v| { v["sequences"][0]["settings"].as_object_mut().unwrap().remove("auto_tone_map"); }).unwrap_err().code, Code::InvalidJson);
    assert_eq!(parse_edited(|v| v["sequences"][0]["settings"]["pixel_aspect_ratio"] = json!(1.0)).unwrap_err().code, Code::InvalidJson);
    assert_eq!(parse_edited(|v| v["sequences"][0]["settings"]["frame_rate"] = json!({"ticks_per_frame": 25.0})).unwrap_err().code, Code::InvalidJson);
    // Valid record round-trips.
    let r = records_with(vec![entry("seq-a", settings())]);
    validate(&r).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(parse_sequence_settings_records(&bytes, &document(), Budget::default(), &token()).unwrap(), r);
    // Values per STU-VID-012/013: a non-canonical rate (the legacy exact-decimal 29.97 spelling) is
    // refused, and the entry rate must equal the sequence's.
    let mut legacy = settings();
    legacy.frame_rate = SequenceFrameRate { ticks_per_frame: 8_475_675_675 };
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", legacy)]))), Some((Code::InvalidField, "seq-a/frame_rate".into())));
    let mut other = settings();
    other.frame_rate = SequenceFrameRate { ticks_per_frame: F2997 };
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", other)]))), Some((Code::InvalidField, "seq-a/frame_rate".into())));
    // Audio and video displays are independent but typed: an audio member in the video slot (and
    // the reverse) is refused.
    let mut swapped = settings();
    swapped.video_time_display = StudioTimeDisplay::Milliseconds;
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", swapped)]))), Some((Code::InvalidField, "seq-a/video_time_display".into())));
    let mut swapped = settings();
    swapped.audio_time_display = StudioTimeDisplay::Frames;
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", swapped)]))), Some((Code::InvalidField, "seq-a/audio_time_display".into())));
    let mut ms = settings();
    ms.audio_time_display = StudioTimeDisplay::Milliseconds;
    validate(&records_with(vec![entry("seq-a", ms)])).unwrap();
    // 5.1 keeps its spec token.
    assert_eq!(serde_json::to_value(AudioMasterChannelType::FivePointOne).unwrap(), json!("5.1"));
}

#[test]
fn time_display_enumeration_is_the_normative_table_mt_33686() {
    let table: [(u16, &str); 15] = [
        (100, "timecode_24"), (101, "timecode_25"), (102, "timecode_2997_drop"), (103, "timecode_2997_non_drop"),
        (104, "timecode_30"), (105, "timecode_50"), (106, "timecode_5994_drop"), (107, "timecode_5994_non_drop"),
        (108, "timecode_60"), (109, "frames"), (110, "timecode_23976"), (111, "feet_frames_16mm"), (112, "feet_frames_35mm"),
        (200, "audio_samples"), (201, "milliseconds"),
    ];
    for (code, token) in table {
        let d = StudioTimeDisplay::from_code(code).unwrap();
        assert_eq!(d.code(), code);
        assert_eq!(serde_json::to_value(d).unwrap(), json!(token));
        assert_eq!(d.is_video(), code < 200);
    }
    assert_eq!(StudioTimeDisplay::ALL.len(), 15);
    assert_eq!(StudioTimeDisplay::from_code(113), None, "code 113 is the declared gap STU-VID-081");
    assert_eq!(StudioTimeDisplay::from_code(99), None);
}

#[test]
fn immersive_video_settings_are_part_of_the_sequence_record_mt_33701() {
    let mut s = settings();
    s.immersive = Some(StudioImmersiveSettings {
        projection_type: IMMERSIVE_PROJECTION_EQUIRECTANGULAR,
        stereoscopic_type: IMMERSIVE_STEREO_OVER_UNDER,
        stereoscopic_eye: 0,
        captured_horizontal_view: 360.0,
        captured_vertical_view: 180.0,
        field_of_horizontal_view: 108.0,
        field_of_vertical_view: 108.0,
        ambisonics_monitoring_type: IMMERSIVE_AMBISONICS_NONE,
        ambisonics_hrir: Some("hrir-default".into()),
    });
    let r = records_with(vec![entry("seq-a", s.clone())]);
    validate(&r).unwrap();
    let v = serde_json::to_value(&r).unwrap();
    let nine: Vec<&str> = v["sequences"][0]["settings"]["immersive"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(nine.len(), 9, "all nine STU-VID-018 fields: {nine:?}");
    assert_eq!(parse_sequence_settings_records(&serde_json::to_vec(&r).unwrap(), &document(), Budget::default(), &token()).unwrap(), r);
    // The composition's immersive rules apply on the sequence record.
    let mut bad = s;
    bad.immersive.as_mut().unwrap().field_of_vertical_view = 181.0;
    assert_eq!(
        code(validate(&records_with(vec![entry("seq-a", bad)]))),
        Some((Code::InvalidField, "seq-a/immersive/field_of_vertical_view".into()))
    );
    // The field is required on the wire: absent is refused, explicit null is a flat sequence.
    assert_eq!(parse_edited(|v| { v["sequences"][0]["settings"].as_object_mut().unwrap().remove("immersive"); }).unwrap_err().code, Code::InvalidJson);
    assert!(parse_edited(|v| v["sequences"][0]["settings"]["immersive"] = Value::Null).is_ok());
}

#[test]
fn preview_render_settings_are_part_of_the_sequence_record_mt_33702() {
    // Per sequence, independent of the export recipe; the frame size MAY differ from the sequence's.
    let mut s = settings();
    s.preview_render_format = StudioPreviewRenderFormat { codec: Some("apcn".into()), frame_size: SequenceFrameSize { width: 960, height: 540 } };
    let mut b = settings();
    b.frame_rate = SequenceFrameRate { ticks_per_frame: F2997 };
    b.video_time_display = StudioTimeDisplay::Timecode2997Drop;
    b.preview_render_format.codec = None;
    let r = records_with(vec![entry("seq-a", s.clone()), entry("seq-b", b)]);
    validate(&r).unwrap();
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["sequences"][0]["settings"]["preview_render_format"], json!({"codec":"apcn","frame_size":{"width":960,"height":540}}));
    assert_eq!(v["sequences"][1]["settings"]["preview_render_format"]["codec"], Value::Null, "explicit null = the preview tier's codec");
    // Refusals: a codec outside the key grammar, a zero or oversize preview frame, a missing codec member.
    for (codec, w) in [(Some("ap cs"), 960), (Some(""), 960), (Some("apcs"), 0), (Some("apcs"), MAX_COMPOSITION_SIDE + 1)] {
        let mut bad = s.clone();
        bad.preview_render_format = StudioPreviewRenderFormat { codec: codec.map(str::to_owned), frame_size: SequenceFrameSize { width: w, height: 540 } };
        assert_eq!(code(validate(&records_with(vec![entry("seq-a", bad)]))), Some((Code::InvalidField, "seq-a/preview_render_format".into())));
    }
    assert_eq!(parse_edited(|v| { v["sequences"][0]["settings"]["preview_render_format"].as_object_mut().unwrap().remove("codec"); }).unwrap_err().code, Code::InvalidJson);
}

#[test]
fn settings_records_are_bound_to_document_sequences_and_hostile_input_is_refused() {
    // Unknown sequence, duplicate entry, wrong document, bad sizes, sample rate bounds.
    assert_eq!(code(validate(&records_with(vec![entry("seq-x", settings())]))), Some((Code::DanglingReference, "seq-x".into())));
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", settings()), entry("seq-a", settings())]))), Some((Code::DuplicateIdentity, "seq-a".into())));
    let mut other = records_with(Vec::new());
    other.document_id = "SDOC-019abcde-0000-7000-8000-000000000002".into();
    assert_eq!(code(validate(&other)), Some((Code::InvalidField, "document_id".into())));
    for (w, h) in [(0, 1080), (u32::MAX, 1080), (1920, u32::MAX)] {
        let mut s = settings();
        s.frame_size = SequenceFrameSize { width: w, height: h };
        assert_eq!(code(validate(&records_with(vec![entry("seq-a", s)]))), Some((Code::InvalidField, "seq-a/frame_size".into())));
    }
    let mut s = settings();
    s.pixel_aspect_ratio = SequencePixelAspectRatio { numerator: 10, denominator: 0 };
    assert_eq!(code(validate(&records_with(vec![entry("seq-a", s)]))), Some((Code::InvalidField, "seq-a/pixel_aspect_ratio".into())));
    for rate in [0, MAX_SEQUENCE_SAMPLE_RATE_HZ + 1] {
        let mut s = settings();
        s.audio_sample_rate_hz = rate;
        assert_eq!(code(validate(&records_with(vec![entry("seq-a", s)]))), Some((Code::InvalidField, "seq-a/audio_sample_rate_hz".into())));
    }
    // Declared, unreduced aspect ratios are kept as declared.
    let mut s = settings();
    s.pixel_aspect_ratio = SequencePixelAspectRatio { numerator: 1920, denominator: 1920 };
    validate(&records_with(vec![entry("seq-a", s)])).unwrap();
    // Over the hard ceiling before any entry is walked; over the caller node budget.
    let many: Vec<_> = (0..=MAX_SEQUENCES).map(|i| entry(&format!("seq-{i}"), settings())).collect();
    assert_eq!(code(validate(&records_with(many))), Some((Code::BudgetExceeded, "sequence_settings".into())));
    let tight = Budget { nodes: 1, ..Budget::default() };
    let two = records_with(vec![entry("seq-a", settings()), entry("seq-b", settings())]);
    assert_eq!(validate_sequence_settings_records(&two, &document(), tight, &token()).unwrap_err().code, Code::BudgetExceeded);
    // Wire: duplicate keys, unknown fields, oversize input.
    let dup = format!(r#"{{"schema_id":"hsk.studio.sequence_settings@1","document_id":"{DOC}","document_id":"{DOC}"}}"#);
    assert_eq!(parse_sequence_settings_records(dup.as_bytes(), &document(), Budget::default(), &token()).unwrap_err().code, Code::DuplicateField);
    assert_eq!(parse_edited(|v| v["sequences"][0]["extra"] = json!(1)).unwrap_err().code, Code::InvalidJson);
    let big = vec![b' '; Budget::default().input_bytes + 1];
    assert_eq!(parse_sequence_settings_records(&big, &document(), Budget::default(), &token()).unwrap_err().code, Code::BudgetExceeded);
    // Cancellation.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(validate_sequence_settings_records(&two, &document(), Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn earlier_shape_sequences_load_unchanged() {
    // `StudioSequence` gains no field: its wire keys are the earlier ones.
    let v = serde_json::to_value(sequence("seq-a", F25)).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["duration", "frame_rate", "name", "schema_id", "sequence_id", "tracks"]);
    // A document without a settings record set is valid as before, and an empty record set is its
    // header only.
    let snapshot = validate_document(document(), Budget::default(), &token(), &Unresolved).unwrap();
    let again = validate_document(serde_json::from_slice(snapshot.encoded_bytes()).unwrap(), Budget::default(), &token(), &Unresolved).unwrap();
    assert_eq!(snapshot.encoded_bytes(), again.encoded_bytes());
    let empty = StudioSequenceSettingsRecords::new(DOC);
    validate(&empty).unwrap();
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({"schema_id":"hsk.studio.sequence_settings@1","document_id":DOC}));
    assert!(empty.entry("seq-a").is_none(), "an earlier-shape sequence has no settings entry");
}

// ---- Presets (STU-VID-016 / 016a) -----------------------------------------------------------

#[test]
fn studiosequencepreset_is_a_named_complete_settings_record_mt_33695() {
    let set = shipped_sequence_presets();
    validate_sequence_presets(&set, Budget::default(), &token()).unwrap();
    assert_eq!(set.presets.len(), 391);
    let groups: std::collections::BTreeSet<Vec<String>> = set.presets.iter().map(|p| p.category_path.clone()).collect();
    assert_eq!(groups.len(), 82, "82 groups, organised as category paths");
    assert!(set.presets.iter().all(|p| (1..=3).contains(&p.category_path.len())));
    // One preset is exactly one complete settings record: every row present on every preset.
    for p in &set.presets {
        let v = serde_json::to_value(&p.settings).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 17, "{}", p.preset_id);
    }
    // There is no partial preset: a preset missing a settings field is refused.
    let mut v = serde_json::to_value(StudioSequencePresetSet { schema_id: StyleRegistrySchema::V1, presets: vec![set.presets[0].clone()] }).unwrap();
    v["presets"][0]["settings"].as_object_mut().unwrap().remove("frame_rate");
    assert_eq!(parse_sequence_presets(&serde_json::to_vec(&v).unwrap(), Budget::default(), &token()).unwrap_err().code, Code::InvalidJson);
    // The table row with no declared frame rate is not shipped.
    assert_eq!(SHIPPED_PRESET_EXCLUSIONS, [("Legacy/DNxHD/720p 29.97", "DNX SQ 720p 29.97")]);
    assert!(!set.presets.iter().any(|p| p.name == "DNX SQ 720p 29.97"));
}

fn preset<'a>(set: &'a StudioSequencePresetSet, path: &str, name: &str) -> &'a StudioSequencePreset {
    set.presets.iter().find(|p| p.category_path.join("/") == path && p.name == name).unwrap_or_else(|| panic!("{path} {name}"))
}

#[test]
fn shipped_preset_rows_are_the_016a_table_mt_33696() {
    let set = shipped_sequence_presets();
    // Column distributions of the 392 rows (STU-VID-011 / 016 counts), less the one excluded
    // progressive 1280x720 1:1 29.97-display row.
    let count = |f: &dyn Fn(&StudioSequencePreset) -> String| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for p in &set.presets {
            *m.entry(f(p)).or_default() += 1;
        }
        m
    };
    let par = count(&|p| format!("{}:{}", p.settings.pixel_aspect_ratio.numerator, p.settings.pixel_aspect_ratio.denominator));
    assert_eq!(par, BTreeMap::from([("1:1".into(), 331), ("1920:1440".into(), 30), ("1920:1920".into(), 10), ("10:11".into(), 6), ("40:33".into(), 6), ("768:702".into(), 3), ("1024:702".into(), 3), ("3:2".into(), 2)]));
    let fps = count(&|p| p.settings.frame_rate.ticks_per_frame.to_string());
    assert_eq!(fps, BTreeMap::from([
        ("8475667200".into(), 103), ("10160640000".into(), 85), ("10594584000".into(), 75), ("10584000000".into(), 43), ("5080320000".into(), 35),
        ("4237833600".into(), 34), ("8467200000".into(), 6), ("4233600000".into(), 5), ("5292000000".into(), 5),
    ]));
    let field = count(&|p| format!("{:?}", p.settings.field_order));
    assert_eq!(field, BTreeMap::from([("Progressive".into(), 345), ("UpperFieldFirst".into(), 34), ("LowerFieldFirst".into(), 12)]));
    let display = count(&|p| p.settings.video_time_display.code().to_string());
    assert_eq!(display, BTreeMap::from([
        ("102".into(), 90), ("101".into(), 83), ("110".into(), 71), ("100".into(), 40), ("105".into(), 38), ("106".into(), 37),
        ("103".into(), 19), ("104".into(), 6), ("109".into(), 5), ("108".into(), 2),
    ]));
    let audio = count(&|p| format!("{} {:?}", p.settings.audio_sample_rate_hz, p.settings.audio_master_channel_type));
    assert_eq!(audio, BTreeMap::from([("48000 Stereo".into(), 366), ("48000 MultichannelAdaptive".into(), 19), ("32000 Stereo".into(), 6)]));
    let declared_cs = count(&|p| if p.defaulted_fields.contains(&SequenceSettingsField::WorkingColorSpace) { "--".into() } else { format!("{:?}", p.settings.working_color_space.color_space) });
    assert_eq!(declared_cs, BTreeMap::from([("--".into(), 364), ("Bt709RgbFull".into(), 22), ("Bt2100HlgRgbFull".into(), 5)]));
    assert_eq!(set.presets.iter().map(|p| p.settings.frame_size).collect::<std::collections::BTreeSet<_>>().len(), 37, "37 distinct frame sizes");
    assert!(set.presets.iter().all(|p| p.settings.initial_video_track_count == 3 && p.settings.audio_time_display == StudioTimeDisplay::AudioSamples));
    // Spot rows, as the table states them.
    let first = preset(&set, "Broadcast", "23.98p 4 mono discrete");
    assert_eq!(first.preset_id, "broadcast-23-98p-4-mono-discrete");
    assert_eq!(first.settings.audio_master_channel_type, AudioMasterChannelType::MultichannelAdaptive);
    assert_eq!(first.settings.video_time_display, StudioTimeDisplay::Timecode23976);
    let avc = preset(&set, "Legacy/AVC-Intra/1080i", "AVC-I 50 1080i50");
    assert_eq!((avc.settings.frame_size, avc.settings.pixel_aspect_ratio), (SequenceFrameSize { width: 1440, height: 1080 }, SequencePixelAspectRatio { numerator: 1920, denominator: 1440 }));
    assert_eq!(avc.settings.field_order, SequenceFieldOrder::UpperFieldFirst);
    // The last HDR row declares 60 fps and 60 fps timecode under a 59.94 name; reproduced as declared.
    let hdr = preset(&set, "UHD (4K) HDR", "UHD (4K) HDR 2160p 59.94 fps");
    assert_eq!((hdr.settings.frame_rate.ticks_per_frame, hdr.settings.video_time_display), (4_233_600_000, StudioTimeDisplay::Timecode60));
    assert_eq!(hdr.settings.working_color_space.color_space, SequenceColorSpace::Bt2100HlgRgbFull);
    assert!(hdr.settings.auto_tone_map, "declared true in the HDR-capable configurations");
    // Code 113 rows keep the frame-rate-matched display (48 fps has no timecode member: frames) and
    // carry the warning instead of a guessed meaning.
    let dnxhr = preset(&set, "Legacy/DNxHR/2K 48", "DNxHR HQ 2K 48");
    assert_eq!(dnxhr.settings.video_time_display, StudioTimeDisplay::Frames);
    assert_eq!(dnxhr.warnings, [SequencePresetWarning::TimeDisplayCode113]);
    assert_eq!(set.presets.iter().filter(|p| !p.warnings.is_empty()).count(), 5);
    // Fields the row does not declare are named.
    assert!(first.defaulted_fields.contains(&SequenceSettingsField::InitialAudioTrackCount));
    assert!(!first.defaulted_fields.contains(&SequenceSettingsField::WorkingColorSpace));
    assert!(!hdr.defaulted_fields.contains(&SequenceSettingsField::AutoToneMap));
}

#[test]
fn preset_library_is_portable_data_mt_33696() {
    let set = shipped_sequence_presets();
    let bytes = serde_json::to_vec(&set).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains('\\') && !text.contains(":/") && !text.contains("file:"), "no path or machine-local value");
    let budget = Budget { input_bytes: bytes.len(), nodes: 1024, ..Budget::default() };
    assert_eq!(parse_sequence_presets(&bytes, budget, &token()).unwrap(), set);
    assert_eq!(serde_json::to_value(&set).unwrap()["schema_id"], json!("hsk.studio.style_registry@1"));
}

#[test]
fn preset_section_hostile_input_is_refused() {
    let one = shipped_sequence_presets().presets[0].clone();
    let set = |presets: Vec<StudioSequencePreset>| StudioSequencePresetSet { schema_id: StyleRegistrySchema::V1, presets };
    let check = |s: &StudioSequencePresetSet| validate_sequence_presets(s, Budget::default(), &token()).err().map(|d| (d.code, d.target));
    // Over the hard ceiling (before any preset is walked) and over the caller node budget.
    let many = set(vec![one.clone(); MAX_SEQUENCE_PRESETS + 1]);
    assert_eq!(check(&many), Some((Code::BudgetExceeded, "sequence_presets".into())));
    let tight = Budget { nodes: 2, ..Budget::default() };
    let three = set((0..3).map(|i| StudioSequencePreset { preset_id: format!("p{i}"), ..one.clone() }).collect());
    assert_eq!(validate_sequence_presets(&three, tight, &token()).unwrap_err().code, Code::BudgetExceeded);
    let tight_text = Budget { payload_bytes: 4, ..Budget::default() };
    assert_eq!(validate_sequence_presets(&set(vec![one.clone()]), tight_text, &token()).unwrap_err().code, Code::BudgetExceeded);
    // Identity, names, category paths, settings.
    assert_eq!(check(&set(vec![one.clone(), one.clone()])).unwrap().0, Code::DuplicateIdentity);
    assert_eq!(check(&set(vec![StudioSequencePreset { preset_id: "a b".into(), ..one.clone() }])).unwrap().0, Code::InvalidId);
    for name in ["", "  ", "tab\there", &"x".repeat(MAX_SEQUENCE_PRESET_TEXT + 1)] {
        let p = StudioSequencePreset { name: name.into(), ..one.clone() };
        assert_eq!(check(&set(vec![p])), Some((Code::InvalidField, format!("sequence_presets/{}/name", one.preset_id))));
    }
    let deep = StudioSequencePreset { category_path: vec!["a".into(); MAX_SEQUENCE_PRESET_CATEGORY_DEPTH + 1], ..one.clone() };
    assert_eq!(check(&set(vec![deep])).unwrap().1, format!("sequence_presets/{}/category_path", one.preset_id));
    let mut bad = one.clone();
    bad.settings.frame_size.width = u32::MAX;
    assert_eq!(check(&set(vec![bad])).unwrap().1, format!("sequence_presets/{}/frame_size", one.preset_id));
    let twice = StudioSequencePreset { warnings: vec![SequencePresetWarning::TimeDisplayCode113; 2], ..one.clone() };
    assert_eq!(check(&set(vec![twice])).unwrap().0, Code::InvalidField);
    // Wire: unknown preset member, wrong schema id.
    let mut v = serde_json::to_value(set(vec![one.clone()])).unwrap();
    v["presets"][0]["extra"] = json!(1);
    assert_eq!(parse_sequence_presets(&serde_json::to_vec(&v).unwrap(), Budget::default(), &token()).unwrap_err().code, Code::InvalidJson);
    let mut v = serde_json::to_value(set(vec![one])).unwrap();
    v["schema_id"] = json!("hsk.studio.sequence@1");
    assert_eq!(parse_sequence_presets(&serde_json::to_vec(&v).unwrap(), Budget::default(), &token()).unwrap_err().code, Code::InvalidJson);
}

fn ctx(command: &str, revision: u64) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), revision, CorrelationId::new(77), None)
}

fn run(
    records: &StudioSequenceSettingsRecords,
    presets: &StudioSequencePresetSet,
    cmd: &StudioSequenceSettingsCommand,
) -> Result<StudioSequenceSettingsOutcome, StudioSequenceSettingsError> {
    apply_sequence_settings_command(&document(), records, presets, &ctx(cmd.command_id(), 7), 7, cmd, Budget::default(), &token())
}

#[test]
fn preset_commands_are_typed_attributed_and_atomic_mt_33697() {
    let records = records_with(vec![entry("seq-a", settings())]);
    let shipped = shipped_sequence_presets();
    // Save a sequence's current settings as a new preset.
    let save = StudioSequenceSettingsCommand::SavePresetFromSequence {
        sequence_id: "seq-a".into(),
        preset_id: "my-hd-25".into(),
        name: "My HD 25".into(),
        category_path: vec!["Custom".into(), "HD".into()],
    };
    assert_eq!(save.command_id(), "sequence.preset.save_from_sequence");
    let saved = run(&records, &StudioSequencePresetSet::new(), &save).unwrap();
    let p = saved.presets.preset("my-hd-25").unwrap();
    assert_eq!(p.settings, settings());
    assert!(p.defaulted_fields.is_empty() && p.warnings.is_empty());
    assert_eq!(saved.records, records, "saving changes no settings");
    assert_eq!(saved.document, document(), "saving changes no sequence");
    assert_eq!(
        saved.report,
        StudioSequenceSettingsReport { command: "sequence.preset.save_from_sequence", base_revision: 7, correlation: 77, presets: 1, settings_entries: 1, frame_rate_changed: false }
    );
    // Apply a shipped 29.97 preset to an earlier-shape 25 fps sequence: settings entry created with
    // the custom mode, the sequence frame rate set from the preset.
    let preset_id = preset(&shipped, "HD 1080p", "HD 1080p 29.97 fps").preset_id.clone();
    let apply = StudioSequenceSettingsCommand::ApplyPreset { preset_id: preset_id.clone(), sequence_id: "seq-a".into() };
    let applied = run(&StudioSequenceSettingsRecords::new(DOC), &shipped, &apply).unwrap();
    let e = applied.records.entry("seq-a").unwrap();
    assert_eq!(e.editing_mode, CUSTOM_EDITING_MODE);
    assert_eq!(e.settings, shipped.preset(&preset_id).unwrap().settings);
    assert_eq!(applied.document.sequences[0].frame_rate, SequenceFrameRate { ticks_per_frame: F2997 });
    assert!(applied.report.frame_rate_changed);
    assert_eq!(applied.report.command, "sequence.preset.apply");
    validate_sequence_settings_records(&applied.records, &applied.document, Budget::default(), &token()).unwrap();
    // Re-saving the applied settings reproduces the preset's settings.
    let resave = StudioSequenceSettingsCommand::SavePresetFromSequence { sequence_id: "seq-a".into(), preset_id: "copy".into(), name: "Copy".into(), category_path: Vec::new() };
    let copy = apply_sequence_settings_command(&applied.document, &applied.records, &applied.presets, &ctx(resave.command_id(), 7), 7, &resave, Budget::default(), &token()).unwrap();
    assert_eq!(copy.presets.preset("copy").unwrap().settings, shipped.preset(&preset_id).unwrap().settings);
    // Typed refusals; the inputs are never changed (the caller keeps them as the undo state).
    let before = (records.clone(), shipped.clone());
    let refuse = |r: &StudioSequenceSettingsRecords, s: &StudioSequencePresetSet, c: StudioSequenceSettingsCommand| run(r, s, &c).unwrap_err();
    let save_as = |sequence_id: &str, preset_id: &str, name: &str, category_path: Vec<String>| StudioSequenceSettingsCommand::SavePresetFromSequence {
        sequence_id: sequence_id.into(),
        preset_id: preset_id.into(),
        name: name.into(),
        category_path,
    };
    assert_eq!(refuse(&StudioSequenceSettingsRecords::new(DOC), &shipped, save_as("seq-a", "x", "X", vec![])), StudioSequenceSettingsError::SettingsMissing);
    assert_eq!(refuse(&records, &shipped, save_as("seq-x", "x", "X", vec![])), StudioSequenceSettingsError::SequenceMissing);
    assert_eq!(refuse(&records, &shipped, save_as("seq-a", &preset_id, "X", vec![])), StudioSequenceSettingsError::DuplicatePreset);
    assert_eq!(refuse(&records, &shipped, save_as("seq-a", "a/b", "X", vec![])), StudioSequenceSettingsError::InvalidPresetId);
    assert_eq!(refuse(&records, &shipped, save_as("seq-a", "x", " ", vec![])), StudioSequenceSettingsError::InvalidName);
    assert_eq!(refuse(&records, &shipped, save_as("seq-a", "x", "X", vec!["".into()])), StudioSequenceSettingsError::InvalidCategory);
    let full = StudioSequencePresetSet {
        schema_id: StyleRegistrySchema::V1,
        presets: (0..MAX_SEQUENCE_PRESETS).map(|i| StudioSequencePreset { preset_id: format!("p{i}"), ..shipped.presets[0].clone() }).collect(),
    };
    let roomy = Budget { nodes: MAX_SEQUENCE_PRESETS + 16, payload_bytes: 1 << 20, ..Budget::default() };
    let at_cap = apply_sequence_settings_command(&document(), &records, &full, &ctx(save.command_id(), 7), 7, &save, roomy, &token());
    assert_eq!(at_cap.unwrap_err(), StudioSequenceSettingsError::BudgetExceeded);
    assert_eq!(refuse(&records, &shipped, StudioSequenceSettingsCommand::ApplyPreset { preset_id: "none".into(), sequence_id: "seq-a".into() }), StudioSequenceSettingsError::PresetMissing);
    assert_eq!((records.clone(), shipped.clone()), before);
    // Attribution: the caller's context is required and must name this command at the current revision.
    assert_eq!(
        apply_sequence_settings_command(&document(), &records, &shipped, &ctx("sequence.preset.apply", 7), 7, &save, Budget::default(), &token()).unwrap_err(),
        StudioSequenceSettingsError::CommandMismatch
    );
    assert_eq!(
        apply_sequence_settings_command(&document(), &records, &shipped, &ctx(save.command_id(), 6), 7, &save, Budget::default(), &token()).unwrap_err(),
        StudioSequenceSettingsError::StaleRevision { expected: 6, current: 7 }
    );
    let anonymous = CommandContext::new(ctx("x", 7).actor().clone(), ClientSessionId::parse("w").unwrap(), None, 7, CorrelationId::new(1), None);
    assert_eq!(
        apply_sequence_settings_command(&document(), &records, &shipped, &anonymous, 7, &save, Budget::default(), &token()).unwrap_err().code(),
        "command_missing"
    );
    // Hostile inputs are refused before the command runs.
    let mut hostile = records.clone();
    hostile.sequences[0].settings.frame_size.width = 0;
    assert_eq!(run(&hostile, &shipped, &save).unwrap_err(), StudioSequenceSettingsError::InvalidInput(Code::InvalidField));
    assert_eq!(SEQUENCE_SETTINGS_COMMAND_IDS, ["sequence.preset.save_from_sequence", "sequence.preset.apply", "sequence.editing_mode.set"]);
}

// ---- Editing modes (STU-VID-017 / 017a) -----------------------------------------------------

#[test]
fn studioeditingmode_is_a_named_constraint_set_mt_33698() {
    let modes = shipped_editing_modes();
    assert_eq!(modes.len(), 53);
    validate_editing_modes(&modes, Budget::default(), &token()).unwrap();
    // `custom` declares no constraints.
    let custom = shipped_editing_mode(CUSTOM_EDITING_MODE).unwrap();
    assert_eq!(custom.name.as_deref(), Some("Custom"));
    assert!(custom.constraints.is_empty());
    // A sequence references exactly one editing mode: required, and a shipped one.
    assert_eq!(parse_edited(|v| { v["sequences"][0].as_object_mut().unwrap().remove("editing_mode"); }).unwrap_err().code, Code::InvalidJson);
    assert_eq!(parse_edited(|v| v["sequences"][0]["editing_mode"] = json!(["custom", "dslr"])).unwrap_err().code, Code::InvalidJson);
    let mut r = records_with(vec![entry("seq-a", settings())]);
    r.sequences[0].editing_mode = "no-such-mode".into();
    assert_eq!(code(validate(&r)), Some((Code::DanglingReference, "seq-a/editing_mode".into())));
    // Set a sequence's editing mode (typed command, attributed).
    let set = StudioSequenceSettingsCommand::SetEditingMode { sequence_id: "seq-a".into(), mode_id: "dslr".into() };
    let out = run(&records_with(vec![entry("seq-a", settings())]), &StudioSequencePresetSet::new(), &set).unwrap();
    assert_eq!(out.records.entry("seq-a").unwrap().editing_mode, "dslr");
    assert_eq!(out.report.command, "sequence.editing_mode.set");
    let unknown = StudioSequenceSettingsCommand::SetEditingMode { sequence_id: "seq-a".into(), mode_id: "nope".into() };
    assert_eq!(run(&records_with(vec![entry("seq-a", settings())]), &StudioSequencePresetSet::new(), &unknown).unwrap_err(), StudioSequenceSettingsError::ModeUnknown);
    assert_eq!(run(&StudioSequenceSettingsRecords::new(DOC), &StudioSequencePresetSet::new(), &set).unwrap_err(), StudioSequenceSettingsError::SettingsMissing);
}

#[test]
fn editing_mode_constraint_lists_are_a_declared_gap_mt_33699() {
    let modes = shipped_editing_modes();
    // Every shipped mode: constraints_known false, every allow-list empty (nothing invented or
    // back-filled from the presets that reference the mode).
    assert!(modes.iter().all(|m| !m.constraints_known && m.constraints.is_empty()));
    // Nothing is enforced: any settings pass every shipped mode.
    let mut odd = settings();
    odd.frame_size = SequenceFrameSize { width: 123, height: 77 };
    odd.field_order = SequenceFieldOrder::LowerFieldFirst;
    assert!(modes.iter().all(|m| editing_mode_violations(m, &odd).is_empty()));
    // The shape: an explicit allow-list per axis; empty = unconstrained on that axis.
    let mut authored = shipped_editing_mode("dslr").unwrap();
    authored.constraints_known = true;
    assert!(editing_mode_violations(&authored, &odd).is_empty(), "known but empty lists constrain nothing");
    authored.constraints.allowed_frame_rates = vec![SequenceFrameRate { ticks_per_frame: F2997 }];
    authored.constraints.allowed_field_orders = vec![SequenceFieldOrder::Progressive];
    assert_eq!(editing_mode_violations(&authored, &odd), [EditingModeAxis::FrameRate, EditingModeAxis::FieldOrder]);
    authored.constraints.allowed_frame_rates.push(SequenceFrameRate { ticks_per_frame: F25 });
    odd.field_order = SequenceFieldOrder::Progressive;
    assert!(editing_mode_violations(&authored, &odd).is_empty());
    validate_editing_modes(&[authored.clone()], Budget::default(), &token()).unwrap();
    // A mode whose constraints are not known never stores lists.
    let mut inconsistent = authored;
    inconsistent.constraints_known = false;
    assert_eq!(validate_editing_modes(&[inconsistent], Budget::default(), &token()).unwrap_err().target, "dslr/constraints");
}

#[test]
fn editing_mode_table_rows_are_shipped_whole_mt_33700() {
    let modes = shipped_editing_modes();
    let sum = |f: &dyn Fn(&StudioEditingModeSlots) -> u16| modes.iter().map(|m| u32::from(f(&m.slots))).sum::<u32>();
    assert_eq!(sum(&|s| s.frame_rate), 188, "188 frame-rate slots");
    assert_eq!(sum(&|s| s.frame_rect), 52, "52 frame-rect slots");
    assert_eq!((sum(&|s| s.pixel_aspect), sum(&|s| s.field_type)), (60, 59));
    let row = |id: &str| shipped_editing_mode(id).unwrap();
    let custom = row("custom");
    assert_eq!(custom.windows_guid.as_deref(), Some("9678AF98-A7B7-4bdb-B477-7AC9C8DF4A4E"));
    assert_eq!((custom.platforms.clone(), custom.slots), (vec![EditingModePlatform::All], StudioEditingModeSlots { frame_rate: 12, frame_rect: 1, pixel_aspect: 1, field_type: 0 }));
    let qt = row("quicktime_dv_ntsc");
    assert_eq!(qt.platforms, [EditingModePlatform::Mac, EditingModePlatform::Win]);
    assert_eq!(qt.slots, StudioEditingModeSlots { frame_rate: 1, frame_rect: 1, pixel_aspect: 2, field_type: 3 });
    assert_eq!(qt.windows_guid, row("dv_ntsc").windows_guid, "the declared shared GUID is kept");
    let raw = row("prores_raw");
    assert_eq!((raw.name.as_deref(), raw.windows_guid.as_deref()), (Some("ProRes RAW"), Some("5125b1eb-7925-433f-beea-f5d884227812")));
    assert_eq!(row("dnxhr_2k").slots.frame_rate, 8);
    assert_eq!(row("hdv_1080i").windows_guid, None);
    let unnamed = row("unnamed");
    assert_eq!((unnamed.name, unnamed.windows_guid, unnamed.platforms.len()), (None, None, 0));
    assert_eq!(modes.first().unwrap().mode_id, "custom");
    assert_eq!(modes.last().unwrap().mode_id, "unnamed");
    // Wire: name and GUID are explicit nulls where the table has none.
    let v = serde_json::to_value(row("unnamed")).unwrap();
    assert_eq!((v["name"].clone(), v["windows_guid"].clone()), (Value::Null, Value::Null));
}

#[test]
fn editing_mode_lists_are_budgeted() {
    let one = shipped_editing_mode("custom").unwrap();
    let many: Vec<_> = (0..=MAX_EDITING_MODES).map(|i| StudioEditingMode { mode_id: format!("m{i}"), ..one.clone() }).collect();
    assert_eq!(validate_editing_modes(&many, Budget::default(), &token()).unwrap_err(), Diagnostic { code: Code::BudgetExceeded, target: "editing_modes".into() });
    let mut long = one.clone();
    long.constraints_known = true;
    long.constraints.playback_devices = (0..=MAX_EDITING_MODE_ALLOW_LIST).map(|i| format!("d{i}")).collect();
    assert_eq!(validate_editing_modes(&[long], Budget::default(), &token()).unwrap_err().code, Code::BudgetExceeded);
    let tight = Budget { nodes: 2, ..Budget::default() };
    assert_eq!(validate_editing_modes(&shipped_editing_modes(), tight, &token()).unwrap_err().code, Code::BudgetExceeded);
    let mut twice = one.clone();
    twice.constraints_known = true;
    twice.constraints.allowed_frame_rates = vec![SequenceFrameRate { ticks_per_frame: F25 }; 2];
    assert_eq!(validate_editing_modes(&[twice], Budget::default(), &token()).unwrap_err().target, "custom/constraints");
    let mut dup = shipped_editing_modes();
    dup.push(one);
    assert_eq!(validate_editing_modes(&dup, Budget::default(), &token()).unwrap_err().code, Code::DuplicateIdentity);
}

/// R55 F-R55-6: a cancelled command and a budget overrun are not bad input, so a host emitting
/// `code()` through Observe can tell them apart. Lane SMB-PR-SEQUENCE-SESSION-1.
#[test]
fn command_codes_tell_cancellation_and_budget_from_bad_input_r55_f6() {
    let records = records_with(vec![entry("seq-a", settings())]);
    let presets = StudioSequencePresetSet::new();
    let set = StudioSequenceSettingsCommand::SetEditingMode { sequence_id: "seq-a".into(), mode_id: CUSTOM_EDITING_MODE.into() };
    let run_with = |records: &StudioSequenceSettingsRecords, budget: Budget, token: &CancellationToken| {
        apply_sequence_settings_command(&document(), records, &presets, &ctx(set.command_id(), 7), 7, &set, budget, token)
    };
    run_with(&records, Budget::default(), &token()).unwrap();
    let canceled = token();
    canceled.cancel();
    let stopped = run_with(&records, Budget::default(), &canceled).unwrap_err();
    assert_eq!((stopped, stopped.code()), (StudioSequenceSettingsError::Canceled, "canceled"));
    let over = run_with(&records, Budget { nodes: 0, ..Budget::default() }, &token()).unwrap_err();
    assert_eq!((over, over.code()), (StudioSequenceSettingsError::BudgetExceeded, "budget_exceeded"));
    // Bad input stays bad input, with its Folio code.
    let mut hostile = records.clone();
    hostile.sequences[0].sequence_id = "seq-x".into();
    let bad = run_with(&hostile, Budget::default(), &token()).unwrap_err();
    assert_eq!(bad.code(), "invalid_input");
    assert_eq!(bad, StudioSequenceSettingsError::InvalidInput(Code::DanglingReference));
}
