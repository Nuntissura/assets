//! Components-local real consumer: a Button set (`small` 100 pt / `large` 200 pt wide) as Folio
//! design records over a Folio document, a brand colour variable with `light`/`dark` modes and a
//! Label `characters` field. `--size`, `--mode` and `--text` pick the variant, the mode and a local
//! text override; the resolved fields and the write/read footprint of the text edit print on stdout.
//! The record set is validated by Folio (`Design::validate`) and so is the proposed successor
//! instance. A stable error code goes to stderr (exit 2). `--descriptor` prints the crate descriptor;
//! `--cancel` cancels before the call.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_components::{
    ComponentsError, DESCRIPTOR, Design, Limits, LocalContent, NodeLocal, OverrideEdit,
    propose_overrides, report,
};
use hsk_studio_folio as folio;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, SinkPort};
use std::{collections::BTreeMap, env};

struct CollectSink(Vec<Vec<u8>>);
impl SinkPort for CollectSink {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn publishing(key: &str) -> folio::StudioPublishing {
    folio::StudioPublishing {
        description: String::new(),
        description_markdown: String::new(),
        documentation_links: vec![],
        publish_key: key.to_owned(),
        is_remote: false,
    }
}
fn group(layer_id: String, name: &str, parent: folio::Parent) -> folio::StudioLayer {
    folio::StudioLayer {
        schema_id: folio::LayerSchema::V1,
        layer_id,
        name: name.to_owned(),
        parent,
        kind: "group".into(),
        payload: folio::Payload::Group,
        visible: true,
        appearance: None,
        text_frame: None,
    }
}
fn layer_parent(layer_id: &str) -> folio::Parent {
    folio::Parent::Node(folio::NodeRef {
        node_kind: folio::NodeKind::Layer,
        node_id: layer_id.to_owned(),
    })
}
fn property(
    name: &str,
    ty: folio::ComponentPropertyType,
    default: folio::StudioPropertyValue,
) -> folio::StudioComponentProperty {
    folio::StudioComponentProperty {
        property_name: name.to_owned(),
        property_type: ty,
        default_value: default,
        options: None,
    }
}

fn run(args: &[String], cancel: &CancellationToken) -> Result<String, ComponentsError> {
    let size = arg(args, "--size").unwrap_or("large");
    let mode = arg(args, "--mode").unwrap_or("dark");
    let text = arg(args, "--text").unwrap_or("Buy now");

    // The Folio document: one artboard, a set layer, one container layer per variant, and the
    // definition layers (Button > Label) under each container.
    let board = folio::Parent::Node(folio::NodeRef {
        node_kind: folio::NodeKind::Artboard,
        node_id: id("SART", 1),
    });
    let set_layer = id("SLYR", 100);
    let mut layers = vec![group(set_layer.clone(), "Button", board)];
    let mut local = LocalContent::default();
    let mut components = Vec::new();
    // (variant, container layer, button layer, label layer, width)
    for (variant, container, root, label, width) in
        [("small", 111, 1, 2, 100.0), ("large", 113, 3, 4, 200.0)]
    {
        let container = id("SLYR", container);
        layers.push(group(container.clone(), variant, layer_parent(&set_layer)));
        layers.push(group(id("SLYR", root), "Button", layer_parent(&container)));
        layers.push(group(
            id("SLYR", label),
            "Label",
            layer_parent(&id("SLYR", root)),
        ));
        local.nodes.insert(
            id("SLYR", root),
            NodeLocal {
                fields: BTreeMap::from([(
                    "width".to_owned(),
                    folio::StudioValue::Float { value: width },
                )]),
                variable_bindings: BTreeMap::from([("fills".to_owned(), id("SVAR", 1))]),
                rect: None,
            },
        );
        local.nodes.insert(
            id("SLYR", label),
            NodeLocal {
                fields: BTreeMap::from([(
                    "characters".to_owned(),
                    folio::StudioValue::String {
                        value: "Label".into(),
                    },
                )]),
                ..NodeLocal::default()
            },
        );
        components.push(folio::StudioComponent {
            schema_id: folio::ComponentSchema::V1,
            component_id: id("SCMP", root + 10),
            layer_id: container,
            publishing: publishing(&format!("component-{variant}")),
            variant_properties: Some(BTreeMap::from([("size".to_owned(), variant.to_owned())])),
            component_property_definitions: vec![],
        });
    }
    let len = |v: f64| folio::Length {
        value: v,
        unit: folio::GeometryUnit::Pt,
    };
    let unassigned = || folio::ProfileBinding::Unassigned(());
    let document = folio::StudioDocument {
        schema_id: folio::DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: folio::GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![folio::StudioArtboard {
            schema_id: folio::ArtboardSchema::V1,
            artboard_id: id("SART", 1),
            name: "Frame".into(),
            parent: folio::Parent::Root(()),
            bounds: folio::Bounds {
                x: len(0.0),
                y: len(0.0),
                width: len(400.0),
                height: len(300.0),
            },
        }],
        page_spreads: vec![],
        layers,
        graph: folio::StudioLayerGraph {
            schema_id: folio::GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    };

    // The Folio design records: the variant set (with its VARIANT property), the components, and
    // the brand colour variable with `light`/`dark` modes.
    let colour = |r: f64, g: f64, b: f64| folio::StudioValue::Color {
        value: folio::StudioRgba {
            r,
            g,
            b,
            a: 1.0,
            colour_profile_id: unassigned(),
        },
    };
    let mut records = folio::StudioDesignRecords::new(id("SDOC", 1));
    records.component_sets.push(folio::StudioComponentSet {
        schema_id: folio::ComponentSchema::V1,
        component_id: id("SCMP", 1),
        layer_id: set_layer,
        publishing: publishing("set-button"),
        default_variant_id: id("SCMP", 11),
        variant_group_properties: BTreeMap::from([(
            "size".to_owned(),
            vec!["small".to_owned(), "large".to_owned()],
        )]),
        component_property_definitions: vec![property(
            "size",
            folio::ComponentPropertyType::Variant,
            folio::StudioPropertyValue::Text("small".into()),
        )],
    });
    records.components = components;
    records
        .variable_collections
        .push(folio::StudioVariableCollection {
            schema_id: folio::VariableCollectionSchema::V1,
            collection_id: id("SVCL", 1),
            name: "Theme/Brand".into(),
            hidden_from_publishing: false,
            publishing: publishing("collection-brand"),
            is_extension: false,
            modes: ["light", "dark"]
                .map(|m| folio::StudioMode {
                    mode_id: m.into(),
                    name: m.into(),
                })
                .to_vec(),
            default_mode_id: "light".into(),
            variable_ids: vec![id("SVAR", 1)],
        });
    records.variables.push(folio::StudioVariable {
        schema_id: folio::VariableSchema::V1,
        variable_id: id("SVAR", 1),
        name: "Brand/Primary".into(),
        hidden_from_publishing: false,
        publishing: publishing("variable-primary"),
        variable_collection_id: id("SVCL", 1),
        resolved_type: folio::VariableType::Color,
        values_by_mode: BTreeMap::from([
            ("light".to_owned(), colour(0.0, 0.0, 1.0)),
            ("dark".to_owned(), colour(0.5, 0.5, 1.0)),
        ]),
        scopes: vec![],
        code_syntax: BTreeMap::new(),
    });

    let design = Design::new(&document, &records, &local);
    let limits = Limits::default();
    design.validate(&limits, cancel, &folio::Unresolved)?;

    // An instance names a component (the default member); its VARIANT property selects the member.
    let label = if size == "small" { 2 } else { 4 };
    let inst = folio::StudioComponentInstance {
        schema_id: folio::ComponentInstanceSchema::V1,
        instance_id: id("SCIN", 1),
        main_component_id: Some(id("SCMP", 11)),
        component_properties: BTreeMap::from([(
            "size".to_owned(),
            folio::StudioInstanceProperty {
                property_type: folio::ComponentPropertyType::Variant,
                value: folio::StudioPropertyValue::Text(size.to_owned()),
                bound_variable_id: None,
            },
        )]),
        overrides: vec![],
        scale_factor: 1.0,
        is_exposed_instance: false,
    };
    let modes = BTreeMap::from([(id("SVCL", 1), mode.to_owned())]);
    let edit = OverrideEdit {
        node_id: id("SLYR", label),
        field: "characters".into(),
        value: folio::StudioValue::String {
            value: text.to_owned(),
        },
    };
    let p = propose_overrides(&inst, &[edit], &design, &modes, 1, &limits, cancel)?;
    let mut out = format!("component={}\n", p.resolution.component_id);
    for f in &p.resolution.fields {
        out += &format!(
            "{} {} = {:?} <- {:?}\n",
            f.node_id, f.field, f.value, f.source
        );
    }
    out += &format!(
        "write_footprint={:?}\nread_footprint={:?}",
        p.write_footprint, p.read_footprint
    );
    Ok(out)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return;
    }
    let cancel = CancellationToken::default();
    if args.iter().any(|a| a == "--cancel") {
        cancel.cancel();
    }
    let result = run(&args, &cancel);
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .expect("static context");
    let resource = DomainId::parse(&id("SDOC", 1)).expect("static id");
    let mut observe = Observe::new(1, 1, resource, actor, Budget::new(0, 0).expect("budget"));
    let mut sink = CollectSink(Vec::new());
    let receipt = report(&result, 1, 1, &cancel, &mut observe, &mut sink);
    match (&result, receipt) {
        (Ok(text), Ok(r)) => {
            println!("{text}");
            eprintln!("outcome_code={} frames={}", r.outcome.wire(), sink.0.len());
        }
        (Err(e), _) => {
            eprintln!(
                "rejected:{}; consult --descriptor and correct only the caller input; no host proof",
                e.code()
            );
            std::process::exit(2)
        }
        (Ok(_), Err(e)) => {
            eprintln!("rejected:{}", e.code());
            std::process::exit(2)
        }
    }
}
