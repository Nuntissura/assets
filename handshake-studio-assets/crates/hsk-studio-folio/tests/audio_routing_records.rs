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
        source_ref: ClipSource::Media { artifact_manifest_id: "a".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "00".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(4 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(4 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
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
    assert_eq!(code(&|r| r.sequence_id = "../seq".into()), Code::InvalidId, "an inert id keeps its grammar");
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

fn lane(track: u32, param: StudioTrackParam, points: &[(u64, f64)]) -> StudioTrackAutomation {
    StudioTrackAutomation { track_id: id(track), param, points: points.iter().map(|&(offset_ticks, value)| StudioAutomationPoint { offset_ticks, value }).collect() }
}

/// Lane SMB-AU-MULTITRACK-2: track volume / pan automation validates, round-trips, refuses unknown
/// or non-audio tracks, duplicates, unordered points and out-of-range values by type, and an absent
/// list keeps the routing's encoding byte for byte.
#[test]
fn track_automation_validates_round_trips_and_refuses_typed() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    let before = serde_json::to_vec(&routing()).unwrap();
    assert!(!String::from_utf8(before.clone()).unwrap().contains("track_automation"), "absent = not encoded");
    let parsed = parse_audio_routing(&before, &d, b, &token).unwrap();
    assert!(parsed.routings[0].track_automation.is_empty());
    assert_eq!(serde_json::to_vec(&parsed).unwrap(), before, "byte-identical re-encoding without automation");

    let mut set = routing();
    set.routings[0].track_automation = vec![
        lane(1, StudioTrackParam::Volume, &[(0, 1.0), (2 * F, 0.5), (3 * F, MAX_AUDIO_LEVEL)]),
        lane(1, StudioTrackParam::Pan, &[(0, -1.0), (F, 1.0)]),
        lane(3, StudioTrackParam::Volume, &[(F, 0.0)]),
    ];
    validate_audio_routing(&set, &d, b, &token).unwrap();
    let json = serde_json::to_vec(&set).unwrap();
    assert!(String::from_utf8(json.clone()).unwrap().contains(r#""param":"volume""#));
    assert_eq!(parse_audio_routing(&json, &d, b, &token).unwrap(), set);
    assert_eq!(set.routings[0].track_lane(&id(1), StudioTrackParam::Pan).map(|l| l.points.len()), Some(2));
    assert!(set.routings[0].track_lane(&id(2), StudioTrackParam::Pan).is_none());

    let code = |f: &dyn Fn(&mut StudioAudioRouting)| {
        let mut s = set.clone();
        f(&mut s.routings[0]);
        validate_audio_routing(&s, &d, b, &token).unwrap_err().code
    };
    assert_eq!(code(&|r| r.track_automation[0].track_id = "STRK-nope".into()), Code::InvalidId, "malformed: only a well-formed absent id is inert");
    assert_eq!(code(&|r| r.track_automation[0].track_id = id(5)), Code::InvalidId, "video track");
    assert_eq!(code(&|r| r.track_automation[1].param = StudioTrackParam::Volume), Code::DuplicateField);
    assert_eq!(code(&|r| r.track_automation[0].points.clear()), Code::InvalidField);
    assert_eq!(code(&|r| r.track_automation[0].points[1].offset_ticks = 0), Code::InvalidField, "not increasing");
    assert_eq!(code(&|r| r.track_automation[0].points[1].value = MAX_AUDIO_LEVEL * 1.01), Code::InvalidField);
    assert_eq!(code(&|r| r.track_automation[0].points[1].value = -0.01), Code::InvalidField);
    assert_eq!(code(&|r| r.track_automation[0].points[1].value = f64::NAN), Code::InvalidField);
    assert_eq!(code(&|r| r.track_automation[1].points[0].value = -1.5), Code::InvalidField, "pan range");
    assert_eq!(
        code(&|r| r.track_automation[2].points = (0..=MAX_TRACK_AUTOMATION_POINTS as u64).map(|k| StudioAutomationPoint { offset_ticks: k, value: 1.0 }).collect()),
        Code::InvalidField,
        "more than the point bound"
    );
    // Lanes and their points are charged: the smallest node budget the routing passes without
    // automation is too small with it.
    let least = (1..10_000).find(|&n| validate_audio_routing(&routing(), &d, Budget { nodes: n, ..b }, &token).is_ok()).unwrap();
    assert_eq!(validate_audio_routing(&set, &d, Budget { nodes: least, ..b }, &token).unwrap_err().code, Code::BudgetExceeded);
    // Wire refusals: an unknown parameter or an unknown field.
    let text = String::from_utf8(json).unwrap();
    assert_eq!(parse_audio_routing(text.replacen(r#""param":"pan""#, r#""param":"width""#, 1).as_bytes(), &d, b, &token).unwrap_err().code, Code::InvalidJson);
    assert_eq!(parse_audio_routing(text.replacen(r#""param":"pan""#, r#""param":"pan","mode":"hold""#, 1).as_bytes(), &d, b, &token).unwrap_err().code, Code::InvalidJson);
}

/// Lane SMB-AU-MULTITRACK-3: clip pan envelopes and master volume automation validate, round-trip,
/// refuse bad clips, duplicates, unordered points and out-of-range values by type, are charged against
/// the node budget, and an absent member keeps the routing's encoding byte for byte.
#[test]
fn clip_pans_and_master_automation_validate_round_trip_and_refuse_typed() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    let before = serde_json::to_vec(&routing()).unwrap();
    let text = String::from_utf8(before.clone()).unwrap();
    assert!(!text.contains("clip_pans") && !text.contains("volume_automation"), "absent = not encoded");
    assert_eq!(serde_json::to_vec(&parse_audio_routing(&before, &d, b, &token).unwrap()).unwrap(), before, "byte-identical re-encoding");

    let mut set = routing();
    set.routings[0].clip_pans = vec![StudioClipPanEnvelope {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000002".into(),
        points: vec![StudioClipPanPoint { offset_ticks: 0, pan: -1.0 }, StudioClipPanPoint { offset_ticks: 2 * F, pan: 1.0 }],
    }];
    set.routings[0].master.volume_automation = vec![StudioAutomationPoint { offset_ticks: 0, value: 1.0 }, StudioAutomationPoint { offset_ticks: F, value: MAX_AUDIO_LEVEL }];
    validate_audio_routing(&set, &d, b, &token).unwrap();
    let json = serde_json::to_vec(&set).unwrap();
    assert_eq!(parse_audio_routing(&json, &d, b, &token).unwrap(), set);

    let code = |f: &dyn Fn(&mut StudioAudioRouting)| {
        let mut s = set.clone();
        f(&mut s.routings[0]);
        validate_audio_routing(&s, &d, b, &token).unwrap_err().code
    };
    assert_eq!(code(&|r| r.clip_pans[0].clip_id = "SCLP-019abcde-0000-7000-8000-000000000005".into()), Code::InvalidId, "video clip");
    assert_eq!(code(&|r| r.clip_pans[0].clip_id = "SCLP-nope".into()), Code::InvalidId, "malformed: only a well-formed absent id is inert");
    assert_eq!(
        code(&|r| {
            let again = r.clip_pans[0].clone();
            r.clip_pans.push(again);
        }),
        Code::DuplicateField
    );
    assert_eq!(code(&|r| r.clip_pans[0].points.clear()), Code::InvalidField);
    assert_eq!(code(&|r| r.clip_pans[0].points[1].offset_ticks = 0), Code::InvalidField, "not increasing");
    assert_eq!(code(&|r| r.clip_pans[0].points[1].pan = 1.0001), Code::InvalidField);
    assert_eq!(code(&|r| r.clip_pans[0].points[0].pan = f64::NEG_INFINITY), Code::InvalidField);
    assert_eq!(
        code(&|r| r.clip_pans[0].points = (0..=MAX_CLIP_PAN_POINTS as u64).map(|k| StudioClipPanPoint { offset_ticks: k, pan: 0.0 }).collect()),
        Code::InvalidField,
        "more than the point bound"
    );
    assert_eq!(code(&|r| r.master.volume_automation[1].offset_ticks = 0), Code::InvalidField);
    assert_eq!(code(&|r| r.master.volume_automation[1].value = MAX_AUDIO_LEVEL * 1.01), Code::InvalidField);
    assert_eq!(code(&|r| r.master.volume_automation[0].value = f64::NAN), Code::InvalidField);
    assert_eq!(
        code(&|r| r.master.volume_automation = (0..=MAX_TRACK_AUTOMATION_POINTS as u64).map(|k| StudioAutomationPoint { offset_ticks: k, value: 1.0 }).collect()),
        Code::InvalidField
    );
    let least = (1..10_000).find(|&n| validate_audio_routing(&routing(), &d, Budget { nodes: n, ..b }, &token).is_ok()).unwrap();
    assert_eq!(validate_audio_routing(&set, &d, Budget { nodes: least, ..b }, &token).unwrap_err().code, Code::BudgetExceeded);
    let text = String::from_utf8(json).unwrap();
    assert_eq!(parse_audio_routing(text.replacen(r#""pan":-1.0"#, r#""pan":-1.0,"curve":"hold""#, 1).as_bytes(), &d, b, &token).unwrap_err().code, Code::InvalidJson);
}

/// Lead decision on R56 F-R56-3: an id the document does not hold is inert. One stale id never makes
/// the set unreadable for another sequence; an id held with the wrong kind stays refused, and what an
/// inert member holds is still checked and still counted.
#[test]
fn an_id_the_document_does_not_hold_is_inert_and_still_checked() {
    use StudioInertRoutingKind as K;
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    let stale_clip = "SCLP-019abcde-0000-7000-8000-0000000000ee";
    let stale_track = "STRK-019abcde-0000-7000-8000-0000000000ee";
    let mut set = routing();
    let live = set.routings[0].clone();
    set.routings.push(StudioAudioRouting::new("seq-gone"));
    let r = &mut set.routings[0];
    r.clip_gains.push(StudioClipGainEnvelope { clip_id: stale_clip.into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] });
    r.clip_pans.push(StudioClipPanEnvelope { clip_id: stale_clip.into(), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: 0.5 }] });
    // An inert route's own output and sends name nothing that plays.
    r.routes.push(StudioTrackRoute {
        track_id: stale_track.into(),
        output: Some(stale_track.into()),
        sends: vec![StudioAudioSend { send_id: "s1".into(), target: stale_track.into(), level: 0.5, pre_fader: true, pan: 0.0 }],
    });
    r.bus_effects.push(StudioBusEffects { track_id: stale_track.into(), effects: vec![] });
    r.track_automation.push(StudioTrackAutomation { track_id: stale_track.into(), param: StudioTrackParam::Pan, points: vec![StudioAutomationPoint { offset_ticks: 0, value: 0.5 }] });
    validate_audio_routing(&set, &d, b, &token).unwrap();
    let parsed = parse_audio_routing(&serde_json::to_vec(&set).unwrap(), &d, b, &token).unwrap();
    assert_eq!(parsed, set, "kept whole");
    let row = |kind, id: &str| StudioInertRoutingReference { sequence_id: "seq-main".into(), kind, id: id.into() };
    assert_eq!(
        inert_audio_routing_references(&set, &d),
        vec![
            row(K::Route, stale_track),
            row(K::BusEffects, stale_track),
            row(K::TrackAutomation, stale_track),
            row(K::ClipGain, stale_clip),
            row(K::ClipPan, stale_clip),
            StudioInertRoutingReference { sequence_id: "seq-gone".into(), kind: K::Routing, id: "seq-gone".into() },
        ]
    );
    assert_eq!(inert_audio_routing_references(&routing(), &d), vec![]);
    assert_eq!(live, routing().routings[0], "the fixture's own routing is untouched");

    let refused = |f: &dyn Fn(&mut StudioAudioRoutingSet)| {
        let mut s = set.clone();
        f(&mut s);
        validate_audio_routing(&s, &d, b, &token).unwrap_err().code
    };
    // What an inert member holds is still checked.
    assert_eq!(refused(&|s| s.routings[0].clip_gains[1].points[0].gain_db = 99.0), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].clip_gains[1].points.clear()), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].clip_pans[0].points[0].pan = 2.0), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].track_automation[0].points[0].value = 9.0), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[1].master.volume = 99.0), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].routes[3].sends[0].level = 99.0), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].routes[3].sends[0].send_id = "../x".into()), Code::InvalidField);
    // Only a well-formed id is inert: every id an inert member holds keeps the grammar, wherever it stands.
    for bad in ["STRK-nope", "../x", "", stale_clip] {
        assert_eq!(refused(&|s| s.routings[0].routes[3].track_id = bad.into()), Code::InvalidId, "route {bad}");
        assert_eq!(refused(&|s| s.routings[0].routes[3].output = Some(bad.into())), Code::InvalidId, "output {bad}");
        assert_eq!(refused(&|s| s.routings[0].routes[3].sends[0].target = bad.into()), Code::InvalidId, "send target {bad}");
        assert_eq!(refused(&|s| s.routings[0].bus_effects[1].track_id = bad.into()), Code::InvalidId, "bus effects {bad}");
        assert_eq!(refused(&|s| s.routings[0].track_automation[0].track_id = bad.into()), Code::InvalidId, "automation {bad}");
    }
    for bad in ["SCLP-nope", "../x", "", stale_track] {
        assert_eq!(refused(&|s| s.routings[0].clip_gains[1].clip_id = bad.into()), Code::InvalidId, "gain {bad}");
        assert_eq!(refused(&|s| s.routings[0].clip_pans[0].clip_id = bad.into()), Code::InvalidId, "pan {bad}");
    }
    // The same inside the routing of an absent sequence, where every member is inert.
    assert_eq!(refused(&|s| s.routings[1].routes.push(StudioTrackRoute { track_id: "STRK-nope".into(), output: None, sends: vec![] })), Code::InvalidId);
    assert_eq!(refused(&|s| s.routings[1].clip_gains.push(StudioClipGainEnvelope { clip_id: "SCLP-nope".into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }] })), Code::InvalidId);
    assert_eq!(refused(&|s| s.routings[1].sequence_id = "../seq".into()), Code::InvalidId);
    // Uniqueness holds for inert ids too.
    assert_eq!(refused(&|s| s.routings.push(StudioAudioRouting::new("seq-gone"))), Code::DuplicateField);
    assert_eq!(refused(&|s| { let again = s.routings[0].clip_gains[1].clone(); s.routings[0].clip_gains.push(again); }), Code::DuplicateField);
    assert_eq!(refused(&|s| { let again = s.routings[0].routes[3].clone(); s.routings[0].routes.push(again); }), Code::DuplicateField);
    // A live route naming an absent bus is not inert: a live track would play into it. Reported gap.
    assert_eq!(refused(&|s| s.routings[0].routes[0].output = Some(stale_track.into())), Code::InvalidField);
    assert_eq!(refused(&|s| s.routings[0].routes[1].sends[0].target = stale_track.into()), Code::InvalidField);
    // Inert members count against the budget and the routing ceiling.
    let least = (1..10_000).find(|&n| validate_audio_routing(&routing(), &d, Budget { nodes: n, ..b }, &token).is_ok()).unwrap();
    assert_eq!(validate_audio_routing(&set, &d, Budget { nodes: least, ..b }, &token).unwrap_err().code, Code::BudgetExceeded);
    let many = StudioAudioRoutingSet { routings: (0..=MAX_AUDIO_ROUTINGS).map(|k| StudioAudioRouting::new(format!("gone-{k}"))).collect() };
    assert_eq!(validate_audio_routing(&many, &d, b, &token).unwrap_err().code, Code::BudgetExceeded);
}

