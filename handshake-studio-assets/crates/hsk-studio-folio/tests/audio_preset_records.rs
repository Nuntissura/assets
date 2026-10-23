//! Lane SMB-AUDITION-SESSION: audio effect presets as stored Folio records (STU-FX-039/142/142a/143).
//! A preset set round-trips through wire JSON unchanged; empty keyframe lists are not encoded;
//! structural refusals are typed and happen before anything is accepted.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;

fn instance(id: &str, kind: &str) -> StudioAudioEffectInstance {
    StudioAudioEffectInstance {
        instance_id: id.into(),
        filter_kind: kind.into(),
        enabled: true,
        params: vec![
            StudioAudioPresetParam { key: "ceiling_db".into(), value: -1.0 },
            StudioAudioPresetParam { key: "release_ms".into(), value: 100.0 },
        ],
        keyframes: Vec::new(),
    }
}

fn set() -> StudioAudioEffectPresetSet {
    let mut keyed = instance("fx-1", "gain");
    keyed.params = vec![StudioAudioPresetParam { key: "db".into(), value: -6.0 }];
    keyed.keyframes = vec![
        StudioAudioPresetKeyframe { key: "db".into(), offset_ticks: 0, value: -6.0 },
        StudioAudioPresetKeyframe { key: "db".into(), offset_ticks: 254_016_000_000, value: 0.0 },
    ];
    StudioAudioEffectPresetSet {
        schema_id: "hsk.studio.style_registry@1".into(),
        presets: vec![
            StudioAudioEffectPreset {
                preset_id: "broadcast-safe".into(),
                name: "Broadcast safe".into(),
                category_path: vec!["Mastering".into(), "Loudness".into()],
                effects: vec![instance("fx-0", "lookahead_limiter"), keyed],
            },
            StudioAudioEffectPreset {
                preset_id: "plain".into(),
                name: "Plain".into(),
                category_path: Vec::new(),
                effects: vec![instance("fx-0", "lookahead_limiter")],
            },
        ],
    }
}

#[test]
fn audio_presets_round_trip_and_refuse_bad_records() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let s = set();
    validate_audio_effect_presets(&s, b, &token).unwrap();
    let json = serde_json::to_vec(&s).unwrap();
    assert_eq!(parse_audio_effect_presets(&json, b, &token).unwrap(), s);
    let text = String::from_utf8(json.clone()).unwrap();
    assert_eq!(text.matches("\"keyframes\"").count(), 1, "empty keyframe lists are not encoded");
    assert_eq!(StudioAudioEffectPresetSet::new().schema_id, "hsk.studio.style_registry@1");

    let refused = |f: &dyn Fn(&mut StudioAudioEffectPresetSet)| {
        let mut x = set();
        f(&mut x);
        validate_audio_effect_presets(&x, b, &token).unwrap_err().code
    };
    assert_eq!(refused(&|x| x.schema_id = "other@1".into()), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[1].preset_id = "broadcast-safe".into()), Code::InvalidId);
    assert_eq!(refused(&|x| x.presets[0].preset_id = "has space".into()), Code::InvalidId);
    assert_eq!(refused(&|x| x.presets[0].name = "   ".into()), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].category_path = vec!["a\u{7}".into()]), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].category_path = vec!["c".into(); 9]), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects.clear()), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects[1].instance_id = "fx-0".into()), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects[0].filter_kind = "bad kind".into()), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects[0].params[1].key = "ceiling_db".into()), Code::DuplicateField);
    assert_eq!(refused(&|x| x.presets[0].effects[0].params[0].value = f64::INFINITY), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects[1].keyframes[1].offset_ticks = 0), Code::InvalidField);
    assert_eq!(refused(&|x| x.presets[0].effects[1].keyframes[0].key = "q".into()), Code::InvalidField);
    // Budget: the node count is charged before the records are walked.
    let tight = Budget { nodes: 3, ..b };
    assert_eq!(validate_audio_effect_presets(&set(), tight, &token).unwrap_err().code, Code::BudgetExceeded);
    // Wire refusals: duplicate key, unknown field, oversize input, cancel.
    let dup = text.replacen("\"name\":\"Plain\"", "\"name\":\"Plain\",\"name\":\"Again\"", 1);
    assert_eq!(parse_audio_effect_presets(dup.as_bytes(), b, &token).unwrap_err().code, Code::DuplicateField);
    let unknown = text.replacen("\"name\":\"Plain\"", "\"name\":\"Plain\",\"colour\":1", 1);
    assert_eq!(parse_audio_effect_presets(unknown.as_bytes(), b, &token).unwrap_err().code, Code::InvalidJson);
    let small = Budget { input_bytes: 16, ..b };
    assert_eq!(parse_audio_effect_presets(&json, small, &token).unwrap_err().code, Code::BudgetExceeded);
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(parse_audio_effect_presets(&json, b, &canceled).unwrap_err().code, Code::Canceled);
}
