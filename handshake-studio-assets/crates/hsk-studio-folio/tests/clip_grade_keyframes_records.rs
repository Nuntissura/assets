//! Lane SMB-PR-SOURCE-MONITOR slice 3: keyframed grade scalars, `basic.sharpen` (STU-COL-220) and
//! `faded_film` (STU-COL-221) on the clip finishing record set: wire round trip, byte identity of
//! older records (the new fields are omitted at their defaults) and typed refusals (hard bounds of
//! every key, one stream per param, vignette params only with a vignette).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;

const F: u64 = 10_160_640_000;
const CLIP: &str = "SCLP-019abcde-0000-7000-8000-000000000005";
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";

fn document() -> StudioDocument {
    let json = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
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
            clips: vec![StudioClip {
                schema_id: ClipSchema::V1,
                clip_id: CLIP.into(),
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
            }],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    });
    d
}

fn linear(keys: &[(u64, f64)]) -> StudioKeyedValue {
    StudioKeyedValue {
        static_value: vec![keys[0].1],
        keyframes: keys
            .iter()
            .map(|&(time, v)| StudioCompositionKeyframe {
                time,
                value: vec![v],
                in_interp: StudioInterpolation::Linear,
                out_interp: StudioInterpolation::Linear,
                in_tangent: vec![],
                out_tangent: vec![],
            })
            .collect(),
    }
}

fn records(g: StudioColorGrade) -> StudioClipFinishingRecords {
    let mut r = StudioClipFinishingRecords::new(DOC);
    r.effect_stacks.push(StudioClipEffectStack {
        clip_id: CLIP.into(),
        effects: vec![StudioClipEffect { effect_id: "grade-1".into(), enabled: true, params: StudioClipEffectParams::ColorGrade(Box::new(g)) }],
    });
    r
}

#[test]
fn keyed_grade_params_sharpen_and_faded_film_round_trip_and_refuse_bad_values() {
    let token = CancellationToken::default();
    let b = Budget::default();
    let d = document();
    // Older records stay byte-identical: the new fields are omitted at their defaults.
    let plain = serde_json::to_string(&records(StudioColorGrade::default())).unwrap();
    for absent in ["sharpen", "faded_film", "keyframes"] {
        assert!(!plain.contains(absent), "{absent} in {plain}");
    }
    let g = StudioColorGrade {
        basic: StudioGradeBasic { sharpen: -40.0, ..StudioGradeBasic::default() },
        faded_film: 120.0,
        vignette: Some(StudioGradeVignette { amount: 0.0, midpoint: 50.0, roundness: 0.0, feather: 50.0 }),
        keyframes: vec![
            StudioGradeKeyedParam { param: StudioGradeParam::Exposure, value: linear(&[(0, 0.0), (10 * F, 1.0)]) },
            StudioGradeKeyedParam { param: StudioGradeParam::VignetteAmount, value: linear(&[(0, 0.0), (F, -2.0)]) },
            StudioGradeKeyedParam { param: StudioGradeParam::FadedFilm, value: linear(&[(0, 150.0), (F, 0.0)]) },
        ],
        ..StudioColorGrade::default()
    };
    let r = records(g.clone());
    validate_clip_finishing_records(&r, &d, b, &token).unwrap();
    let json = serde_json::to_vec(&r).unwrap();
    let text = String::from_utf8(json.clone()).unwrap();
    assert!(text.contains(r#""param":"vignette_amount""#) && text.contains(r#""sharpen":-40.0"#), "{text}");
    assert_eq!(parse_clip_finishing_records(&json, &d, b, &token).unwrap(), r);

    let code = |f: &dyn Fn(&mut StudioColorGrade)| {
        let mut g = g.clone();
        f(&mut g);
        validate_clip_finishing_records(&records(g), &d, b, &token).map_err(|e| e.code)
    };
    assert_eq!(code(&|g| g.basic.sharpen = 100.5), Err(Code::InvalidField));
    assert_eq!(code(&|g| g.basic.sharpen = f64::NAN), Err(Code::InvalidField));
    assert_eq!(code(&|g| g.faded_film = 150.5), Err(Code::InvalidField));
    assert_eq!(code(&|g| g.faded_film = -0.5), Err(Code::InvalidField));
    // Every key is checked against the param's hard bound (exposure -7..=7).
    assert_eq!(code(&|g| g.keyframes[0].value = linear(&[(0, 0.0), (F, 7.5)])), Err(Code::InvalidField));
    // One stream per param.
    assert_eq!(code(&|g| g.keyframes.push(g.keyframes[0].clone())), Err(Code::DuplicateIdentity));
    // A vignette param needs a vignette.
    assert_eq!(code(&|g| g.vignette = None), Err(Code::InvalidField));
    // A stream of the wrong dimension is refused.
    assert_eq!(
        code(&|g| g.keyframes[0].value = StudioKeyedValue::constant(vec![0.0, 1.0])),
        Err(Code::InvalidField)
    );
    // Unknown param keys are refused at parse.
    let bad = text.replace("vignette_amount", "vignette_size");
    assert!(parse_clip_finishing_records(bad.as_bytes(), &d, b, &token).is_err());

    // StudioGradeParam::set clamps into the hard bound and ignores non-finite values.
    let mut g2 = StudioColorGrade::default();
    StudioGradeParam::Exposure.set(&mut g2, 9.0);
    assert_eq!(g2.basic.exposure, 7.0);
    StudioGradeParam::Exposure.set(&mut g2, f64::NAN);
    assert_eq!(g2.basic.exposure, 7.0);
    StudioGradeParam::VignetteAmount.set(&mut g2, 1.0);
    assert!(g2.vignette.is_none(), "no vignette to animate");
    StudioGradeParam::Sharpen.set(&mut g2, -30.0);
    assert_eq!(g2.basic.sharpen, -30.0);
}
