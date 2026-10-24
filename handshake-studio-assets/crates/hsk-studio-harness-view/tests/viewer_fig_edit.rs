//! Lane SMB-FIG-EDIT-1 (feature `viewer`): FILE > Open turns a design file (`.fig`) into an editable
//! native document in the window, and the Properties panel shows the instance inspector for a
//! component instance (variant switch, component swap, override edit and reset; STU-DS-003/008/
//! 016/017, bound contextually per STU-DS-160). Driven headlessly (one-frame egui draws into a bare
//! context, no window). Oracles: the importer's own proposal over the same checked-in fixtures
//! (`hsk-studio-interop-fig/tests/fixtures`, repository-relative), the session's document and
//! entries, the run history and the import receipt. Figma's instance panel behaviour has no oracle
//! here (UNVERIFIED, D5).
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Payload, StudioValue};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::design::{DesignCommand, is_design_file};
use hsk_studio_harness_view::view::file_dialog::Purpose;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::ShellCommand;
use hsk_studio_harness_view::view::vector::PathEdit;
use hsk_studio_interop_fig::{FigDocument, FigOutcome, Limits, NativeConfig, NativeProposal, SequentialIdMint, VersionPolicy, decode_document, import_fig, to_native};
use hsk_studio_nib::anchor_path::{AnchorAddr, Constraint};
use serde_json::Value;
use std::path::PathBuf;

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-fig-edit-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    // The fixtures live with the importer (repository-relative, never machine-local).
    PathBuf::from(format!("{}/../hsk-studio-interop-fig/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
}

fn copy_fixture(scratch: &Scratch, name: &str) -> PathBuf {
    let to = scratch.0.join(name);
    std::fs::copy(fixture_path(name), &to).unwrap();
    to
}

/// The importer's decoded document and proposal (the oracle), independent of the window.
fn oracle(name: &str) -> (FigDocument, NativeProposal) {
    struct Host;
    impl folio::Resolver for Host {
        fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
            folio::Resolution::Absent
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
        fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
            kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
        }
    }
    let bytes = std::fs::read(fixture_path(name)).unwrap();
    let cancel = CancellationToken::default();
    let Ok(FigOutcome::Imported(import)) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel) else { panic!("fixture import") };
    let doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    let proposal = to_native(&doc, &NativeConfig { budget: folio::Budget::default(), resolver: &Host }, &mut SequentialIdMint::new(7), &cancel).unwrap();
    (doc, proposal)
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(input(events), |ctx| v.ui(ctx))
}

/// Frames until the drawn widget list repeats (the first frames size the panels; F-TL-2).
fn settle(v: &mut Viewer, ctx: &egui::Context) {
    let mut last: Option<Vec<String>> = None;
    for _ in 0..6 {
        let _ = frame(v, ctx, vec![]);
        let now = v.drawn().to_vec();
        if last.as_ref() == Some(&now) {
            return;
        }
        last = Some(now);
    }
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

/// Opens `name` through FILE > Open with the "Design file (.fig)" filter; returns the native path.
fn open_through_dialog(v: &mut Viewer, scratch: &Scratch, name: &str) -> PathBuf {
    let fig = copy_fixture(scratch, name);
    v.save_input = scratch.0.join("unused.handshake").to_string_lossy().into_owned();
    v.shell_command(ShellCommand::Open).unwrap();
    let d = v.file_dialog().expect("open dialog");
    let at = d.filters().iter().position(|f| f.name == "Design file (.fig)").expect("Open offers the design file filter");
    d.set_filter(at).unwrap();
    d.typed = fig.to_string_lossy().into_owned();
    assert_eq!(v.dialog_accept(), Ok(true), "{}", v.state_json()["status"]);
    fig.with_extension("handshake")
}

fn opened(label: &str, name: &str) -> (Scratch, Viewer, PathBuf) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    let dest = open_through_dialog(&mut v, &scratch, name);
    (scratch, v, dest)
}

fn doc(v: &Viewer) -> folio::StudioDocument {
    v.run().snapshot().expect("open").document().clone()
}

