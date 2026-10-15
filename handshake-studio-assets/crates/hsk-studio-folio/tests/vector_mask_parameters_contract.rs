//! `StudioVectorMaskParameters` contract (lane SMB-COMPOSITE-VECTOR-MASK-2): STU-RAS-016 density,
//! feather and link, and Illustrator's unclipped opacity-mask reading, as a sidecar keyed by
//! `mask_id`. Record sets without it serialize to the bytes the folio code before this lane
//! produced (pinned from a run of that code: `OLD_BYTES`), every field is skipped at its default,
//! and the ranges and references are validated with typed codes.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn records() -> Value {
    json!({
        "schema_id":"hsk.studio.vector_records@1","document_id":id("SDOC",1),
        "clips":[{"clip_id":"clip-a","source_layer_id":id("SLYR",3),"clip_rule":"EVENODD"}],
        "masks":[{"mask_id":"mask-a","source_layer_id":id("SLYR",7),"mode":"LUMINANCE","mask_clip":true,
            "region":{"x":pt(0.0),"y":pt(0.0),"width":pt(100.0),"height":pt(50.0)}},
            {"mask_id":"mask-b","source_layer_id":id("SLYR",3),"mode":"ALPHA","invert_mask":true}],
        "attachments":[{"layer_id":id("SLYR",4),"clip_id":"clip-a","mask_id":"mask-a"}]
    })
}
/// `serde_json::to_string` of [`records`] under the folio code before this lane (scratch export of
/// a78cb6d, throwaway probe `print_old_bytes`, 2026-10-15).
const OLD_BYTES: &str = r#"{"schema_id":"hsk.studio.vector_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","clips":[{"clip_id":"clip-a","source_layer_id":"SLYR-019abcde-0000-7000-8000-000000000003","clip_rule":"EVENODD"}],"masks":[{"mask_id":"mask-a","source_layer_id":"SLYR-019abcde-0000-7000-8000-000000000007","mode":"LUMINANCE","mask_clip":true,"region":{"x":{"value":0.0,"unit":"pt"},"y":{"value":0.0,"unit":"pt"},"width":{"value":100.0,"unit":"pt"},"height":{"value":50.0,"unit":"pt"}}},{"mask_id":"mask-b","source_layer_id":"SLYR-019abcde-0000-7000-8000-000000000003","mode":"ALPHA","invert_mask":true}],"attachments":[{"layer_id":"SLYR-019abcde-0000-7000-8000-000000000004","clip_id":"clip-a","mask_id":"mask-a"}]}"#;

fn document() -> StudioDocument {
    let layer = |n: u32, kind: &str, payload: Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",n),"name":format!("l{n}"),
            "parent":null,"kind":kind,"payload":payload,"visible":true})
    };
    let vector = json!({"payload_kind":"vector","path_ids":[id("SVPT",1)]});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(3, "vector", vector.clone()), layer(4, "raster", json!({"payload_kind":"raster","tiles":[]})),
            layer(7, "vector", vector)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn validate(v: &Value) -> Result<(), Diagnostic> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default())
}

#[test]
fn mask_parameters_are_skipped_at_defaults_round_trip_and_are_validated() {
    // Byte-identical: a record set without parameters serializes exactly as before.
    let r: StudioVectorRecords = serde_json::from_value(records()).unwrap();
    assert!(r.mask_parameters.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), OLD_BYTES);
    assert_eq!(validate(&records()), Ok(()));

    // An all-default entry is only its key; every default is read back.
    let mut v = records();
    v["mask_parameters"] = json!([{"mask_id":"mask-a"}]);
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).unwrap();
    let p = r.mask_parameters("mask-a").unwrap();
    assert_eq!((p.density, p.feather, p.linked, p.reveal_outside), (100.0, 0.0, true, false));
    let bytes = serde_json::to_string(&r).unwrap();
    assert!(bytes.ends_with(r#","mask_parameters":[{"mask_id":"mask-a"}]}"#), "{bytes}");
    assert_eq!(validate(&v), Ok(()));

    // Non-default values round trip.
    let mut v = records();
    v["mask_parameters"] = json!([{"mask_id":"mask-b","density":40.0,"feather":2.5,"linked":false,"reveal_outside":true}]);
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).unwrap();
    let back: StudioVectorRecords = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
    let p = r.mask_parameters("mask-b").unwrap();
    assert_eq!((p.density, p.feather, p.linked, p.reveal_outside), (40.0, 2.5, false, true));
    assert_eq!(validate(&v), Ok(()));

    // Typed refusals: dangling mask, duplicate entry, density and feather out of range.
    let refused = |params: Value| {
        let mut v = records();
        v["mask_parameters"] = params;
        validate(&v).expect_err("refused").code
    };
    assert_eq!(refused(json!([{"mask_id":"nope"}])), Code::DanglingReference);
    assert_eq!(refused(json!([{"mask_id":"mask-a"},{"mask_id":"mask-a","density":5.0}])), Code::DuplicateIdentity);
    assert_eq!(refused(json!([{"mask_id":"mask-a","density":100.5}])), Code::InvalidField);
    assert_eq!(refused(json!([{"mask_id":"mask-a","density":-1.0}])), Code::InvalidField);
    assert_eq!(refused(json!([{"mask_id":"mask-a","feather":10000.5}])), Code::InvalidField);
    assert_eq!(refused(json!([{"mask_id":"mask-a","feather":-0.5}])), Code::InvalidField);
    // Unknown fields stay refused (deny_unknown_fields).
    let mut v = records();
    v["mask_parameters"] = json!([{"mask_id":"mask-a","density_percent":5.0}]);
    assert!(serde_json::from_value::<StudioVectorRecords>(v).is_err());
}
