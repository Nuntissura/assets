//! Components-local real consumer: a Button set (`small` 100 pt / `large` 200 pt wide), a brand
//! colour variable with `light`/`dark` modes and a Label `characters` field. `--size`, `--mode` and
//! `--text` pick the variant, the mode and a local text override; the resolved fields and the
//! write/read footprint of the text edit print on stdout. A stable error code goes to stderr
//! (exit 2). `--descriptor` prints the crate descriptor; `--cancel` cancels before the call.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_components::{
    Collection, ComponentDef, ComponentSet, ComponentsError, DESCRIPTOR, DefNode, DefRegistry,
    Instance, InstanceProp, Limits, Mode, OverrideEdit, PropType, PropertyDef, VarEntry,
    VarRegistry, VarType, Variable, Value, propose_overrides, report,
};
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

fn run(args: &[String], cancel: &CancellationToken) -> Result<String, ComponentsError> {
    let size = arg(args, "--size").unwrap_or("large");
    let mode = arg(args, "--mode").unwrap_or("dark");
    let text = arg(args, "--text").unwrap_or("Buy now");
    let mut vars = VarRegistry::new();
    vars.add_collection(Collection {
        collection_id: id("SVCL", 1),
        modes: ["light", "dark"]
            .map(|m| Mode { mode_id: m.into(), name: m.into() })
            .to_vec(),
        default_mode_id: "light".into(),
    })?;
    let color = |c: [f64; 4]| VarEntry::Literal(Value::Color(c));
    vars.add_variable(Variable {
        variable_id: id("SVAR", 1),
        collection_id: id("SVCL", 1),
        resolved_type: VarType::Color,
        values_by_mode: BTreeMap::from([
            ("light".to_owned(), color([0.0, 0.0, 1.0, 1.0])),
            ("dark".to_owned(), color([0.5, 0.5, 1.0, 1.0])),
        ]),
    })?;
    let button = |root: u32, label: u32, size: &str, width: f64| {
        let mut r = DefNode::new(&id("SLYR", root), "Button", None);
        r.fields.insert("width".into(), Value::Float(width));
        r.variable_bindings.insert("fills".into(), id("SVAR", 1));
        let mut l = DefNode::new(&id("SLYR", label), "Label", Some(id("SLYR", root).as_str()));
        l.fields.insert("characters".into(), Value::String("Label".into()));
        ComponentDef {
            component_id: id("SCMP", root + 10),
            variant_properties: Some(BTreeMap::from([("size".to_owned(), size.to_owned())])),
            property_definitions: Vec::<PropertyDef>::new(),
            nodes: vec![r, l],
            size: None,
        }
    };
    let mut defs = DefRegistry::new();
    defs.add_set(ComponentSet {
        component_id: id("SCMP", 1),
        default_variant_id: id("SCMP", 11),
        variant_group_properties: BTreeMap::from([(
            "size".to_owned(),
            vec!["small".to_owned(), "large".to_owned()],
        )]),
        property_definitions: vec![],
        members: vec![button(1, 2, "small", 100.0), button(3, 4, "large", 200.0)],
    })?;
    let label = if size == "small" { 2 } else { 4 };
    let inst = Instance {
        instance_id: id("SCIN", 1),
        main_component_id: Some(id("SCMP", 1)),
        component_properties: BTreeMap::from([(
            "size".to_owned(),
            InstanceProp {
                ty: PropType::Variant,
                value: Value::String(size.to_owned()),
                bound_variable_id: None,
            },
        )]),
        overrides: vec![],
    };
    let modes = BTreeMap::from([(id("SVCL", 1), mode.to_owned())]);
    let edit = OverrideEdit {
        node_id: id("SLYR", label),
        field: "characters".into(),
        value: Value::String(text.to_owned()),
    };
    let p = propose_overrides(&inst, &[edit], &defs, &vars, &modes, 1, &Limits::default(), cancel)?;
    let mut out = format!("component={}\n", p.resolution.component_id);
    for f in &p.resolution.fields {
        out += &format!("{} {} = {:?} <- {:?}\n", f.node_id, f.field, f.value, f.source);
    }
    out += &format!("write_footprint={:?}\nread_footprint={:?}", p.write_footprint, p.read_footprint);
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
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
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
            eprintln!("rejected:{}; consult --descriptor and correct only the caller input; no host proof", e.code());
            std::process::exit(2)
        }
        (Ok(_), Err(e)) => {
            eprintln!("rejected:{}", e.code());
            std::process::exit(2)
        }
    }
}
