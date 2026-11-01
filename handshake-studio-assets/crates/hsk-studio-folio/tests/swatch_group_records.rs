//! Lane SMB-COL-SWATCH-GROUPS-1, MT-31239 (STU-COL-177) and the group operations of MT-31241
//! (STU-COL-179): swatch group records in `StudioVectorRecords`, their validation rules and the
//! attributed, atomic, undoable group commands.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn swatch(n: u32, group: Option<&str>) -> Value {
    let mut v = json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",n),"name":format!("Swatch {n}"),"swatch_kind":"process","color_model":"RGB","components":[0.1,0.2,0.3]});
    if let Some(g) = group {
        v["group_ref"] = json!(g);
    }
    v
}
/// Root group `root`, named groups `warm` and `cool`; swatches 1 (root), 2 and 3 (warm), 4 (cool).
fn grouped() -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "swatches":[swatch(1,None),swatch(2,Some("warm")),swatch(3,Some("warm")),swatch(4,Some("cool"))],
        "swatch_groups":[{"group_id":"root","name":"Swatches","root":true},{"group_id":"warm","name":"Warm"},{"group_id":"cool","name":"Cool"}]})
}
fn records(v: &Value) -> StudioVectorRecords {
    serde_json::from_value(v.clone()).expect("records shape")
}
fn check(v: &Value) -> Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidField)?;
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}
fn ctx(command: &str, revision: u64) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), revision, CorrelationId::new(41), None)
}
fn run(r: &StudioVectorRecords, cmd: StudioSwatchGroupCommand) -> Result<StudioSwatchGroupOutcome<StudioVectorRecords>, StudioSwatchGroupError> {
    apply_swatch_group_command(r, &ctx(cmd.command_id(), 7), 7, &cmd)
}
/// `(group, [swatch numbers in member order])` read back from the serialized records, root first.
fn membership(r: &StudioVectorRecords) -> Vec<(String, Vec<String>)> {
    let v = serde_json::to_value(r).unwrap();
    let groups = v["swatch_groups"].as_array().cloned().unwrap_or_default();
    let swatches = v["swatches"].as_array().cloned().unwrap_or_default();
    groups
        .iter()
        .map(|g| {
            let id = g["group_id"].as_str().unwrap().to_string();
            let root = g["root"].as_bool().unwrap_or(false);
            let members = swatches
                .iter()
                .filter(|s| if root { s.get("group_ref").is_none() } else { s["group_ref"].as_str() == Some(id.as_str()) })
                .map(|s| s["name"].as_str().unwrap().trim_start_matches("Swatch ").to_string())
                .collect();
            (id, members)
        })
        .collect()
}
fn m(v: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    v.iter().map(|(g, s)| (g.to_string(), s.iter().map(|x| x.to_string()).collect())).collect()
}

#[test]
fn group_records_round_trip_and_enforce_one_root_and_resolvable_membership() {
    // Absent groups: byte-identical to a record set written before them.
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    // A grouped palette validates and round-trips value for value.
    let g = grouped();
    assert_eq!(check(&g), Ok(()));
    assert_eq!(serde_json::to_value(records(&g)).unwrap(), g);
    assert_eq!(membership(&records(&g)), m(&[("root", &["1"]), ("warm", &["2", "3"]), ("cool", &["4"])]));
    let edit = |f: &dyn Fn(&mut Value)| {
        let mut v = grouped();
        f(&mut v);
        check(&v)
    };
    // Exactly one root: a second root, a missing root, a root that is not first.
    assert_eq!(edit(&|v| v["swatch_groups"][2]["root"] = json!(true)), Err(Code::DuplicateIdentity), "second root");
    assert_eq!(edit(&|v| v["swatch_groups"][0]["root"] = json!(false)), Err(Code::MissingField), "no root");
    assert_eq!(edit(&|v| { v["swatch_groups"].as_array_mut().unwrap().swap(0, 1); }), Err(Code::InvalidField), "root listed first");
    // A swatch belongs to at most one group: group_ref names one existing named group.
    assert_eq!(edit(&|v| v["swatches"][1]["group_ref"] = json!("gone")), Err(Code::DanglingReference), "dangling group_ref");
    assert_eq!(edit(&|v| v["swatches"][1]["group_ref"] = json!("root")), Err(Code::InvalidField), "root membership is an absent group_ref");
    assert_eq!(edit(&|v| v["swatch_groups"][2]["group_id"] = json!("warm")), Err(Code::DuplicateIdentity), "group ids unique");
    assert_eq!(edit(&|v| v["swatch_groups"][1]["name"] = json!("")), Err(Code::InvalidField), "named groups carry a name");
    assert_eq!(edit(&|v| v["swatch_groups"][1]["group_id"] = json!("not a key")), Err(Code::InvalidId));
    // Groups do not nest: a parent member is an unknown field.
    assert_eq!(edit(&|v| v["swatch_groups"][1]["parent"] = json!("cool")), Err(Code::InvalidField), "no nesting member");
    // With no groups listed, only the implicit root exists: any group_ref dangles.
    let mut v = grouped();
    v.as_object_mut().unwrap().remove("swatch_groups");
    assert_eq!(check(&v), Err(Code::DanglingReference));
}

