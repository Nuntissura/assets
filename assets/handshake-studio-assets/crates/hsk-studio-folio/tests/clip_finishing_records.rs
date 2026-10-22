//! Lane SMB-PREMIERE-COLOR-TITLES: the clip finishing record set (`hsk.studio.clip_finishing@1`)
//! round-trips through wire JSON and is validated against the document: clip references, spec hard
//! bounds (STU-COL-220/221/222/225/227/228), counts and the input byte budget are typed refusals.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;

const F: u64 = 10_160_640_000;
const CLIP: &str = "SCLP-019abcde-0000-7000-8000-000000000005";

fn clip() -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: CLIP.into(),
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
    }
}
pub fn document() -> StudioDocument {
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
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip()],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    });
    d
}

fn grade() -> StudioColorGrade {
    StudioColorGrade {
        input_lut: Some(StudioLutRef {
            artifact_manifest_id: "lut-in".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab12".into() },
        }),
        basic: StudioGradeBasic { temperature: 20.0, exposure: 0.5, saturation: 120.0, ..StudioGradeBasic::default() },
        curves: StudioGradeCurves { composite: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]], ..StudioGradeCurves::default() },
        wheels: StudioGradeWheels {
            shadows: StudioWheelBand { offset: [0.02, 0.0, -0.01], ..StudioWheelBand::default() },
            ..StudioGradeWheels::default()
        },
        secondaries: vec![StudioGradeSecondary {
            key: StudioGradeKey {
                hue: StudioKeyRange { lo: 100.0, hi: 140.0, soft: 20.0 },
                saturation: StudioKeyRange { lo: 0.2, hi: 1.0, soft: 0.1 },
                luminance: StudioKeyRange { lo: 0.0, hi: 1.0, soft: 0.0 },
                invert: false,
            },
            temperature: -250.0,
            tint: 0.0,
            contrast: 0.0,
            saturation: 80.0,
        }],
        vignette: Some(StudioGradeVignette { amount: -1.0, midpoint: 50.0, roundness: 0.0, feather: 50.0 }),
        ..StudioColorGrade::default()
    }
}
fn records() -> StudioClipFinishingRecords {
    let mut r = StudioClipFinishingRecords::new("SDOC-019abcde-0000-7000-8000-000000000001");
    r.effect_stacks.push(StudioClipEffectStack {
        clip_id: CLIP.into(),
        effects: vec![StudioClipEffect {
            effect_id: "grade-1".into(),
            enabled: true,
            params: StudioClipEffectParams::ColorGrade(Box::new(grade())),
        }],
    });
    r
}

#[test]
fn caption_tracks_validate_timing_activity_and_bombs() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let mut d = document();
    let mut cap = d.sequences[0].tracks[0].clone();
    cap.kind = TrackKind::Caption;
    cap.index = 0;
    cap.clips = Vec::new();
    for n in [3u32, 4] {
        let mut t = cap.clone();
        t.track_id = format!("STRK-019abcde-0000-7000-8000-{n:012x}");
        t.index = n - 3;
        d.sequences[0].tracks.push(t);
    }
    let seg = |s: u64, e: u64, text: &str| StudioCaptionSegment { start: s, end: e, text: text.into(), style: StudioCaptionStyle::default() };
    let track = |n: u32, active: bool, segments: Vec<StudioCaptionSegment>| StudioCaptionTrack {
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        language: if n == 3 { "en".into() } else { "nl-BE".into() },
        active,
        constraints: StudioCaptionConstraints { max_line_chars: Some(8), max_lines: Some(1), min_duration: Some(F) },
        segments,
    };
    let set = |tracks: Vec<StudioCaptionTrack>| {
        let mut r = StudioClipFinishingRecords::new("SDOC-019abcde-0000-7000-8000-000000000001");
        r.caption_tracks = tracks;
        validate_clip_finishing_records(&r, &d, b, &token).map_err(|e| e.code)
    };
    // Two languages, one active; overlapping speakers are legal.
    let en = track(3, true, vec![seg(0, 2 * F, "Hi"), seg(F, 3 * F, "two speakers"), seg(3 * F, 3 * F + 1, "a\nb")]);
    assert_eq!(set(vec![en.clone(), track(4, false, vec![seg(0, F, "Hoi")])]), Ok(()));
    // STU-VID-037 validation-warning display state: warnings, not refusals.
    use CaptionWarning::*;
    assert_eq!(en.warnings(), vec![(1, LineTooLong), (2, TooManyLines), (2, TooShort)]);
    // One active track per sequence.
    assert_eq!(set(vec![en.clone(), track(4, true, vec![])]), Err(Code::InvalidField));
    // Timing: end after start, ordered by start.
    assert_eq!(set(vec![track(3, true, vec![seg(F, F, "x")])]), Err(Code::InvalidField));
    assert_eq!(set(vec![track(3, true, vec![seg(F, 2 * F, "x"), seg(0, F, "y")])]), Err(Code::InvalidField));
    // Only caption tracks; control characters refused; language grammar.
    let mut v = track(3, true, vec![]);
    v.track_id = "STRK-019abcde-0000-7000-8000-000000000001".into();
    assert_eq!(set(vec![v]), Err(Code::InvalidField), "a video track");
    assert_eq!(set(vec![track(3, true, vec![seg(0, F, "a\u{7}b")])]), Err(Code::InvalidField));
    let mut v = track(3, true, vec![]);
    v.language = "en us".into();
    assert_eq!(set(vec![v]), Err(Code::InvalidField));
    // Caption bomb: the segment ceiling is checked on the count before any segment.
    let bomb = track(3, true, vec![seg(0, 1, "x"); MAX_CAPTION_SEGMENTS + 1]);
    assert_eq!(set(vec![bomb]), Err(Code::BudgetExceeded));
    let small = Budget { payload_bytes: 4, ..Budget::default() };
    let mut r = StudioClipFinishingRecords::new("SDOC-019abcde-0000-7000-8000-000000000001");
    r.caption_tracks = vec![track(3, true, vec![seg(0, F, "longer than four")])];
    assert_eq!(validate_clip_finishing_records(&r, &d, small, &token).unwrap_err().code, Code::BudgetExceeded);
}

