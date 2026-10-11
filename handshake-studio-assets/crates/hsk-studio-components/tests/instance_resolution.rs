//! Hand-computed oracles for components, variables, constraints and the prototype graph. Ids are
//! `PREFIX-0198a000-0000-7000-8000-<n as 12 hex>`; numbers below are the `n`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    Action, Collection, ComponentDef, ComponentSet, ComponentsError as E, ConstraintKind,
    Constraints, DefNode, DefRegistry, Instance, InstanceProp, Limits, Mode, Navigation,
    NodeGeometry, OverrideEdit, OverrideRecord, PropType, PropertyDef, PrototypeFlow, Reaction,
    RectPt, SizePt, Source, Trigger, Value, VarEntry, VarRegistry, VarType, Variable,
    apply_constraints, check_footprint, instance_layer, propose_overrides, resize_component,
    resolve_instance, resolve_variable, switch_variant, validate_flow,
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

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-0198a000-0000-7000-8000-{n:012x}")
}
fn s(v: &str) -> Value {
    Value::String(v.into())
}
fn node(n: u32, name: &str, parent: Option<u32>) -> DefNode {
    DefNode::new(&id("SLYR", n), name, parent.map(|p| id("SLYR", p)).as_deref())
}
fn modes(pairs: &[(u32, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(c, m)| (id("SVCL", *c), (*m).to_owned())).collect()
}
fn no_modes() -> BTreeMap<String, String> {
    BTreeMap::new()
}
fn instance(n: u32, main: &str) -> Instance {
    Instance {
        instance_id: id("SCIN", n),
        main_component_id: Some(main.to_owned()),
        component_properties: BTreeMap::new(),
        overrides: Vec::new(),
    }
}
fn variant_prop(v: &str) -> InstanceProp {
    InstanceProp {
        ty: PropType::Variant,
        value: s(v),
        bound_variable_id: None,
    }
}
fn record(node_id: u32, field: &str, v: Value) -> OverrideRecord {
    OverrideRecord {
        overridden_node_id: id("SLYR", node_id),
        overridden_fields: vec![field.to_owned()],
        values: BTreeMap::from([(field.to_owned(), v)]),
    }
}
fn plain(comp: u32, nodes: Vec<DefNode>) -> ComponentDef {
    ComponentDef {
        component_id: id("SCMP", comp),
        variant_properties: None,
        property_definitions: vec![],
        nodes,
        size: None,
    }
}
fn member(comp: u32, size: &str, nodes: Vec<DefNode>) -> ComponentDef {
    ComponentDef {
        variant_properties: Some(BTreeMap::from([("size".to_owned(), size.to_owned())])),
        ..plain(comp, nodes)
    }
}
fn two_mode_collection(n: u32, a: &str, b: &str) -> Collection {
    Collection {
        collection_id: id("SVCL", n),
        modes: vec![
            Mode { mode_id: a.into(), name: a.into() },
            Mode { mode_id: b.into(), name: b.into() },
        ],
        default_mode_id: a.into(),
    }
}
fn color_var(n: u32, coll: u32, entries: &[(&str, [f64; 4])]) -> Variable {
    Variable {
        variable_id: id("SVAR", n),
        collection_id: id("SVCL", coll),
        resolved_type: VarType::Color,
        values_by_mode: entries
            .iter()
            .map(|(m, c)| ((*m).to_owned(), VarEntry::Literal(Value::Color(*c))))
            .collect(),
    }
}

/// Set SCMP-10 `size = small | large` (default small = SCMP-11, large = SCMP-12). Small nodes:
/// Button 1 (width 100, fills = variable 1) > Label 2. Large nodes: Button 3 (width 200, same fills
/// binding) > Label 4, Badge 5. Label `visible` is driven by the Boolean property `showIcon`
/// (default true); label `characters` is the literal "Label". Variable 1 is a Color with modes
/// light = (0,0,1,1) and dark = (0.5,0.5,1,1).
fn fixture() -> (DefRegistry, VarRegistry) {
    let mut vars = VarRegistry::new();
    vars.add_collection(two_mode_collection(1, "light", "dark")).unwrap();
    vars.add_variable(color_var(
        1,
        1,
        &[("light", [0.0, 0.0, 1.0, 1.0]), ("dark", [0.5, 0.5, 1.0, 1.0])],
    ))
    .unwrap();
    let button = |root: u32, label: u32, width: f64| {
        let mut r = node(root, "Button", None);
        r.fields.insert("width".into(), Value::Float(width));
        r.variable_bindings.insert("fills".into(), id("SVAR", 1));
        let mut l = node(label, "Label", Some(root));
        l.fields.insert("characters".into(), s("Label"));
        l.property_references.insert("visible".into(), "showIcon".into());
        vec![r, l]
    };
    let mut large_nodes = button(3, 4, 200.0);
    large_nodes.push(node(5, "Badge", Some(3)));
    let mut defs = DefRegistry::new();
    defs.add_set(ComponentSet {
        component_id: id("SCMP", 10),
        default_variant_id: id("SCMP", 11),
        variant_group_properties: BTreeMap::from([(
            "size".to_owned(),
            vec!["small".to_owned(), "large".to_owned()],
        )]),
        property_definitions: vec![PropertyDef {
            name: "showIcon".into(),
            ty: PropType::Boolean,
            default_value: Value::Boolean(true),
        }],
        members: vec![
            member(11, "small", button(1, 2, 100.0)),
            member(12, "large", large_nodes),
        ],
    })
    .unwrap();
    (defs, vars)
}

#[test]
fn variant_mode_override_resolution() {
    bounded_case("variant_mode_override_resolution", || {
        let (defs, vars) = fixture();
        let (defs0, vars0) = (defs.clone(), vars.clone());
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let mut inst = instance(1, &id("SCMP", 10));
        inst.component_properties.insert("size".into(), variant_prop("large"));
        inst.overrides.push(record(4, "characters", s("Buy now")));
        inst.overrides.push(record(5, "opacity", Value::Float(0.5)));

        let r = resolve_instance(&inst, &defs, &vars, &modes(&[(1, "dark")]), 7, &limits, &cancel)
            .unwrap();
        assert_eq!((r.base_revision, r.component_id.as_str()), (7, id("SCMP", 12).as_str()));
        // Node order, then field-name order: 3 {fills, width}, 4 {characters, visible}, 5 {opacity}.
        assert_eq!(r.fields.len(), 5);
        let f = |n: u32, name: &str| r.field(&id("SLYR", n), name).unwrap();
        assert_eq!(f(3, "width").value, Value::Float(200.0));
        assert_eq!(f(3, "width").source, Source::Definition);
        assert_eq!(f(3, "width").reads, vec![(id("SCMP", 12), String::new())]);
        assert_eq!(f(3, "fills").value, Value::Color([0.5, 0.5, 1.0, 1.0]));
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
        assert_eq!(f(4, "visible").value, Value::Boolean(true));
        assert_eq!(f(4, "visible").source, Source::PropertyDefault("showIcon".into()));
        assert_eq!(f(5, "opacity").value, Value::Float(0.5));

        // Default mode, and the instance's own property binding beats the property default.
        inst.component_properties.insert(
            "showIcon".into(),
            InstanceProp { ty: PropType::Boolean, value: Value::Boolean(false), bound_variable_id: None },
        );
        let r = resolve_instance(&inst, &defs, &vars, &no_modes(), 8, &limits, &cancel).unwrap();
        let (fills, visible) = (
            r.field(&id("SLYR", 3), "fills").unwrap(),
            r.field(&id("SLYR", 4), "visible").unwrap(),
        );
        assert_eq!(fills.value, Value::Color([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(visible.value, Value::Boolean(false));
        assert_eq!(visible.source, Source::ComponentProperty("showIcon".into()));
        assert!(visible.reads.contains(&(id("SCIN", 1), "property/showIcon".into())));

        // Instantiating the set without a variant selection gives the default member (small).
        let r = resolve_instance(
            &instance(2, &id("SCMP", 10)),
            &defs,
            &vars,
            &no_modes(),
            1,
            &limits,
            &cancel,
        )
        .unwrap();
        assert_eq!(r.component_id, id("SCMP", 11));
        assert_eq!(r.field(&id("SLYR", 1), "width").unwrap().value, Value::Float(100.0));

        // Variant switch: Label 4 -> Label 2 by (name, position) so its override is carried; Badge 5
        // has no counterpart in `small`, so its override is dropped and reported.
        let before = inst.clone();
        let (next, dropped) = switch_variant(&inst, &defs, "size", "small").unwrap();
        assert_eq!(dropped.len(), 1);
        assert_eq!((dropped[0].node_id.as_str(), dropped[0].field.as_str()), (id("SLYR", 5).as_str(), "opacity"));
        assert_eq!(next.overrides, vec![record(2, "characters", s("Buy now"))]);
        let r = resolve_instance(&next, &defs, &vars, &no_modes(), 9, &limits, &cancel).unwrap();
        assert_eq!(r.component_id, id("SCMP", 11));
        assert_eq!(r.field(&id("SLYR", 2), "characters").unwrap().source, Source::Override);

        // Nothing the caller owns was mutated.
        assert_eq!(inst, before);
        assert_eq!((defs, vars), (defs0, vars0));
    });
}

#[test]
fn alias_type_disjoint_stale_cancel() {
    bounded_case("alias_type_disjoint_stale_cancel", || {
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let mut vars = VarRegistry::new();
        vars.add_collection(Collection {
            collection_id: id("SVCL", 1),
            modes: vec![Mode { mode_id: "m1".into(), name: "Mode 1".into() }],
            default_mode_id: "m1".into(),
        })
        .unwrap();
        let var = |n: u32, ty: VarType, e: VarEntry| Variable {
            variable_id: id("SVAR", n),
            collection_id: id("SVCL", 1),
            resolved_type: ty,
            values_by_mode: BTreeMap::from([("m1".to_owned(), e)]),
        };
        let alias = |n: u32| VarEntry::Alias(id("SVAR", n));
        // A -> B -> A (cycle); C(Float) -> D(String) (type); E -> F = 4.0 (good chain).
        for v in [
            var(1, VarType::Float, alias(2)),
            var(2, VarType::Float, alias(1)),
            var(3, VarType::Float, alias(4)),
            var(4, VarType::String, VarEntry::Literal(s("x"))),
            var(5, VarType::Float, alias(6)),
            var(6, VarType::Float, VarEntry::Literal(Value::Float(4.0))),
        ] {
            vars.add_variable(v).unwrap();
        }
        let rv = |n: u32, l: &Limits| resolve_variable(&vars, &id("SVAR", n), &no_modes(), l, &cancel);
        assert_eq!(rv(1, &limits), Err(E::AliasCycle(id("SVAR", 1))));
        assert_eq!(rv(3, &limits), Err(E::TypeMismatch(id("SVAR", 4))));
        let ok = rv(5, &limits).unwrap();
        assert_eq!(ok.value, Value::Float(4.0));
        assert_eq!(ok.chain, vec![id("SVAR", 5), id("SVAR", 6)]);
        assert_eq!((ok.collection_id.as_str(), ok.mode_id.as_str()), (id("SVCL", 1).as_str(), "m1"));
        assert_eq!(rv(5, &Limits { max_alias_depth: 1, ..limits }), Err(E::BudgetExceeded));
        assert_eq!(rv(99, &limits), Err(E::NoSuchVariable(id("SVAR", 99))));
        assert_eq!(
            resolve_variable(&vars, &id("SVAR", 5), &modes(&[(1, "zz")]), &limits, &cancel),
            Err(E::NoSuchMode { collection_id: id("SVCL", 1), mode_id: "zz".into() })
        );

        // Footprints: one proposal edits Label 4 `characters` on an instance of the large member.
        let (defs, fx_vars) = fixture();
        let inst = instance(1, &id("SCMP", 12));
        let before = inst.clone();
        let edit = OverrideEdit { node_id: id("SLYR", 4), field: "characters".into(), value: s("Hi") };
        let p = propose_overrides(
            &inst, std::slice::from_ref(&edit), &defs, &fx_vars, &no_modes(), 3, &limits, &cancel,
        )
        .unwrap();
        assert_eq!(p.write_footprint, vec![(id("SLYR", 4), "characters".to_owned())]);
        assert_eq!(p.read_footprint, vec![(id("SCMP", 12), String::new())]);
        assert_eq!(p.instance.overrides, vec![record(4, "characters", s("Hi"))]);
        assert_eq!(inst, before, "the caller's instance is untouched");
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
            Err(E::StaleProperty { node_id: id("SLYR", 4), field: "characters".into() })
        );
        // Record-level drift on the component definition read by the proposal is also stale.
        let mut current = expected.clone();
        current.insert((id("SCMP", 12), String::new()), 4);
        assert_eq!(
            check_footprint(&expected, &current, &p.footprint()),
            Err(E::StaleProperty { node_id: id("SCMP", 12), field: String::new() })
        );

        // A cancelled token produces no resolution and no proposal.
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            resolve_instance(&inst, &defs, &fx_vars, &no_modes(), 3, &limits, &stop),
            Err(E::Canceled)
        );
        assert_eq!(
            propose_overrides(&inst, &[edit], &defs, &fx_vars, &no_modes(), 3, &limits, &stop),            Err(E::Canceled)
        );
    });
}

#[test]
fn override_rules_and_properties() {
    bounded_case("override_rules_and_properties", || {
        let (defs, vars) = fixture();
        let (limits, cancel) = (Limits::default(), CancellationToken::default());
        let run = |i: &Instance| resolve_instance(i, &defs, &vars, &no_modes(), 1, &limits, &cancel);
        let large = id("SCMP", 12);

        // STU-DS-115 is closed: `width` is inherited, not writable.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "width", Value::Float(1.0)));
        let not_overridable = E::FieldNotOverridable { node_id: id("SLYR", 4), field: "width".into() };
        assert_eq!(run(&i), Err(not_overridable.clone()));
        let edit = OverrideEdit { node_id: id("SLYR", 4), field: "width".into(), value: Value::Float(1.0) };
        assert_eq!(
            propose_overrides(&instance(1, &large), &[edit], &defs, &vars, &no_modes(), 1, &limits, &cancel),
            Err(not_overridable)
        );
        // An override value of a different type than the definition literal is refused.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "characters", Value::Float(1.0)));
        assert_eq!(run(&i), Err(E::TypeMismatch("characters".into())));
        // Two records claiming one node/field = ambiguous double source; unknown node refused.
        let mut i = instance(1, &large);
        i.overrides.push(record(4, "characters", s("a")));
        i.overrides.push(record(4, "characters", s("b")));
        assert_eq!(
            run(&i),
            Err(E::AmbiguousSource { node_id: id("SLYR", 4), field: "characters".into() })
        );
        let mut i = instance(1, &large);
        i.overrides.push(record(99, "characters", s("a")));
        assert_eq!(run(&i), Err(E::NoSuchNode(id("SLYR", 99))));
        // A definition node with a literal AND a variable binding for one field cannot be registered.
        let mut both = node(40, "Both", None);
        both.fields.insert("fills".into(), Value::Color([0.0; 4]));
        both.variable_bindings.insert("fills".into(), id("SVAR", 1));
        assert_eq!(
            DefRegistry::new().add_component(plain(40, vec![both])),
            Err(E::AmbiguousSource { node_id: id("SLYR", 40), field: "fills".into() })
        );

        // Unknown instance property and a definition reference to a property nobody defines.
        let mut i = instance(1, &large);
        i.component_properties.insert(
            "nope".into(),
            InstanceProp { ty: PropType::Text, value: s("x"), bound_variable_id: None },
        );
        assert_eq!(run(&i), Err(E::UnsupportedProperty("nope".into())));
        let mut ghost = node(41, "Ghost", None);
        ghost.property_references.insert("visible".into(), "ghost".into());
        let mut d2 = DefRegistry::new();
        d2.add_component(plain(42, vec![ghost])).unwrap();
        assert_eq!(
            resolve_instance(&instance(2, &id("SCMP", 42)), &d2, &vars, &no_modes(), 1, &limits, &cancel),
            Err(E::UnsupportedProperty("ghost".into()))
        );
        // A property of the wrong type for its definition is a type mismatch.
        let mut i = instance(1, &large);
        i.component_properties.insert(
            "showIcon".into(),
            InstanceProp { ty: PropType::Boolean, value: s("yes"), bound_variable_id: None },
        );
        assert_eq!(run(&i), Err(E::TypeMismatch("showIcon".into())));

        // A mode with no stored value is an error everywhere, never zero or black.
        let mut v2 = VarRegistry::new();
        v2.add_collection(two_mode_collection(2, "a", "b")).unwrap();
        v2.add_variable(color_var(20, 2, &[("a", [1.0, 0.0, 0.0, 1.0])])).unwrap();
        let missing = E::MissingModeValue { variable_id: id("SVAR", 20), mode_id: "b".into() };
        assert_eq!(v2.validate(), Err(missing.clone()));
        let mut painted = node(43, "Painted", None);
        painted.variable_bindings.insert("fills".into(), id("SVAR", 20));
        let mut d3 = DefRegistry::new();
        d3.add_component(plain(44, vec![painted])).unwrap();
        let i = instance(3, &id("SCMP", 44));
        assert!(resolve_instance(&i, &d3, &v2, &modes(&[(2, "a")]), 1, &limits, &cancel).is_ok());
        assert_eq!(
            resolve_instance(&i, &d3, &v2, &modes(&[(2, "b")]), 1, &limits, &cancel),
            Err(missing)
        );

        // Variant rules: unknown value, deleted main, and set completeness/uniqueness at registration.
        let mut i = instance(1, &id("SCMP", 10));
        i.component_properties.insert("size".into(), variant_prop("huge"));
        assert_eq!(
            run(&i),
            Err(E::UnknownVariantValue { property: "size".into(), value: "huge".into() })
        );
        let mut i = instance(1, &large);
        i.main_component_id = None;
        assert_eq!(run(&i), Err(E::NoSuchComponent("deleted-main".into())));
        let set = |members: Vec<ComponentDef>| ComponentSet {
            component_id: id("SCMP", 50),
            default_variant_id: id("SCMP", 51),
            variant_group_properties: BTreeMap::from([
                ("size".to_owned(), vec!["small".to_owned(), "large".to_owned()]),
                ("tone".to_owned(), vec!["a".to_owned()]),
            ]),
            property_definitions: vec![],
            members,
        };
        let with_tone = |n: u32, size: &str| ComponentDef {
            variant_properties: Some(BTreeMap::from([
                ("size".to_owned(), size.to_owned()),
                ("tone".to_owned(), "a".to_owned()),
            ])),
            ..plain(n, vec![])
        };
        assert_eq!(
            DefRegistry::new().add_set(set(vec![with_tone(51, "small"), member(52, "large", vec![])])),
            Err(E::VariantAssignmentIncomplete(id("SCMP", 52)))
        );
        assert_eq!(
            DefRegistry::new().add_set(set(vec![with_tone(51, "small"), with_tone(52, "small")])),
            Err(E::VariantAssignmentDuplicate(id("SCMP", 52)))
        );
    });
}

#[test]
fn constraints_hand_computed() {
    bounded_case("constraints_hand_computed", || {
        use ConstraintKind::{Center, Max, Min, Scale, Stretch};
        let cons = |h, v| Constraints { horizontal: h, vertical: v };
        // Horizontal: child x=10, w=20 in a parent going 100 -> 200 wide (vertical untouched).
        let child = RectPt { x: 10.0, y: 5.0, w: 20.0, h: 10.0 };
        let same = SizePt { w: 100.0, h: 50.0 };
        let wide = SizePt { w: 200.0, h: 50.0 };
        for (kind, x, w) in [
            (Min, 10.0, 20.0),
            (Max, 110.0, 20.0),    // right offset 100-30=70 kept: 200-70-20
            (Center, 60.0, 20.0),  // centre offset 20-50=-30 kept: 100-30-10
            (Stretch, 10.0, 120.0), // offsets 10 and 70 kept: 200-10-70
            (Scale, 20.0, 40.0),   // 10/100 and 20/100 of the parent
        ] {
            let r = apply_constraints(child, same, wide, cons(kind, Min)).unwrap();
            assert_eq!(r, RectPt { x, y: 5.0, w, h: 10.0 }, "{kind:?}");
        }
        // Vertical: child y=5, h=10 in a parent going 50 -> 80 tall (horizontal untouched).
        let tall = SizePt { w: 100.0, h: 80.0 };
        for (kind, y, h) in [
            (Min, 5.0, 10.0),
            (Max, 35.0, 10.0),     // bottom offset 50-15=35: 80-35-10
            (Center, 20.0, 10.0),  // centre 10 vs 25 => -15: 40-15=25 => y 20
            (Stretch, 5.0, 40.0),  // offsets 5 and 35: 80-5-35
            (Scale, 8.0, 16.0),    // 5*80/50 and 10*80/50
        ] {
            let r = apply_constraints(child, same, tall, cons(Min, kind)).unwrap();
            assert_eq!(r, RectPt { x: 10.0, y, w: 20.0, h }, "{kind:?}");
        }
        // Both axes at once, and the STU-DS-152 default (top-left) leaves the rectangle alone.
        let both = apply_constraints(child, same, SizePt { w: 200.0, h: 80.0 }, cons(Max, Scale)).unwrap();
        assert_eq!(both, RectPt { x: 110.0, y: 8.0, w: 20.0, h: 16.0 });
        assert_eq!(Constraints::default(), cons(Min, Min));
        assert_eq!(apply_constraints(child, same, wide, Constraints::default()), Ok(child));

        // Invalid geometry is refused, never clamped.
        let bad = |c: RectPt, old: SizePt, new: SizePt, k: Constraints| apply_constraints(c, old, new, k);
        assert_eq!(bad(RectPt { w: -1.0, ..child }, same, wide, cons(Min, Min)), Err(E::InvalidGeometry));
        assert_eq!(bad(RectPt { x: f64::NAN, ..child }, same, wide, cons(Min, Min)), Err(E::InvalidGeometry));
        assert_eq!(bad(child, SizePt { w: 0.0, h: 50.0 }, wide, cons(Scale, Min)), Err(E::InvalidGeometry));
        // Stretch into a parent of width 10: 20 + (10 - 100) < 0 => refused.
        assert_eq!(bad(child, same, SizePt { w: 10.0, h: 50.0 }, cons(Stretch, Min)), Err(E::InvalidGeometry));

        // Hierarchy: root 100x50 -> 200x80. A (stretch/stretch) 10,5 60x40 becomes 10,5 160x70
        // (60+100, 40+30); B (max/max) in A at 5,5 10x10 moves by A's growth (100,30): 105,35.
        let geo = |r: RectPt, c: Constraints| Some(NodeGeometry { rect: r, constraints: c });
        let mut a = node(1, "A", None);
        a.geometry = geo(RectPt { x: 10.0, y: 5.0, w: 60.0, h: 40.0 }, cons(Stretch, Stretch));
        let mut b = node(2, "B", Some(1));
        b.geometry = geo(RectPt { x: 5.0, y: 5.0, w: 10.0, h: 10.0 }, cons(Max, Max));
        let c = node(3, "C-no-geometry", None);
        let def = ComponentDef { size: Some(same), ..plain(1, vec![a, b, c]) };
        let cancel = CancellationToken::default();
        let placed = resize_component(&def, SizePt { w: 200.0, h: 80.0 }, &cancel).unwrap();
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[0].rect, RectPt { x: 10.0, y: 5.0, w: 160.0, h: 70.0 });
        assert_eq!(placed[1].rect, RectPt { x: 105.0, y: 35.0, w: 10.0, h: 10.0 });
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(resize_component(&def, wide, &stop), Err(E::Canceled));
    });
}

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

fn parent_artboard(n: u32) -> folio::Parent {
    folio::Parent::Node(folio::NodeRef { node_kind: folio::NodeKind::Artboard, node_id: id("SART", n) })
}
fn folio_document() -> folio::StudioDocument {
    let len = |v: f64| folio::Length { value: v, unit: folio::GeometryUnit::Pt };
    let button = folio::StudioLayer {
        schema_id: folio::LayerSchema::V1,
        layer_id: id("SLYR", 1),
        name: "Button".into(),
        parent: parent_artboard(1),
        kind: "group".into(),
        payload: folio::Payload::Group,
        visible: true,
        appearance: None,
        text_frame: None,
    };
    let card = instance_layer(&id("SLYR", 2), "Card", parent_artboard(2), &id("SCIN", 1)).unwrap();
    folio::StudioDocument {
        schema_id: folio::DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: folio::GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: folio::ProfileBinding::Unassigned(()),
        working_space_cmyk: folio::ProfileBinding::Unassigned(()),
        working_space_gray: folio::ProfileBinding::Unassigned(()),
        working_space_spot: folio::ProfileBinding::Unassigned(()),
        blending_space: folio::ProfileBinding::Unassigned(()),
        artboards: (1..=4)
            .map(|n| folio::StudioArtboard {
                schema_id: folio::ArtboardSchema::V1,
                artboard_id: id("SART", n),
                name: format!("Frame {n}"),
                parent: folio::Parent::Root(()),
                bounds: folio::Bounds { x: len(0.0), y: len(0.0), width: len(100.0), height: len(100.0) },
            })
            .collect(),
        page_spreads: vec![],
        layers: vec![button, card],
        graph: folio::StudioLayerGraph {
            schema_id: folio::GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}

#[test]
fn folio_shape_and_prototype_graph() {
    bounded_case("folio_shape_and_prototype_graph", || {
        let token = CancellationToken::default();
        let doc = folio_document();
        // The instance layer is a Folio layer: valid when the host resolver accepts the binding...
        let snap = folio::validate_document(
            doc.clone(), folio::Budget::default(), &token, &Host(folio::Resolution::Available),
        )
        .unwrap();
        assert!(snap.resolution_issues().is_empty());
        // ...an unresolved instance record is reported (not hidden)...
        let snap_absent = folio::validate_document(
            doc.clone(), folio::Budget::default(), &token, &Host(folio::Resolution::Absent),
        )
        .unwrap();
        assert_eq!(snap_absent.resolution_issues().len(), 1);
        assert_eq!(snap_absent.resolution_issues()[0].target, id("SLYR", 2));
        // ...and a resolver that does not know the binding rejects the document.
        let err = folio::validate_document(doc.clone(), folio::Budget::default(), &token, &folio::Unresolved)
            .unwrap_err();
        assert_eq!(err.code, folio::Code::UnsupportedDescriptor);
        assert!(instance_layer(&id("SLYR", 2), "x", folio::Parent::Root(()), &id("SCMP", 1)).is_err());

        let (f1, f2, f3, f4) = (id("SART", 1), id("SART", 2), id("SART", 3), id("SART", 4));
        let to = |dest: &str, navigation, pos| Action::Node {
            destination_id: Some(dest.to_owned()),
            navigation,
            overlay_relative_position: pos,
        };
        let react = |node: &str, trigger, actions| Reaction { node_id: node.to_owned(), trigger, actions };
        let flow = PrototypeFlow {
            flow_id: id("SPTF", 1),
            start_frame_id: f1.clone(),
            reactions: vec![
                // Button on Frame 1 -> Frame 2.
                react(&id("SLYR", 1), Some(Trigger::OnClick), vec![to(&f2, Navigation::Navigate, None)]),
                // Card (instance) on Frame 2 -> overlay Frame 3, then a Back that can never run.
                react(
                    &id("SLYR", 2),
                    Some(Trigger::OnClick),
                    vec![to(&f3, Navigation::Overlay, Some((10.0, 20.0))), Action::Back],
                ),
                // Frame 3 itself -> back to Frame 1: a cycle, legal.
                react(&f3, Some(Trigger::OnPress), vec![to(&f1, Navigation::Navigate, None)]),
                // Authored but not armed (null trigger): Frame 4 stays unreachable.
                react(&id("SLYR", 1), None, vec![to(&f4, Navigation::Navigate, None)]),
            ],
        };
        let limits = hsk_studio_components::FlowLimits::default();
        let report = validate_flow(&flow, snap.document(), &limits, &token).unwrap();
        assert_eq!(report.reachable_frames, vec![f1.clone(), f2.clone(), f3.clone()]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!((report.warnings[0].reaction_index, report.warnings[0].action_index), (1, 1));

        // Destinations are checked against the document's frames.
        let with = |a: Action| PrototypeFlow {
            reactions: vec![react(&id("SLYR", 1), Some(Trigger::OnClick), vec![a])],
            ..flow.clone()
        };
        let run = |f: &PrototypeFlow| validate_flow(f, snap.document(), &limits, &token);
        assert_eq!(run(&with(to(&id("SART", 9), Navigation::Navigate, None))), Err(E::PrototypeTarget(id("SART", 9))));
        assert_eq!(run(&with(to(&id("SLYR", 1), Navigation::Navigate, None))), Err(E::PrototypeTarget(id("SLYR", 1))));
        assert_eq!(
            run(&with(Action::Node { destination_id: None, navigation: Navigation::Navigate, overlay_relative_position: None })),
            Err(E::PrototypeTarget("null".into()))
        );
        assert_eq!(
            run(&with(to(&f2, Navigation::ChangeTo, None))),
            Err(E::UnsupportedPrototype("ChangeTo".into()))
        );
        assert_eq!(
            run(&PrototypeFlow { start_frame_id: id("SART", 8), ..flow.clone() }),
            Err(E::PrototypeTarget(id("SART", 8)))
        );
        let missing = PrototypeFlow {
            reactions: vec![react(&id("SLYR", 77), Some(Trigger::OnClick), vec![Action::Close])],
            ..flow.clone()
        };
        assert_eq!(run(&missing), Err(E::NoSuchNode(id("SLYR", 77))));
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(validate_flow(&flow, snap.document(), &limits, &stop), Err(E::Canceled));
    });
}