/// The receipt's instance layer of the fixture instance named `name` (located by its Figma source id).
fn instance_layer(v: &Viewer, fig: &FigDocument, name: &str) -> String {
    let src = fig.named(name).next().unwrap_or_else(|| panic!("{name} in the fixture")).id.to_string();
    let r = v.receipt().expect("receipt");
    r["design"]["instances"].as_array().unwrap().iter().find(|b| b["source"] == src.as_str()).unwrap_or_else(|| panic!("binding of {name}"))["layer_id"].as_str().unwrap().to_owned()
}

fn children(v: &Viewer, layer: &str) -> Vec<String> {
    doc(v).layers.iter().filter(|l| matches!(&l.parent, folio::Parent::Node(p) if p.node_id == layer)).map(|l| l.layer_id.clone()).collect()
}

/// Every layer under `group` (breadth first), the group excluded.
fn descendants(v: &Viewer, group: &str) -> Vec<folio::StudioLayer> {
    let d = doc(v);
    let mut out: Vec<folio::StudioLayer> = Vec::new();
    let mut parents = vec![group.to_owned()];
    while let Some(g) = parents.pop() {
        for l in d.layers.iter().filter(|l| matches!(&l.parent, folio::Parent::Node(p) if p.node_id == g)) {
            parents.push(l.layer_id.clone());
            out.push(l.clone());
        }
    }
    out
}

fn text_of_layer_under(v: &Viewer, group: &str) -> Vec<String> {
    let entry = v.text_entry().unwrap();
    descendants(v, group)
        .iter()
        .filter_map(|l| if let Payload::Text { story_id } = &l.payload { Some(story_id.clone()) } else { None })
        .map(|id| entry.stories.iter().find(|s| s.story_id == id).expect("story of a drawn text layer").inline_text().unwrap().to_owned())
        .collect()
}

/// The window's component id of the definition layer named `name`.
fn component_named(v: &Viewer, name: &str) -> (String, String) {
    let layer = doc(v).layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("{name}")).layer_id.clone();
    let id = v.receipt().unwrap()["design"]["records"]["components"].as_array().unwrap().iter().find(|k| k["layer_id"] == layer.as_str()).unwrap()["component_id"].as_str().unwrap().to_owned();
    (id, layer)
}

// ---- slice 1: .fig -> native package -> open ---------------------------------------------------------

/// FILE > Open with a `.fig` writes `<stem>.handshake` beside it and opens it: the importer's layers,
/// artboards, paths and text stories are the window's document and entries, it renders, saves and
/// reopens unchanged; a second open refuses to replace the package without overwrite.
#[test]
fn opening_a_fig_yields_an_editable_native_document() {
    let (scratch, mut v, dest) = opened("open", "fig-components-autolayout.fig");
    let (_fig, p) = oracle("fig-components-autolayout.fig");
    assert!(dest.is_file(), "the native package is written beside the source");
    let s = v.state_json();
    assert_eq!(s["source"].as_str(), Some(dest.to_string_lossy().as_ref()));
    assert_eq!(s["receipt"]["source_format"], "fig");
    assert!(s["shell"]["dialog"].is_null());
    let d = doc(&v);
    // Every importer layer except the instance binding layers, with the same names and kinds.
    let expected: Vec<(String, String)> = p.snapshot.document().layers.iter().filter(|l| l.kind != "component_instance").map(|l| (l.name.clone(), l.kind.clone())).collect();
    let got: Vec<(String, String)> = d.layers.iter().map(|l| (l.name.clone(), l.kind.clone())).collect();
    assert_eq!(got, expected);
    assert_eq!(d.artboards.len(), p.snapshot.document().artboards.len());
    assert_eq!(d.geometry_unit, folio::GeometryUnit::Px);
    // Paths: one anchor record per importer path, listed by the same vector layers.
    let entry = v.vector_entry().unwrap();
    assert_eq!(entry.paths.len(), p.paths.len());
    for fp in &p.paths {
        assert!(entry.path(&fp.path_id).is_some(), "path {} carried", fp.path_id);
    }
    // Text: a record and the importer's story per text layer.
    let text = v.text_entry().unwrap();
    let text_layers = d.layers.iter().filter(|l| matches!(l.payload, Payload::Text { .. })).count();
    assert!(text_layers > 0);
    assert_eq!(text.layers.len(), text_layers);
    for r in &text.layers {
        let story = text.stories.iter().find(|s| s.story_id == r.story_id).unwrap();
        assert_eq!(Some(story), p.stories.iter().find(|s| s.story_id == r.story_id));
    }
    // It renders (CPU) and the layer panel lists it.
    v.render_if_dirty();
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
    let f = v.frame().expect("rendered frame");
    assert!(f.width > 0 && f.height > 0);
    assert_eq!(v.layers().len(), d.layers.len());
    // Save and reopen: the same document and entries.
    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    assert_eq!(doc(&w).layers, d.layers);
    assert_eq!(w.vector_entry().unwrap(), entry);
    assert_eq!(w.text_entry().unwrap(), text);
    assert_eq!(w.receipt().unwrap()["design"], v.receipt().unwrap()["design"]);
    // A second FILE > Open of the same file does not replace the package without overwrite.
    let before = std::fs::read(&dest).unwrap();
    v.shell_command(ShellCommand::Open).unwrap();
    v.file_dialog().unwrap().typed = scratch.0.join("fig-components-autolayout.fig").to_string_lossy().into_owned();
    let e = v.dialog_accept().unwrap_err();
    assert!(e.contains("exists"), "{e}");
    assert_eq!(std::fs::read(&dest).unwrap(), before);
    assert!(v.state_json()["shell"]["dialog"]["error"].as_str().is_some_and(|t| t.contains("exists")));
}

