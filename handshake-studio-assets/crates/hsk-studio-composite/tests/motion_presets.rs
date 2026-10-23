//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 3 (feature `motion`): the Studio-native animation
//! preset record (Folio `StudioMotionPresetSet`, a style-registry entry, STU-MOT-110) round-trips
//! through wire JSON and applies to a layer's property tree at a time offset through motion
//! (STU-MOT-111 all-or-nothing). After Effects `.ffx` (proprietary RIFX/FaFX binary) is refused with a
//! typed error and never read. `.ffx` semantics UNVERIFIED (D5).
#![cfg(feature = "motion")]
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_composite::temporal::motion_preset::*;
use hsk_studio_folio::{Budget, Code, StudioMotionPresetSet, validate_motion_presets, MOTION_PRESET_FFX, parse_motion_presets};
use hsk_studio_motion::{Keyframe, PresetEffect, Property, PropertyPath, PropertyStore, capture_preset};

fn p(s: &str) -> PropertyPath {
    PropertyPath::new(s).unwrap()
}

#[test]
fn preset_records_round_trip_through_json_apply_at_an_offset_and_refuse_ffx() {
    let cancel = CancellationToken::default();
    // Save: capture a layer's eased scale animation, its expression and a glow instance.
    let mut src = PropertyStore::new();
    src.insert(
        p("c/a/transform/scale"),
        Property::new("scale", vec![100.0, 100.0], vec![Keyframe::eased(Ticks::new(500), vec![0.0, 0.0]), Keyframe::eased(Ticks::new(1500), vec![120.0, 120.0])]).unwrap(),
    )
    .unwrap();
    src.insert(p("c/a/effects/glow/intensity"), Property::new("intensity", vec![2.5], vec![]).unwrap().with_expression("time*2").unwrap()).unwrap();
    let fx = vec![PresetEffect { instance: "glow".into(), effect_kind: "glow".into() }];
    let preset = capture_preset(&src, "c/a", "Pop in", &fx, &["transform/scale", "effects/glow/intensity"], None).unwrap();
    let mut set = StudioMotionPresetSet::new();
    set.presets.push(record_from_preset(&preset, "pop_in", &["Transitions".to_string()]));
    validate_motion_presets(&set, Budget::default(), &cancel).unwrap();
    let wire = serde_json::to_vec(&set).unwrap();
    let back = import_preset_bytes(&wire, Budget::default(), &cancel).unwrap();
    assert_eq!(back, set);
    assert_eq!(preset_from_record(&back.presets[0]), preset, "record <-> motion preset is lossless");

    // Apply at tick 9000 to another layer: scale keys at 9000 / 10000 (eased), glow inserted.
    let mut dst = PropertyStore::new();
    dst.insert(p("c/b/transform/scale"), Property::new("scale", vec![100.0, 100.0], vec![]).unwrap()).unwrap();
    let applied = apply_preset_record(&mut dst, "c/b", &back.presets[0], Ticks::new(9000)).unwrap();
    assert_eq!(applied.inserted, vec![p("c/b/effects/glow/intensity")]);
    let s = dst.get(&p("c/b/transform/scale")).unwrap();
    assert_eq!(s.keyframes().iter().map(|k| k.time.value()).collect::<Vec<_>>(), vec![9000, 10_000]);
    assert_eq!(s.keyframes()[1].value, vec![120.0, 120.0]);
    assert_eq!(dst.get(&p("c/b/effects/glow/intensity")).unwrap().expression(), Some("time*2"));
    // A target without the scale property: PRESET_TARGET_INCOMPATIBLE, nothing applied.
    let mut solid = PropertyStore::new();
    let e = apply_preset_record(&mut solid, "c/s", &back.presets[0], Ticks::new(0)).unwrap_err();
    assert_eq!(e.to_string(), "PRESET_TARGET_INCOMPATIBLE:transform/scale");
    assert!(solid.is_empty());

    // .ffx: refused by its magic before any parsing, typed at both layers.
    let mut ffx = b"RIFX\x00\x00\x10\x00FaFXhead".to_vec();
    ffx.extend_from_slice(&[0u8; 64]);
    assert_eq!(import_preset_bytes(&ffx, Budget::default(), &cancel).unwrap_err(), PresetImportError::ProprietaryFfx);
    let d = parse_motion_presets(&ffx, Budget::default(), &cancel).unwrap_err();
    assert_eq!((d.code, d.target.as_str()), (Code::UnsupportedSchema, MOTION_PRESET_FFX));

    // Hostile records: an insert flag that names no instance, a value beyond the bound, a key out
    // of order, unknown fields, a blank name.
    let mut bad = set.clone();
    bad.presets[0].properties[0].insert = true;
    assert!(validate_motion_presets(&bad, Budget::default(), &cancel).is_err());
    let mut bad = set.clone();
    bad.presets[0].properties[0].value.static_value = vec![1.0e300, 0.0];
    assert!(validate_motion_presets(&bad, Budget::default(), &cancel).is_err());
    let mut bad = set.clone();
    bad.presets[0].properties[0].value.keyframes.swap(0, 1);
    assert!(validate_motion_presets(&bad, Budget::default(), &cancel).is_err());
    let mut bad = set.clone();
    bad.presets[0].name = "  ".into();
    assert!(validate_motion_presets(&bad, Budget::default(), &cancel).is_err());
    let unknown = String::from_utf8(wire.clone()).unwrap().replacen("\"presets\"", "\"extra\":1,\"presets\"", 1);
    assert!(matches!(import_preset_bytes(unknown.as_bytes(), Budget::default(), &cancel), Err(PresetImportError::Record(_))));
}
