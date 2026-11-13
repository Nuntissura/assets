//! Lane SMB-LAYOUT-DELETE-CASCADE-1 (feature `viewer`; STU-LAY-003 delete, STU-LAY-122 threads, STU-LAY-020 and
//! STU-LAY-123 autoflow and smart text reflow, STU-LAY-131 frame fitting): the layout deletes InDesign performs
//! and the flow modes of a loaded text cursor, each one named undo step that survives save and reopen. Every
//! command goes through its public method or its command id (`Viewer::run_command`), one test through the real
//! process. Oracles: the session's document (pages, layers, thread links), the stored layout records entry, the
//! history and the reopened package. InDesign parity of the thread closing, the frame geometry and the page
//! pairing is UNVERIFIED (D5).
#![cfg(feature = "viewer")]
mod common;
use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::LayoutEdit;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_interop_raster as raster;
use hsk_studio_package::{Extras, Limits, MemoryAssetSource, WriteOptions, write_package_with};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Words for a story several 200 x 200 pt pages long (margin box 128 x 128 pt).
fn long_text(words: usize) -> String {
    (0..words).map(|i| format!("word{}", i % 10)).collect::<Vec<_>>().join(" ")
}

fn params(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("parameters must be an object: {other}"),
    }
}

fn cmd(v: &mut Viewer, id: &str, p: Value) -> Result<Value, (String, Value)> {
    v.run_command(id, &params(p)).map_err(|r| (r.code, r.detail))
}

fn ok(v: &mut Viewer, id: &str, p: Value) -> Value {
    cmd(v, id, p).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}

/// A new layout document of `pages` 200 x 200 pt pages on one spread (`document.new`, profile `layout`).
fn layout(pages: usize) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((200.0, 200.0))), None).unwrap();
    for _ in 1..pages {
        let last = page_ids(&v).last().cloned().unwrap();
        v.add_page(Some(&last), false, None, None).unwrap();
    }
    v
}

fn document(v: &Viewer) -> StudioDocument {
    v.run().snapshot().unwrap().document().clone()
}

fn page_ids(v: &Viewer) -> Vec<String> {
    document(v).page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.clone()).collect()
}

fn layer_ids(v: &Viewer) -> Vec<String> {
    document(v).layers.iter().map(|l| l.layer_id.clone()).collect()
}

/// (previous, next) of a text frame.
fn links(v: &Viewer, frame: &str) -> (Option<String>, Option<String>) {
    let d = document(v);
    let f = d.layers.iter().find(|l| l.layer_id == frame).and_then(|l| l.text_frame.clone()).unwrap_or_else(|| panic!("no frame {frame}"));
    (f.previous_text_frame, f.next_text_frame)
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    serde_json::from_value(v.run().history()["undo"].clone()).unwrap()
}

fn records(v: &Viewer) -> Value {
    v.run().document_entry("layout_records").map(|b| serde_json::from_slice(b).unwrap()).unwrap_or(Value::Null)
}

fn stories(v: &Viewer) -> Vec<String> {
    records(v)["stories"].as_array().map(|s| s.iter().map(|x| x["story_id"].as_str().unwrap().to_owned()).collect()).unwrap_or_default()
}

fn graphic_frames(v: &Viewer) -> usize {
    records(v)["longdoc"]["graphic_frames"].as_array().map_or(0, Vec::len)
}

/// A text frame on `page` (layout.draw_frame): its layer id.
fn frame(v: &mut Viewer, page: &str, x: f64) -> String {
    ok(v, "layout.draw_frame", json!({"page_id": page, "x": x, "y": 40.0, "w": 100.0, "h": 100.0}));
    v.layout_editor().unwrap().selected.clone().unwrap()
}

fn thread(v: &mut Viewer, from: &str, to: &str) {
    ok(v, "layout.thread", json!({"from": from, "to": to}));
}

fn story_of(v: &mut Viewer, frame: &str) -> String {
    v.layout_editor().unwrap().thread_info(frame).unwrap().parent_story
}

