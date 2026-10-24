//! Lane SMB-PS-SOFT-PROOF-2: the proof record set (`hsk.studio.proof_records@1`, STU-COL-240..244)
//! round-trips through wire JSON and is validated against the document: the six STU-COL-241 fields,
//! names, the active condition, operator matrix bounds, the ICC reference shape and the STU-COL-244
//! gamut warning parameters are typed refusals.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const PROFILE: &str = "SCPF-019abcde-0000-7000-8000-000000000007";

fn document() -> StudioDocument {
    let json = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    serde_json::from_value(json).unwrap()
}

fn condition(name: &str, proof_profile: StudioProofProfile) -> StudioProofConditionRecord {
    StudioProofConditionRecord {
        name: name.into(),
        proof_profile,
        proof_intent: StudioProofIntent::Perceptual,
        display_intent: StudioProofIntent::AbsoluteColorimetric,
        black_point_compensation: false,
        simulate_paper_color: true,
        simulate_black_ink: true,
    }
}

fn icc() -> StudioProofProfile {
    StudioProofProfile::Icc {
        profile_id: PROFILE.into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab".repeat(32) },
    }
}

fn records() -> StudioProofRecords {
    let mut r = StudioProofRecords::new(DOC);
    r.conditions = vec![
        condition("Press coated", icc()),
        condition("Working CMYK, perceptual", StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingCmyk }),
        condition("Deutan", StudioProofProfile::ColourVision { vision: ProofColourVision::Deuteranopia }),
        condition("Operator", StudioProofProfile::OperatorMatrix { matrix: [0.5, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] }),
    ];
    r.active_condition = Some("Press coated".into());
    r.proof_colours = true;
    r.gamut_warning = StudioGamutWarningState { enabled: true, colour: [1.0, 0.0, 1.0], opacity: 40 };
    r
}

fn check(r: &StudioProofRecords) -> Result<(), (Code, String)> {
    validate_proof_records(r, &document(), Budget::default(), &CancellationToken::default()).map_err(|e| (e.code, e.target))
}

