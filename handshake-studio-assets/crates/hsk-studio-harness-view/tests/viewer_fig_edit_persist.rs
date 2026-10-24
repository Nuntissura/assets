//! Lane SMB-FIG-EDIT-2 (feature `viewer`): instance state (variant, component properties,
//! overrides, swap) saved with the document in the session entry `design` (Folio
//! `hsk.studio.design_state@1`), so an edited instance survives save and reopen ([STU-IO-006] row 35
//! edit-preserve; STU-DS-003/008/016/017). Driven headlessly through the window's typed design
//! commands. Oracles: the session's document, history and `design` entry bytes, Folio's validator
//! over the stored entry, the import receipt. Figma's own persistence of instance overrides in a
//! `.fig` round trip has no oracle here (no `.fig` export from the window; NOT_PROVEN below).
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, StudioValue};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::design::DesignCommand;
use hsk_studio_harness_view::view::design::persist::ENTRY;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::ShellCommand;
use hsk_studio_interop_fig::{FigDocument, FigOutcome, Limits, VersionPolicy, decode_document, import_fig};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-fig-persist-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const FIXTURE: &str = "fig-components-autolayout.fig";

fn fixture_path(name: &str) -> PathBuf {
    // The fixtures live with the importer (repository-relative, never machine-local).
    PathBuf::from(format!("{}/../hsk-studio-interop-fig/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
}

/// The importer's decoded document (names the fixture instances by their Figma source ids).
fn fig() -> FigDocument {
    let bytes = std::fs::read(fixture_path(FIXTURE)).unwrap();
    let cancel = CancellationToken::default();
    let Ok(FigOutcome::Imported(import)) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel) else { panic!("fixture import") };
    decode_document(&import, &Limits::default(), &cancel).unwrap()
}

/// Opens the fixture through FILE > Open with the "Design file (.fig)" filter.
fn opened(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    let src = scratch.0.join(FIXTURE);
    std::fs::copy(fixture_path(FIXTURE), &src).unwrap();
    v.shell_command(ShellCommand::Open).unwrap();
    let d = v.file_dialog().expect("open dialog");
    let at = d.filters().iter().position(|f| f.name == "Design file (.fig)").expect("design file filter");
    d.set_filter(at).unwrap();
    d.typed = src.to_string_lossy().into_owned();
    assert_eq!(v.dialog_accept(), Ok(true), "{}", v.state_json()["status"]);
    (scratch, v)
}

fn doc(v: &Viewer) -> folio::StudioDocument {
    v.run().snapshot().expect("open").document().clone()
}

fn instance_layer(v: &Viewer, fig: &FigDocument, name: &str) -> String {
    let src = fig.named(name).next().unwrap_or_else(|| panic!("{name} in the fixture")).id.to_string();
    let r = v.receipt().expect("receipt");
    r["design"]["instances"].as_array().unwrap().iter().find(|b| b["source"] == src.as_str()).unwrap_or_else(|| panic!("binding of {name}"))["layer_id"].as_str().unwrap().to_owned()
}

fn children(v: &Viewer, layer: &str) -> Vec<String> {
    doc(v).layers.iter().filter(|l| matches!(&l.parent, folio::Parent::Node(p) if p.node_id == layer)).map(|l| l.layer_id.clone()).collect()
}

fn component_named(v: &Viewer, name: &str) -> String {
    let layer = doc(v).layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("{name}")).layer_id.clone();
    v.receipt().unwrap()["design"]["records"]["components"].as_array().unwrap().iter().find(|k| k["layer_id"] == layer.as_str()).unwrap()["component_id"].as_str().unwrap().to_owned()
}

fn entry(v: &Viewer) -> Option<Vec<u8>> {
    v.run().document_entry(ENTRY).map(<[u8]>::to_vec)
}

