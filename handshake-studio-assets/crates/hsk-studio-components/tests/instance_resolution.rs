//! Hand-computed oracles for components, variables, constraints and the prototype graph, over Folio
//! design records (`StudioDesignRecords`) and a Folio document. Ids are
//! `PREFIX-0198a000-0000-7000-8000-<n as 12 hex>`; numbers below are the `n`.
use folio::{
    ComponentPropertyType as PropType, ConstraintType, NodeKind, NodeRef, Parent, ProfileBinding,
    StudioAction, StudioPropertyValue, StudioReaction, StudioTrigger, StudioValue, VariableType,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    ComponentsError as E, Design, FlowLimits, Limits, LocalContent, NodeLocal, OverrideEdit,
    PrototypeFlow, RectPt, SizePt, Source, VariableTable, apply_constraints, check_footprint,
    instance_layer, propose_overrides, resize_component, resolve_instance, resolve_variable,
    swap_component, switch_variant, validate_flow,
};
use hsk_studio_folio as folio;
use std::collections::BTreeMap;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

// ---- values and records --------------------------------------------------------------------------

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-0198a000-0000-7000-8000-{n:012x}")
}
fn s(v: &str) -> StudioValue {
    StudioValue::String { value: v.into() }
}
fn float(v: f64) -> StudioValue {
    StudioValue::Float { value: v }
}
fn flag(v: bool) -> StudioValue {
    StudioValue::Boolean { value: v }
}
fn color(r: f64, g: f64, b: f64, a: f64) -> StudioValue {
    StudioValue::Color {
        value: folio::StudioRgba {
            r,
            g,
            b,
            a,
            colour_profile_id: ProfileBinding::Unassigned(()),
        },
    }
}
fn alias_to(n: u32, resolved_type: VariableType) -> StudioValue {
    StudioValue::VariableAlias {
        id: id("SVAR", n),
        resolved_type,
    }
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
fn modes(pairs: &[(u32, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(c, m)| (id("SVCL", *c), (*m).to_owned()))
        .collect()
}
fn no_modes() -> BTreeMap<String, String> {
    BTreeMap::new()
}
fn prop_def(
    name: &str,
    ty: PropType,
    default: StudioPropertyValue,
) -> folio::StudioComponentProperty {
    folio::StudioComponentProperty {
        property_name: name.to_owned(),
        property_type: ty,
        default_value: default,
        options: None,
    }
}
fn instance(n: u32, main: &str) -> folio::StudioComponentInstance {
    folio::StudioComponentInstance {
        schema_id: folio::ComponentInstanceSchema::V1,
        instance_id: id("SCIN", n),
        main_component_id: Some(main.to_owned()),
        component_properties: BTreeMap::new(),
        overrides: Vec::new(),
        scale_factor: 1.0,
        is_exposed_instance: false,
    }
}
fn instance_prop(ty: PropType, value: StudioPropertyValue) -> folio::StudioInstanceProperty {
    folio::StudioInstanceProperty {
        property_type: ty,
        value,
        bound_variable_id: None,
    }
}
fn variant_prop(v: &str) -> folio::StudioInstanceProperty {
    instance_prop(PropType::Variant, StudioPropertyValue::Text(v.to_owned()))
}
fn record(node_id: u32, field: &str, v: StudioValue) -> folio::StudioOverrideRecord {
    folio::StudioOverrideRecord {
        overridden_node_id: id("SLYR", node_id),
        overridden_fields: vec![field.to_owned()],
        values: BTreeMap::from([(field.to_owned(), v)]),
    }
}
fn cons(horizontal: ConstraintType, vertical: ConstraintType) -> folio::StudioConstraint {
    folio::StudioConstraint {
        schema_id: folio::ConstraintSchema::V1,
        horizontal,
        vertical,
    }
}
fn variable(
    n: u32,
    coll: u32,
    ty: VariableType,
    entries: &[(&str, StudioValue)],
) -> folio::StudioVariable {
    folio::StudioVariable {
        schema_id: folio::VariableSchema::V1,
        variable_id: id("SVAR", n),
        name: format!("Brand/Variable {n}"),
        hidden_from_publishing: false,
        publishing: publishing(&format!("variable-{n}")),
        variable_collection_id: id("SVCL", coll),
        resolved_type: ty,
        values_by_mode: entries
            .iter()
            .map(|(m, v)| ((*m).to_owned(), v.clone()))
            .collect(),
        scopes: vec![],
        code_syntax: BTreeMap::new(),
    }
}
fn color_var(n: u32, coll: u32, entries: &[(&str, [f64; 4])]) -> folio::StudioVariable {
    let entries: Vec<(&str, StudioValue)> = entries
        .iter()
        .map(|(m, c)| (*m, color(c[0], c[1], c[2], c[3])))
        .collect();
    variable(n, coll, VariableType::Color, &entries)
}
fn collection(n: u32, mode_ids: &[&str], variables: &[u32]) -> folio::StudioVariableCollection {
    folio::StudioVariableCollection {
        schema_id: folio::VariableCollectionSchema::V1,
        collection_id: id("SVCL", n),
        name: format!("Theme/Collection {n}"),
        hidden_from_publishing: false,
        publishing: publishing(&format!("collection-{n}")),
        is_extension: false,
        modes: mode_ids
            .iter()
            .map(|m| folio::StudioMode {
                mode_id: (*m).to_owned(),
                name: (*m).to_owned(),
            })
            .collect(),
        default_mode_id: mode_ids[0].to_owned(),
        variable_ids: variables.iter().map(|v| id("SVAR", *v)).collect(),
    }
}

// ---- a Folio document and record set assembled from compact descriptions -------------------------

/// One definition node: a Folio group layer under the component's container layer, plus its design
/// entry (property references, constraint) and its crate-local content (literals, bindings, rect).
struct TNode {
    n: u32,
    name: String,
    parent: Option<u32>,
    fields: BTreeMap<String, StudioValue>,
    refs: BTreeMap<String, String>,
    bindings: BTreeMap<String, String>,
    rect: Option<RectPt>,
    constraint: Option<folio::StudioConstraint>,
}
fn node(n: u32, name: &str, parent: Option<u32>) -> TNode {
    TNode {
        n,
        name: name.to_owned(),
        parent,
        fields: BTreeMap::new(),
        refs: BTreeMap::new(),
        bindings: BTreeMap::new(),
        rect: None,
        constraint: None,
    }
}
struct TComp {
    comp: u32,
    variant: Option<BTreeMap<String, String>>,
    props: Vec<folio::StudioComponentProperty>,
    nodes: Vec<TNode>,
    size: Option<SizePt>,
}
fn plain(comp: u32, nodes: Vec<TNode>) -> TComp {
    TComp {
        comp,
        variant: None,
        props: vec![],
        nodes,
        size: None,
    }
}
fn member(comp: u32, size: &str, nodes: Vec<TNode>) -> TComp {
    TComp {
        variant: Some(BTreeMap::from([("size".to_owned(), size.to_owned())])),
        ..plain(comp, nodes)
    }
}

fn on_board() -> Parent {
    Parent::Node(NodeRef {
        node_kind: NodeKind::Artboard,
        node_id: id("SART", 1),
    })
}
fn under(layer_id: &str) -> Parent {
    Parent::Node(NodeRef {
        node_kind: NodeKind::Layer,
        node_id: layer_id.to_owned(),
    })
}
fn layer_ref(n: u32) -> NodeRef {
    NodeRef {
        node_kind: NodeKind::Layer,
        node_id: id("SLYR", n),
    }
}
fn board_ref(n: u32) -> NodeRef {
    NodeRef {
        node_kind: NodeKind::Artboard,
        node_id: id("SART", n),
    }
}
fn group(layer_id: String, name: &str, parent: Parent) -> folio::StudioLayer {
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
fn document(boards: u32, layers: Vec<folio::StudioLayer>) -> folio::StudioDocument {
    let len = |v: f64| folio::Length {
        value: v,
        unit: folio::GeometryUnit::Pt,
    };
    folio::StudioDocument {
        schema_id: folio::DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: folio::GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: (1..=boards)
            .map(|n| folio::StudioArtboard {
                schema_id: folio::ArtboardSchema::V1,
                artboard_id: id("SART", n),
                name: format!("Frame {n}"),
                parent: Parent::Root(()),
                bounds: folio::Bounds {
                    x: len(0.0),
                    y: len(0.0),
                    width: len(100.0),
                    height: len(100.0),
                },
            })
            .collect(),
        page_spreads: vec![],
        layers,
        graph: folio::StudioLayerGraph {
            schema_id: folio::GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}
fn design_entry(node: NodeRef) -> folio::StudioNodeDesign {
    folio::StudioNodeDesign {
        node,
        auto_layout: None,
        child_layout: None,
        size_clamps: None,
        constraints: None,
        reactions: vec![],
        component_property_references: BTreeMap::new(),
        explicit_variable_modes: BTreeMap::new(),
        style_refs: BTreeMap::new(),
    }
}

struct World {
    layers: Vec<folio::StudioLayer>,
    records: folio::StudioDesignRecords,
    local: LocalContent,
    next_layer: u32,
}
struct Fixture {
    doc: folio::StudioDocument,
    records: folio::StudioDesignRecords,
    local: LocalContent,
}
impl Fixture {
    fn design(&self) -> Design<'_> {
        Design::new(&self.doc, &self.records, &self.local)
    }
}
impl World {
    fn new() -> Self {
        Self {
            layers: vec![],
            records: folio::StudioDesignRecords::new(id("SDOC", 1)),
            local: LocalContent::default(),
            next_layer: 1000,
        }
    }
    fn entry(&mut self, node: NodeRef) -> &mut folio::StudioNodeDesign {
        let at = match self.records.nodes.iter().position(|d| d.node == node) {
            Some(i) => i,
            None => {
                self.records.nodes.push(design_entry(node));
                self.records.nodes.len() - 1
            }
        };
        &mut self.records.nodes[at]
    }
    fn container(&mut self, name: &str, parent: Parent) -> String {
        self.next_layer += 1;
        let layer_id = id("SLYR", self.next_layer);
        self.layers.push(group(layer_id.clone(), name, parent));
        layer_id
    }
    /// A component: a container group layer under `parent` holding the definition's node layers.
    fn add_component(&mut self, c: TComp, parent: Parent) {
        let layer_id = self.container(&format!("Component {}", c.comp), parent);
        if let Some(size) = c.size {
            let rect = RectPt {
                x: 0.0,
                y: 0.0,
                w: size.w,
                h: size.h,
            };
            self.local.nodes.entry(layer_id.clone()).or_default().rect = Some(rect);
        }
        for t in c.nodes {
            let node_id = id("SLYR", t.n);
            let parent_id = t.parent.map_or_else(|| layer_id.clone(), |p| id("SLYR", p));
            self.layers
                .push(group(node_id.clone(), &t.name, under(&parent_id)));
            if !t.fields.is_empty() || !t.bindings.is_empty() || t.rect.is_some() {
                self.local.nodes.insert(
                    node_id.clone(),
                    NodeLocal {
                        fields: t.fields,
                        variable_bindings: t.bindings,
                        rect: t.rect,
                    },
                );
            }
            if !t.refs.is_empty() || t.constraint.is_some() {
                let entry = self.entry(NodeRef {
                    node_kind: NodeKind::Layer,
                    node_id,
                });
                entry.component_property_references = t.refs;
                entry.constraints = t.constraint;
            }
        }
        self.records.components.push(folio::StudioComponent {
            schema_id: folio::ComponentSchema::V1,
            component_id: id("SCMP", c.comp),
            layer_id,
            publishing: publishing(&format!("component-{}", c.comp)),
            variant_properties: c.variant,
            component_property_definitions: c.props,
        });
    }
    /// A variant set: a container group layer on the artboard whose child layers are the members.
    fn add_set(
        &mut self,
        set: u32,
        default: u32,
        groups: &[(&str, &[&str])],
        props: Vec<folio::StudioComponentProperty>,
        members: Vec<TComp>,
    ) {
        let layer_id = self.container(&format!("Set {set}"), on_board());
        for m in members {
            self.add_component(m, under(&layer_id));
        }
        self.records.component_sets.push(folio::StudioComponentSet {
            schema_id: folio::ComponentSchema::V1,
            component_id: id("SCMP", set),
            layer_id,
            publishing: publishing(&format!("set-{set}")),
            default_variant_id: id("SCMP", default),
            variant_group_properties: groups
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.iter().map(|x| (*x).to_owned()).collect()))
                .collect(),
            component_property_definitions: props,
        });
    }
    fn variables(
        &mut self,
        collection: folio::StudioVariableCollection,
        vars: Vec<folio::StudioVariable>,
    ) {
        self.records.variable_collections.push(collection);
        self.records.variables.extend(vars);
    }
    fn build(self) -> Fixture {
        Fixture {
            doc: document(1, self.layers),
            records: self.records,
            local: self.local,
        }
    }
    /// Builds the fixture and proves it is valid Folio data: the document, the record set, and the
    /// crate's `Design::validate` (Folio, then the crate-local rules) with no unresolved issues.
    fn checked(self) -> Fixture {
        let fx = self.build();
        let token = CancellationToken::default();
        let budget = folio::Budget::default();
        folio::validate_document(fx.doc.clone(), budget, &token, &folio::Unresolved).unwrap();
        folio::validate_design_records(&fx.records, &fx.doc, budget, &token, &folio::Unresolved)
            .unwrap();
        assert_eq!(
            fx.design()
                .validate(&Limits::default(), &token, &folio::Unresolved),
            Ok(vec![])
        );
        fx
    }
}

/// Set SCMP-10 `size = small | large` (default small = SCMP-11, large = SCMP-12). Small nodes:
/// Button 1 (width 100, fills = variable 1) > Label 2. Large nodes: Button 3 (width 200, same fills
/// binding) > Label 4, Badge 5. Label `visible` is driven by the Boolean property `showIcon`
/// (default true); label `characters` is the literal "Label". Variable 1 is a Color with modes
/// light = (0,0,1,1) and dark = (0.5,0.5,1,1).
fn fixture_world() -> World {
    let mut w = World::new();
    w.variables(
        collection(1, &["light", "dark"], &[1]),
        vec![color_var(
            1,
            1,
            &[
                ("light", [0.0, 0.0, 1.0, 1.0]),
                ("dark", [0.5, 0.5, 1.0, 1.0]),
            ],
        )],
    );
    let button = |root: u32, label: u32, width: f64| {
        let mut r = node(root, "Button", None);
        r.fields.insert("width".into(), float(width));
        r.bindings.insert("fills".into(), id("SVAR", 1));
        let mut l = node(label, "Label", Some(root));
        l.fields.insert("characters".into(), s("Label"));
        l.refs.insert("visible".into(), "showIcon".into());
        vec![r, l]
    };
    let mut large_nodes = button(3, 4, 200.0);
    large_nodes.push(node(5, "Badge", Some(3)));
    w.add_set(
        10,
        11,
        &[("size", &["small", "large"])],
        vec![
            prop_def(
                "showIcon",
                PropType::Boolean,
                StudioPropertyValue::Boolean(true),
            ),
            prop_def(
                "size",
                PropType::Variant,
                StudioPropertyValue::Text("small".into()),
            ),
        ],
        vec![
            member(11, "small", button(1, 2, 100.0)),
            member(12, "large", large_nodes),
        ],
    );
    w
}
fn fixture() -> Fixture {
    fixture_world().checked()
}

#[test]
fn variant_mode_override_resolution() {
    bounded_case("variant_mode_override_resolution", || {
        let fx = fixture();
        let design = fx.design();
        let (records0, local0) = (fx.records.clone(), fx.local.clone());
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let mut inst = instance(1, &id("SCMP", 10));
        inst.component_properties
            .insert("size".into(), variant_prop("large"));
        inst.overrides.push(record(4, "characters", s("Buy now")));
        inst.overrides.push(record(5, "opacity", float(0.5)));

        let r =
            resolve_instance(&inst, &design, &modes(&[(1, "dark")]), 7, &limits, &cancel).unwrap();
        assert_eq!(
            (r.base_revision, r.component_id.as_str()),
            (7, id("SCMP", 12).as_str())
        );
        // Node order, then field-name order: 3 {fills, width}, 4 {characters, visible}, 5 {opacity}.
        assert_eq!(r.fields.len(), 5);
        let f = |n: u32, name: &str| r.field(&id("SLYR", n), name).unwrap();
        assert_eq!(f(3, "width").value, float(200.0));
        assert_eq!(f(3, "width").source, Source::Definition);
        assert_eq!(f(3, "width").reads, vec![(id("SCMP", 12), String::new())]);
        assert_eq!(f(3, "fills").value, color(0.5, 0.5, 1.0, 1.0));
        assert_eq!(
            f(3, "fills").source,
            Source::Variable {
                variable_id: id("SVAR", 1),
                collection_id: id("SVCL", 1),
                mode_id: "dark".into()
            }
        );
        assert_eq!(f(4, "characters").value, s("Buy now"));
        assert_eq!(f(4, "characters").source, Source::Override);
        assert_eq!(f(4, "visible").value, flag(true));
        assert_eq!(
            f(4, "visible").source,
            Source::PropertyDefault("showIcon".into())
        );
        assert_eq!(f(5, "opacity").value, float(0.5));

        // Default mode, and the instance's own property binding beats the property default.
        inst.component_properties.insert(
            "showIcon".into(),
            instance_prop(PropType::Boolean, StudioPropertyValue::Boolean(false)),
        );
        let r = resolve_instance(&inst, &design, &no_modes(), 8, &limits, &cancel).unwrap();
        let (fills, visible) = (
            r.field(&id("SLYR", 3), "fills").unwrap(),
            r.field(&id("SLYR", 4), "visible").unwrap(),
        );
        assert_eq!(fills.value, color(0.0, 0.0, 1.0, 1.0));
        assert_eq!(visible.value, flag(false));
        assert_eq!(visible.source, Source::ComponentProperty("showIcon".into()));
        assert!(
            visible
                .reads
                .contains(&(id("SCIN", 1), "property/showIcon".into()))
        );

        // Instantiating the set without a variant selection gives the default member (small).
        let r = resolve_instance(
            &instance(2, &id("SCMP", 10)),
            &design,
            &no_modes(),
            1,
            &limits,
            &cancel,
        )
        .unwrap();
        assert_eq!(r.component_id, id("SCMP", 11));
        assert_eq!(
            r.field(&id("SLYR", 1), "width").unwrap().value,
            float(100.0)
        );

        // Variant switch: Label 4 -> Label 2 by (name, position) so its override is carried; Badge 5
        // has no counterpart in `small`, so its override is dropped and reported.
        let before = inst.clone();
        let (next, dropped) = switch_variant(&inst, &design, "size", "small").unwrap();
        assert_eq!(dropped.len(), 1);
        assert_eq!(
            (dropped[0].node_id.as_str(), dropped[0].field.as_str()),
            (id("SLYR", 5).as_str(), "opacity")
        );
        assert_eq!(next.overrides, vec![record(2, "characters", s("Buy now"))]);
        let r = resolve_instance(&next, &design, &no_modes(), 9, &limits, &cancel).unwrap();
        assert_eq!(r.component_id, id("SCMP", 11));
        assert_eq!(
            r.field(&id("SLYR", 2), "characters").unwrap().source,
            Source::Override
        );

        // Nothing the caller owns was mutated.
        assert_eq!(inst, before);
        assert_eq!((&fx.records, &fx.local), (&records0, &local0));
    });
}

/// Records holding one collection with mode `m1` and exactly these variables.
fn variable_records(vars: Vec<folio::StudioVariable>) -> folio::StudioDesignRecords {
    let ids: Vec<u32> = (1..=6)
        .filter(|n| vars.iter().any(|v| v.variable_id == id("SVAR", *n)))
        .collect();
    let mut records = folio::StudioDesignRecords::new(id("SDOC", 1));
    records
        .variable_collections
        .push(collection(1, &["m1"], &ids));
    records.variables = vars;
    records
}

#[test]
fn alias_type_disjoint_stale_cancel() {
    bounded_case("alias_type_disjoint_stale_cancel", || {
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let alias = |n: u32| alias_to(n, VariableType::Float);
        let var = |n: u32, ty: VariableType, v: StudioValue| variable(n, 1, ty, &[("m1", v)]);
        // A -> B -> A (cycle); C(Float) -> D(String) (type); E -> F = 4.0 (good chain).
        let all = vec![
            var(1, VariableType::Float, alias(2)),
            var(2, VariableType::Float, alias(1)),
            var(3, VariableType::Float, alias(4)),
            var(4, VariableType::String, s("x")),
            var(5, VariableType::Float, alias(6)),
            var(6, VariableType::Float, float(4.0)),
        ];
        // Folio is the validator: it refuses the cycle and the alias type clash. The evaluator
        // still refuses both typed when it is handed such a set, never looping or guessing.
        let empty = document(1, vec![]);
        let token = CancellationToken::default();
        let refuse = |vars: Vec<folio::StudioVariable>| {
            folio::validate_design_records(
                &variable_records(vars),
                &empty,
                folio::Budget::default(),
                &token,
                &folio::Unresolved,
            )
            .unwrap_err()
        };
        let cycle = refuse(all[..2].to_vec());
        assert_eq!(
            (cycle.code, cycle.target.as_str()),
            (folio::Code::VariableAliasCycle, id("SVAR", 1).as_str())
        );
        let clash = refuse(all[2..4].to_vec());
        assert_eq!(clash.code, folio::Code::InvalidField);
        assert!(clash.target.starts_with(&id("SVAR", 3)));

        let records = variable_records(all);
        let table = VariableTable::new(&records.variable_collections, &records.variables);
        let rv =
            |n: u32, l: &Limits| resolve_variable(&table, &id("SVAR", n), &no_modes(), l, &cancel);
        assert_eq!(rv(1, &limits), Err(E::AliasCycle(id("SVAR", 1))));
        assert_eq!(rv(3, &limits), Err(E::TypeMismatch(id("SVAR", 4))));
        let ok = rv(5, &limits).unwrap();
        assert_eq!(ok.value, float(4.0));
        assert_eq!(ok.chain, vec![id("SVAR", 5), id("SVAR", 6)]);
        assert_eq!(
            (ok.collection_id.as_str(), ok.mode_id.as_str()),
            (id("SVCL", 1).as_str(), "m1")
        );
        assert_eq!(
            rv(
                5,
                &Limits {
                    max_alias_depth: 1,
                    ..limits
                }
            ),
            Err(E::BudgetExceeded)
        );
        assert_eq!(rv(99, &limits), Err(E::NoSuchVariable(id("SVAR", 99))));
        assert_eq!(
            resolve_variable(
                &table,
                &id("SVAR", 5),
                &modes(&[(1, "zz")]),
                &limits,
                &cancel
            ),
            Err(E::NoSuchMode {
                collection_id: id("SVCL", 1),
                mode_id: "zz".into()
            })
        );

        // Footprints: one proposal edits Label 4 `characters` on an instance of the large member.
        let fx = fixture();
        let design = fx.design();
        let inst = instance(1, &id("SCMP", 12));
        let before = inst.clone();
        let edit = OverrideEdit {
            node_id: id("SLYR", 4),
            field: "characters".into(),
            value: s("Hi"),
        };
        let p = propose_overrides(
            &inst,
            std::slice::from_ref(&edit),
            &design,
            &no_modes(),
            3,
            &limits,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            p.write_footprint,
            vec![(id("SLYR", 4), "characters".to_owned())]
        );
        assert_eq!(p.read_footprint, vec![(id("SCMP", 12), String::new())]);
        assert_eq!(p.instance.overrides, vec![record(4, "characters", s("Hi"))]);
        assert_eq!(inst, before, "the caller's instance is untouched");
        // The proposal is a valid Folio record: the record set with the successor instance validates.
        let mut with_instance = fx.records.clone();
        with_instance.component_instances.push(p.instance.clone());
        folio::validate_design_records(
            &with_instance,
            &fx.doc,
            folio::Budget::default(),
            &token,
            &folio::Unresolved,
        )
        .unwrap();
        let key = |n: u32, f: &str| (id("SLYR", n), f.to_owned());
        let expected = BTreeMap::from([
            (key(4, "characters"), 1u64),
            (key(3, "width"), 1),
            ((id("SCMP", 12), String::new()), 3),
        ]);
        // Same property moved 1 -> 2: refused. Disjoint property (width 1 -> 9): admissible.
        let mut current = expected.clone();
        current.insert(key(3, "width"), 9);
        assert_eq!(p.admit(&expected, &current), Ok(()));
        current.insert(key(4, "characters"), 2);
        assert_eq!(
            p.admit(&expected, &current),
            Err(E::StaleProperty {
                node_id: id("SLYR", 4),
                field: "characters".into()
            })
        );
        // Record-level drift on the component definition read by the proposal is also stale.
        let mut current = expected.clone();
        current.insert((id("SCMP", 12), String::new()), 4);
        assert_eq!(
            check_footprint(&expected, &current, &p.footprint()),
            Err(E::StaleProperty {
                node_id: id("SCMP", 12),
                field: String::new()
            })
        );

        // A cancelled token produces no resolution and no proposal.
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            resolve_instance(&inst, &design, &no_modes(), 3, &limits, &stop),
            Err(E::Canceled)
        );
        assert_eq!(
            propose_overrides(&inst, &[edit], &design, &no_modes(), 3, &limits, &stop),
            Err(E::Canceled)
        );
    });
}

#[test]
fn override_rules_and_properties() {
    bounded_case("override_rules_and_properties", || {
        let fx = fixture();
        let design = fx.design();
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let run = |i: &folio::StudioComponentInstance| {
            resolve_instance(i, &design, &no_modes(), 1, &limits, &cancel)
        };
        let large = id("SCMP", 12);

        // STU-DS-115 is closed: `width` is inherited, not writable.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "width", float(1.0)));
        let not_overridable = E::FieldNotOverridable {
            node_id: id("SLYR", 4),
            field: "width".into(),
        };
        assert_eq!(run(&i), Err(not_overridable.clone()));
        let edit = OverrideEdit {
            node_id: id("SLYR", 4),
            field: "width".into(),
            value: float(1.0),
        };
        assert_eq!(
            propose_overrides(
                &instance(1, &large),
                &[edit],
                &design,
                &no_modes(),
                1,
                &limits,
                &cancel
            ),
            Err(not_overridable)
        );
        // An override value of a different type than the definition literal is refused.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "characters", float(1.0)));
        assert_eq!(run(&i), Err(E::TypeMismatch("characters".into())));
        // Two records claiming one node/field = ambiguous double source; unknown node refused.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "characters", s("a")));
        i.overrides.push(record(4, "characters", s("b")));
        assert_eq!(
            run(&i),
            Err(E::AmbiguousSource {
                node_id: id("SLYR", 4),
                field: "characters".into()
            })
        );
        let mut i = instance(1, &large);
        i.overrides.push(record(99, "characters", s("a")));
        assert_eq!(run(&i), Err(E::NoSuchNode(id("SLYR", 99))));
        // A definition node with a literal AND a variable binding for one field cannot be validated.
        let mut both = node(40, "Both", None);
        both.fields
            .insert("fills".into(), color(0.0, 0.0, 0.0, 0.0));
        both.bindings.insert("fills".into(), id("SVAR", 1));
        let mut w = World::new();
        w.add_component(plain(40, vec![both]), on_board());
        let ambiguous = w.build();
        assert_eq!(
            ambiguous
                .design()
                .validate(&limits, &cancel, &folio::Unresolved),
            Err(E::AmbiguousSource {
                node_id: id("SLYR", 40),
                field: "fills".into()
            })
        );

        // Unknown instance property and a definition reference to a property nobody defines.
        let mut i = instance(1, &large);
        i.component_properties.insert(
            "nope".into(),
            instance_prop(PropType::Text, StudioPropertyValue::Text("x".into())),
        );
        assert_eq!(run(&i), Err(E::UnsupportedProperty("nope".into())));
        let mut ghost = node(41, "Ghost", None);
        ghost.refs.insert("visible".into(), "ghost".into());
        let mut w = World::new();
        w.add_component(plain(42, vec![ghost]), on_board());
        let ghosts = w.checked();
        assert_eq!(
            resolve_instance(
                &instance(2, &id("SCMP", 42)),
                &ghosts.design(),
                &no_modes(),
                1,
                &limits,
                &cancel
            ),
            Err(E::UnsupportedProperty("ghost".into()))
        );
        // A property of the wrong type for its definition is a type mismatch.
        let mut i = instance(1, &large);
        i.component_properties.insert(
            "showIcon".into(),
            instance_prop(PropType::Boolean, StudioPropertyValue::Text("yes".into())),
        );
        assert_eq!(run(&i), Err(E::TypeMismatch("showIcon".into())));

        // A mode with no stored value is an error everywhere, never zero or black. Folio refuses the
        // record set (a variable must hold exactly its collection's modes); the evaluator, handed
        // that set, still refuses at the mode that has no value.
        let mut painted = node(43, "Painted", None);
        painted.bindings.insert("fills".into(), id("SVAR", 20));
        let mut w = World::new();
        w.variables(
            collection(2, &["a", "b"], &[20]),
            vec![color_var(20, 2, &[("a", [1.0, 0.0, 0.0, 1.0])])],
        );
        w.add_component(plain(44, vec![painted]), on_board());
        let missing_mode = w.build();
        let refused = missing_mode
            .design()
            .validate(&limits, &cancel, &folio::Unresolved)
            .unwrap_err();
        assert_eq!(
            refused,
            E::Folio(folio::Diagnostic {
                code: folio::Code::InvalidField,
                target: format!("{}/values_by_mode", id("SVAR", 20)),
            })
        );
        let d3 = missing_mode.design();
        let i = instance(3, &id("SCMP", 44));
        assert!(resolve_instance(&i, &d3, &modes(&[(2, "a")]), 1, &limits, &cancel).is_ok());
        assert_eq!(
            resolve_instance(&i, &d3, &modes(&[(2, "b")]), 1, &limits, &cancel),
            Err(E::MissingModeValue {
                variable_id: id("SVAR", 20),
                mode_id: "b".into()
            })
        );

        // Variant rules: unknown value, deleted main, and set completeness/uniqueness in validation.
        let mut i = instance(1, &id("SCMP", 10));
        i.component_properties
            .insert("size".into(), variant_prop("huge"));
        assert_eq!(
            run(&i),
            Err(E::UnknownVariantValue {
                property: "size".into(),
                value: "huge".into()
            })
        );
        let mut i = instance(1, &large);
        i.main_component_id = None;
        assert_eq!(run(&i), Err(E::NoSuchComponent("deleted-main".into())));
        let set_world = |members: Vec<TComp>| {
            let mut w = World::new();
            w.add_set(
                50,
                51,
                &[("size", &["small", "large"]), ("tone", &["a"])],
                vec![],
                members,
            );
            w.build()
        };
        let with_tone = |n: u32, size: &str| TComp {
            variant: Some(BTreeMap::from([
                ("size".to_owned(), size.to_owned()),
                ("tone".to_owned(), "a".to_owned()),
            ])),
            ..plain(n, vec![])
        };
        let validate = |fx: &Fixture| fx.design().validate(&limits, &cancel, &folio::Unresolved);
        assert_eq!(
            validate(&set_world(vec![
                with_tone(51, "small"),
                member(52, "large", vec![])
            ])),
            Err(E::VariantAssignmentIncomplete(id("SCMP", 52)))
        );
        assert_eq!(
            validate(&set_world(vec![
                with_tone(51, "small"),
                with_tone(52, "small")
            ])),
            Err(E::VariantAssignmentDuplicate(id("SCMP", 52)))
        );
        // STU-DS-102 (two or more members) is a crate-local rule: Folio leaves it unenforced.
        assert_eq!(
            validate(&set_world(vec![with_tone(51, "small")])),
            Err(E::NoSuchVariant)
        );
    });
}

#[test]
fn constraints_hand_computed() {
    bounded_case("constraints_hand_computed", || {
        use ConstraintType::{Center, Max, Min, Scale, Stretch};
        // Horizontal: child x=10, w=20 in a parent going 100 -> 200 wide (vertical untouched).
        let child = RectPt {
            x: 10.0,
            y: 5.0,
            w: 20.0,
            h: 10.0,
        };
        let same = SizePt { w: 100.0, h: 50.0 };
        let wide = SizePt { w: 200.0, h: 50.0 };
        for (kind, x, w) in [
            (Min, 10.0, 20.0),
            (Max, 110.0, 20.0),     // right offset 100-30=70 kept: 200-70-20
            (Center, 60.0, 20.0),   // centre offset 20-50=-30 kept: 100-30-10
            (Stretch, 10.0, 120.0), // offsets 10 and 70 kept: 200-10-70
            (Scale, 20.0, 40.0),    // 10/100 and 20/100 of the parent
        ] {
            let r = apply_constraints(child, same, wide, &cons(kind, Min)).unwrap();
            assert_eq!(
                r,
                RectPt {
                    x,
                    y: 5.0,
                    w,
                    h: 10.0
                },
                "{kind:?}"
            );
        }
        // Vertical: child y=5, h=10 in a parent going 50 -> 80 tall (horizontal untouched).
        let tall = SizePt { w: 100.0, h: 80.0 };
        for (kind, y, h) in [
            (Min, 5.0, 10.0),
            (Max, 35.0, 10.0),    // bottom offset 50-15=35: 80-35-10
            (Center, 20.0, 10.0), // centre 10 vs 25 => -15: 40-15=25 => y 20
            (Stretch, 5.0, 40.0), // offsets 5 and 35: 80-5-35
            (Scale, 8.0, 16.0),   // 5*80/50 and 10*80/50
        ] {
            let r = apply_constraints(child, same, tall, &cons(Min, kind)).unwrap();
            assert_eq!(
                r,
                RectPt {
                    x: 10.0,
                    y,
                    w: 20.0,
                    h
                },
                "{kind:?}"
            );
        }
        // Both axes at once, and the STU-DS-152 default (top-left) leaves the rectangle alone.
        let both = apply_constraints(child, same, SizePt { w: 200.0, h: 80.0 }, &cons(Max, Scale))
            .unwrap();
        assert_eq!(
            both,
            RectPt {
                x: 110.0,
                y: 8.0,
                w: 20.0,
                h: 16.0
            }
        );
        assert_eq!(folio::StudioConstraint::top_left(), cons(Min, Min));
        assert_eq!(
            apply_constraints(child, same, wide, &folio::StudioConstraint::top_left()),
            Ok(child)
        );

        // Invalid geometry is refused, never clamped.
        let bad = |c: RectPt, old: SizePt, new: SizePt, k: folio::StudioConstraint| {
            apply_constraints(c, old, new, &k)
        };
        assert_eq!(
            bad(RectPt { w: -1.0, ..child }, same, wide, cons(Min, Min)),
            Err(E::InvalidGeometry)
        );
        assert_eq!(
            bad(
                RectPt {
                    x: f64::NAN,
                    ..child
                },
                same,
                wide,
                cons(Min, Min)
            ),
            Err(E::InvalidGeometry)
        );
        assert_eq!(
            bad(child, SizePt { w: 0.0, h: 50.0 }, wide, cons(Scale, Min)),
            Err(E::InvalidGeometry)
        );
        // Stretch into a parent of width 10: 20 + (10 - 100) < 0 => refused.
        assert_eq!(
            bad(child, same, SizePt { w: 10.0, h: 50.0 }, cons(Stretch, Min)),
            Err(E::InvalidGeometry)
        );

        // Hierarchy: root 100x50 -> 200x80. A (stretch/stretch) 10,5 60x40 becomes 10,5 160x70
        // (60+100, 40+30); B (max/max) in A at 5,5 10x10 moves by A's growth (100,30): 105,35.
        let mut a = node(1, "A", None);
        a.rect = Some(RectPt {
            x: 10.0,
            y: 5.0,
            w: 60.0,
            h: 40.0,
        });
        a.constraint = Some(cons(Stretch, Stretch));
        let mut b = node(2, "B", Some(1));
        b.rect = Some(RectPt {
            x: 5.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        });
        b.constraint = Some(cons(Max, Max));
        let c = node(3, "C-no-geometry", None);
        let mut w = World::new();
        w.add_component(
            TComp {
                size: Some(same),
                ..plain(1, vec![a, b, c])
            },
            on_board(),
        );
        let fx = w.checked();
        let design = fx.design();
        let cancel = CancellationToken::default();
        let placed = resize_component(
            &design,
            &id("SCMP", 1),
            SizePt { w: 200.0, h: 80.0 },
            &cancel,
        )
        .unwrap();
        assert_eq!(placed.len(), 2);
        assert_eq!(
            placed[0].rect,
            RectPt {
                x: 10.0,
                y: 5.0,
                w: 160.0,
                h: 70.0
            }
        );
        assert_eq!(
            placed[1].rect,
            RectPt {
                x: 105.0,
                y: 35.0,
                w: 10.0,
                h: 10.0
            }
        );
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            resize_component(&design, &id("SCMP", 1), wide, &stop),
            Err(E::Canceled)
        );
        // A definition whose container layer has no rectangle (no component size) is refused.
        let mut sized = node(1, "A", None);
        sized.rect = Some(RectPt {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        });
        let mut w = World::new();
        w.add_component(plain(2, vec![sized]), on_board());
        let no_size = w.build();
        assert_eq!(
            no_size
                .design()
                .validate(&Limits::default(), &cancel, &folio::Unresolved),
            Err(E::InvalidGeometry)
        );
        assert_eq!(
            resize_component(&no_size.design(), &id("SCMP", 2), wide, &cancel),
            Err(E::InvalidGeometry)
        );
    });
}

