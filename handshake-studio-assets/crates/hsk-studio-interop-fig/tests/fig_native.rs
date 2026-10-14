//! Native (Folio) mapping tests (lane SMB-INTEROP-FIG-NATIVE) on the three Operator-authored Figma
//! desktop 126.10.7 fixtures: structure, per-fixture loss receipts, and a package save + reopen
//! round trip with an edit. Editable import stays NOT_PROVEN beyond what the edit test shows.
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_folio::{
    Budget, NodeKind, Payload, Resolution, Resolver, StudioAppearanceRow, StudioLayer, StudioPaint,
    TileRef,
};
use hsk_studio_interop_fig::*;
use hsk_studio_package::{
    Extras, ImportEntry, ImportRole, Limits as PackageLimits, MemoryAssetSource, WriteOptions,
    read_package, write_package_with,
};
use std::collections::BTreeMap;

/// Host stand-in: accepts the component-instance layer binding, resolves nothing else.
struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

const FIXTURES: [&str; 3] = [
    "fig-basic-shapes.fig",
    "fig-components-autolayout.fig",
    "fig-prototype-pages.fig",
];

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

struct Mapped {
    bytes: Vec<u8>,
    import: FigImport,
    doc: FigDocument,
    proposal: NativeProposal,
}

fn mapped(name: &str) -> Mapped {
    let bytes = fixture(name);
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(
        &bytes,
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &cancel,
    )
    .unwrap() else {
        panic!("{name} imports")
    };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let cfg = NativeConfig {
        budget: Budget::default(),
        resolver: &Host,
    };
    let proposal = to_native(&doc, &cfg, &mut SequentialIdMint::new(0xF16), &cancel)
        .unwrap_or_else(|e| panic!("{name} maps: {e:?}"));
    Mapped {
        bytes,
        import: *import,
        doc,
        proposal,
    }
}

fn layer<'a>(p: &'a NativeProposal, name: &str) -> &'a StudioLayer {
    let found: Vec<_> = p
        .snapshot
        .document()
        .layers
        .iter()
        .filter(|l| l.name == name)
        .collect();
    assert_eq!(found.len(), 1, "layer {name}");
    found[0]
}