fn set_text(v: &mut Viewer, story: &str, text: &str) {
    ok(v, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": text}));
}

/// A solid opaque RGB PNG.
fn png(width: u32, height: u32) -> Vec<u8> {
    let samples = raster::Samples::U8((0..width * height).flat_map(|_| [200u8, 40, 40]).collect());
    raster::encode_png(&raster::PngEncodeRequest { width, height, colour: raster::Colour::Rgb, samples: &samples, icc: None, compression: raster::PngCompression::Fast }).unwrap()
}

/// `asset.place` of a 64 x 48 PNG into a frame on `page`: its layer id.
fn picture(v: &mut Viewer, s: &Scratch, page: &str, extra: Value) -> Value {
    let path = s.0.join(format!("pic-{page}.png"));
    std::fs::write(&path, png(64, 48)).unwrap();
    let mut p = json!({"path": path.to_str().unwrap(), "placement_kind": "embedded", "page": page, "frame_x": 40.0, "frame_y": 40.0, "frame_width": 100.0, "frame_height": 96.0});
    if let Value::Object(more) = extra {
        p.as_object_mut().unwrap().extend(more);
    }
    ok(v, "asset.place", p)
}

fn reopen(v: &mut Viewer, s: &Scratch, name: &str) -> Viewer {
    let path = s.0.join(name);
    v.save(&path, false).unwrap_or_else(|e| panic!("{e}"));
    let mut back = Viewer::new(Prefer::Cpu);
    back.open(&path).unwrap_or_else(|e| panic!("{e}"));
    back
}

/// The layout takes an edit (it is not locked by a stale entry).
fn editable(v: &mut Viewer) {
    let first = page_ids(v)[0].clone();
    v.layout_apply(LayoutEdit::DrawFrame { page_id: first, x: 150.0, y: 150.0, w: 10.0, h: 10.0 }).expect("the layout takes an edit");
}

/// Ledger 2026-11-11T10:39:51Z / 2026-11-12T17:43:00Z: deleting a threaded text-frame layer closes the thread over
/// it (a -> c) in ONE undo step, the story stays; undo restores the frame and its links; after save and reopen the
/// links hold and the layout is editable. The last frame of a story takes the story with it.
#[test]
fn a_threaded_frame_delete_relinks_the_thread_in_one_undo_step() {
    let s = Scratch::new("cascade-relink");
    let mut v = layout(3);
    let a = frame(&mut v, "page-1", 40.0);
    let b = frame(&mut v, "page-2", 40.0);
    let c = frame(&mut v, "page-3", 40.0);
    thread(&mut v, &a, &b);
    thread(&mut v, &b, &c);
    let story = story_of(&mut v, &a);
    set_text(&mut v, &story, &long_text(200));
    let steps = undo_labels(&v).len();
    v.delete_layer(&b).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undo_labels(&v).len(), steps + 1, "one step: {:?}", undo_labels(&v));
    assert!(!layer_ids(&v).contains(&b));
    assert_eq!((links(&v, &a).1, links(&v, &c).0), (Some(c.clone()), Some(a.clone())), "the thread continues past the deleted frame");
    assert!(stories(&v).contains(&story), "the story stays with its other frames");
    let detail = v.run().ops().last().unwrap()["detail"].clone();
    assert_eq!((detail["layers_removed"].as_u64(), detail["frames_relinked"].as_u64()), (Some(1), Some(2)), "{detail}");
    v.undo().unwrap();
    assert_eq!((links(&v, &a).1, links(&v, &b), links(&v, &c).0), (Some(b.clone()), (Some(a.clone()), Some(c.clone())), Some(b.clone())), "undo restores the frame and its links");
    v.redo().unwrap();
    let mut back = reopen(&mut v, &s, "relinked.handshake");
    assert_eq!((links(&back, &a).1, links(&back, &c).0), (Some(c.clone()), Some(a.clone())));
    editable(&mut back);
    // The story's last frames: deleting a, then c, removes the story with the last one.
    back.delete_layer(&a).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(links(&back, &c), (None, None), "c starts the thread now");
    back.delete_layer(&c).unwrap_or_else(|e| panic!("{e}"));
    assert!(!stories(&back).contains(&story), "no frame shows the story: it goes");
    editable(&mut back);
}