// ---- the Folio document, instance layer and prototype graph --------------------------------------

struct Host(folio::Resolution);
impl folio::Resolver for Host {
    fn primitive(&self, schema_id: &str, _object_id: &str) -> folio::Resolution {
        if schema_id == hsk_studio_components::INSTANCE_LAYER_BINDING.1 {
            self.0
        } else {
            folio::Resolution::Absent
        }
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        (kind, schema_id) == hsk_studio_components::INSTANCE_LAYER_BINDING
    }
}

fn parent_artboard(n: u32) -> Parent {
    Parent::Node(board_ref(n))
}
/// Four frames; Button (a group) on Frame 1 and Card (a component instance) on Frame 2.
fn folio_document() -> folio::StudioDocument {
    let button = group(id("SLYR", 1), "Button", parent_artboard(1));
    let card = instance_layer(&id("SLYR", 2), "Card", parent_artboard(2), &id("SCIN", 1)).unwrap();
    document(4, vec![button, card])
}
/// A record set that holds only reactions: one design entry per node, in the given order.
fn reaction_records(entries: Vec<(NodeRef, Vec<StudioReaction>)>) -> folio::StudioDesignRecords {
    let mut records = folio::StudioDesignRecords::new(id("SDOC", 1));
    records.nodes = entries
        .into_iter()
        .map(|(node, reactions)| folio::StudioNodeDesign {
            reactions,
            ..design_entry(node)
        })
        .collect();
    records
}
fn to(dest: &str, navigation: folio::Navigation, pos: Option<(f64, f64)>) -> StudioAction {
    node_action(Some(dest), navigation, pos)
}
fn node_action(
    dest: Option<&str>,
    navigation: folio::Navigation,
    pos: Option<(f64, f64)>,
) -> StudioAction {
    StudioAction::Node {
        destination_id: dest.map(str::to_owned),
        navigation,
        transition: None,
        preserve_scroll_position: false,
        overlay_relative_position: pos.map(|(x, y)| folio::OverlayPosition { x, y }),
        reset_video_position: false,
        reset_scroll_position: false,
        reset_interactive_components: false,
    }
}
fn react(trigger: Option<StudioTrigger>, actions: Vec<StudioAction>) -> StudioReaction {
    StudioReaction { trigger, actions }
}
fn click() -> Option<StudioTrigger> {
    Some(StudioTrigger::OnClick {})
}

