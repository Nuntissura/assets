//! Lane SMB-DEVELOP-LENS-PRESETS slice 5: model-assisted masks (subject, sky, people, ...;
//! STU-RAW-014a / STU-RAW-123). No model provider exists in this build and nothing is downloaded:
//! the record is valid and retained, evaluating it is the typed refusal `model_unavailable:<kind>`.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_observe::{FailureCode, Outcome};

fn recipe(masks_on: bool, kind: ModelMaskKind, version: Option<String>) -> DevelopRecipe {
    let profile = ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] };
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile);
    r.enables.enable_mask_group_based_corrections = masks_on;
    let mask = Mask {
        id: "m1".into(),
        active: true,
        inverted: false,
        value: 1.0,
        blend: MaskBlend::Add,
        source: MaskSource::Model(ModelMask { kind, subcategory: Some(50_008), model_version: version }),
    };
    r.adjust.local = vec![Correction::new("c1", vec![mask], LocalStack { exposure: 0.5, ..LocalStack::default() })];
    r
}

#[test]
fn model_masks_are_typed_model_unavailable_and_never_evaluated() {
    let plane = CfaPlane::with_limits(
        PlaneInit {
            width: 16,
            height: 16,
            stride_bytes: 32,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..256u32).flat_map(|_| 1000u16.to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap();
    let cancel = CancellationToken::default();
    for kind in ModelMaskKind::ALL {
        assert_eq!(ModelMaskKind::from_key(kind.key()), Some(kind));
        // Retained and valid while the mask group is off (the record round-trips untouched).
        assert_eq!(recipe(false, kind, None).validate(), Ok(()));
        // Evaluated: the typed refusal names the source kind; its outcome class is Unavailable.
        let err = develop_to_image(&plane, &recipe(true, kind, None), 1, &cancel).unwrap_err();
        assert_eq!(err, DevelopError::ModelUnavailable(kind.key()));
        assert_eq!(err.to_string(), format!("model_unavailable:{}", kind.key()));
        assert_eq!(err.outcome(), Outcome::Failure(FailureCode::Unavailable));
    }
    assert_eq!(ModelMaskKind::from_key("depth_range"), None);
    // A model version longer than the cap is an invalid record even while the group is off.
    let long = recipe(false, ModelMaskKind::Sky, Some("v".repeat(MAX_MODEL_VERSION + 1)));
    assert_eq!(long.validate(), Err(DevelopError::InvalidMask));
}