fn rows<'a>(p: &'a NativeProposal, layer_id: &str) -> &'a [StudioAppearanceRow] {
    let d = p
        .design
        .nodes
        .iter()
        .find(|d| d.node.node_id == layer_id)
        .expect("node design");
    let style = &d.style_refs["fill_style_id"];
    &p.design
        .paint_styles
        .iter()
        .find(|s| &s.style_id == style)
        .unwrap()
        .rows
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn real_native_mapping_structure() {
    let m = mapped("fig-basic-shapes.fig");
    let d = m.proposal.snapshot.document();
    // Two pages -> two root group layers (the internal one hidden); one artboard.
    let pages: Vec<_> = d
        .layers
        .iter()
        .filter(|l| matches!(l.parent, hsk_studio_folio::Parent::Root(())))
        .map(|l| (l.name.as_str(), l.visible))
        .collect();
    assert_eq!(
        pages,
        [("basic-shapes", true), ("Internal Only Canvas", false)]
    );
    assert_eq!(d.artboards.len(), 1);
    let a = &d.artboards[0];
    assert_eq!(a.name, "basic-shapes-artboard");
    assert_eq!(
        (
            a.bounds.x.value,
            a.bounds.y.value,
            a.bounds.width.value,
            a.bounds.height.value
        ),
        (100.0, 100.0, 900.0, 640.0)
    );
    // Rounded rectangle: vector layer, fill + stroke rows from the Figma paints.
    let r = layer(&m.proposal, "rounded-solid-shadow");
    assert_eq!((r.kind.as_str(), r.visible), ("vector", true));
    // Geometry slice: the fill outline is one path on the layer (fig_geometry pins the numbers).
    assert!(matches!(r.payload, Payload::Vector { ref path_ids } if path_ids.len() == 1));
    let rs = rows(&m.proposal, &r.layer_id);
    assert_eq!(rs.len(), 2);
    let StudioAppearanceRow::Fill {
        paint: StudioPaint::Solid { colour },
        opacity,
        ..
    } = &rs[0]
    else {
        panic!("fill row")
    };
    assert!(
        close(colour.r, 0.16)
            && close(colour.g, 0.38)
            && close(colour.b, 0.90)
            && close(*opacity, 100.0)
    );
    let StudioAppearanceRow::Stroke { stroke, .. } = &rs[1] else {
        panic!("stroke row")
    };
    assert_eq!(stroke.weight.value, 3.0);
    assert_eq!(stroke.align, hsk_studio_folio::StrokeAlign::Inside);
    // Gradient ellipse: linear gradient row, red -> yellow, node size folded into the transform.
    let e = layer(&m.proposal, "ellipse-gradient");
    let StudioAppearanceRow::Fill {
        paint: StudioPaint::Gradient { gradient },
        ..
    } = &rows(&m.proposal, &e.layer_id)[0]
    else {
        panic!("gradient row")
    };
    assert_eq!(gradient.stops.len(), 2);
    assert_eq!(gradient.transform, [150.0, 0.0, 0.0, 110.0, 0.0, 0.0]);
    // Appearance: star at 50 %, multiply blend (Studio discriminant 5), hidden internal page.
    let star = layer(&m.proposal, "star-half-opacity")
        .appearance
        .clone()
        .unwrap();
    assert_eq!((star.opacity, star.blend_mode), (50.0, 2));
    let mul = layer(&m.proposal, "multiply-overlay")
        .appearance
        .clone()
        .unwrap();
    assert_eq!(mul.blend_mode, 5);
    // Groups keep their children in Figma stacking order (ascending position = bottom to top).
    let g = layer(&m.proposal, "group-multiply");
    assert_eq!(g.kind, "group");
    let kids: Vec<_> = d
        .layers
        .iter()
        .filter(
            |l| matches!(&l.parent, hsk_studio_folio::Parent::Node(n) if n.node_id == g.layer_id),
        )
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(kids, ["group-background", "multiply-overlay"]);
    // Text: a text layer bound to an inline story, one character range per style run (bytes).
    let t = layer(&m.proposal, "mixed-style-text");
    let Payload::Text { story_id } = &t.payload else {
        panic!("text payload")
    };
    let story = m
        .proposal
        .stories
        .iter()
        .find(|s| &s.story_id == story_id)
        .unwrap();
    assert_eq!(
        story.content,
        hsk_studio_folio::StoryContent::Inline {
            text_utf8: "Regular text + bold text".into()
        }
    );
    let ranges: Vec<_> = story
        .character_ranges
        .iter()
        .map(|r| (r.start, r.end))
        .collect();
    assert_eq!(ranges, [(0, 15), (15, 24)]);
    assert_eq!(story.paragraphs.len(), 1);
    let StudioAppearanceRow::Fill {
        paint: StudioPaint::Solid { colour },
        ..
    } = &rows(&m.proposal, &t.layer_id)[0]
    else {
        panic!("text fill")
    };
    assert!(close(colour.r, 0.1) && close(colour.g, 0.15) && close(colour.b, 0.25));
    let comps = mapped("fig-components-autolayout.fig");
    assert_eq!(comps.proposal.stories.len(), 3, "three TEXT nodes");
    // Every mapped node keeps its Figma source id.
    let index = id_index(&m.proposal.id_map);
    let src = m.doc.named("rounded-solid-shadow").next().unwrap().id;
    assert_eq!(index[&src].node_id, r.layer_id);
    assert_eq!(m.proposal.editable, EditableStatus::NotProven);

    for name in FIXTURES {
        let m = mapped(name);
        assert!(!m.proposal.snapshot.document().layers.is_empty(), "{name}");
    }
}

/// (path, code) -> count of a loss list.
fn counts(list: &[NodeLoss]) -> BTreeMap<(String, &'static str), u32> {
    list.iter()
        .map(|l| ((l.path.clone(), l.code), l.count))
        .collect()
}