/// Imported layers and paths edit through the existing vector and layer surfaces, one undo step each.
#[test]
fn fig_layers_and_paths_edit_through_the_vector_surface_and_undo() {
    let (_s, mut v, _dest) = opened("edit", "fig-basic-shapes.fig");
    let d = doc(&v);
    let layer = d
        .layers
        .iter()
        .find(|l| matches!(&l.payload, Payload::Vector { path_ids } if path_ids.len() == 1) && v.vector_path(&l.layer_id).is_ok_and(|r| r.path.contours.first().is_some_and(|c| c.anchors.len() >= 2)))
        .expect("an imported single-path vector layer")
        .layer_id
        .clone();
    let before = v.vector_path(&layer).unwrap();
    let a0 = before.path.contours[0].anchors[0].position;
    v.edit_path(&layer, PathEdit::MoveAnchor { at: AnchorAddr { contour: 0, anchor: 0 }, dx: 10.0, dy: -4.0, constraint: Constraint::Free }).unwrap();
    let after = v.vector_path(&layer).unwrap();
    assert_ne!(after.path_id, before.path_id, "a new path record");
    let a1 = after.path.contours[0].anchors[0].position;
    assert!(((a1.x - a0.x) - 10.0).abs() < 1e-9 && ((a1.y - a0.y) + 4.0).abs() < 1e-9, "{a0:?} -> {a1:?}");
    assert!(doc(&v).layers.iter().any(|l| l.layer_id == layer), "the layer keeps its id");
    v.undo().unwrap();
    assert_eq!(v.vector_path(&layer).unwrap().path, before.path);
    v.redo().unwrap();
    assert_eq!(v.vector_path(&layer).unwrap().path, after.path);
    // Layer surfaces: rename and hide, each undoable.
    v.rename_layer(&layer, "renamed shape").unwrap();
    v.set_visible(&layer, false).unwrap();
    let l = doc(&v).layers.into_iter().find(|l| l.layer_id == layer).unwrap();
    assert_eq!((l.name.as_str(), l.visible), ("renamed shape", false));
    v.undo().unwrap();
    v.undo().unwrap();
    let l = doc(&v).layers.into_iter().find(|l| l.layer_id == layer).unwrap();
    assert_eq!(l.visible, true);
    assert_ne!(l.name, "renamed shape");
    v.render_if_dirty();
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
}

