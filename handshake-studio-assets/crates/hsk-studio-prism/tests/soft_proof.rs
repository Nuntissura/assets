//! Soft proofing (SMB-PS-SOFT-PROOF-1): proof condition record and presets (STU-COL-241/242/243), the
//! two-intent proof chain (STU-COL-022/023/240), colour-vision conditions (STU-COL-243), gamut warning and
//! in-gamut substitute (STU-COL-244/245/138). Profiles: CC0 `tests/profiles/sRGB-v4.icc` and
//! `DisplayP3-v4.icc`, project-made synthetic `tests/oracle/profiles/hsk-synth-cmyk-v2.icc`.
//!
//! NOT_PROVEN (printed by `not_proven_parity_lines`): equality with Photoshop View > Proof Colors / Gamut
//! Warning output and with LittleCMS proofing transforms (no proof case in either oracle corpus).
mod wide_support;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_prism::*;
use wide_support::{Ctx, SRGB};

const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const CMYK: &[u8] = include_bytes!("oracle/profiles/hsk-synth-cmyk-v2.icc");

fn id(n: u32) -> DomainId {
    DomainId::parse(&format!("SCPF-019abcde-0000-7000-8000-{n:012x}")).unwrap()
}
fn input<'a>(id: &'a DomainId, bytes: &'a [u8]) -> ProfileInput<'a> {
    ProfileInput { profile_id: id, bytes, expected_sha256: profile_hash(bytes) }
}

#[test]
fn not_proven_parity_lines() {
    println!("NOT_PROVEN: soft-proof output equals Photoshop View > Proof Colors (no proof case in the D5 Photoshop corpus)");
    println!("NOT_PROVEN: gamut warning equals Photoshop View > Gamut Warning and LittleCMS cmsCreateProofingTransform gamut check (no oracle file)");
}

#[test]
fn proof_condition_has_six_independent_fields() {
    let (cid, cmyk) = (id(1), CMYK);
    let base = ProofCondition::working_cmyk(input(&cid, cmyk));
    let r0 = base.record();
    assert_eq!(r0.kind, ProofProfileKind::Icc);
    assert_eq!(r0.profile_sha256, Some(profile_hash(CMYK)));
    // Each mutation changes exactly one of the six fields of the record.
    let (oid, srgb) = (id(2), SRGB);
    for i in 0..6 {
        let mut c = base.clone();
        match i {
            0 => c.proof_profile = ProofProfile::Icc(input(&oid, srgb)),
            1 => c.proof_intent = Intent::Perceptual,
            2 => c.display_intent = Intent::Saturation,
            3 => c.black_point_compensation = !c.black_point_compensation,
            4 => c.simulate_paper_color = !c.simulate_paper_color,
            _ => c.simulate_black_ink = !c.simulate_black_ink,
        }
        let r = c.record();
        let changed = [
            r.profile_sha256 != r0.profile_sha256,
            r.proof_intent != r0.proof_intent,
            r.display_intent != r0.display_intent,
            r.black_point_compensation != r0.black_point_compensation,
            r.simulate_paper_color != r0.simulate_paper_color,
            r.simulate_black_ink != r0.simulate_black_ink,
        ];
        let expected: Vec<bool> = (0..6).map(|j| j == i).collect();
        assert_eq!(changed.to_vec(), expected, "mutation {i} touched another field");
    }
    // Paper colour and black ink are separate booleans: all four combinations are representable.
    for (paper, ink) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut c = base.clone();
        c.simulate_paper_color = paper;
        c.simulate_black_ink = ink;
        let r = c.record();
        assert_eq!((r.simulate_paper_color, r.simulate_black_ink), (paper, ink));
        let json = r.to_json();
        assert!(json.contains(&format!("\"simulate_paper_color\":{paper}")) && json.contains(&format!("\"simulate_black_ink\":{ink}")));
    }
    // The two intents are separate fields of the saved record.
    let mut c = base.clone();
    c.proof_intent = Intent::Perceptual;
    c.display_intent = Intent::AbsoluteColorimetric;
    let json = c.record().to_json();
    assert!(json.contains("\"proof_intent\":\"perceptual\"") && json.contains("\"display_intent\":\"absolute_colorimetric\""), "{json}");
    // Names are validated and escaped.
    assert_eq!(ProofCondition::new("", ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameEmpty));
    assert_eq!(ProofCondition::new(&"x".repeat(MAX_PROOF_NAME_BYTES + 1), ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameTooLong));
    assert_eq!(ProofCondition::new("a\u{7}b", ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameControl));
    let quoted = ProofCondition::new("say \"hi\" \\", ProofProfile::LegacyUncompensatedRgb).record().to_json();
    assert!(quoted.starts_with("{\"name\":\"say \\\"hi\\\" \\\\\""), "{quoted}");
}

#[test]
fn presets_include_the_spec_minimum() {
    let (cid, rid) = (id(1), id(2));
    let presets = shipped_proof_conditions(input(&cid, CMYK), input(&rid, SRGB));
    let names: Vec<&str> = presets.iter().map(|c| c.name.as_str()).collect();
    for required in [PRESET_WORKING_CMYK, PRESET_WORKING_RGB, PRESET_LEGACY_UNCOMPENSATED_RGB, PRESET_PROTANOPIA, PRESET_DEUTERANOPIA] {
        assert!(names.contains(&required), "missing preset {required}: {names:?}");
    }
    for c in &presets {
        c.validate().unwrap();
    }
    let cmyk = select_proof_condition(&presets, PRESET_WORKING_CMYK).unwrap();
    assert_eq!(cmyk.record().profile_sha256, Some(profile_hash(CMYK)));
    let rgb = select_proof_condition(&presets, PRESET_WORKING_RGB).unwrap();
    assert_eq!(rgb.record().profile_sha256, Some(profile_hash(SRGB)));
    assert_eq!(
        select_proof_condition(&presets, PRESET_LEGACY_UNCOMPENSATED_RGB).unwrap().proof_profile.kind(),
        ProofProfileKind::LegacyUncompensatedRgb
    );
    // The accessibility conditions are first-class proof profiles, not a filter.
    assert_eq!(
        select_proof_condition(&presets, PRESET_PROTANOPIA).unwrap().proof_profile.kind(),
        ProofProfileKind::ColourVision(ColourVision::Protanopia)
    );
    assert_eq!(
        select_proof_condition(&presets, PRESET_DEUTERANOPIA).unwrap().proof_profile.kind(),
        ProofProfileKind::ColourVision(ColourVision::Deuteranopia)
    );
    assert!(select_proof_condition(&presets, "no_such_condition").is_none());
}