#[test]
fn folio_shape_and_prototype_graph() {
    bounded_case("folio_shape_and_prototype_graph", || {
        let token = CancellationToken::default();
        let doc = folio_document();
        // The instance layer is a Folio layer: valid when the host resolver accepts the binding...
        let snap = folio::validate_document(
            doc.clone(),
            folio::Budget::default(),
            &token,
            &Host(folio::Resolution::Available),
        )
        .unwrap();
        assert!(snap.resolution_issues().is_empty());
        // ...an unresolved instance record is reported (not hidden)...
        let snap_absent = folio::validate_document(
            doc.clone(),
            folio::Budget::default(),
            &token,
            &Host(folio::Resolution::Absent),
        )
        .unwrap();
        assert_eq!(snap_absent.resolution_issues().len(), 1);
        assert_eq!(snap_absent.resolution_issues()[0].target, id("SLYR", 2));
        // ...and a resolver that does not know the binding rejects the document.
        let err = folio::validate_document(
            doc.clone(),
            folio::Budget::default(),
            &token,
            &folio::Unresolved,
        )
        .unwrap_err();
        assert_eq!(err.code, folio::Code::UnsupportedDescriptor);
        assert!(instance_layer(&id("SLYR", 2), "x", Parent::Root(()), &id("SCMP", 1)).is_err());

        let (f1, f2, f3, f4) = (id("SART", 1), id("SART", 2), id("SART", 3), id("SART", 4));
        let records = reaction_records(vec![
            // Button on Frame 1 -> Frame 2.
            (
                layer_ref(1),
                vec![react(
                    click(),
                    vec![to(&f2, folio::Navigation::Navigate, None)],
                )],
            ),
            (
                layer_ref(2),
                vec![
                    // Card (instance) on Frame 2 -> overlay Frame 3, then a Back that can never run.
                    react(
                        click(),
                        vec![
                            to(&f3, folio::Navigation::Overlay, Some((10.0, 20.0))),
                            StudioAction::Back {},
                        ],
                    ),
                    // Authored but not armed (null trigger): Frame 4 stays unreachable.
                    react(None, vec![to(&f4, folio::Navigation::Navigate, None)]),
                ],
            ),
            // Frame 3 itself -> back to Frame 1: a cycle, legal.
            (
                board_ref(3),
                vec![react(
                    Some(StudioTrigger::OnPress {}),
                    vec![to(&f1, folio::Navigation::Navigate, None)],
                )],
            ),
        ]);
        // Folio accepts the reaction records of this document.
        folio::validate_design_records(
            &records,
            snap.document(),
            folio::Budget::default(),
            &token,
            &Host(folio::Resolution::Available),
        )
        .unwrap();
        let flow = PrototypeFlow {
            flow_id: id("SPTF", 1),
            start_frame_id: f1.clone(),
        };
        let limits = FlowLimits::default();
        let local = LocalContent::default();
        let design = Design::new(snap.document(), &records, &local);
        let report = validate_flow(&flow, &design, &limits, &token).unwrap();
        assert_eq!(
            report.reachable_frames,
            vec![f1.clone(), f2.clone(), f3.clone()]
        );
        assert_eq!(report.warnings.len(), 1);
        // Reaction order is the design entries in record order: Button 0, Card 1 and 2, Frame 3: 3.
        assert_eq!(
            (
                report.warnings[0].reaction_index,
                report.warnings[0].action_index
            ),
            (1, 1)
        );
        assert_eq!(report.warnings[0].node_id, id("SLYR", 2));

        // Destinations are checked against the document's frames.
        let with =
            |a: StudioAction| reaction_records(vec![(layer_ref(1), vec![react(click(), vec![a])])]);
        let run = |records: &folio::StudioDesignRecords, f: &PrototypeFlow| {
            validate_flow(
                f,
                &Design::new(snap.document(), records, &local),
                &limits,
                &token,
            )
        };
        let nav = folio::Navigation::Navigate;
        assert_eq!(
            run(&with(to(&id("SART", 9), nav, None)), &flow),
            Err(E::PrototypeTarget(id("SART", 9)))
        );
        assert_eq!(
            run(&with(to(&id("SLYR", 1), nav, None)), &flow),
            Err(E::PrototypeTarget(id("SLYR", 1)))
        );
        assert_eq!(
            run(&with(node_action(None, nav, None)), &flow),
            Err(E::PrototypeTarget("null".into()))
        );
        assert_eq!(
            run(&with(to(&f2, folio::Navigation::ChangeTo, None)), &flow),
            Err(E::UnsupportedPrototype("ChangeTo".into()))
        );
        assert_eq!(
            run(
                &records,
                &PrototypeFlow {
                    start_frame_id: id("SART", 8),
                    ..flow.clone()
                }
            ),
            Err(E::PrototypeTarget(id("SART", 8)))
        );
        // Folio refuses a reaction on a node the document does not have; the walk refuses it too.
        let missing = reaction_records(vec![(
            layer_ref(77),
            vec![react(click(), vec![StudioAction::Close {}])],
        )]);
        assert_eq!(
            folio::validate_design_records(
                &missing,
                snap.document(),
                folio::Budget::default(),
                &token,
                &Host(folio::Resolution::Available)
            )
            .unwrap_err()
            .code,
            folio::Code::DanglingReference
        );
        assert_eq!(run(&missing, &flow), Err(E::NoSuchNode(id("SLYR", 77))));
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            validate_flow(&flow, &design, &limits, &stop),
            Err(E::Canceled)
        );
    });
}