/// The receipt lists every importer loss (mapping and decode, with counts) and the window's own
/// (font substitution), and the receipt panel draws them.
#[test]
fn fig_import_receipt_lists_losses() {
    let (_s, mut v, _dest) = opened("receipt", "fig-basic-shapes.fig");
    let (_fig, p) = oracle("fig-basic-shapes.fig");
    let r = v.receipt().unwrap().clone();
    let loss = r["loss"].as_array().unwrap();
    for (list, kind) in [(&p.mapping_loss, "mapping"), (&p.decode_loss, "decode")] {
        for l in list {
            assert!(
                loss.iter().any(|e| e["path"] == l.path.as_str() && e["code"] == l.code && e["kind"] == kind && e["count"] == l.count),
                "{kind} loss {} {} x{} missing",
                l.path,
                l.code,
                l.count
            );
        }
    }
    assert!(!p.mapping_loss.is_empty());
    // Inter is not among the window's fonts: substituted, and said so.
    assert!(loss.iter().any(|e| e["kind"] == "viewer" && e["path"].as_str().is_some_and(|p| p.starts_with("viewer.text.font=Inter")) && e["code"] == "font_substituted:Ubuntu-Light"), "{loss:?}");
    assert!(r["not_proven"].as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(r["source"]["byte_len"], std::fs::metadata(fixture_path("fig-basic-shapes.fig")).unwrap().len());
    assert_eq!(v.state_json()["receipt"]["loss_entries"], loss.len());
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, "receipt"));
}

/// Hostile design files are refused typed before anything is written: garbage after the prelude, a
/// truncated real file, a ZIP named `.fig` that is not one, and a file over the source size bound
/// (refused from its length, never read).
#[test]
fn hostile_fig_bytes_are_refused_bounded() {
    let scratch = Scratch::new("hostile");
    let mut v = Viewer::new(Prefer::Cpu);
    let real = std::fs::read(fixture_path("fig-components-autolayout.fig")).unwrap();
    let mut garbage = b"fig-kiwi".to_vec();
    garbage.extend([0xffu8; 64]);
    let cases: Vec<(&str, Vec<u8>)> = vec![("garbage.fig", garbage), ("truncated.fig", real[..real.len() / 2].to_vec()), ("zip.fig", b"PK\x03\x04not a zip at all".to_vec())];
    for (name, bytes) in cases {
        let src = scratch.0.join(name);
        std::fs::write(&src, &bytes).unwrap();
        assert!(is_design_file(&src), "{name} is routed to the importer");
        let dest = src.with_extension("handshake");
        let e = v.import_fig(&src, &dest, false).unwrap_err();
        assert!(e.starts_with("fig_import:"), "{name}: {e}");
        assert!(!dest.exists(), "{name}: nothing written");
        assert!(v.run().snapshot().is_none(), "{name}: nothing opened");
    }
    // Over the source bound: refused from the length.
    let big = scratch.0.join("big.fig");
    {
        let f = std::fs::File::create(&big).unwrap();
        std::io::Write::write_all(&mut &f, b"fig-kiwi").unwrap();
        f.set_len(512 * 1024 * 1024 + 1).unwrap();
    }
    let e = v.import_fig(&big, &big.with_extension("handshake"), false).unwrap_err();
    assert!(e.contains("exceeds"), "{e}");
    // A non-design file still goes the ordinary open route.
    let png = scratch.0.join("x.fig.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\nxxxx").unwrap();
    assert!(!is_design_file(&png));
    // Through the dialog the refusal stays on the open dialog.
    std::fs::write(scratch.0.join("again.fig"), b"fig-kiwi\0\0\0\0").unwrap();
    v.shell_command(ShellCommand::Open).unwrap();
    v.file_dialog().unwrap().typed = scratch.0.join("again.fig").to_string_lossy().into_owned();
    assert!(v.dialog_accept().unwrap_err().starts_with("fig_import:"));
    assert!(v.state_json()["shell"]["dialog"]["error"].as_str().is_some_and(|t| t.starts_with("fig_import:")));
}

