//! Lane SMB-R60-FOLLOWUPS-1 item 6 (feature `viewer`; validation round R60 attempt 2 probes b9 and i3):
//! `page.delete` of a page a parent page is applied to updates the stored layout records in the same undo
//! step, so the layout stays editable after save and reopen and undo restores the page and its binding
//! together; `layout.open`, which commits a sized open as an undo step, declares it mutates (STU-AUT-147).
//! Oracles: the reopened layout editor, the session history, the document's pages.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutEdit, PageEdit};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::action;
use hsk_studio_package::{Extras, Limits, MemoryAssetSource, WriteOptions, write_package_with};
use std::path::{Path, PathBuf};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-00000000c060";
const SPREAD_A: &str = "SPGS-019abcde-0000-7000-8000-00000000c060";
const SPREAD_B: &str = "SPGS-019abcde-0000-7000-8000-00000000c061";

fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn page(key: &str) -> StudioPage {
    let m = || len(10.0);
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::RightHand, margins: LayoutEdges { top: m(), right: m(), bottom: m(), left: m() }, bleed: None, applied_trap_preset: None }
}
/// One spread of three 200 x 200 pages, no layers.
fn document() -> StudioDocument {
    document_of(&[(SPREAD_A, &["page-1", "page-2", "page-3"])])
}

/// `spreads` of 200 x 200 pages side by side, no layers.
fn document_of(spreads: &[(&str, &[&str])]) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: DOC.into(),
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
        page_spreads: spreads
            .iter()
            .enumerate()
            .map(|(i, (id, keys))| StudioPageSpread {
                schema_id: PageSpreadSchema::V1,
                page_spread_id: (*id).into(),
                name: format!("spread {i}"),
                parent: Parent::Root(()),
                bounds: Bounds { x: len(0.0), y: len(250.0 * f64::from(u32::try_from(i).unwrap())), width: len(200.0 * f64::from(u32::try_from(keys.len()).unwrap())), height: len(200.0) },
                pages: keys.iter().map(|k| page(k)).collect(),
            })
            .collect(),
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}

fn package(dir: &Path, name: &str) -> PathBuf {
    package_of(dir, name, &document())
}

fn package_of(dir: &Path, name: &str, document: &StudioDocument) -> PathBuf {
    let token = CancellationToken::default();
    let snapshot = match inspect_bytes(&serde_json::to_vec(document).unwrap(), Budget::default(), &token, &Unresolved).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let mut bytes = Vec::new();
    let assets = MemoryAssetSource::new();
    write_package_with(&snapshot, &assets, &Extras { quarantined: &[], previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("{e}"));
    v
}

fn pages(v: &Viewer) -> Vec<String> {
    v.run().snapshot().unwrap().document().page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.clone()).collect()
}

fn bound_pages(v: &mut Viewer) -> Vec<String> {
    v.layout_sync();
    v.layout_editor().unwrap().records().parents.page_bindings.iter().map(|b| b.page_id.clone()).collect()
}

fn frame_on_page_1() -> LayoutEdit {
    LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }
}

/// A parent page applied to page-2 and page-3; returns the viewer.
fn parented(scratch: &Scratch) -> Viewer {
    let mut v = open(&package(&scratch.0, "parented.handshake"));
    v.layout_apply(LayoutEdit::Page(PageEdit::NewParent { name: "A-Feature".into(), based_on: None })).unwrap();
    let parent = v.layout_editor().unwrap().records().parents.parents[0].parent_id.clone();
    v.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-2".into(), "page-3".into()], parent: Some(parent) })).unwrap();
    v
}