// ---- R3 regression tests: one per recorded finding ----------------------------------------------

#[test]
fn override_value_type_is_checked_against_property_and_variable_types() {
    bounded_case("override_value_type_vs_property_and_variable", || {
        let fx = fixture();
        let design = fx.design();
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let run = |i: &folio::StudioComponentInstance| {
            resolve_instance(i, &design, &no_modes(), 1, &limits, &cancel)
        };
        let large = id("SCMP", 12);

        // Label 4 `visible` is driven by the Boolean property showIcon (no definition literal), so a
        // String override has no type to hide behind; it used to resolve as Source::Override.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "visible", s("x")));
        assert_eq!(run(&i), Err(E::TypeMismatch("visible".into())));
        // Button 3 `fills` is bound to a Color variable: a Float override is refused likewise.
        let mut i = instance(1, &large);
        i.overrides.push(record(3, "fills", float(1.0)));
        assert_eq!(run(&i), Err(E::TypeMismatch("fills".into())));
        // The proposal path resolves the successor, so it refuses the same edit.
        let edit = OverrideEdit {
            node_id: id("SLYR", 4),
            field: "visible".into(),
            value: s("x"),
        };
        assert_eq!(
            propose_overrides(
                &instance(1, &large),
                &[edit],
                &design,
                &no_modes(),
                1,
                &limits,
                &cancel
            ),
            Err(E::TypeMismatch("visible".into()))
        );

        // Right-typed overrides still win over the property default and the variable binding.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "visible", flag(false)));
        i.overrides
            .push(record(3, "fills", color(1.0, 0.0, 0.0, 1.0)));
        let r = run(&i).unwrap();
        let (visible, fills) = (
            r.field(&id("SLYR", 4), "visible").unwrap(),
            r.field(&id("SLYR", 3), "fills").unwrap(),
        );
        assert_eq!(
            (&visible.value, &visible.source),
            (&flag(false), &Source::Override)
        );
        assert_eq!(
            (&fills.value, &fills.source),
            (&color(1.0, 0.0, 0.0, 1.0), &Source::Override)
        );
    });
}