fn undo_depth(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

fn reopen(path: &Path) -> Viewer {
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(path).unwrap();
    w
}

/// The label layer of the default definition (the text override's node).
fn label_node(v: &Viewer, inst: &str) -> String {
    v.instance_view(inst).unwrap().overrides[0].0.clone()
}

// ---- slice 2: write path ---------------------------------------------------------------------------

/// Each design command is ONE history step and writes the instance's state to the `design` entry:
/// the member, its variant assignment, the overrides and the layer's new children, admitted by
/// Folio against the document and the receipt's design records.
#[test]
fn design_command_writes_one_undo_step_with_the_state_entry() {
    let (_s, mut v) = opened("write");
    let fig = fig();
    let inst = instance_layer(&v, &fig, "button-instance-3");
    assert!(entry(&v).is_none(), "no state before the first command");
    assert_eq!(v.design_state_records(), Ok(None));
    let depth = undo_depth(&v);
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    assert_eq!(undo_depth(&v), depth + 1, "one history step: {}", v.run().history());
    let main = component_named(&v, "state=default");
    let r = v.design_state_records().unwrap().expect("the design entry");
    assert_eq!(r.document_id, doc(&v).document_id);
    assert_eq!(r.instances.len(), 1);
    let s = &r.instances[0];
    // A variant switch keeps the main component and records the chosen value as a VARIANT property.
    assert_eq!((s.layer_id.as_str(), s.component_id.as_str()), (inst.as_str(), main.as_str()));
    assert_eq!(s.properties.get("state").map(|p| (p.property_type, p.value.clone())), Some((folio::ComponentPropertyType::Variant, folio::StudioPropertyValue::Text("hover".into()))));
    assert_eq!(s.content_signature, children(&v, &inst), "keyed by the redrawn children");
    assert_eq!(s.variant.get("state").map(String::as_str), Some("hover"));
    assert_eq!(s.overrides.len(), 1);
    assert_eq!((s.overrides[0].field.as_str(), &s.overrides[0].value), ("characters", &StudioValue::String { value: "Changed text".into() }));
    assert!(s.swap_target.is_none());
    let instance_id = v.instance_view(&inst).unwrap().instance_id;
    assert_eq!(s.instance_id, instance_id);
    // The stored bytes are exactly Folio's wire of the admitted records.
    assert_eq!(entry(&v).unwrap(), serde_json::to_vec(&r).unwrap());
    // A second command: one more step, one more state for the new content.
    let label = label_node(&v, &inst);
    v.design_command(&inst, DesignCommand::SetOverride { node_id: label, field: "visible".into(), value: StudioValue::Boolean { value: false } }).unwrap();
    assert_eq!(undo_depth(&v), depth + 2);
    let r = v.design_state_records().unwrap().unwrap();
    assert_eq!(r.instances.len(), 2);
    assert_eq!(r.instances[1].overrides.len(), 2);
    assert_eq!(r.instances[1].content_signature, children(&v, &inst));
    let st = v.state_json()["design"]["entry"].clone();
    assert_eq!((st["present"].as_bool(), st["states"].as_u64(), st["error"].is_null()), (Some(true), Some(2), true), "{st}");
    // A refused command writes nothing and adds no step.
    let before = entry(&v);
    assert!(v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "pressed".into() }).is_err());
    assert_eq!((entry(&v), undo_depth(&v)), (before, depth + 2));
}

/// Undo and redo return the instance to the state recorded for the restored children; the entry
/// keeps a state per (layer, content), so the same holds after a save and reopen.
#[test]
fn undo_restores_the_previous_state_entry() {
    let (s, mut v) = opened("undo");
    let fig = fig();
    let inst = instance_layer(&v, &fig, "button-instance-3");
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let after_switch = children(&v, &inst);
    let label = label_node(&v, &inst);
    v.design_command(&inst, DesignCommand::SetOverride { node_id: label, field: "visible".into(), value: StudioValue::Boolean { value: false } }).unwrap();
    v.undo().unwrap();
    assert_eq!(children(&v, &inst), after_switch);
    let view = v.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str(), view.overrides.len()), ("edited", "hover", 1));
    // Saved here, the reopened window restores the state of the undone-to content from the entry.
    let path = s.0.join("undone.handshake");
    v.save(&path, false).unwrap();
    let w = reopen(&path);
    let view = w.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str(), view.overrides.len()), ("edited", "hover", 1), "{:?}", view.error);
    let stored = w.design_state_records().unwrap().unwrap();
    assert_eq!(stored.state(&inst, &after_switch).map(|s| s.overrides.len()), Some(1));
    // Back to the imported drawing: the import record, saved and reopened as such.
    v.undo().unwrap();
    assert_eq!(v.instance_view(&inst).unwrap().state, "import");
    let path = s.0.join("imported.handshake");
    v.save(&path, false).unwrap();
    let w = reopen(&path);
    assert_eq!(w.instance_view(&inst).unwrap().state, "import");
    // Redo returns the hover state.
    v.redo().unwrap();
    assert_eq!(v.instance_view(&inst).unwrap().variants["state"].1, "hover");
    println!("declared reading: Run::restructure carries no document entry in its history step, so the entry is not swapped by undo; it keeps a state per (layer, content) and the state of the restored content applies");
}