#[test]
fn real_native_losses_accounted() {
    for name in FIXTURES {
        let m = mapped(name);
        let c = counts(&m.proposal.mapping_loss);
        let get = |p: &str, code: &str| {
            c.iter()
                .filter(|((path, k), _)| path == p && *k == code)
                .map(|(_, n)| *n)
                .sum::<u32>()
        };
        // Every node except the DOCUMENT root is mapped: a document node (id map) or a design
        // record (record map: variable collections and variables).
        assert_eq!(
            m.proposal.id_map.len() + m.proposal.record_map.len(),
            m.doc.nodes.len() - 1,
            "{name}"
        );
        // Geometry slice: shape geometry is emitted as vector paths (pinned per fixture in
        // fig_geometry::real_geometry_losses_shrink_and_are_pinned); the old edge-missing code
        // is gone.
        assert_eq!(
            get("native.geometry", "vector_geometry_needs_nib_edge"),
            0,
            "{name}"
        );
        // Receipt entry carries the same mapping losses and the decode losses.
        let entries = m.proposal.import_entries(&m.import, &m.bytes).unwrap();
        let receipt: serde_json::Value = serde_json::from_slice(
            &entries
                .iter()
                .find(|e| e.role == ImportRole::Receipt)
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(
            receipt["mapping_loss"].as_array().unwrap().len(),
            m.proposal.mapping_loss.len()
        );
        assert_eq!(
            receipt["decode_loss"].as_array().unwrap().len(),
            m.doc.loss.len()
        );
        assert_eq!(receipt["editable"], "NOT_PROVEN");
        eprintln!(
            "loss receipt {name}: mapping={} entries/{} occurrences, decode={} entries",
            m.proposal.mapping_loss.len(),
            m.proposal.mapping_loss.iter().map(|l| l.count).sum::<u32>(),
            m.doc.loss.len()
        );
    }
    let shapes = mapped("fig-basic-shapes.fig");
    let c = counts(&shapes.proposal.mapping_loss);
    assert_eq!(
        c[&(
            "native.fills.IMAGE".to_owned(),
            "image_paint_not_a_folio_paint"
        )],
        1
    );
    // Slice 2: the drop shadow and the mask are records now (fig_geometry pins them); only the
    // blur (no Folio effect kind) stays a loss.
    assert!(!c.keys().any(|(p, _)| p == "native.effects.DROP_SHADOW"));
    assert!(!c.keys().any(|(p, _)| p == "native.mask"));
    assert_eq!(
        c[&(
            "native.effects.FOREGROUND_BLUR".to_owned(),
            "effect_kind_not_in_folio"
        )],
        1
    );
    assert_eq!(
        c[&(
            "native.boolean_operation".to_owned(),
            "boolean_operation_not_mapped"
        )],
        1
    );
}

fn package(
    p: &NativeProposal,
    snapshot: &hsk_studio_folio::Snapshot,
    entries: &[ImportEntry],
) -> Vec<u8> {
    let held: Vec<_> = entries.iter().map(ImportEntry::to_opaque).collect();
    let _ = p;
    let mut out = Vec::new();
    write_package_with(
        snapshot,
        &MemoryAssetSource::new(),
        &Extras {
            quarantined: &held,
            previews: &[],
        },
        &WriteOptions::default(),
        &PackageLimits::default(),
        &CancellationToken::default(),
        &mut out,
    )
    .unwrap();
    out
}

#[test]
fn real_native_components_instances_overrides() {
    let m = mapped("fig-components-autolayout.fig");
    let p = &m.proposal;
    let index = id_index(&p.id_map);
    let layer_of = |name: &str| index[&m.doc.named(name).next().unwrap().id].node_id.clone();
    // Component set: the state-group frame's layer, variants in member order, first is default.
    assert_eq!(p.design.component_sets.len(), 1);
    let set = &p.design.component_sets[0];
    assert_eq!(set.layer_id, layer_of("fixture-button"));
    assert_eq!(
        set.variant_group_properties,
        BTreeMap::from([(
            "state".to_owned(),
            vec!["default".to_owned(), "hover".to_owned()]
        )])
    );
    // Components: one per SYMBOL, on its group layer, with its variant assignment.
    assert_eq!(p.design.components.len(), 2);
    let component = |name: &str| {
        let layer = layer_of(name);
        p.design
            .components
            .iter()
            .find(|k| k.layer_id == layer)
            .unwrap()
            .clone()
    };
    let default = component("state=default");
    let hover = component("state=hover");
    assert_eq!(set.default_variant_id, default.component_id);
    assert_eq!(
        default.variant_properties,
        Some(BTreeMap::from([("state".to_owned(), "default".to_owned())]))
    );
    assert_eq!(
        hover.variant_properties,
        Some(BTreeMap::from([("state".to_owned(), "hover".to_owned())]))
    );
    // Instances: component_instance layers bound to SCIN records naming the right component.
    assert_eq!(p.design.component_instances.len(), 3);
    let instance = |name: &str| {
        let l = layer(p, name);
        assert_eq!(l.kind, "component_instance");
        let Payload::PrimitiveReference {
            schema_id,
            object_id,
        } = &l.payload
        else {
            panic!("instance payload")
        };
        assert_eq!(schema_id, SCHEMA_STUDIO_COMPONENT_INSTANCE);
        p.design
            .component_instances
            .iter()
            .find(|i| &i.instance_id == object_id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        instance("button-instance-1").main_component_id,
        Some(default.component_id.clone())
    );
    assert_eq!(
        instance("button-instance-2").main_component_id,
        Some(hover.component_id.clone())
    );
    let third = instance("button-instance-3");
    assert_eq!(third.main_component_id, Some(default.component_id.clone()));
    // The text override targets the definition's TEXT layer (source 4:10) with the new characters.
    let label = index[&SourceId {
        session: 4,
        local: 10,
    }]
        .node_id
        .clone();
    let ov = third
        .overrides
        .iter()
        .find(|o| o.overridden_node_id == label)
        .unwrap();
    assert_eq!(ov.overridden_fields, ["characters"]);
    assert_eq!(
        ov.values["characters"],
        hsk_studio_folio::StudioValue::String {
            value: "Changed text".into()
        }
    );
    // Size and name overrides have no Folio field: counted losses (instance 3 resizes; all three
    // carry a name override).
    let c = counts(&p.mapping_loss);
    assert_eq!(
        c[&(
            "native.instance.override_size".to_owned(),
            "override_size_no_folio_home"
        )],
        1
    );
    assert_eq!(
        c[&(
            "native.instance.override_name".to_owned(),
            "override_name_no_folio_home"
        )],
        3
    );
}

#[test]
fn real_native_auto_layout_reactions_variables() {
    let m = mapped("fig-components-autolayout.fig");
    let p = &m.proposal;
    let index = id_index(&p.id_map);
    let node_of = |name: &str| index[&m.doc.named(name).next().unwrap().id].node_id.clone();
    let design = |id: &str| {
        p.design
            .nodes
            .iter()
            .find(|d| d.node.node_id == id)
            .cloned()
    };
    // Auto layout on group layers (symbol, component set, nested frame) and on an artboard.
    use hsk_studio_folio::LayoutMode::{Horizontal, Vertical};
    for (name, mode, spacing) in [
        ("state=default", Horizontal, 8.0),
        ("fixture-button", Horizontal, 24.0),
        ("horizontal-stack", Horizontal, 18.0),
        ("component-instances-artboard", Vertical, 24.0),
    ] {
        let a = design(&node_of(name))
            .and_then(|d| d.auto_layout)
            .unwrap_or_else(|| panic!("{name} auto layout"));
        assert_eq!((a.layout_mode, a.item_spacing), (mode, spacing), "{name}");
    }
    // Instances carry stack fields too, but an instance layer is no container: typed loss.
    let c = counts(&p.mapping_loss);
    assert_eq!(
        c[&(
            "native.auto_layout".to_owned(),
            "auto_layout_on_non_container_no_folio_home"
        )],
        3
    );
    // Variables: one collection with the single Figma mode, three colour variables.
    assert_eq!(p.design.variable_collections.len(), 1);
    let k = &p.design.variable_collections[0];
    assert_eq!((k.name.as_str(), k.modes.len()), ("fixture-theme", 1));
    assert_eq!(
        (k.modes[0].mode_id.as_str(), k.modes[0].name.as_str()),
        ("4:0", "light")
    );
    assert_eq!(k.variable_ids.len(), 3);
    let fill = p
        .design
        .variables
        .iter()
        .find(|v| v.name == "button-fill")
        .unwrap();
    assert_eq!(fill.scopes, ["FRAME_FILL"]);
    let hsk_studio_folio::StudioValue::Color { value } = &fill.values_by_mode["4:0"] else {
        panic!("colour value")
    };
    assert_eq!(
        (value.r, value.g, value.b, value.a),
        (0.15, 0.35, 0.85, 1.0)
    );
    assert_eq!(p.record_map.len(), 4);
    assert!(
        c[&(
            "native.paint.color_variable".to_owned(),
            "paint_variable_binding_no_folio_home"
        )] >= 2
    );

    // Reactions: click -> navigate (instant), click -> smart animate 350 ms, click -> back.
    let m = mapped("fig-prototype-pages.fig");
    let p = &m.proposal;
    let index = id_index(&p.id_map);
    let node_of = |name: &str| index[&m.doc.named(name).next().unwrap().id].node_id.clone();
    let reaction = |name: &str| {
        let id = node_of(name);
        let d = p
            .design
            .nodes
            .iter()
            .find(|d| d.node.node_id == id)
            .unwrap();
        assert_eq!(d.reactions.len(), 1, "{name}");
        d.reactions[0].clone()
    };
    let r = reaction("Click: navigate to details");
    assert_eq!(r.trigger, Some(hsk_studio_folio::StudioTrigger::OnClick {}));
    let hsk_studio_folio::StudioAction::Node {
        destination_id,
        navigation,
        transition,
        ..
    } = &r.actions[0]
    else {
        panic!("node action")
    };
    assert_eq!(
        destination_id.as_deref(),
        Some(node_of("detail-screen").as_str())
    );
    assert_eq!(*navigation, hsk_studio_folio::Navigation::Navigate);
    assert!(transition.is_none(), "instant transition");
    let r = reaction("Click: smart animate");
    let hsk_studio_folio::StudioAction::Node {
        destination_id,
        transition: Some(t),
        ..
    } = &r.actions[0]
    else {
        panic!("animated action")
    };
    assert_eq!(
        destination_id.as_deref(),
        Some(node_of("animated-screen").as_str())
    );
    assert_eq!(
        t.transition_type,
        hsk_studio_folio::TransitionType::SmartAnimate
    );
    assert_eq!(t.duration, 350);
    let r = reaction("Click: back to start");
    let hsk_studio_folio::StudioAction::Node { destination_id, .. } = &r.actions[0] else {
        panic!("node action")
    };
    assert_eq!(
        destination_id.as_deref(),
        Some(node_of("start-screen").as_str())
    );
    let c = counts(&p.mapping_loss);
    assert_eq!(
        c[&(
            "native.interactions.easing=INOUT_CUBIC".to_owned(),
            "easing_curve_unverified_linear_used"
        )],
        1
    );
}

#[test]
fn real_native_package_round_trip() {
    let cancel = CancellationToken::default();
    for name in FIXTURES {
        let m = mapped(name);
        let entries = m.proposal.import_entries(&m.import, &m.bytes).unwrap();
        let bytes = package(&m.proposal, &m.proposal.snapshot, &entries);
        let opened = read_package(
            &bytes,
            &PackageLimits::default(),
            Budget::default(),
            &Host,
            &cancel,
        )
        .unwrap();
        let snap = opened.snapshot().expect("editable folio document");
        // Same layers, artboards and ids after save + reopen.
        assert_eq!(snap.document(), m.proposal.snapshot.document(), "{name}");
        // Records entry: same design records and the same source-id map.
        let records =
            read_native_records(opened.import_bytes(ImportRole::Records).unwrap()).unwrap();
        assert_eq!(records.design, m.proposal.design, "{name}");
        assert_eq!(records.id_map, m.proposal.id_map, "{name}");
        assert_eq!(records.stories, m.proposal.stories, "{name}");
        assert_eq!(records.paths, m.proposal.paths, "{name}: vector geometry");
        assert_eq!(records.raster_records, m.proposal.raster_records, "{name}");
        assert_eq!(records.vector_records, m.proposal.vector_records, "{name}");
        assert_eq!(records.image_paints, m.proposal.image_paints, "{name}");
        assert_eq!(records.record_map, m.proposal.record_map, "{name}");
        let texts = m.doc.nodes.iter().filter(|n| n.type_name == "TEXT").count();
        assert_eq!(
            records.stories.len(),
            texts,
            "{name}: one story per TEXT node"
        );
        for e in &records.id_map {
            assert!(
                m.doc.node(e.source).is_some(),
                "{name}: source {} exists",
                e.source
            );
            let present = match e.node.node_kind {
                NodeKind::Layer => snap
                    .document()
                    .layers
                    .iter()
                    .any(|l| l.layer_id == e.node.node_id),
                NodeKind::Artboard => snap
                    .document()
                    .artboards
                    .iter()
                    .any(|a| a.artboard_id == e.node.node_id),
                NodeKind::PageSpread => false,
            };
            assert!(
                present,
                "{name}: {} in the reopened document",
                e.node.node_id
            );
        }
        // The original .fig rides along byte-identical.
        assert_eq!(
            opened.import_bytes(ImportRole::Original).unwrap(),
            &m.bytes[..]
        );
    }

    // Edit + reopen: a rename of a mapped layer survives save and reopen with the same id and the
    // same source id.
    let m = mapped("fig-basic-shapes.fig");
    let target = layer(&m.proposal, "star-half-opacity").layer_id.clone();
    let renamed = m
        .proposal
        .snapshot
        .rename(
            &target,
            "star-renamed".into(),
            1,
            2,
            Budget::default(),
            &cancel,
            &Host,
        )
        .unwrap();
    let entries = m.proposal.import_entries(&m.import, &m.bytes).unwrap();
    let bytes = package(&m.proposal, &renamed, &entries);
    let opened = read_package(
        &bytes,
        &PackageLimits::default(),
        Budget::default(),
        &Host,
        &cancel,
    )
    .unwrap();
    let doc = opened.snapshot().unwrap().document();
    assert_eq!(doc.revision, 2);
    let l = doc.layers.iter().find(|l| l.layer_id == target).unwrap();
    assert_eq!(l.name, "star-renamed");
    let records = read_native_records(opened.import_bytes(ImportRole::Records).unwrap()).unwrap();
    let src = m.doc.named("star-half-opacity").next().unwrap().id;
    assert_eq!(id_index(&records.id_map)[&src].node_id, target);
}