#[test]
fn set_members_must_have_distinct_ids() {
    bounded_case("set_members_distinct_ids", || {
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let set = |default: u32, members: Vec<TComp>| {
            let mut w = World::new();
            w.add_set(
                50,
                default,
                &[("size", &["small", "large"])],
                vec![],
                members,
            );
            w.build()
        };
        let validate = |fx: &Fixture| fx.design().validate(&limits, &cancel, &folio::Unresolved);
        // Two different definitions under one member id: the lookup would return the first while a
        // variant selection could pick the second.
        assert_eq!(
            validate(&set(
                51,
                vec![member(51, "small", vec![]), member(51, "large", vec![])]
            )),
            Err(E::Duplicate(id("SCMP", 51)))
        );
        // A member sharing the set's own id is the same ambiguity.
        assert_eq!(
            validate(&set(
                51,
                vec![member(51, "small", vec![]), member(50, "large", vec![])]
            )),
            Err(E::Duplicate(id("SCMP", 50)))
        );
        // Distinct ids validate, and each member is found under its own id.
        let fx = set(
            51,
            vec![member(51, "small", vec![]), member(52, "large", vec![])],
        );
        assert_eq!(validate(&fx), Ok(vec![]));
        let design = fx.design();
        for (comp, size) in [(51, "small"), (52, "large")] {
            let found = design.component(&id("SCMP", comp)).unwrap();
            assert_eq!(found.component_id, id("SCMP", comp));
            assert_eq!(found.variant_properties.as_ref().unwrap()["size"], size);
        }
    });
}