#[test]
fn group_list_is_budgeted() {
    let mut groups = vec![json!({"group_id":"root","name":"Swatches","root":true})];
    groups.extend((0..SWATCH_GROUPS_MAX).map(|i| json!({"group_id":format!("g{i}"),"name":"G"})));
    let v = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),"swatch_groups":groups});
    assert_eq!(check(&v), Err(Code::BudgetExceeded), "aggregate over the group budget");
    let r = records(&grouped());
    let small = Budget { nodes: 6, ..Budget::default() };
    assert_eq!(validate_vector_records(&r, &document(), None, small, &CancellationToken::default()).map_err(|d| d.code), Err(Code::BudgetExceeded), "groups charged against Budget::nodes");
}

#[test]
fn group_operations_mutate_membership_and_order_and_undo_restores_them() {
    let start = records(&grouped());
    // Create a named group at index 0 (right after the root).
    let o = run(&start, StudioSwatchGroupCommand::Create { group_id: "brand".into(), name: "Brand".into(), index: Some(0) }).unwrap();
    assert_eq!(membership(&o.palette), m(&[("root", &["1"]), ("brand", &[]), ("warm", &["2", "3"]), ("cool", &["4"])]));
    assert_eq!((o.report.command, o.report.groups, o.report.correlation, o.report.base_revision), ("swatch.group.create", 4, 41, 7));
    // Move swatch 3 into brand, then swatch 1 into brand ahead of it.
    let o = run(&o.palette, StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 3), group_id: "brand".into(), index: None }).unwrap();
    assert_eq!(o.report.swatches_moved, 1);
    let o = run(&o.palette, StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 1), group_id: "brand".into(), index: Some(0) }).unwrap();
    assert_eq!(membership(&o.palette), m(&[("root", &[]), ("brand", &["1", "3"]), ("warm", &["2"]), ("cool", &["4"])]));
    // Reorder within a group (move to the end of its own group).
    let o = run(&o.palette, StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 1), group_id: "brand".into(), index: None }).unwrap();
    assert_eq!(o.report.swatches_moved, 0, "a reorder inside one group changes no membership");
    assert_eq!(membership(&o.palette), m(&[("root", &[]), ("brand", &["3", "1"]), ("warm", &["2"]), ("cool", &["4"])]));
    // Rename and reorder groups.
    let o = run(&o.palette, StudioSwatchGroupCommand::Rename { group_id: "cool".into(), name: "Cool tones".into() }).unwrap();
    let o = run(&o.palette, StudioSwatchGroupCommand::Reorder { group_id: "cool".into(), index: 0 }).unwrap();
    assert_eq!(membership(&o.palette), m(&[("root", &[]), ("cool", &["4"]), ("brand", &["3", "1"]), ("warm", &["2"])]));
    assert_eq!(o.palette.swatch_groups[1].name, "Cool tones");
    // Delete brand with warm as the replace-on-delete target: members follow warm's own, in order.
    let before_delete = o.palette.clone();
    let o = run(&o.palette, StudioSwatchGroupCommand::Delete { group_id: "brand".into(), replace_with: "warm".into() }).unwrap();
    assert_eq!(o.report.swatches_moved, 2);
    assert_eq!(membership(&o.palette), m(&[("root", &[]), ("cool", &["4"]), ("warm", &["2", "3", "1"])]));
    assert_eq!(validate_vector_records(&o.palette, &document(), None, Budget::default(), &CancellationToken::default()), Ok(()));
    // Delete into the root: members become root members (absent group_ref).
    let into_root = run(&o.palette, StudioSwatchGroupCommand::Delete { group_id: "warm".into(), replace_with: "root".into() }).unwrap();
    assert_eq!(membership(&into_root.palette), m(&[("root", &["2", "3", "1"]), ("cool", &["4"])]));
    // Undo restores the prior membership and order exactly (not orphaned swatches).
    let undo = undo_swatch_group_command(&o.palette, &ctx("swatch.group.undo", 7), 7, &o.undo).unwrap();
    assert_eq!(undo.palette, before_delete);
    assert_eq!(undo.report.swatches_moved, 2);
    // The serialized document keeps exactly one root after every step.
    for r in [&o.palette, &into_root.palette, &undo.palette] {
        assert_eq!(r.swatch_groups.iter().filter(|g| g.root).count(), 1);
    }
}