#[test]
fn graphic_clips_honour_template_permissions_and_placement() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let mut d = document();
    let mut g = clip();
    g.clip_id = "SCLP-019abcde-0000-7000-8000-000000000007".into();
    g.source_ref = ClipSource::Primitive { schema_id: MOTION_TEMPLATE_SCHEMA.into(), object_id: "lower-third".into() };
    d.sequences[0].tracks[0].clips[0].duration = Ticks::new(4 * F);
    let mut t2 = d.sequences[0].tracks[0].clone();
    t2.track_id = "STRK-019abcde-0000-7000-8000-000000000002".into();
    t2.index = 1;
    t2.clips = vec![g];
    d.sequences[0].tracks.push(t2);
    let tpl: StudioMotionTemplate = serde_json::from_value(serde_json::json!({
        "template_id":"lower-third","name":"Lower third","composition_id":"SCOM-019abcde-0000-7000-8000-000000000009",
        "required_fonts":["Inter"],
        "controls":[
            {"control_id":"name","name":"Name","can_animate":false,"control":{"control_type":"source_text",
                "layer_id":"SLYR-019abcde-0000-7000-8000-000000000003","default_text":"Name","font_family_editable":false,
                "faux_style_editable":false,"font_size_editable":true,"locked_font":"Inter","locked_size":24.0}},
            {"control_id":"fade","name":"Fade","can_animate":false,"control":{"control_type":"numeric_slider",
                "layer_id":"SLYR-019abcde-0000-7000-8000-000000000002","property":"opacity","component":0,"min":0.0,"max":100.0,"default":100.0}}]}))
    .unwrap();
    let inst = |values: serde_json::Value| -> StudioGraphicInstance {
        serde_json::from_value(serde_json::json!({"clip_id":"SCLP-019abcde-0000-7000-8000-000000000007","template_id":"lower-third","values":values})).unwrap()
    };
    let set = |i: StudioGraphicInstance| {
        let mut r = StudioClipFinishingRecords::new("SDOC-019abcde-0000-7000-8000-000000000001");
        r.templates.push(tpl.clone());
        r.graphics.push(i);
        validate_clip_finishing_records(&r, &d, b, &token).map_err(|e| e.code)
    };
    assert_eq!(set(inst(serde_json::json!([{"control_id":"name","setting":{"value_type":"text","text":"Ilka","font_size":30.0}}]))), Ok(()));
    // STU-VID-070a: locked typography is a HARD permission.
    assert_eq!(
        set(inst(serde_json::json!([{"control_id":"name","setting":{"value_type":"text","text":"Ilka","font_family":"Comic"}}]))),
        Err(Code::InvalidField)
    );
    assert_eq!(
        set(inst(serde_json::json!([{"control_id":"name","setting":{"value_type":"text","text":"Ilka","faux_bold":true}}]))),
        Err(Code::InvalidField)
    );
    // STU-VID-070b: keyframes only where the control can animate; values within the hard range.
    let keyed = serde_json::json!({"static_value":[50.0],"keyframes":[{"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"}]});
    assert_eq!(set(inst(serde_json::json!([{"control_id":"fade","setting":{"value_type":"number","value":keyed}}]))), Err(Code::InvalidField));
    assert_eq!(
        set(inst(serde_json::json!([{"control_id":"fade","setting":{"value_type":"number","value":{"static_value":[150.0]}}}]))),
        Err(Code::InvalidField)
    );
    // Value type must match the control; unknown control; duplicate value.
    assert_eq!(set(inst(serde_json::json!([{"control_id":"fade","setting":{"value_type":"checkbox","checked":true}}]))), Err(Code::InvalidField));
    assert_eq!(set(inst(serde_json::json!([{"control_id":"nope","setting":{"value_type":"checkbox","checked":true}}]))), Err(Code::DanglingReference));
    let v = serde_json::json!({"control_id":"name","setting":{"value_type":"text","text":"a"}});
    assert_eq!(set(inst(serde_json::json!([v.clone(), v]))), Err(Code::InvalidField));
    // The clip must actually place this template (STU-VID-070 "an ordinary clip").
    let mut wrong = inst(serde_json::json!([]));
    wrong.clip_id = CLIP.into();
    assert_eq!(set(wrong), Err(Code::InvalidField));
    // Hostile sizes: a caption-bomb-sized source text is refused.
    let huge = "x".repeat(MAX_TEMPLATE_TEXT_BYTES + 1);
    assert_eq!(
        set(inst(serde_json::json!([{"control_id":"name","setting":{"value_type":"text","text":huge}}]))),
        Err(Code::InvalidField)
    );
}