/// Live again when the id comes back: a member listed inert stops being listed once its track or clip
/// exists, validates as live, and the checks that need the owner apply to it again.
#[test]
fn an_inert_member_is_live_again_when_its_track_or_clip_exists() {
    use StudioInertRoutingKind as K;
    let token = CancellationToken::default();
    let b = Budget::default();
    let back_clip = clip(7).clip_id;
    let d = document();
    let mut set = routing();
    let r = &mut set.routings[0];
    r.routes.push(StudioTrackRoute { track_id: id(6), output: Some(id(3)), sends: vec![] });
    r.bus_effects.push(StudioBusEffects { track_id: id(6), effects: vec![] });
    r.track_automation.push(StudioTrackAutomation { track_id: id(6), param: StudioTrackParam::Pan, points: vec![StudioAutomationPoint { offset_ticks: 0, value: 0.5 }] });
    r.clip_gains.push(StudioClipGainEnvelope { clip_id: back_clip.clone(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] });
    r.clip_pans.push(StudioClipPanEnvelope { clip_id: back_clip.clone(), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: 0.5 }] });
    validate_audio_routing(&set, &d, b, &token).unwrap();
    let listed: Vec<_> = inert_audio_routing_references(&set, &d).into_iter().map(|row| (row.kind, row.id)).collect();
    assert_eq!(
        listed,
        vec![
            (K::Route, id(6)),
            (K::BusEffects, id(6)),
            (K::TrackAutomation, id(6)),
            (K::ClipGain, back_clip.clone()),
            (K::ClipPan, back_clip.clone()),
        ]
    );

    let holding = |bus: TrackKind, clip_track: TrackKind| {
        let mut back = document();
        back.sequences[0].tracks.push(track(6, bus, vec![]));
        back.sequences[0].tracks.push(track(7, clip_track, vec![clip(7)]));
        back
    };
    let back = holding(TrackKind::Submix, TrackKind::Audio);
    assert_eq!(inert_audio_routing_references(&set, &back), vec![], "nothing is inert with the ids back");
    validate_audio_routing(&set, &back, b, &token).unwrap();

    // Live: what the owner decides applies again, where the same record was inert a moment ago.
    let mut to_itself = set.clone();
    to_itself.routings[0].routes[3].output = Some(id(6));
    validate_audio_routing(&to_itself, &d, b, &token).unwrap();
    assert_eq!(validate_audio_routing(&to_itself, &back, b, &token).unwrap_err().code, Code::InvalidField, "a live route may not output to itself");
    let as_video = holding(TrackKind::Video, TrackKind::Audio);
    assert_eq!(validate_audio_routing(&set, &as_video, b, &token).unwrap_err().code, Code::InvalidId, "a track that comes back with the wrong kind");
    let clip_on_video = holding(TrackKind::Submix, TrackKind::Video);
    assert_eq!(validate_audio_routing(&set, &clip_on_video, b, &token).unwrap_err().code, Code::InvalidId, "a clip that comes back on a video track");
}