/// The prototype player keeps its own dialog purpose and filter and still plays a `.fig`.
#[test]
fn prototype_player_still_opens_fig_files() {
    let names: Vec<&str> = Purpose::OpenPrototype.filters().iter().map(|f| f.name).collect();
    assert_eq!(names, ["Design file (.fig)", "All files"]);
    let open: Vec<&str> = Purpose::Open.filters().iter().map(|f| f.name).collect();
    assert_eq!(open.first(), Some(&"Documents and images"));
    assert_eq!(open.last(), Some(&"All files"));
    assert!(open.contains(&"Design file (.fig)"));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open_prototype(&std::fs::read(fixture_path("fig-prototype-pages.fig")).unwrap()).unwrap();
    assert!(v.prototype().is_some());
    assert!(v.run().snapshot().is_none(), "the player does not open a document");
}

// ---- slice 2: the instance inspector ------------------------------------------------------------------

/// The design-instance section is drawn in the Properties panel only while an instance layer is
/// selected (no panel or mode switch).
#[test]
fn instance_section_appears_only_for_instances() {
    let (_s, mut v, _dest) = opened("section", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let ctx = egui::Context::default();
    let inst = instance_layer(&v, &fig, "button-instance-1");
    let other = doc(&v).layers.iter().find(|l| l.layer_id != inst && !matches!(l.payload, Payload::Group)).unwrap().layer_id.clone();
    v.select(Some(other.clone()));
    settle(&mut v, &ctx);
    assert!(drew(&v, "properties"));
    assert!(!drew(&v, "design.instance"));
    assert!(v.state_json()["design"]["instance"].is_null());
    v.select(Some(inst.clone()));
    settle(&mut v, &ctx);
    assert!(drew(&v, "design.instance"), "{:?}", v.drawn());
    assert!(drew(&v, "design.instance.variant.state.hover"));
    assert!(drew(&v, "design.instance.reset_all"));
    assert_eq!(v.state_json()["design"]["instance"]["layer_id"], inst.as_str());
    assert_eq!(v.state_json()["design"]["instance_layers"], 3);
    v.select(None);
    settle(&mut v, &ctx);
    assert!(!drew(&v, "design.instance"));
}

/// STU-DS-008/017: switching the variant re-selects the hover member in place, keeps the text
/// override (same layer name and hierarchy), redraws the instance from the hover definition with
/// the override applied, as one undo step.
#[test]
fn switching_a_variant_keeps_compatible_overrides() {
    let (_s, mut v, _dest) = opened("variant", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let view = v.instance_view(&inst).unwrap();
    assert_eq!(view.state, "import");
    assert_eq!(view.variants["state"].1, "default");
    assert_eq!(view.overrides.len(), 1);
    assert_eq!((view.overrides[0].2.as_str(), view.overrides[0].3.as_str()), ("characters", "Changed text"));
    let before = children(&v, &inst);
    let r = v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let (hover, hover_layer) = component_named(&v, "state=hover");
    assert_eq!(r["component_id"], hover.as_str());
    assert_eq!(r["dropped"].as_array().map(Vec::len), Some(0));
    let view = v.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str()), ("edited", "hover"));
    assert_eq!(view.overrides.len(), 1, "the text override survives the switch");
    let now = children(&v, &inst);
    assert!(now.iter().all(|c| !before.contains(c)), "redrawn from the hover definition");
    assert_eq!(now.len(), children(&v, &hover_layer).len());
    assert_eq!(text_of_layer_under(&v, &inst), ["Changed text"]);
    // The redrawn background carries the hover definition's paint style.
    let entry = v.vector_entry().unwrap();
    let style_of = |layer: &str| entry.design.as_ref().and_then(|d| d.nodes.iter().find(|n| n.node.node_id == layer)).and_then(|n| n.style_refs.get("fill_style_id").cloned());
    let def_bg = children(&v, &hover_layer).into_iter().find_map(|c| style_of(&c));
    let inst_bg = now.iter().find_map(|c| style_of(c));
    assert!(def_bg.is_some() && inst_bg == def_bg, "{def_bg:?} vs {inst_bg:?}");
    v.render_if_dirty();
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
    // One undo step back to the imported drawing and record; redo returns the edit.
    v.undo().unwrap();
    assert_eq!(children(&v, &inst), before);
    let view = v.instance_view(&inst).unwrap();
    assert_eq!((view.state, view.variants["state"].1.as_str()), ("import", "default"));
    v.redo().unwrap();
    assert_eq!(v.instance_view(&inst).unwrap().variants["state"].1, "hover");
}

