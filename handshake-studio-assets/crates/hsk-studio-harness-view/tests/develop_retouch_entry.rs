//! Lane SMB-LR-HEALING-1 (feature `viewer`): the session document entry `develop` carries the
//! develop retouch stack (STU-RAW-124 areas, core distraction areas, adapter fill records) through
//! encode and decode (the bytes the package save writes and the reopen reads), keeps the bytes of
//! an edit without retouch unchanged (no `retouch` key), and refuses hostile retouch lists typed.
//! NOT_PROVEN: the window's spot tool and the viewer's enable wiring (no develop GUI hunk in this lane).
#![cfg(feature = "viewer")]

use hsk_studio_develop::retouch::{MAX_AREA_MASKS, MAX_RETOUCH_AREAS};
use hsk_studio_develop::{
    AdapterFillRecord, AdapterRect, Adjustments, Brush, DistractionArea, PatchVariation, RetouchArea, RetouchMethod, RetouchStack, SourceState,
    SpotType,
};
use hsk_studio_harness_view::view::develop::DevelopEdit;
use hsk_studio_harness_view::view::develop_entry;
use serde_json::{Value, json};

fn stack() -> RetouchStack {
    RetouchStack {
        areas: vec![
            RetouchArea {
                spot_type: SpotType::Heal,
                heal_version: 3,
                feather: 0.35,
                opacity: 0.9,
                seed: u64::MAX - 7,
                method: RetouchMethod::Gaussian,
                source_state: SourceState::SourceSetExplicitly,
                masks: vec![Brush { dabs: vec![(0.1, 0.2), (0.125, 0.2)], radius: 0.03, flow: 0.8, feather: 0.5 }],
                source_x: 0.6,
                source_y: 0.7,
                offset_y: -0.015625,
                centre_x: 0.1,
                centre_y: 0.2,
                radius: 0.03,
            },
            RetouchArea { source_state: SourceState::EngineChosen, ..RetouchArea::circle(SpotType::Clone, (0.5, 0.5), 0.05, (0.5, 0.5)) },
        ],
        distraction: vec![DistractionArea { masks: vec![Brush { dabs: vec![(0.8, 0.8)], radius: 0.04, flow: 1.0, feather: 0.0 }], seed: 42 }],
        adapter_fills: vec![AdapterFillRecord {
            adapter_id: "studio.adapter.genfill".into(),
            model_id: "vendor/remove".into(),
            model_version: "3.1.0".into(),
            ran_locally: true,
            fill_method: "generative".into(),
            image_extent: (6000, 4000),
            search_rect: AdapterRect { x: 10, y: 20, width: 300, height: 200 },
            target_rect: AdapterRect { x: 40, y: 50, width: 60, height: 70 },
            black_level: 512.5,
            gamma_type: "linear".into(),
            input_digest: [0xab; 32],
            patch_digest: [0x01; 32],
            remap_info: vec!["a".into(), "b".into()],
            variations: vec![PatchVariation { id: "v0".into(), patch_digest: [0xfe; 32] }],
            selected_variation: Some(0),
        }],
    }
}

fn edit(retouch: RetouchStack) -> DevelopEdit {
    DevelopEdit { exposure_stops: 0.5, adjust: Adjustments { retouch, ..Adjustments::default() } }
}

#[test]
fn develop_entry_round_trips_retouch_areas() {
    let with = edit(stack());
    let bytes = develop_entry::encode("layer-1", None, &with).unwrap();
    let decoded = develop_entry::decode(&bytes).unwrap();
    assert_eq!(decoded.edit, with, "lossless retouch round trip");
    assert_eq!(decoded.layer_id, "layer-1");
    // Re-encoding the decoded edit is byte-identical (stable save after reopen).
    assert_eq!(develop_entry::encode("layer-1", None, &decoded.edit).unwrap(), bytes);

    // No retouch: no `retouch` key, so entries written before this lane keep their bytes; a missing
    // key decodes as the empty stack.
    let without = edit(RetouchStack::default());
    let plain = develop_entry::encode("layer-1", None, &without).unwrap();
    let v: Value = serde_json::from_slice(&plain).unwrap();
    assert!(v["edit"].get("retouch").is_none());
    assert_eq!(develop_entry::decode(&plain).unwrap().edit, without);

    // Hostile retouch objects: typed refusals.
    let base: Value = serde_json::from_slice(&bytes).unwrap();
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut x = base.clone();
        f(&mut x);
        serde_json::to_vec(&x).unwrap()
    };
    let area = base["edit"]["retouch"]["areas"][1].clone();
    let mask = base["edit"]["retouch"]["areas"][0]["masks"][0].clone();
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("areas over the cap", mutate(&|x| x["edit"]["retouch"]["areas"] = json!(vec![area.clone(); MAX_RETOUCH_AREAS + 1])), "develop_entry_limit"),
        ("masks over the cap", mutate(&|x| x["edit"]["retouch"]["areas"][0]["masks"] = json!(vec![mask.clone(); MAX_AREA_MASKS + 1])), "develop_entry_limit"),
        ("dabs over the cap", mutate(&|x| x["edit"]["retouch"]["areas"][0]["masks"][0]["dabs"] = json!(vec![[0.5, 0.5]; hsk_studio_develop::local::MAX_DABS + 1])), "develop_entry_limit"),
        ("unknown spot type", mutate(&|x| x["edit"]["retouch"]["areas"][0]["spot_type"] = json!("patch")), "develop_entry_field"),
        ("unknown source state", mutate(&|x| x["edit"]["retouch"]["areas"][0]["source_state"] = json!("auto")), "develop_entry_field"),
        ("unknown method", mutate(&|x| x["edit"]["retouch"]["areas"][0]["method"] = json!("box")), "develop_entry_field"),
        ("seed negative", mutate(&|x| x["edit"]["retouch"]["areas"][0]["seed"] = json!(-1)), "develop_entry_field"),
        ("heal version over u32", mutate(&|x| x["edit"]["retouch"]["areas"][0]["heal_version"] = json!(1u64 << 32)), "develop_entry_field"),
        ("area unknown key", mutate(&|x| x["edit"]["retouch"]["areas"][0]["extra"] = json!(1)), "develop_entry_field"),
        ("area key missing", mutate(&|x| { x["edit"]["retouch"]["areas"][0].as_object_mut().unwrap().remove("offset_y"); }), "develop_entry_field"),
        ("feather a string", mutate(&|x| x["edit"]["retouch"]["areas"][0]["feather"] = json!("0.3")), "develop_entry_field"),
        ("digest short", mutate(&|x| x["edit"]["retouch"]["adapter_fills"][0]["input_digest"] = json!("abcd")), "develop_entry_field"),
        ("digest not hex", mutate(&|x| x["edit"]["retouch"]["adapter_fills"][0]["patch_digest"] = json!("zz".repeat(32))), "develop_entry_field"),
        ("rect short", mutate(&|x| x["edit"]["retouch"]["adapter_fills"][0]["search_rect"] = json!([1, 2, 3])), "develop_entry_field"),
        ("retouch key missing", mutate(&|x| { x["edit"]["retouch"].as_object_mut().unwrap().remove("distraction"); }), "develop_entry_field"),
    ];
    for (what, bytes, code) in &cases {
        assert_eq!(develop_entry::decode(bytes).map(|_| ()).map_err(|r| r.code), Err(*code), "{what}");
    }
}
