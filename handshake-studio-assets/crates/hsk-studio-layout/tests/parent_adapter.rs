//! SMB-LAYOUT-PARENT-ADAPTER: Folio `hsk.studio.parent_page_records@1` <-> layout `ParentSet`.
//! Oracles are hand-written rectangles and item orders.
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema,
    Length, LayoutEdges, NodeKind, NodeRef, PageSide, PageSpreadSchema, Parent, ParentPageSchema,
    Payload, ProfileBinding, StudioDocument, StudioItemOverride, StudioLayer, StudioLayerGraph,
    StudioPage, StudioPageParentBinding, StudioPageSpread, StudioParentPage, StudioParentPageItem,
    StudioParentPageRecords, StudioTextFrame, VerticalJustification,
};
use hsk_studio_layout::*;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length {
        value: v,
        unit: GeometryUnit::Pt,
    }
}
fn edges() -> LayoutEdges {
    LayoutEdges {
        top: len(0.0),
        right: len(0.0),
        bottom: len(0.0),
        left: len(0.0),
    }
}
fn page(key: &str) -> StudioPage {
    StudioPage {
        page_id: key.into(),
        width: len(100.0),
        height: len(100.0),
        side: PageSide::RightHand,
        margins: edges(),
        bleed: None,
    }
}
fn parent_ref() -> Parent {
    Parent::Node(NodeRef {
        node_kind: NodeKind::PageSpread,
        node_id: id("SPGS", 1),
    })
}
/// Text-frame layer `n` at (x, 0) size 10 x 10.
fn text_layer(n: u32, x: f64, w: f64) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("t{n}"),
        parent: parent_ref(),
        kind: "text".into(),
        payload: Payload::Text {
            story_id: id("STXT", 1),
        },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: "page-1".into(),
            bounds: Bounds {
                x: len(x),
                y: len(0.0),
                width: len(w),
                height: len(10.0),
            },
            inset_spacing: edges(),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: None,
            next_text_frame: None,
        }),
    }
}
/// Raster layer without stored geometry.
fn raster_layer(n: u32) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("r{n}"),
        parent: parent_ref(),
        kind: "raster".into(),
        payload: Payload::Raster { tiles: vec![] },
        visible: true,
        appearance: None,
        text_frame: None,
    }
}
fn document(layers: Vec<StudioLayer>) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", 1),
            name: "s".into(),
            parent: Parent::Root(()),
            bounds: Bounds {
                x: len(0.0),
                y: len(0.0),
                width: len(200.0),
                height: len(100.0),
            },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers,
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}
/// Layers 1 (logo, x 0..10), 2 (rule, raster), 3 (num, x 20..30), 4 (page-1 num replacement,
/// x 50..60), 5 (a layer used for refusals).
fn doc() -> StudioDocument {
    document(vec![
        text_layer(1, 0.0, 10.0),
        raster_layer(2),
        text_layer(3, 20.0, 10.0),
        text_layer(4, 50.0, 10.0),
        raster_layer(5),
    ])
}
fn rects() -> LayerRects {
    let mut m = LayerRects::new();
    m.insert(
        id("SLYR", 2),
        Rect {
            x0_pt: 1.0,
            y0_pt: 2.0,
            x1_pt: 3.0,
            y1_pt: 4.0,
        },
    );
    m
}
fn item(i: &str, l: u32) -> StudioParentPageItem {
    StudioParentPageItem {
        item_id: i.into(),
        layer_id: id("SLYR", l),
    }
}
fn parent(pid: &str, based_on: Option<&str>, items: Vec<StudioParentPageItem>) -> StudioParentPage {
    StudioParentPage {
        schema_id: ParentPageSchema::V1,
        parent_id: pid.into(),
        name: pid.into(),
        based_on: based_on.map(Into::into),
        parent_page_items: items,
    }
}
/// A holds logo + rule, B (based on A) holds num; page-1 applies B, replaces num by layer 4 and
/// suppresses logo; page-2 applies A with no overrides.
fn records() -> StudioParentPageRecords {
    let mut r = StudioParentPageRecords::new(id("SDOC", 1));
    r.parents = vec![
        parent("A", None, vec![item("logo", 1), item("rule", 2)]),
        parent("B", Some("A"), vec![item("num", 3)]),
    ];
    r.page_bindings = vec![
        StudioPageParentBinding {
            page_id: "page-1".into(),
            applied_parent: Some("B".into()),
            override_list: vec![
                StudioItemOverride::Replace {
                    parent_item_id: "num".into(),
                    replacement_layer_id: id("SLYR", 4),
                },
                StudioItemOverride::Suppress {
                    parent_item_id: "logo".into(),
                },
            ],
        },
        StudioPageParentBinding {
            page_id: "page-2".into(),
            applied_parent: Some("A".into()),
            override_list: vec![],
        },
    ];
    r
}
fn r(x0: f64, x1: f64) -> Rect {
    Rect {
        x0_pt: x0,
        y0_pt: 0.0,
        x1_pt: x1,
        y1_pt: 10.0,
    }
}