/// Probe b9: delete a page with a parent page, save, reopen: the layout is still editable, the other
/// page keeps its binding.
#[test]
fn r60_b9_a_deleted_parented_page_leaves_the_layout_editable_after_reopen() {
    let scratch = Scratch::new("r60-b9-reopen");
    let mut v = parented(&scratch);
    v.delete_page("page-2").unwrap();
    assert_eq!(pages(&v), ["page-1", "page-3"]);
    v.layout_apply(frame_on_page_1()).expect("the layout is editable after the delete");
    let saved = scratch.0.join("deleted.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    assert_eq!(bound_pages(&mut back), ["page-3"], "the deleted page's binding is gone, the other kept");
    back.layout_apply(frame_on_page_1()).expect("editable after reopen");
    back.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-1".into()], parent: None })).expect("page edits go on after reopen");
}

/// Probe b9: the delete is ONE undo step for the page and its records: undo restores both, redo removes
/// both again.
#[test]
fn r60_b9_undo_restores_the_page_and_its_binding_together() {
    let scratch = Scratch::new("r60-b9-undo");
    let mut v = parented(&scratch);
    let depth = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    v.delete_page("page-2").unwrap();
    let undo = v.edit_json()["history"]["undo"].as_array().unwrap().clone();
    assert_eq!(undo.len(), depth + 1, "one step: {undo:?}");
    assert_eq!(bound_pages(&mut v), ["page-3"]);
    v.undo().unwrap();
    assert_eq!(pages(&v), ["page-1", "page-2", "page-3"]);
    assert_eq!(bound_pages(&mut v), ["page-2", "page-3"], "the binding comes back with the page");
    v.redo().unwrap();
    assert_eq!(pages(&v), ["page-1", "page-3"]);
    assert_eq!(bound_pages(&mut v), ["page-3"]);
    v.layout_apply(frame_on_page_1()).expect("editable after redo");
}

/// Item 6: a page delete never leaves a stale entry. A page a layout text frame sits on is refused typed
/// `page_not_empty` with nothing changed, and the layout stays editable, also after save and reopen.
#[test]
fn r60_b9_a_page_delete_never_leaves_the_layout_locked() {
    let scratch = Scratch::new("r60-b9-frame");
    let mut v = parented(&scratch);
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).unwrap();
    let depth = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    let e = v.delete_page("page-2").expect_err("a text frame sits on page-2");
    assert!(e.contains("page_not_empty"), "a typed refusal: {e}");
    assert_eq!(pages(&v), ["page-1", "page-2", "page-3"], "a refusal changes nothing");
    assert_eq!(v.edit_json()["history"]["undo"].as_array().unwrap().len(), depth, "and adds no step");
    v.layout_apply(frame_on_page_1()).expect("the layout stays editable");
    let saved = scratch.0.join("frame.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    back.layout_apply(frame_on_page_1()).expect("editable after reopen");
}

/// Ruling 12 (review c43f2d90): a parent-bound page alone on its spread. The delete removes the spread with
/// the page and the binding in one step; undo restores the spread (same id), the page and the binding; the
/// layout reopens editable.
#[test]
fn r60_b9_a_parented_page_alone_on_its_spread_deletes_and_undoes_with_its_spread() {
    let scratch = Scratch::new("r60-b9-spread");
    let mut v = open(&package_of(&scratch.0, "spreads.handshake", &document_of(&[(SPREAD_A, &["page-1"]), (SPREAD_B, &["page-2"])])));
    v.layout_apply(LayoutEdit::Page(PageEdit::NewParent { name: "A-Feature".into(), based_on: None })).unwrap();
    let parent = v.layout_editor().unwrap().records().parents.parents[0].parent_id.clone();
    v.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-2".into()], parent: Some(parent) })).unwrap();
    let spreads = |v: &Viewer| v.run().snapshot().unwrap().document().page_spreads.iter().map(|s| s.page_spread_id.clone()).collect::<Vec<_>>();
    v.delete_page("page-2").unwrap();
    assert_eq!((pages(&v), spreads(&v)), (vec!["page-1".to_string()], vec![SPREAD_A.to_string()]), "the spread goes with its only page");
    assert!(bound_pages(&mut v).is_empty());
    v.undo().unwrap();
    assert_eq!(pages(&v), ["page-1", "page-2"]);
    assert_eq!(spreads(&v), [SPREAD_A, SPREAD_B], "undo restores the spread with its id");
    assert_eq!(bound_pages(&mut v), ["page-2"], "and the binding");
    let saved = scratch.0.join("spreads-undone.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    assert_eq!(bound_pages(&mut back), ["page-2"]);
    back.layout_apply(frame_on_page_1()).expect("editable after reopen");
}

/// Review F2 of c43f2d90 (lead ruling 2026-11-11): a page key at the last u64 leaves no number past it, so
/// the mint takes the lowest number no page key names: two page adds return two pages, never
/// `page_number_exhausted`.
#[test]
fn r60_f2_a_page_key_at_the_last_u64_still_lets_page_add_mint() {
    let scratch = Scratch::new("r60-f2");
    let mut v = open(&package_of(&scratch.0, "last.handshake", &document_of(&[(SPREAD_A, &["page-18446744073709551615"])])));
    let first = v.add_page(None, false, Some((100.0, 100.0)), None).expect("a page");
    let second = v.add_page(None, false, Some((100.0, 100.0)), None).expect("another page");
    assert_eq!((first.as_str(), second.as_str()), ("page-1", "page-2"));
}

/// Lead ruling 2026-11-11T10:39Z: deleting a text-frame layer that a thread or layout flow record names is
/// refused typed `layer_bound_by_layout_records`, naming the record, before any change; the document is
/// unchanged and the layout stays editable after save and reopen.
#[test]
fn r60_l1_a_threaded_text_frame_layer_delete_is_refused_typed_and_leaves_the_layout_editable() {
    let scratch = Scratch::new("r60-l1");
    let mut v = open(&package(&scratch.0, "threaded.handshake"));
    v.layout_apply(frame_on_page_1()).unwrap();
    let a = v.layout_editor().unwrap().selected.clone().unwrap();
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).unwrap();
    let b = v.layout_editor().unwrap().selected.clone().unwrap();
    v.layout_apply(LayoutEdit::Thread { from: a.clone(), to: b.clone() }).unwrap();
    let before = v.run().snapshot().unwrap().document().clone();
    let depth = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    let refused = v.delete_layer(&b).expect_err("a threaded frame layer is bound by the thread record");
    assert!(refused.contains("layer_bound_by_layout_records") && refused.contains(&a), "typed, naming the record: {refused}");
    assert_eq!(v.run().snapshot().unwrap().document(), &before, "nothing changed");
    assert_eq!(v.edit_json()["history"]["undo"].as_array().unwrap().len(), depth, "no step");
    let saved = scratch.0.join("threaded-saved.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    back.layout_apply(LayoutEdit::DrawFrame { page_id: "page-3".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).expect("editable after reopen");
}

/// Probe i3: a sized `layout.open` on an open document adds an undo step, and its row says it mutates.
#[test]
fn r60_i3_layout_open_that_adds_an_undo_step_declares_it_mutates() {
    let entry = action::command("layout.open").expect("layout.open row");
    assert!(entry.mutates_authority, "layout.open commits a sized open as a document step");
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&hsk_studio_harness_core::document_new::NewDocument::new(hsk_studio_harness_core::document_new::DocumentProfile::Vector, Some((128.0, 128.0))), None).unwrap();
    let before = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    v.layout_open(Some((200.0, 200.0)), Some(1)).unwrap();
    assert_eq!(v.edit_json()["history"]["undo"].as_array().unwrap().len(), before + 1, "a sized open is a step");

    // Review F1 of c43f2d90: the drive reply names that step (the process, as a model drives it). A sized
    // open needs a document without pages: the harness fixture.
    let scratch = Scratch::new("r60-i3-drive");
    let package = scratch.0.join("demo.handshake");
    std::fs::write(&package, include_bytes!("fixtures/demo.handshake")).unwrap();
    let line = serde_json::json!({"command": "command", "command_id": "layout.open", "parameters": {"page_width_pt": 200, "page_height_pt": 200, "pages": 1}});
    let (_, replies) = session(&scratch.0, &package, &[line]);
    assert_eq!(replies[1]["ok"], true, "{}", replies[1]);
    assert_eq!(replies[1]["detail"]["undo_name"], "layout.open", "the drive names the step a mutating row made: {}", replies[1]);
}

// ---- the process: no Rust call (as tests/layout_pdf_host.rs) ---------------------------------------

#[cfg(not(feature = "desktop"))]
fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}
#[cfg(feature = "desktop")]
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
}

/// One quiet `command PACKAGE --stdin` session; one JSON value per stdout line.
fn session(dir: &Path, package: &Path, input: &[serde_json::Value]) -> (i32, Vec<serde_json::Value>) {
    use std::io::Write as _;
    let mut command = std::process::Command::new(binary());
    command
        .args(["command", package.to_str().unwrap(), "--actor", "layout-model", "--session", "s-1", "--stdin"])
        .current_dir(dir)
        .env("STUDIO_DIAG_DIR", dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().expect("spawn studio-harness");
    let text: String = input.iter().map(|l| format!("{l}\n")).collect();
    child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let lines = String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (out.status.code().expect("an exit code"), lines)
}