#[test]
fn actions_after_the_first_add_no_reachability_edge() {
    bounded_case("dead_actions_add_no_reachability_edge", || {
        let token = CancellationToken::default();
        let doc = folio_document();
        let snap = folio::validate_document(
            doc,
            folio::Budget::default(),
            &token,
            &Host(folio::Resolution::Available),
        )
        .unwrap();
        let (f1, f2, f3) = (id("SART", 1), id("SART", 2), id("SART", 3));
        let nav = folio::Navigation::Navigate;
        let flow = PrototypeFlow {
            flow_id: id("SPTF", 1),
            start_frame_id: f1.clone(),
        };
        let limits = FlowLimits::default();
        let local = LocalContent::default();
        // One Button (on Frame 1) reaction with these actions.
        let run = |actions: Vec<StudioAction>| {
            let records = reaction_records(vec![(layer_ref(1), vec![react(click(), actions)])]);
            validate_flow(
                &flow,
                &Design::new(snap.document(), &records, &local),
                &limits,
                &token,
            )
        };

        // [Navigate F2, Navigate F3]: only the first runs, so F3 is warned about and unreachable
        // (it used to be reported reachable through the dead second action).
        let report = run(vec![to(&f2, nav, None), to(&f3, nav, None)]).unwrap();
        assert_eq!(report.reachable_frames, vec![f1.clone(), f2.clone()]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            (
                report.warnings[0].reaction_index,
                report.warnings[0].action_index
            ),
            (0, 1)
        );
        // The same destination as the first action does arm the edge.
        let report = run(vec![to(&f3, nav, None), to(&f2, nav, None)]).unwrap();
        assert_eq!(report.reachable_frames, vec![f1.clone(), f3.clone()]);
        // A dead action's destination is still validated.
        assert_eq!(
            run(vec![to(&f2, nav, None), to(&id("SART", 9), nav, None)]),
            Err(E::PrototypeTarget(id("SART", 9)))
        );
        // Only a navigation ends the array (STU-PRO-104): an action that does not navigate leaves the
        // next one running, and it warns after BACK or CLOSE.
        let url = StudioAction::Url {
            url: "https://example.invalid/".into(),
            open_in_new_tab: false,
        };
        let report = run(vec![url.clone(), to(&f2, nav, None)]).unwrap();
        assert_eq!(report.reachable_frames, vec![f1.clone(), f2.clone()]);
        assert!(report.warnings.is_empty());
        let report = run(vec![StudioAction::Close {}, url]).unwrap();
        assert_eq!(report.reachable_frames, vec![f1.clone()]);
        assert_eq!(
            (
                report.warnings[0].reaction_index,
                report.warnings[0].action_index
            ),
            (0, 1)
        );
    });
}