#[test]
fn proof_records_validate_six_fields_and_bounds() {
    let token = CancellationToken::default();
    let d = document();
    let r = records();
    assert_eq!(check(&r), Ok(()));
    // Wire round trip: the six fields of every condition come back unchanged and independent.
    let bytes = serde_json::to_vec(&r).unwrap();
    let back = parse_proof_records(&bytes, &d, Budget::default(), &token).unwrap();
    assert_eq!(back, r);
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let c0 = &wire["conditions"][0];
    for field in ["proof_profile", "proof_intent", "display_intent", "black_point_compensation", "simulate_paper_color", "simulate_black_ink"] {
        assert!(c0.get(field).is_some(), "{field} missing: {c0}");
    }
    assert_eq!(c0.as_object().unwrap().len(), 7, "name + six fields: {c0}");
    assert_eq!((c0["proof_intent"].as_str(), c0["display_intent"].as_str()), (Some("perceptual"), Some("absolute_colorimetric")));
    assert_eq!(c0["proof_profile"]["kind"], "icc");
    // Unknown fields and duplicate keys are refused; the input bound comes before the parse.
    let mut extra = wire.clone();
    extra["conditions"][0]["proof_gamma"] = serde_json::json!(2.2);
    assert_eq!(parse_proof_records(&serde_json::to_vec(&extra).unwrap(), &d, Budget::default(), &token).unwrap_err().code, Code::InvalidJson);
    let dup = format!(r#"{{"schema_id":"hsk.studio.proof_records@1","document_id":"{DOC}","document_id":"{DOC}"}}"#);
    assert_eq!(parse_proof_records(dup.as_bytes(), &d, Budget::default(), &token).unwrap_err().code, Code::DuplicateField);
    let tight = Budget { input_bytes: 16, ..Budget::default() };
    assert_eq!(parse_proof_records(&bytes, &d, tight, &token).unwrap_err().code, Code::BudgetExceeded);

    // Typed refusals, one per rule.
    let refuse = |f: &dyn Fn(&mut StudioProofRecords), code: Code, target: &str| {
        let mut r = records();
        f(&mut r);
        assert_eq!(check(&r), Err((code, target.to_string())), "{target}");
    };
    refuse(&|r| r.document_id = "SDOC-019abcde-0000-7000-8000-000000000002".into(), Code::InvalidField, "document_id");
    refuse(&|r| r.conditions[1].name = "Press coated".into(), Code::DuplicateIdentity, "conditions/1/name");
    refuse(&|r| r.conditions[0].name = "  ".into(), Code::InvalidField, "conditions/0/name");
    refuse(&|r| r.conditions[0].name = "a\u{7}b".into(), Code::InvalidField, "conditions/0/name");
    refuse(&|r| r.conditions[0].name = "x".repeat(MAX_PROOF_CONDITION_NAME_BYTES + 1), Code::InvalidField, "conditions/0/name");
    refuse(&|r| r.conditions[0].name = "working_cmyk".into(), Code::InvalidField, "conditions/0/name");
    refuse(&|r| r.active_condition = Some("Nope".into()), Code::DanglingReference, "active_condition");
    refuse(
        &|r| r.conditions[0].proof_profile = StudioProofProfile::Icc { profile_id: "SDOC-019abcde-0000-7000-8000-000000000007".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab".repeat(32) } },
        Code::InvalidId,
        "conditions/0/proof_profile/profile_id",
    );
    refuse(
        &|r| r.conditions[0].proof_profile = StudioProofProfile::Icc { profile_id: PROFILE.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "AB".repeat(32) } },
        Code::InvalidField,
        "conditions/0/proof_profile/content_digest",
    );
    // Operator matrix bounds equal Prism's: finite, |c| <= 4, row sums 0..=2.
    for (matrix, ok) in [
        ([4.0, -2.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], true),
        ([4.5, -2.5, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
        ([1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], true),
        ([1.0, 1.0, 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
        ([-0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
        ([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
        ([f64::INFINITY, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
    ] {
        let mut r = records();
        r.conditions[3].proof_profile = StudioProofProfile::OperatorMatrix { matrix };
        let expected = if ok { Ok(()) } else { Err((Code::InvalidField, "conditions/3/proof_profile/matrix".to_string())) };
        assert_eq!(check(&r), expected, "{matrix:?}");
    }
    // A toggle on without an active condition proofs nothing.
    refuse(&|r| {
        r.active_condition = None;
        r.gamut_warning.enabled = false;
    }, Code::InvalidField, "proof_colours");
    refuse(&|r| {
        r.active_condition = None;
        r.proof_colours = false;
    }, Code::InvalidField, "gamut_warning/enabled");
    // Shipped names are selectable as the active condition.
    for preset in SHIPPED_PROOF_PRESETS {
        let mut r = records();
        r.active_condition = Some(preset.into());
        assert_eq!(check(&r), Ok(()), "{preset}");
    }
    // Count ceiling and node budget before the walk.
    let mut r = records();
    r.conditions = (0..=MAX_PROOF_CONDITIONS).map(|i| condition(&format!("c{i}"), icc())).collect();
    r.active_condition = Some("c0".into());
    assert_eq!(check(&r), Err((Code::BudgetExceeded, "conditions".into())));
    let small = Budget { nodes: 2, ..Budget::default() };
    assert_eq!(validate_proof_records(&records(), &d, small, &token).unwrap_err().code, Code::BudgetExceeded);
    // Cancellation is honoured.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(validate_proof_records(&records(), &d, Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn gamut_opacity_follows_the_parameter_table() {
    // STU-COL-244 table row: gamut_warning.opacity 0..100, default 100, percent.
    assert_eq!((PROOF_GAMUT_OPACITY_MIN, PROOF_GAMUT_OPACITY_MAX, PROOF_GAMUT_OPACITY_DEFAULT), (0, 100, 100));
    assert_eq!(StudioGamutWarningState::default().opacity, 100);
    assert_eq!(StudioGamutWarningState::default().colour, PROOF_DEFAULT_WARNING_COLOUR);
    // Absent opacity and colour on the wire take the defaults.
    let wire = format!(r#"{{"schema_id":"hsk.studio.proof_records@1","document_id":"{DOC}","active_condition":"protanopia","gamut_warning":{{"enabled":true}}}}"#);
    let r = parse_proof_records(wire.as_bytes(), &document(), Budget::default(), &CancellationToken::default()).unwrap();
    assert_eq!((r.gamut_warning.opacity, r.gamut_warning.colour), (100, [0.5, 0.5, 0.5]));
    for (opacity, ok) in [(0, true), (1, true), (99, true), (100, true), (101, false), (u32::MAX, false)] {
        let mut r = records();
        r.gamut_warning.opacity = opacity;
        let expected = if ok { Ok(()) } else { Err((Code::InvalidField, "gamut_warning/opacity".to_string())) };
        assert_eq!(check(&r), expected, "opacity {opacity}");
    }
    for (colour, ok) in [([0.0, 0.5, 1.0], true), ([1.01, 0.0, 0.0], false), ([0.0, -0.01, 0.0], false), ([0.0, 0.0, f64::NAN], false)] {
        let mut r = records();
        r.gamut_warning.colour = colour;
        let expected = if ok { Ok(()) } else { Err((Code::InvalidField, "gamut_warning/colour".to_string())) };
        assert_eq!(check(&r), expected, "colour {colour:?}");
    }
    // A negative or fractional opacity is not an integer percent: the wire refuses it.
    for bad in ["-1", "50.5"] {
        let wire = format!(r#"{{"schema_id":"hsk.studio.proof_records@1","document_id":"{DOC}","gamut_warning":{{"enabled":false,"opacity":{bad}}}}}"#);
        assert_eq!(parse_proof_records(wire.as_bytes(), &document(), Budget::default(), &CancellationToken::default()).unwrap_err().code, Code::InvalidJson, "{bad}");
    }
}

#[test]
fn proof_records_absent_keep_documents_byte_identical() {
    let d = document();
    let before = serde_json::to_vec(&d).unwrap();
    assert_eq!(check(&records()), Ok(()));
    // The record set lives beside the document: the document wire and schema are untouched.
    assert_eq!(serde_json::to_vec(&d).unwrap(), before);
    let reread: StudioDocument = serde_json::from_slice(&before).unwrap();
    assert_eq!(serde_json::to_vec(&reread).unwrap(), before);
    let doc_schema = serde_json::to_string(&document_schema()).unwrap();
    assert!(!doc_schema.contains("proof_records") && !doc_schema.contains("simulate_paper_color"));
    // An empty record set: identity, both toggles off, the default gamut warning.
    let empty = StudioProofRecords::new(DOC);
    assert_eq!(
        serde_json::to_string(&empty).unwrap(),
        format!(r#"{{"schema_id":"hsk.studio.proof_records@1","document_id":"{DOC}","proof_colours":false,"gamut_warning":{{"enabled":false,"colour":[0.5,0.5,0.5],"opacity":100}}}}"#)
    );
    assert_eq!(check(&empty), Ok(()));
    let schema = serde_json::to_string(&proof_records_schema()).unwrap();
    assert!(schema.contains("hsk.studio.proof_records@1") && schema.contains("simulate_black_ink") && schema.contains("operator_matrix"));
}