#[test]
fn group_commands_refuse_without_mutation() {
    let start = records(&grouped());
    let refuse = |cmd: StudioSwatchGroupCommand| run(&start, cmd).unwrap_err();
    assert_eq!(refuse(StudioSwatchGroupCommand::Delete { group_id: "root".into(), replace_with: "warm".into() }), StudioSwatchGroupError::RootNotDeletable);
    assert_eq!(refuse(StudioSwatchGroupCommand::CreateRoot { group_id: "root2".into(), name: "Again".into() }), StudioSwatchGroupError::SecondRoot);
    assert_eq!(refuse(StudioSwatchGroupCommand::Delete { group_id: "warm".into(), replace_with: "warm".into() }), StudioSwatchGroupError::InvalidReplacement);
    assert_eq!(refuse(StudioSwatchGroupCommand::Delete { group_id: "warm".into(), replace_with: "gone".into() }), StudioSwatchGroupError::InvalidReplacement);
    assert_eq!(refuse(StudioSwatchGroupCommand::Delete { group_id: "gone".into(), replace_with: "root".into() }), StudioSwatchGroupError::GroupMissing);
    assert_eq!(refuse(StudioSwatchGroupCommand::Create { group_id: "warm".into(), name: "Dup".into(), index: None }), StudioSwatchGroupError::DuplicateGroup);
    assert_eq!(refuse(StudioSwatchGroupCommand::Create { group_id: "new".into(), name: "".into(), index: None }), StudioSwatchGroupError::InvalidName);
    assert_eq!(refuse(StudioSwatchGroupCommand::Create { group_id: "new".into(), name: "N".into(), index: Some(3) }), StudioSwatchGroupError::IndexOutOfRange);
    assert_eq!(refuse(StudioSwatchGroupCommand::Reorder { group_id: "root".into(), index: 1 }), StudioSwatchGroupError::RootNotMovable);
    assert_eq!(refuse(StudioSwatchGroupCommand::Reorder { group_id: "warm".into(), index: 2 }), StudioSwatchGroupError::IndexOutOfRange);
    assert_eq!(refuse(StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 9), group_id: "warm".into(), index: None }), StudioSwatchGroupError::SwatchMissing);
    assert_eq!(refuse(StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 1), group_id: "warm".into(), index: Some(5) }), StudioSwatchGroupError::IndexOutOfRange);
    // Attribution: the command id, the base revision.
    let cmd = StudioSwatchGroupCommand::Rename { group_id: "warm".into(), name: "W".into() };
    assert_eq!(apply_swatch_group_command(&start, &ctx("swatch.group.delete", 7), 7, &cmd).unwrap_err(), StudioSwatchGroupError::CommandMismatch);
    assert_eq!(apply_swatch_group_command(&start, &ctx("swatch.group.rename", 6), 7, &cmd).unwrap_err(), StudioSwatchGroupError::StaleRevision { expected: 6, current: 7 });
    let anonymous = CommandContext::new(ctx("x", 7).actor().clone(), ClientSessionId::parse("w").unwrap(), None, 7, CorrelationId::new(1), None);
    assert_eq!(apply_swatch_group_command(&start, &anonymous, 7, &cmd).unwrap_err().code(), "command_missing");
    // An implicit-root palette needs its root before named groups.
    let bare = StudioVectorRecords::new(identity("SDOC", 1));
    assert_eq!(run(&bare, StudioSwatchGroupCommand::Create { group_id: "warm".into(), name: "Warm".into(), index: None }).unwrap_err(), StudioSwatchGroupError::RootMissing);
    let rooted = run(&bare, StudioSwatchGroupCommand::CreateRoot { group_id: "root".into(), name: "Swatches".into() }).unwrap();
    assert_eq!(rooted.palette.swatch_groups.len(), 1);
    // The input never changed.
    assert_eq!(serde_json::to_value(&start).unwrap(), grouped());
}