#[test]
fn switch_variant_and_swap_never_return_an_unresolvable_instance() {
    bounded_case("switch_and_swap_resolvable", || {
        let mut w = fixture_world();
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        // A plain component (60) and one whose Label `characters` literal is a Float (61).
        w.add_component(plain(60, vec![node(70, "Button", None)]), on_board());
        let mut odd_label = node(72, "Label", Some(71));
        odd_label.fields.insert("characters".into(), float(1.0));
        w.add_component(
            plain(61, vec![node(71, "Button", None), odd_label]),
            on_board(),
        );
        let fx = w.checked();
        let design = fx.design();
        let resolves = |i: &folio::StudioComponentInstance| {
            resolve_instance(i, &design, &no_modes(), 1, &limits, &cancel)
        };

        // A plain instance has no variant to switch. It used to return Ok with a VARIANT property
        // that resolve_instance then refuses (UnsupportedProperty).
        let on_plain = instance(1, &id("SCMP", 60));
        assert_eq!(
            switch_variant(&on_plain, &design, "size", "small"),
            Err(E::UnsupportedProperty("size".into()))
        );
        // Swapping a set instance that carries a variant selection to a plain component is refused
        // for the same reason; the caller's instance is untouched.
        let mut on_set = instance(2, &id("SCMP", 10));
        on_set
            .component_properties
            .insert("size".into(), variant_prop("large"));
        let before = on_set.clone();
        assert_eq!(
            swap_component(&on_set, &design, &id("SCMP", 60)),
            Err(E::UnsupportedProperty("size".into()))
        );
        assert_eq!(on_set, before);
        // A carried override whose value no longer fits the target node's type is refused.
        let mut typed = instance(3, &id("SCMP", 12));
        typed.overrides.push(record(4, "characters", s("Buy now")));
        assert_eq!(
            swap_component(&typed, &design, &id("SCMP", 61)),
            Err(E::TypeMismatch("characters".into()))
        );
        // A compatible swap still succeeds and the result resolves.
        let (next, dropped) = swap_component(&typed, &design, &id("SCMP", 60)).unwrap();
        assert_eq!(
            dropped.len(),
            1,
            "Label 4 has no counterpart in the plain component"
        );
        assert!(resolves(&next).is_ok());
        let (next, dropped) = switch_variant(&on_set, &design, "size", "small").unwrap();
        assert!(dropped.is_empty());
        assert_eq!(resolves(&next).unwrap().component_id, id("SCMP", 11));
    });
}