/// STU-DS-016/017: an override edit, a per-property reset and reset-all, each one undo step; a reset
/// text draws the definition's text.
#[test]
fn reset_override_and_reset_all() {
    let (_s, mut v, _dest) = opened("reset", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let view = v.instance_view(&inst).unwrap();
    let label = view.overrides[0].0.clone();
    let definition_text = view.texts.iter().find(|t| t.0 == label).map(|t| t.2.clone()).unwrap();
    assert_eq!(definition_text, "Changed text", "the inspector shows the overridden text");
    v.design_command(&inst, DesignCommand::ResetOverride { node_id: label.clone(), field: "characters".into() }).unwrap();
    let view = v.instance_view(&inst).unwrap();
    assert!(view.overrides.is_empty());
    let def_story = text_of_layer_under(&v, &inst);
    assert_eq!(def_story.len(), 1);
    assert_ne!(def_story[0], "Changed text", "the definition's own text is drawn");
    assert_eq!(view.texts.iter().find(|t| t.0 == label).unwrap().2, def_story[0]);
    v.undo().unwrap();
    assert_eq!(text_of_layer_under(&v, &inst), ["Changed text"]);
    // Two overrides, then reset-all.
    v.design_command(&inst, DesignCommand::SetOverride { node_id: label.clone(), field: "visible".into(), value: StudioValue::Boolean { value: false } }).unwrap();
    let texts: Vec<folio::StudioLayer> = descendants(&v, &inst).into_iter().filter(|l| matches!(l.payload, Payload::Text { .. })).collect();
    assert_eq!(texts.len(), 1);
    assert!(!texts[0].visible, "the overridden label is drawn hidden");
    assert_eq!(v.instance_view(&inst).unwrap().overrides.len(), 2);
    v.design_command(&inst, DesignCommand::ResetAll).unwrap();
    assert!(v.instance_view(&inst).unwrap().overrides.is_empty());
    assert_eq!(text_of_layer_under(&v, &inst), def_story);
    // Refusals change nothing: an override field the redraw does not apply, a node outside the definition.
    let rev = doc(&v).revision;
    assert!(v.design_command(&inst, DesignCommand::SetOverride { node_id: label.clone(), field: "fills".into(), value: StudioValue::Boolean { value: true } }).unwrap_err().contains("not drawn here"));
    assert!(v.design_command(&inst, DesignCommand::SetOverride { node_id: inst.clone(), field: "visible".into(), value: StudioValue::Boolean { value: true } }).unwrap_err().contains("not a layer of the instance's definition"));
    assert!(v.design_command(&inst, DesignCommand::ResetOverride { node_id: label, field: "characters".into() }).unwrap_err().contains("no override"));
    assert_eq!(doc(&v).revision, rev);
}

/// Component swap: the instance points at the other component and its text override is re-matched
/// by layer name and hierarchy (kept), as one undo step.
#[test]
fn component_swap_rematches_by_name_and_hierarchy() {
    let (_s, mut v, _dest) = opened("swap", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let (hover, hover_layer) = component_named(&v, "state=hover");
    let r = v.design_command(&inst, DesignCommand::Swap { component_id: hover.clone() }).unwrap();
    assert_eq!(r["main_component_id"], hover.as_str());
    assert_eq!(r["dropped"].as_array().map(Vec::len), Some(0));
    let view = v.instance_view(&inst).unwrap();
    assert_eq!(view.record.main_component_id.as_deref(), Some(hover.as_str()));
    assert_eq!(view.overrides.len(), 1);
    let target = &view.overrides[0].0;
    // The kept override now names the hover definition's label layer.
    assert!(descendants(&v, &hover_layer).iter().any(|l| &l.layer_id == target), "re-matched onto the hover definition");
    assert_eq!(text_of_layer_under(&v, &inst), ["Changed text"]);
    v.undo().unwrap();
    assert_eq!(v.instance_view(&inst).unwrap().state, "import");
    // An unknown component is refused typed.
    assert!(v.design_command(&inst, DesignCommand::Swap { component_id: "SCMP-019a0000-0000-7000-8000-00000000dead".into() }).is_err());
    println!("NOT_PROVEN: dropped-override branch of a swap (the fixture's two components share one structure; covered by hsk-studio-components rematch tests)");
}

/// Feature `desktop`: the instance section is AccessKit-steerable: the variant value and reset-all
/// buttons are nodes, and clicks run the commands.
#[cfg(feature = "desktop")]
#[test]
fn accesskit_drives_the_instance_section() {
    let (_s, mut v, _dest) = opened("accesskit", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let inst = instance_layer(&v, &fig, "button-instance-3");
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    v.select(Some(inst.clone()));
    settle(&mut v, &ctx);
    let find = |out: &egui::FullOutput, label: &str| -> egui::accesskit::NodeId {
        let tree = out.platform_output.accesskit_update.as_ref().expect("accesskit tree");
        tree.nodes.iter().find(|(_, n)| n.label().as_deref() == Some(label)).map(|(id, _)| *id).unwrap_or_else(|| panic!("node {label}"))
    };
    let click = |id| egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None });
    let out = frame(&mut v, &ctx, vec![]);
    let hover = find(&out, "hover");
    let _ = frame(&mut v, &ctx, vec![click(hover)]);
    assert_eq!(v.instance_view(&inst).unwrap().variants["state"].1, "hover", "{}", v.state_json()["status"]);
    let out = frame(&mut v, &ctx, vec![]);
    let reset = find(&out, "Reset all overrides");
    let _ = frame(&mut v, &ctx, vec![click(reset)]);
    assert!(v.instance_view(&inst).unwrap().overrides.is_empty(), "{}", v.state_json()["design"]);
    assert_eq!(v.state_json()["design"]["last"]["command"], "reset_all");
}

// ---- slice 3: inspection ---------------------------------------------------------------------------

/// `state_json()["design"]` reports the selected instance, its component, variant assignment,
/// overrides and the last command result.
#[test]
fn state_json_reports_instance_and_overrides() {
    let (_s, mut v, _dest) = opened("state", "fig-components-autolayout.fig");
    let (fig, _p) = oracle("fig-components-autolayout.fig");
    let inst = instance_layer(&v, &fig, "button-instance-3");
    v.select(Some(inst.clone()));
    let s: Value = v.state_json()["design"].clone();
    let i = &s["instance"];
    assert_eq!(i["layer_id"], inst.as_str());
    assert_eq!(i["state"], "import");
    assert_eq!(i["variants"]["state"]["current"], "default");
    assert_eq!(i["variants"]["state"]["values"], serde_json::json!(["default", "hover"]));
    assert_eq!(i["overrides"][0]["field"], "characters");
    assert_eq!(i["overrides"][0]["value"], "Changed text");
    assert_eq!(i["component_name"], "state=default");
    assert!(i["instance_id"].as_str().is_some_and(|id| id.starts_with("SCIN-")));
    assert!(s["last"].is_null());
    assert_eq!(i["main_component_id"], component_named(&v, "state=default").0.as_str());
    v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "hover".into() }).unwrap();
    let s = v.state_json()["design"].clone();
    assert_eq!(s["last"]["command"], "switch_variant");
    assert_eq!(s["instance"]["state"], "edited");
    assert_eq!(s["instance"]["component_name"], "state=hover");
    let e = v.design_command(&inst, DesignCommand::SwitchVariant { property: "state".into(), value: "pressed".into() }).unwrap_err();
    assert!(e.contains("UnknownVariantValue"), "{e}");
    assert_eq!(v.state_json()["design"]["last"]["error"].as_str(), Some(e.as_str()));
}