/// folio -> ParentSet -> resolved items: page-1 shows the ancestor's rule, then the replaced num
/// (page-local geometry, kind from its frame), with logo suppressed; page-2 shows A unchanged;
/// the reverse direction reproduces the records exactly.
#[test]
fn round_trip_folio_to_parent_set_to_resolved_items() {
    let (set, assign) = parent_set_from_records(&records(), &doc(), &rects()).expect("adapts");
    assert_eq!(set.parents.len(), 2);
    assert_eq!(set.parents[0].items[1].kind, ParentItemKind::Frame);
    assert_eq!(set.parents[0].items[0].kind, ParentItemKind::TextFrame);
    assert_eq!(set.parents[0].items[0].rect, r(0.0, 10.0));
    let p1 = set.resolve("page-1").expect("resolves");
    let ids: Vec<_> = p1.iter().map(|x| x.item.id.as_str()).collect();
    assert_eq!(ids, ["rule", "num"]);
    assert_eq!(p1[0].source, ItemSource::Parent { parent_id: "A".into() });
    assert_eq!(p1[1].item.rect, r(50.0, 60.0));
    assert_eq!(p1[1].source, ItemSource::Override { parent_id: "B".into() });
    let p2 = set.resolve("page-2").expect("resolves");
    let ids2: Vec<_> = p2.iter().map(|x| x.item.id.as_str()).collect();
    assert_eq!(ids2, ["logo", "rule"]);
    // the convenience entry point agrees
    let direct = resolve_page_from_records(&records(), &doc(), &rects(), "page-1").unwrap();
    assert_eq!(direct, p1);
    // reverse
    let back = records_from_parent_set(&set, &id("SDOC", 1), &assign).expect("reverse");
    assert_eq!(back, records());
}

/// A layout set with page-local items has no Folio record: typed refusal, nothing dropped. The
/// same set without local items maps back.
#[test]
fn page_local_items_are_refused_in_reverse() {
    let (mut set, assign) = parent_set_from_records(&records(), &doc(), &rects()).unwrap();
    set.bindings[1].local_items.push(ParentItem {
        id: "extra".into(),
        kind: ParentItemKind::Frame,
        rect: r(0.0, 5.0),
    });
    assert_eq!(
        records_from_parent_set(&set, &id("SDOC", 1), &assign),
        Err(ParentAdapterError::PageLocalItemsNoRecord {
            page_id: "page-2".into()
        })
    );
    set.bindings[1].local_items.clear();
    assert!(records_from_parent_set(&set, &id("SDOC", 1), &assign).is_ok());
    // missing assignment and a replacement id that Folio cannot express
    let empty = LayerAssignment::default();
    assert!(matches!(
        records_from_parent_set(&set, &id("SDOC", 1), &empty),
        Err(ParentAdapterError::MissingLayerAssignment { .. })
    ));
    if let ItemOverride::Replace { replacement, .. } = &mut set.bindings[0].overrides[0] {
        replacement.id = "other".into();
    }
    assert!(matches!(
        records_from_parent_set(&set, &id("SDOC", 1), &assign),
        Err(ParentAdapterError::ReplacementIdMismatch { .. })
    ));
}

/// Hostile or inconsistent records are refused with a typed code, never mis-laid.
#[test]
fn hostile_records_are_refused() {
    let code = |recs: &StudioParentPageRecords, rc: &LayerRects| -> String {
        parent_set_from_records(recs, &doc(), rc)
            .expect_err("refused")
            .code()
            .to_string()
    };
    // no geometry for a raster layer
    assert_eq!(code(&records(), &LayerRects::new()), "no_geometry");
    // document mismatch
    let mut bad = records();
    bad.document_id = id("SDOC", 9);
    assert_eq!(code(&bad, &rects()), "document_mismatch");
    // unknown page
    let mut bad = records();
    bad.page_bindings[1].page_id = "nope".into();
    assert_eq!(code(&bad, &rects()), "unknown_page");
    // missing layer
    let mut bad = records();
    bad.parents[0].parent_page_items[0].layer_id = id("SLYR", 99);
    assert_eq!(code(&bad, &rects()), "missing_layer");
    // based_on cycle (layout validation)
    let mut bad = records();
    bad.parents[0].based_on = Some("B".into());
    assert_eq!(code(&bad, &rects()), "invalid_input");
    // override of an item not on the applied chain
    let mut bad = records();
    bad.page_bindings[1].override_list = vec![StudioItemOverride::Suppress {
        parent_item_id: "num".into(),
    }];
    assert_eq!(code(&bad, &rects()), "invalid_input");
    // non-finite supplied rectangle
    let mut rc = rects();
    rc.insert(
        id("SLYR", 2),
        Rect {
            x0_pt: f64::NAN,
            y0_pt: 0.0,
            x1_pt: 1.0,
            y1_pt: 1.0,
        },
    );
    assert_eq!(code(&records(), &rc), "invalid_geometry");
    // text frame in a non-pt unit
    let mut d = doc();
    d.layers[0].text_frame.as_mut().unwrap().bounds.x.unit = GeometryUnit::Px;
    assert_eq!(
        parent_set_from_records(&records(), &d, &rects())
            .expect_err("refused")
            .code(),
        "invalid_geometry"
    );
    // budget: too many parents
    let mut big = StudioParentPageRecords::new(id("SDOC", 1));
    for i in 0..1025 {
        big.parents.push(parent(&format!("P{i}"), None, vec![]));
    }
    assert_eq!(code(&big, &rects()), "budget_exceeded");
}