#[test]
fn clip_finishing_records_round_trip_and_refuse_bad_values() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    let r = records();
    validate_clip_finishing_records(&r, &d, b, &token).unwrap();
    let json = serde_json::to_vec(&r).unwrap();
    assert_eq!(parse_clip_finishing_records(&json, &d, b, &token).unwrap(), r);
    // Identity blocks are not encoded beyond their defaults; an empty set is two keys.
    let empty = serde_json::to_string(&StudioClipFinishingRecords::new("SDOC-019abcde-0000-7000-8000-000000000001")).unwrap();
    assert_eq!(empty, r#"{"schema_id":"hsk.studio.clip_finishing@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001"}"#);

    let code = |f: &dyn Fn(&mut StudioColorGrade)| {
        let mut r = records();
        #[allow(irrefutable_let_patterns)]
        let StudioClipEffectParams::ColorGrade(g) = &mut r.effect_stacks[0].effects[0].params else {
            panic!("grade entry")
        };
        f(g);
        validate_clip_finishing_records(&r, &d, b, &token).unwrap_err().code
    };
    // STU-COL-220 hard bounds (primary temperature -150..150; saturation 0..300 with no soft min).
    assert_eq!(code(&|g| g.basic.temperature = 151.0), Code::InvalidField);
    assert_eq!(code(&|g| g.basic.exposure = f64::NAN), Code::InvalidField);
    assert_eq!(code(&|g| g.basic.saturation = 300.5), Code::InvalidField);
    // STU-COL-227: the SECONDARY temperature is a different parameter, hard -300..300.
    assert_eq!(code(&|g| g.secondaries[0].temperature = -301.0), Code::InvalidField);
    assert_eq!(code(&|g| g.secondaries[0].key.saturation.lo = 1.5), Code::InvalidField);
    assert_eq!(code(&|g| g.look_intensity = 201.0), Code::InvalidField);
    assert_eq!(code(&|g| g.wheels.contrast = -1.01), Code::InvalidField);
    assert_eq!(code(&|g| g.wheels.input_saturation = 2.5), Code::InvalidField);
    assert_eq!(code(&|g| g.vignette.as_mut().unwrap().amount = 5.5), Code::InvalidField);
    assert_eq!(code(&|g| g.curves.red = vec![[0.5, 0.5], [0.4, 0.6]]), Code::InvalidField, "x not increasing");
    assert_eq!(code(&|g| g.curves.red = vec![[0.5, 0.5]]), Code::InvalidField, "one point");
    assert_eq!(code(&|g| g.curves.hue_vs_hue = vec![[0.0, 0.5]; 20]), Code::BudgetExceeded);
    assert_eq!(code(&|g| g.secondaries = vec![g.secondaries[0]; 9]), Code::BudgetExceeded);
    assert_eq!(code(&|g| g.input_lut.as_mut().unwrap().content_digest.digest = "../x".into()), Code::InvalidField);

    // References and identity.
    let mut bad = records();
    bad.effect_stacks[0].clip_id = "SCLP-019abcde-0000-7000-8000-000000000009".into();
    assert_eq!(validate_clip_finishing_records(&bad, &d, b, &token).unwrap_err().code, Code::DanglingReference);
    let mut bad = records();
    let again = bad.effect_stacks[0].clone();
    bad.effect_stacks.push(again);
    assert_eq!(validate_clip_finishing_records(&bad, &d, b, &token).unwrap_err().code, Code::DuplicateIdentity);
    let mut bad = records();
    let e = bad.effect_stacks[0].effects[0].clone();
    bad.effect_stacks[0].effects = vec![e; 65];
    assert_eq!(validate_clip_finishing_records(&bad, &d, b, &token).unwrap_err().code, Code::BudgetExceeded);
    // Node budget charged per stack, effect and curve point.
    let tight = Budget { nodes: 3, ..Budget::default() };
    assert_eq!(validate_clip_finishing_records(&records(), &d, tight, &token).unwrap_err().code, Code::BudgetExceeded);
    // Input byte budget before any parse; unknown fields and duplicate keys refused.
    let small = Budget { input_bytes: 32, ..Budget::default() };
    assert_eq!(parse_clip_finishing_records(&json, &d, small, &token).unwrap_err().code, Code::BudgetExceeded);
    let mut v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    v["effect_stacks"][0]["effects"][0]["params"]["bogus"] = serde_json::json!(1);
    assert_eq!(parse_clip_finishing_records(&serde_json::to_vec(&v).unwrap(), &d, b, &token).unwrap_err().code, Code::InvalidJson);
    // Slice 2 entries: keyed values follow the transform-stream keyframe rules and their ranges.
    let fx = |p: StudioClipEffectParams| {
        let mut r = records();
        r.effect_stacks[0].effects.push(StudioClipEffect { effect_id: "fx-2".into(), enabled: true, params: p });
        validate_clip_finishing_records(&r, &d, b, &token).map_err(|e| e.code)
    };
    let k = |time: u64, v: f64| StudioCompositionKeyframe {
        time,
        value: vec![v],
        in_interp: StudioInterpolation::Linear,
        out_interp: StudioInterpolation::Linear,
        in_tangent: vec![],
        out_tangent: vec![],
    };
    let fade = StudioKeyedValue { static_value: vec![100.0], keyframes: vec![k(0, 0.0), k(F, 100.0)] };
    assert_eq!(fx(StudioClipEffectParams::Opacity { opacity: fade.clone() }), Ok(()));
    assert_eq!(fx(StudioClipEffectParams::Opacity { opacity: StudioKeyedValue::constant(vec![101.0]) }), Err(Code::InvalidField));
    let backwards = StudioKeyedValue { static_value: vec![1.0], keyframes: vec![k(F, 1.0), k(0, 2.0)] };
    assert_eq!(fx(StudioClipEffectParams::GaussianBlur { blurriness: backwards }), Err(Code::InvalidField));
    assert_eq!(
        fx(StudioClipEffectParams::GaussianBlur { blurriness: StudioKeyedValue::constant(vec![1e9]) }),
        Err(Code::InvalidField)
    );
    let two = StudioKeyedValue::constant(vec![0.0, 0.0]);
    assert_eq!(
        fx(StudioClipEffectParams::Transform { anchor: two.clone(), position: two.clone(), scale: two.clone(), rotation: two.clone() }),
        Err(Code::InvalidField),
        "rotation has one component"
    );
    let mesh = StudioLiquifyMesh { origin_x: 0, origin_y: 0, width: 2, height: 2, spacing: 1, cols: 2, rows: 2, displacement: vec![[0.0; 2]; 4], freeze: vec![0.0; 4] };
    assert_eq!(
        fx(StudioClipEffectParams::PigmentFilter { filter: StudioSmartFilterKind::Liquify { mesh, resample: SmartResample::Bilinear } }),
        Err(Code::InvalidField),
        "liquify is layer-bound"
    );
    assert_eq!(
        fx(StudioClipEffectParams::PigmentFilter { filter: StudioSmartFilterKind::GaussianBlur { radius_px: f32::NAN } }),
        Err(Code::InvalidField)
    );
    let dup = br#"{"schema_id":"hsk.studio.clip_finishing@1","document_id":"x","document_id":"y"}"#;
    assert_eq!(parse_clip_finishing_records(dup, &d, b, &token).unwrap_err().code, Code::DuplicateField);
}