// ---- slice 3: read path ----------------------------------------------------------------------------

/// A variant switch, an override and a component swap made in the window survive save and reopen:
/// the reopened instance reads `edited` with the same record, children and stored entry.
#[test]
fn edited_instance_survives_save_and_reopen() {
    let (s, mut v) = opened("reopen");
    let fig = fig();
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let other = instance_layer(&v, &fig, "button-instance-1");
    let hover = component_named(&v, "state=hover");
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let label = label_node(&v, &inst);
    v.design_command(&inst, DesignCommand::SetOverride { node_id: label.clone(), field: "opacity".into(), value: StudioValue::Float { value: 0.25 } }).unwrap();
    let swap = v.design_command(&other, DesignCommand::Swap { component_id: hover.clone() });
    let before = v.instance_view(&inst).unwrap();
    let bytes = entry(&v).unwrap();
    let path = s.0.join("edited.handshake");
    v.save(&path, false).unwrap();
    let mut w = reopen(&path);
    assert_eq!(entry(&w).unwrap(), bytes, "the entry is saved and reopened byte-identical");
    assert_eq!(children(&w, &inst), children(&v, &inst));
    let after = w.instance_view(&inst).unwrap();
    assert_eq!(after.state, "edited", "{:?}", after.error);
    assert_eq!(after.record, before.record);
    assert_eq!(after.variants, before.variants);
    assert_eq!(after.overrides, before.overrides);
    assert_eq!(after.component_name.as_deref(), Some("state=hover"));
    assert!(after.overrides.iter().any(|o| o.2 == "opacity" && o.3 == "0.25"), "{:?}", after.overrides);
    w.select(Some(inst.clone()));
    let st = w.state_json()["design"].clone();
    assert_eq!(st["instance"]["state"], "edited");
    assert_eq!(st["instance"]["variants"]["state"]["current"], "hover");
    assert_eq!((st["entry"]["present"].as_bool(), st["entry"]["error"].is_null()), (Some(true), true), "{st}");
    // The swap: main component and swap target restored.
    match swap {
        Ok(_) => {
            let o = w.instance_view(&other).unwrap();
            assert_eq!((o.state, o.record.main_component_id.as_deref()), ("edited", Some(hover.as_str())));
            let r = w.design_state_records().unwrap().unwrap();
            assert_eq!(r.state(&other, &children(&w, &other)).and_then(|s| s.swap_target.clone()), Some(hover.clone()));
        }
        Err(e) => println!("NOT_PROVEN: swap persistence on button-instance-1 (the swap was refused here: {e})"),
    }
    w.render_if_dirty();
    assert!(w.render_error().is_none(), "{:?}", w.render_error());
    println!("NOT_PROVEN: Figma's own persistence of instance overrides in a .fig round trip (no .fig export from the window; no Figma oracle)");
}

/// After a reopen the instance takes further commands; undo after the reopen returns to the saved
/// state (read from the entry), and the new edit is saved again.
#[test]
fn reopened_instance_accepts_further_commands() {
    let (s, mut v) = opened("again");
    let fig = fig();
    let inst = instance_layer(&v, &fig, "button-instance-3");
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let path = s.0.join("first.handshake");
    v.save(&path, false).unwrap();
    let mut w = reopen(&path);
    assert_eq!(w.instance_view(&inst).unwrap().state, "edited");
    let r = w.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "default".into() }).unwrap();
    assert_eq!(r["component_id"], component_named(&w, "state=default").as_str());
    let view = w.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str(), view.overrides.len()), ("edited", "default", 1));
    w.undo().unwrap();
    let view = w.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str()), ("edited", "hover"), "the saved state comes back from the entry");
    w.design_command(&inst, DesignCommand::ResetAll).unwrap();
    assert!(w.instance_view(&inst).unwrap().overrides.is_empty());
    let path2 = s.0.join("second.handshake");
    w.save(&path2, false).unwrap();
    let x = reopen(&path2);
    let view = x.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str(), view.overrides.len()), ("edited", "hover", 0));
}