/// Ledger 2026-11-12T17:43:00Z: page.delete with content removes the page's text frames and graphic frames (layer
/// and record) and closes the thread over them, in ONE undo step; undo restores page, frames, picture, record and
/// links; the layout reopens editable.
#[test]
fn a_page_delete_takes_its_frames_and_relinks_the_thread() {
    let s = Scratch::new("cascade-page");
    let mut v = layout(3);
    let a = frame(&mut v, "page-1", 40.0);
    let b = frame(&mut v, "page-2", 40.0);
    let c = frame(&mut v, "page-3", 40.0);
    thread(&mut v, &a, &b);
    thread(&mut v, &b, &c);
    let pic = picture(&mut v, &s, "page-2", json!({}))["layer_id"].as_str().unwrap().to_owned();
    let steps = undo_labels(&v).len();
    v.delete_page("page-2").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undo_labels(&v).len(), steps + 1, "one step: {:?}", undo_labels(&v));
    assert_eq!(page_ids(&v), ["page-1", "page-3"]);
    assert!(!layer_ids(&v).contains(&b) && !layer_ids(&v).contains(&pic), "the page's frames go with it");
    assert_eq!(graphic_frames(&v), 0, "and the picture's frame record");
    assert_eq!((links(&v, &a).1, links(&v, &c).0), (Some(c.clone()), Some(a.clone())));
    let detail = v.run().ops().last().unwrap()["detail"].clone();
    assert_eq!((detail["page_id"].as_str(), detail["layers_removed"].as_u64()), (Some("page-2"), Some(2)), "{detail}");
    v.undo().unwrap();
    assert_eq!(page_ids(&v), ["page-1", "page-2", "page-3"]);
    assert!(layer_ids(&v).contains(&b) && layer_ids(&v).contains(&pic));
    assert_eq!((graphic_frames(&v), links(&v, &b)), (1, (Some(a.clone()), Some(c.clone()))), "undo restores all of it");
    v.redo().unwrap();
    let mut back = reopen(&mut v, &s, "page.handshake");
    assert_eq!(page_ids(&back), ["page-1", "page-3"]);
    editable(&mut back);
}

/// Ledger 2026-11-11T12:51:10Z: a page alone on its spread goes with the spread and every frame layer the spread
/// holds; undo restores the spread under its id with the frames; reopen editable.
#[test]
fn a_spread_holding_frame_layers_goes_with_its_page() {
    let s = Scratch::new("cascade-spread");
    let mut v = layout(1);
    v.add_page(Some("page-1"), true, None, None).unwrap();
    let spreads = |v: &Viewer| document(v).page_spreads.iter().map(|s| s.page_spread_id.clone()).collect::<Vec<_>>();
    let before = spreads(&v);
    assert_eq!(before.len(), 2);
    let t = frame(&mut v, "page-2", 40.0);
    let pic = picture(&mut v, &s, "page-2", json!({}))["layer_id"].as_str().unwrap().to_owned();
    v.delete_page("page-2").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((spreads(&v), layer_ids(&v)), (vec![before[0].clone()], Vec::<String>::new()), "spread, frame and picture go");
    v.undo().unwrap();
    assert_eq!(spreads(&v), before, "undo restores the spread under its id");
    assert!(layer_ids(&v).contains(&t) && layer_ids(&v).contains(&pic));
    v.redo().unwrap();
    let mut back = reopen(&mut v, &s, "spread.handshake");
    editable(&mut back);
}

const DOC: &str = "SDOC-019abcde-0000-7000-8000-00000000d0c1";
const SPREAD: &str = "SPGS-019abcde-0000-7000-8000-00000000d0c1";
const GROUP: &str = "SLYR-019abcde-0000-7000-8000-00000000d0c0";
const STORY: &str = "STXT-019abcde-0000-7000-8000-00000000d0c1";

fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}

fn frame_layer(n: u32, page: &str, parent: Parent, previous: Option<u32>, next: Option<u32>) -> StudioLayer {
    let id = |n: u32| format!("SLYR-019abcde-0000-7000-8000-00000000d0c{n}");
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id(n),
        name: format!("frame {n}"),
        parent,
        kind: "text".into(),
        payload: Payload::Text { story_id: STORY.into() },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: page.into(),
            bounds: Bounds { x: len(40.0), y: len(40.0), width: len(100.0), height: len(100.0) },
            inset_spacing: LayoutEdges { top: len(0.0), right: len(0.0), bottom: len(0.0), left: len(0.0) },
            text_column_count: 1,
            text_column_gutter: len(12.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: previous.map(id),
            next_text_frame: next.map(id),
        }),
    }
}

