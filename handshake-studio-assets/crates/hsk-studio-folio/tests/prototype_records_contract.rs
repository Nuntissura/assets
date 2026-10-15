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

#[test]
fn overlay_records_round_trip_and_keep_empty_sets_identical() {
    // Lane SMB-FIG-SMART-ANIMATE: STU-PRO-122..124 frame overlay fields.
    let before = r#"{"schema_id":"hsk.studio.design_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001"}"#;
    let mut r: StudioDesignRecords = serde_json::from_str(before).unwrap();
    assert!(r.frame_overlay.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    let node = NodeRef { node_kind: NodeKind::Layer, node_id: "SLYR-019abcde-0000-7000-8000-000000000002".into() };
    r.frame_overlay.push(StudioFrameOverlay {
        node: node.clone(),
        overlay_position_type: Some(StudioOverlayPositionType::BottomCenter),
        overlay_background: StudioOverlayBackground::SolidColor {
            color: StudioRgba { r: 0.0, g: 0.0, b: 0.0, a: 0.4, colour_profile_id: ProfileBinding::Unassigned(()) },
        },
        overlay_background_interaction: StudioOverlayBackgroundInteraction::CloseOnClickOutside,
    });
    r.frame_overlay.push(StudioFrameOverlay {
        node: NodeRef { node_kind: NodeKind::Layer, node_id: "SLYR-019abcde-0000-7000-8000-000000000003".into() },
        overlay_position_type: None,
        overlay_background: StudioOverlayBackground::None {},
        overlay_background_interaction: StudioOverlayBackgroundInteraction::None,
    });
    let text = serde_json::to_string(&r).unwrap();
    assert!(text.contains(r#""overlay_position_type":"BOTTOM_CENTER""#), "{text}");
    assert!(text.contains(r#""overlay_background":{"type":"SOLID_COLOR","color":{"r":0.0,"g":0.0,"b":0.0,"a":0.4"#), "{text}");
    assert!(text.contains(r#""overlay_background_interaction":"CLOSE_ON_CLICK_OUTSIDE""#));
    assert!(text.contains(r#""overlay_background":{"type":"NONE"}"#));
    let back: StudioDesignRecords = serde_json::from_str(&text).unwrap();
    assert_eq!(back, r);
    // Unset resolves to CENTER (STU-PRO-123); MANUAL is a member; unknown members are refused.
    assert_eq!(r.frame_overlay[1].position_type(), StudioOverlayPositionType::Center);
    assert!(serde_json::from_str::<StudioOverlayPositionType>(r#""MANUAL""#).is_ok());
    assert!(serde_json::from_str::<StudioOverlayPositionType>(r#""LEFT""#).is_err());
    assert!(serde_json::from_str::<StudioOverlayBackground>(r#"{"type":"GRADIENT"}"#).is_err());
}