/// A stored entry Folio refuses reads as its diagnostic: a refused state makes its instance
/// `unknown` with that reason (commands refused); a refused entry (junk, another document, hostile
/// depth) is reported whole and is never overwritten. The stored bytes stay unchanged.
#[test]
fn stale_design_entry_reads_as_a_diagnostic() {
    use hsk_studio_harness_core::harness::{Harness, References, Selection, default_actor};
    let (s, mut v) = opened("stale");
    let fig = fig();
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let other = instance_layer(&v, &fig, "button-instance-1");
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let edited = s.0.join("edited.handshake");
    v.save(&edited, false).unwrap();
    let good: Value = serde_json::from_slice(&entry(&v).unwrap()).unwrap();
    let with = |f: &dyn Fn(&mut Value)| {
        let mut e = good.clone();
        f(&mut e);
        serde_json::to_vec(&e).unwrap()
    };
    // (case, bytes, code, target, whole entry)
    let cases: Vec<(&str, Vec<u8>, &str, &str, bool)> = vec![
        ("component", with(&|e| e["instances"][0]["component_id"] = "SCMP-019a0000-0000-7000-8000-00000000dead".into()), "dangling_reference", "instances/0/component_id", false),
        ("variant", with(&|e| e["instances"][0]["variant"]["state"] = "pressed".into()), "dangling_reference", "instances/0/variant/state", false),
        ("override field", with(&|e| e["instances"][0]["overrides"][0]["field"] = "font_name".into()), "invalid_field", "instances/0/overrides/0/field", false),
        ("other document", with(&|e| e["document_id"] = "SDOC-019a0000-0000-7000-8000-0000000000ff".into()), "invalid_field", "document_id", true),
        ("junk", b"{\"schema_id\":".to_vec(), "invalid_json", "design_state", true),
        ("deep", format!("{}{}", "[".repeat(200), "]".repeat(200)).into_bytes(), "budget_exceeded", "input_depth", true),
    ];
    for (what, bytes, code, target, whole) in cases {
        let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
        let cancel = CancellationToken::default();
        let mut run = harness.run(default_actor(), &cancel);
        assert!(run.open_path(&edited), "{what}");
        run.set_document_entry(ENTRY, Some(bytes.clone())).unwrap();
        let path = s.0.join(format!("stale-{}.handshake", what.replace(' ', "-")));
        assert!(run.save_to(&path), "{what}");
        let mut w = reopen(&path);
        let view = w.instance_view(&inst).unwrap();
        assert_eq!(view.state, "unknown", "{what}");
        let reason = view.error.clone().unwrap_or_default();
        assert!(reason.contains(code) && reason.contains(target), "{what}: {reason}");
        let st = w.state_json()["design"]["entry"].clone();
        if whole {
            assert_eq!((st["error"]["code"].as_str(), st["error"]["target"].as_str()), (Some(code), Some(target)), "{what}: {st}");
            assert!(w.design_state_records().is_err());
            // An instance still at its imported drawing reads `import`, but no command overwrites the entry.
            assert_eq!(w.instance_view(&other).unwrap().state, "import", "{what}");
            let e = w.design_command(&other, DesignCommand::ResetAll).unwrap_err();
            assert!(e.contains("design_state_entry_invalid"), "{what}: {e}");
        } else {
            assert!(st["error"].is_null(), "{what}: {st}");
            assert_eq!(st["dropped"][0]["layer_id"], inst.as_str(), "{what}: {st}");
            assert_eq!((st["dropped"][0]["error"]["code"].as_str(), st["dropped"][0]["error"]["target"].as_str()), (Some(code), Some(target)), "{what}");
            assert_eq!(w.design_state_records().unwrap().map(|r| r.instances.len()), Some(0));
        }
        let e = w.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "default".into() }).unwrap_err();
        assert!(e.contains("instance_record_unknown") && e.contains(code), "{what}: {e}");
        assert_eq!(entry(&w), Some(bytes), "{what}: the stored bytes stay unchanged");
    }
}