/// Three pages; frame 1 (page-1) -> frame 2 (page-2, inside group GROUP) -> frame 3 (page-3), one story.
fn grouped_frame_package(dir: &Path) -> PathBuf {
    let page = |key: &str| StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::SingleSided, margins: LayoutEdges { top: len(10.0), right: len(10.0), bottom: len(10.0), left: len(10.0) }, bleed: None, applied_trap_preset: None };
    let on_spread = || Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: SPREAD.into() });
    let in_group = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: GROUP.into() });
    let group: StudioLayer = serde_json::from_value(json!({"schema_id": "hsk.studio.layer@1", "layer_id": GROUP, "name": "group", "parent": Parent::Root(()), "kind": "group", "payload": {"payload_kind": "group"}, "visible": true})).unwrap();
    let op = |layer: &str, group: bool| -> Operation {
        serde_json::from_value(if group {
            json!({"layer_id": layer, "operation_key": "mix", "operation_kind": "composite", "inputs": [{"port_key": "inputs", "value_type": "image", "cardinality": "many", "semantic": "ordered_composite"}], "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "composite_result"}]})
        } else {
            json!({"layer_id": layer, "operation_key": "source", "operation_kind": "source", "inputs": [], "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]})
        })
        .unwrap()
    };
    let inner = frame_layer(2, "page-2", in_group, Some(1), Some(3));
    let edge: Edge = serde_json::from_value(json!({"source": {"layer_id": inner.layer_id, "operation_key": "source", "port_key": "out"}, "target": {"layer_id": GROUP, "operation_key": "mix", "port_key": "inputs"}, "dependency_kind": "data", "input_ordinal": 0})).unwrap();
    let output: PortAddress = serde_json::from_value(json!({"layer_id": GROUP, "operation_key": "mix", "port_key": "out"})).unwrap();
    let unassigned = || ProfileBinding::Unassigned(());
    let d = StudioDocument {
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
        page_spreads: vec![StudioPageSpread { schema_id: PageSpreadSchema::V1, page_spread_id: SPREAD.into(), name: "spread".into(), parent: Parent::Root(()), bounds: Bounds { x: len(0.0), y: len(0.0), width: len(600.0), height: len(200.0) }, pages: vec![page("page-1"), page("page-2"), page("page-3")] }],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![op(GROUP, true), op(&inner.layer_id, false)], edges: vec![edge], outputs: vec![output] },
        layers: vec![frame_layer(1, "page-1", on_spread(), None, Some(2)), group, inner, frame_layer(3, "page-3", on_spread(), Some(2), None)],
        sequences: vec![],
    };
    let token = CancellationToken::default();
    let snapshot = match inspect_bytes(&serde_json::to_vec(&d).unwrap(), Budget::default(), &token, &Unresolved).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let mut bytes = Vec::new();
    write_package_with(&snapshot, &MemoryAssetSource::new(), &Extras { quarantined: &[], previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    let path = dir.join("grouped.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Ledger 2026-11-11T12:51:10Z: deleting a group that holds a threaded frame layer removes the group, the frame
/// and its graph entries and closes the thread over the frame, in ONE undo step; undo restores all of it.
#[test]
fn a_group_holding_a_frame_layer_deletes_with_the_thread_closed() {
    let s = Scratch::new("cascade-group");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&grouped_frame_package(&s.0)).unwrap_or_else(|e| panic!("{e}"));
    let id = |n: u32| format!("SLYR-019abcde-0000-7000-8000-00000000d0c{n}");
    let before = document(&v);
    let steps = undo_labels(&v).len();
    v.delete_layer(GROUP).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undo_labels(&v).len(), steps + 1);
    assert_eq!(layer_ids(&v), [id(1), id(3)], "the group and the frame inside it go");
    let d = document(&v);
    assert!(d.graph.operations.is_empty() && d.graph.edges.is_empty() && d.graph.outputs.is_empty(), "with their graph entries");
    assert_eq!((links(&v, &id(1)).1, links(&v, &id(3)).0), (Some(id(3)), Some(id(1))));
    v.undo().unwrap();
    let mut restored = document(&v);
    restored.revision = before.revision;
    assert_eq!(restored, before, "undo restores the group, the frame, the graph and the links");
    v.redo().unwrap();
    let mut back = reopen(&mut v, &s, "grouped-deleted.handshake");
    editable(&mut back);
}

/// Ledger 2026-11-11T12:51:10Z: asset.place with `frame_to_content` stores the content-fitted frame (64 x 48 pt for
/// a 64 x 48 pixel source at 72 ppi), not the bounds given, as asset.fit does.
#[test]
fn asset_place_frame_to_content_stores_the_fitted_frame() {
    let s = Scratch::new("cascade-fit");
    let mut v = layout(1);
    let placed = picture(&mut v, &s, "page-1", json!({"fit": "frame_to_content"}));
    let stored = records(&v)["longdoc"]["graphic_frames"][0]["bounds"].clone();
    let side = |b: &Value, k: &str| b[k]["value"].as_f64().unwrap();
    assert_eq!((side(&stored, "width"), side(&stored, "height")), (64.0, 48.0), "{stored}");
    assert_eq!((placed["frame"]["width"].as_f64(), placed["frame"]["height"].as_f64()), (Some(64.0), Some(48.0)), "{placed}");
}

/// STU-LAY-020 / STU-LAY-123 flow modes on an overset story in a frame on page-1 of three pages: `flow.semi_auto`
/// places one frame on the next page; `flow.fixed_page_auto` fills the existing pages and stops with the cursor
/// loaded; `flow.auto` adds pages until the story ends. Each is ONE undo step `layout.autoflow`; undo takes it all
/// back; the frames and pages survive save and reopen.
#[test]
fn autoflow_modes_place_frames_and_pages_in_one_undo_step() {
    let s = Scratch::new("cascade-autoflow");
    let mut v = layout(3);
    let a = frame(&mut v, "page-1", 40.0);
    let story = story_of(&mut v, &a);
    set_text(&mut v, &story, &long_text(400));
    let start = (layer_ids(&v), page_ids(&v), undo_labels(&v).len());

    let semi = ok(&mut v, "layout_flow.autoflow", json!({"frame": a, "mode": "flow.semi_auto"}));
    assert_eq!((semi["frames"].as_array().map(Vec::len), semi["cursor_loaded"].as_bool(), semi["undo_name"].as_str()), (Some(1), Some(true), Some("layout.autoflow")), "{semi}");
    let b = semi["frames"][0].as_str().unwrap().to_owned();
    assert_eq!((links(&v, &a).1, document(&v).layers.iter().find(|l| l.layer_id == b).unwrap().text_frame.as_ref().unwrap().page_id.as_str()), (Some(b.clone()), "page-2"));
    v.undo().unwrap();
    assert_eq!((layer_ids(&v), page_ids(&v), undo_labels(&v).len()), start.clone(), "undo takes the flow back");

    let fixed = ok(&mut v, "layout_flow.autoflow", json!({"frame": a, "mode": "flow.fixed_page_auto"}));
    assert_eq!((fixed["frames"].as_array().map(Vec::len), fixed["pages_added"].as_array().map(Vec::len), fixed["cursor_loaded"].as_bool(), fixed["stopped"].as_str()), (Some(2), Some(0), Some(true), Some("no_page")), "{fixed}");
    v.undo().unwrap();

    let auto = ok(&mut v, "layout_flow.autoflow", json!({"frame": a, "mode": "flow.auto"}));
    let added = auto["pages_added"].as_array().map_or(0, Vec::len);
    assert!(added >= 1, "the story needs more than three pages: {auto}");
    assert_eq!((auto["cursor_loaded"].as_bool(), auto["stopped"].clone()), (Some(false), Value::Null), "{auto}");
    assert_eq!(undo_labels(&v).len(), start.2 + 1, "one step");
    assert_eq!(page_ids(&v).len(), 3 + added);
    assert!(!v.layout_editor().unwrap().thread_info(&a).unwrap().overflows, "the story fits now");
    let mut back = reopen(&mut v, &s, "flowed.handshake");
    assert_eq!(page_ids(&back).len(), 3 + added);
    back.layout_sync();
    assert!(!back.layout_editor().unwrap().thread_info(&a).unwrap().overflows, "the reopened story fits");
    editable(&mut back);
}

/// Hostile and wrong input refuses typed and changes nothing: an unknown mode, an unknown frame, a frame that
/// does not end its thread, an unknown page.
#[test]
fn autoflow_refusals_are_typed_and_change_nothing() {
    let mut v = layout(2);
    let a = frame(&mut v, "page-1", 40.0);
    let b = frame(&mut v, "page-2", 40.0);
    thread(&mut v, &a, &b);
    let story = story_of(&mut v, &a);
    set_text(&mut v, &story, &long_text(400));
    let before = (document(&v), undo_labels(&v));
    let code = |v: &mut Viewer, p: Value| cmd(v, "layout_flow.autoflow", p).expect_err("refused").0;
    assert_eq!(code(&mut v, json!({"frame": b, "mode": "flow.sideways"})), "parameter_invalid");
    assert_eq!(code(&mut v, json!({"frame": "SLYR-nope", "mode": "flow.auto"})), "unknown_frame");
    assert_eq!(code(&mut v, json!({"frame": a, "mode": "flow.auto"})), "already_threaded");
    assert_eq!(code(&mut v, json!({"frame": b, "mode": "flow.manual", "page": "page-99"})), "unknown_page");
    assert_eq!((document(&v), undo_labels(&v)), before, "nothing changed");
}

/// STU-LAY-123 smart text reflow, run once: a grown story adds frames and pages (one step `layout.smart_reflow`);
/// after the text shrinks, `delete_empty_pages` removes the trailing pages holding only empty frames of the story,
/// in pairs on this facing-pages document with `preserve_facing_page_spreads`.
#[test]
fn smart_reflow_adds_pages_for_overset_text_and_removes_empty_ones() {
    let mut v = layout(1);
    let a = frame(&mut v, "page-1", 40.0);
    let story = story_of(&mut v, &a);
    set_text(&mut v, &story, &long_text(400));
    let controls = |delete: bool| json!({"frame": a, "delete_empty_pages": delete, "preserve_facing_page_spreads": true});
    let grown = ok(&mut v, "layout_flow.smart_reflow", controls(false));
    let added = grown["pages_added"].as_array().map_or(0, Vec::len);
    assert!(added >= 2 && added % 2 == 0, "pages in pairs: {grown}");
    assert_eq!((grown["cursor_loaded"].as_bool(), grown["undo_name"].as_str()), (Some(false), Some("layout.smart_reflow")), "{grown}");
    // The story shrinks to one frame's worth: the pages after page-1 hold empty frames only.
    let text_len = u64::try_from(long_text(400).len()).unwrap();
    ok(&mut v, "layout.replace_text", json!({"story": story, "start": 0, "end": text_len, "text": "short"}));
    let kept = ok(&mut v, "layout_flow.smart_reflow", controls(false));
    assert_eq!(kept["changed"], false, "without delete_empty_pages nothing goes: {kept}");
    let shrunk = ok(&mut v, "layout_flow.smart_reflow", controls(true));
    let removed = shrunk["pages_removed"].as_array().map_or(0, Vec::len);
    assert_eq!(removed % 2, 0, "{shrunk}");
    assert!(removed >= added - 1, "{shrunk}");
    assert_eq!(page_ids(&v).len(), 1 + added - removed);
    assert_eq!(links(&v, &a).0, None);
}

// ---- the process: no Rust call (as tests/r60_layout_followups.rs) -------------------------------------

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

/// A model drives page.delete with content and layout_flow.autoflow through the JSONL loop: both reply ok and
/// the reopened package holds the result.
#[test]
fn the_process_drives_the_cascade_and_autoflow() {
    use std::io::Write as _;
    let s = Scratch::new("cascade-drive");
    let mut v = layout(3);
    let a = frame(&mut v, "page-1", 40.0);
    let b = frame(&mut v, "page-2", 40.0);
    let story = story_of(&mut v, &a);
    set_text(&mut v, &story, &long_text(400));
    let package = s.0.join("drive.handshake");
    v.save(&package, false).unwrap();
    let saved = s.0.join("driven.handshake");
    let lines = [
        json!({"command": "command", "command_id": "page.delete", "parameters": {"page": "page-2"}}),
        json!({"command": "command", "command_id": "layout_flow.autoflow", "parameters": {"frame": a, "mode": "flow.fixed_page_auto"}}),
        json!({"command": "command", "command_id": "document.save", "parameters": {"path": saved.to_str().unwrap()}}),
    ];
    let mut command = std::process::Command::new(binary());
    command
        .args(["command", package.to_str().unwrap(), "--actor", "layout-model", "--session", "s-1", "--stdin"])
        .current_dir(&s.0)
        .env("STUDIO_DIAG_DIR", &s.0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().expect("spawn studio-harness");
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let replies: Vec<Value> = String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for i in 1..=lines.len() {
        assert_eq!(replies[i]["ok"], true, "reply {i}: {}", replies[i]);
    }
    let mut back = Viewer::new(Prefer::Cpu);
    back.open(&saved).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(page_ids(&back), ["page-1", "page-3"]);
    assert!(!layer_ids(&back).contains(&b));
    assert_eq!(links(&back, &a).1.map(|f| document(&back).layers.iter().find(|l| l.layer_id == f).unwrap().text_frame.as_ref().unwrap().page_id.clone()), Some("page-3".to_owned()), "autoflow placed a frame on page-3");
}
