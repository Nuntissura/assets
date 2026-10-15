//! Lane SMB-INTEROP-FIG-PROTOTYPE: the prototype members of the design records (`prototype_pages`:
//! STU-PRO-128/129 flow starting points and entry frame; `frame_overflow`: STU-PRO-138) are
//! serde-default and skipped when empty, so earlier record sets read and write byte-identically;
//! the STU-PRO-120 easing presets are valid Folio easings. Validation against a document is proven
//! on the imported Figma fixture (interop-fig tests/fig_prototype.rs).
use hsk_studio_folio::*;

#[test]
fn empty_members_keep_records_byte_identical() {
    // A record set written before the members existed.
    let before = r#"{"schema_id":"hsk.studio.design_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001"}"#;
    let r: StudioDesignRecords = serde_json::from_str(before).unwrap();
    assert!(r.prototype_pages.is_empty() && r.frame_overflow.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    assert_eq!(serde_json::to_string(&StudioDesignRecords::new("SDOC-019abcde-0000-7000-8000-000000000001")).unwrap(), before);
    // With members: the wire shape, and a round trip.
    let mut r = r;
    r.prototype_pages.push(StudioPrototypePage {
        page: NodeRef { node_kind: NodeKind::Layer, node_id: "SLYR-019abcde-0000-7000-8000-000000000002".into() },
        flow_starting_points: vec![StudioFlowStart { node_id: "SLYR-019abcde-0000-7000-8000-000000000003".into(), name: "Flow 1".into() }],
        prototype_start_node_id: None,
    });
    r.frame_overflow.push(StudioFrameOverflow {
        node: NodeRef { node_kind: NodeKind::Artboard, node_id: "SART-019abcde-0000-7000-8000-000000000004".into() },
        overflow_direction: StudioOverflowDirection::Vertical,
    });
    let text = serde_json::to_string(&r).unwrap();
    assert!(text.contains(r#""flow_starting_points":[{"node_id":"SLYR-019abcde-0000-7000-8000-000000000003","name":"Flow 1"}]"#));
    assert!(text.contains(r#""overflow_direction":"VERTICAL""#));
    assert!(!text.contains("prototype_start_node_id"));
    let back: StudioDesignRecords = serde_json::from_str(&text).unwrap();
    assert_eq!(back, r);
    // Unknown members stay refused (deny_unknown_fields).
    assert!(serde_json::from_str::<StudioFrameOverflow>(r#"{"node":{"node_kind":"artboard","node_id":"x"},"overflow_direction":"VERTICAL","x":1}"#).is_err());
    // STU-PRO-120 presets: seven named tuples, ease-in-and-out symmetric.
    assert_eq!(EASING_PRESETS.len(), 7);
    let b = easing_preset("EASE_IN_AND_OUT").unwrap();
    assert_eq!((b.x1, b.y1, b.x2, b.y2), (0.42, 0.0, 0.58, 1.0));
    assert!(easing_preset("GENTLE").is_none());
}
