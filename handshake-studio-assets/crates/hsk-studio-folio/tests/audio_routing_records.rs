//! Lane SMB-AUDITION-SESSION-2: the audio routing record of a sequence (STU-FX-137 submix tracks and
//! sends) with bus / master effects and clip gain envelopes round-trips through wire JSON and is
//! validated against the document: references, cycles, ranges and counts are typed refusals.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;

const F: u64 = 10_160_640_000;

fn id(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn clip(n: u32) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media { artifact_manifest_id: "a".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "00".into() } },
        source_in: Ticks::new(0),
        source_out: Ticks::new(4 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(4 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id(n),
        kind,
        index: 0,
        name: format!("T{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}
fn document() -> StudioDocument {
    let json = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[]
    });
    let mut d: StudioDocument = serde_json::from_value(json).unwrap();
    d.sequences.push(StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(4 * F),
        tracks: vec![
            track(1, TrackKind::Audio, vec![clip(1)]),
            track(2, TrackKind::Audio, vec![clip(2)]),
            track(3, TrackKind::Submix, vec![]),
            track(4, TrackKind::Submix, vec![]),
            track(5, TrackKind::Video, vec![clip(5)]),
        ],
        markers: Vec::new(),
        work_area: None,
    });
    d
}
fn fx() -> StudioAudioEffectInstance {
    StudioAudioEffectInstance {
        instance_id: "fx-0".into(),
        filter_kind: "lookahead_limiter".into(),
        enabled: true,
        params: vec![StudioAudioPresetParam { key: "ceiling_db".into(), value: -1.0 }],
        keyframes: vec![],
    }
}
fn routing() -> StudioAudioRoutingSet {
    let mut r = StudioAudioRouting::new("seq-main");
    r.routes = vec![
        StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] },
        StudioTrackRoute {
            track_id: id(2),
            output: None,
            sends: vec![StudioAudioSend { send_id: "s1".into(), target: id(4), level: 0.5, pre_fader: true, pan: -0.25 }],
        },
        StudioTrackRoute { track_id: id(3), output: Some(id(4)), sends: vec![] },
    ];
    r.bus_effects = vec![StudioBusEffects { track_id: id(3), effects: vec![fx()] }];
    r.master.effects = vec![fx()];
    r.master.volume = 0.8;
    r.clip_gains = vec![StudioClipGainEnvelope {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }, StudioClipGainPoint { offset_ticks: F, gain_db: -6.0 }],
    }];
    StudioAudioRoutingSet { routings: vec![r] }
}

#[test]
fn audio_routing_round_trips_and_refuses_bad_references_cycles_and_ranges() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    let set = routing();
    validate_audio_routing(&set, &d, b, &token).unwrap();
    let json = serde_json::to_vec(&set).unwrap();
    assert_eq!(parse_audio_routing(&json, &d, b, &token).unwrap(), set);
    let minimal = serde_json::to_string(&StudioAudioRoutingSet { routings: vec![StudioAudioRouting::new("seq-main")] }).unwrap();
    assert_eq!(minimal, r#"{"routings":[{"sequence_id":"seq-main","master":{"volume":1.0}}]}"#, "empty members are not encoded");

    let code = |f: &dyn Fn(&mut StudioAudioRouting)| {
        let mut s = routing();
        f(&mut s.routings[0]);
        validate_audio_routing(&s, &d, b, &token).unwrap_err().code
    };
    assert_eq!(code(&|r| r.sequence_id = "seq-x".into()), Code::InvalidId);
    assert_eq!(code(&|r| r.routes[0].track_id = id(5)), Code::InvalidId, "video track");
    assert_eq!(code(&|r| r.routes[2].track_id = id(1)), Code::DuplicateField);
    assert_eq!(code(&|r| r.routes[0].output = Some(id(2))), Code::InvalidField, "output to an audio track");
    assert_eq!(code(&|r| r.routes.push(StudioTrackRoute { track_id: id(4), output: Some(id(3)), sends: vec![] })), Code::InvalidField, "cycle 3 -> 4 -> 3");
    assert_eq!(code(&|r| r.routes[2].output = Some(id(3))), Code::InvalidField, "self output");
    assert_eq!(code(&|r| r.routes[1].sends[0].level = 5.0), Code::InvalidField);
    assert_eq!(code(&|r| r.routes[1].sends[0].pan = 1.5), Code::InvalidField);
    assert_eq!(
        code(&|r| {
            let again = r.routes[1].sends[0].clone();
            r.routes[1].sends.push(again);
        }),
        Code::InvalidField,
        "duplicate send id"
    );
    assert_eq!(code(&|r| r.bus_effects[0].track_id = id(1)), Code::InvalidId, "bus effects on an audio track");
    assert_eq!(code(&|r| r.bus_effects[0].effects[0].params[0].value = f64::NAN), Code::InvalidField);
    assert_eq!(code(&|r| r.master.volume = -0.1), Code::InvalidField);
    assert_eq!(code(&|r| r.clip_gains[0].clip_id = "SCLP-019abcde-0000-7000-8000-000000000005".into()), Code::InvalidId, "video clip");
    assert_eq!(code(&|r| r.clip_gains[0].points[1].offset_ticks = 0), Code::InvalidField);
    assert_eq!(code(&|r| r.clip_gains[0].points[1].gain_db = 20.0), Code::InvalidField);
    assert_eq!(code(&|r| r.clip_gains[0].points.clear()), Code::InvalidField);
    let mut twice = routing();
    twice.routings.push(twice.routings[0].clone());
    assert_eq!(validate_audio_routing(&twice, &d, b, &token).unwrap_err().code, Code::DuplicateField);
    let tight = Budget { nodes: 4, ..b };
    assert_eq!(validate_audio_routing(&set, &d, tight, &token).unwrap_err().code, Code::BudgetExceeded);
    let dup = String::from_utf8(json).unwrap().replacen("\"volume\":0.8", "\"volume\":0.8,\"volume\":0.9", 1);
    assert_eq!(parse_audio_routing(dup.as_bytes(), &d, b, &token).unwrap_err().code, Code::DuplicateField);
}
