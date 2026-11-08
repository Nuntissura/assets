//! Lane SMB-LAYOUT-PLACE-1 (feature `viewer`; STU-LAY-028, STU-LAY-029, STU-LAY-131, STU-LAY-132,
//! STU-RAS-012, STU-ASSET-007, STU-ASSET-009): a picture placed into a graphic frame on a layout page is
//! stored, fitted, link-checked, one undo step, saved and drawn in the layout PDF. Every command goes by
//! its id string (`Viewer::run_command`), one test drives the real process over JSONL. Oracles read back
//! independently of the export code: press's renderer gives the PDF page's pixels, the session's entries
//! give the stored records. Scenario steps: print-publication 48 and 51, board-slides-web 42.
#![cfg(feature = "viewer")]
mod common;
use common::Scratch;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_interop_raster as raster;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

const RED: [u8; 3] = [220, 40, 40];
const BLUE: [u8; 3] = [40, 60, 220];

/// A solid opaque RGB PNG.
fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let samples = raster::Samples::U8((0..width * height).flat_map(|_| rgb).collect());
    raster::encode_png(&raster::PngEncodeRequest { width, height, colour: raster::Colour::Rgb, samples: &samples, icc: None, compression: raster::PngCompression::Fast }).unwrap()
}

fn write(path: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

fn params(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("parameters must be an object: {other}"),
    }
}

/// One typed command by id; the refusal as `Err((code, detail))`.
fn cmd(v: &mut Viewer, id: &str, p: Value) -> Result<Value, (String, Value)> {
    v.run_command(id, &params(p)).map_err(|r| (r.code, r.detail))
}

fn ok(v: &mut Viewer, id: &str, p: Value) -> Value {
    cmd(v, id, p).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}

fn refused(v: &mut Viewer, id: &str, p: Value) -> String {
    match cmd(v, id, p) {
        Ok(r) => panic!("{id} should refuse: {r}"),
        Err((code, _)) => code,
    }
}

/// A new 595 x 842 pt layout document (`document.new`, profile `layout`).
fn layout(pages: usize) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((595.0, 842.0))), None).unwrap();
    for _ in 1..pages {
        v.add_page(Some("page-1"), false, None, None).unwrap();
    }
    v
}

/// A layout document saved in `s` and reopened from there, so links resolve against `s`.
fn saved_layout(s: &Scratch) -> Viewer {
    let mut v = layout(1);
    let package = s.0.join("issue.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    w
}

/// `asset.place` of `path` into a frame on `page`.
fn place(v: &mut Viewer, path: &Path, kind: &str, page: &str, frame: [f64; 4], extra: Value) -> Result<Value, (String, Value)> {
    let mut p = json!({"path": path.to_str().unwrap(), "placement_kind": kind, "page": page, "frame_x": frame[0], "frame_y": frame[1], "frame_width": frame[2], "frame_height": frame[3]});
    if let Value::Object(more) = extra {
        p.as_object_mut().unwrap().extend(more);
    }
    cmd(v, "asset.place", p)
}

fn export(v: &mut Viewer, pdf: &Path) -> Result<Value, (String, Value)> {
    cmd(v, "layout.export_pdf", json!({"path": pdf.to_str().unwrap(), "overwrite": true}))
}

/// Page `page` of `pdf` at 1 px per pt (`layout.render_pdf_page`), decoded: (width, rgb rows).
fn render(v: &mut Viewer, pdf: &Path, page: usize, out: &Path) -> (usize, Vec<u8>, Value) {
    let r = ok(v, "layout.render_pdf_page", json!({"source": pdf.to_str().unwrap(), "page": page, "px_per_pt": 1.0, "path": out.to_str().unwrap(), "overwrite": true}));
    let decoded = raster::decode(&std::fs::read(out).unwrap(), &raster::Limits::default(), &hsk_studio_accord::CancellationToken::default()).unwrap();
    let raster::Samples::U8(samples) = decoded.samples else { panic!("8-bit render") };
    assert_eq!(decoded.colour.channels(), 3);
    (decoded.width as usize, samples, r)
}

fn at(img: &(usize, Vec<u8>, Value), x: usize, y: usize) -> [u8; 3] {
    let i = (y * img.0 + x) * 3;
    [img.1[i], img.1[i + 1], img.1[i + 2]]
}

fn white(p: [u8; 3]) -> bool {
    p.iter().all(|c| *c >= 250)
}

/// Near `rgb` (the renderer's colour route: within 12 per channel).
fn near(p: [u8; 3], rgb: [u8; 3]) -> bool {
    p.iter().zip(rgb).all(|(a, b)| a.abs_diff(b) <= 12)
}

fn layout_records(v: &Viewer) -> Option<Value> {
    v.run().document_entry("layout_records").map(|b| serde_json::from_slice(b).unwrap())
}

fn frames(v: &Viewer) -> Vec<Value> {
    layout_records(v).and_then(|r| r["longdoc"]["graphic_frames"].as_array().cloned()).unwrap_or_default()
}

fn layers(v: &Viewer) -> usize {
    v.run().snapshot().unwrap().document().layers.len()
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    serde_json::from_value(v.run().history()["undo"].clone()).unwrap()
}

/// Everything a refused command must leave as it was: document (revision included), both entries, history.
fn state(v: &Viewer) -> (Value, Option<Vec<u8>>, Option<Vec<u8>>, Value) {
    let doc = serde_json::to_value(v.run().snapshot().unwrap().document()).unwrap();
    let entry = |t: &str| v.run().document_entry(t).map(<[u8]>::to_vec);
    (doc, entry("layout_records"), entry("raster_records"), v.run().history()["undo"].clone())
}

fn losses(r: &Value) -> Vec<String> {
    r["loss"].as_array().unwrap().iter().map(|l| l["code"].as_str().unwrap().to_owned()).collect()
}

/// Steps print 48 and 51, deck 42: an embedded 64 x 48 picture placed into a frame on page 1 is drawn by
/// the layout PDF inside the frame and clipped to it (fill proportionally overflows the frame's sides);
/// two exports are byte-identical, and save, reopen, export gives the same bytes.
#[test]
fn a_picture_placed_into_a_frame_is_drawn_inside_it_and_clipped() {
    let s = Scratch::new("layout-place-draw");
    let mut v = layout(1);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    // Frame 100 x 96 at (100, 200); content 64 x 48 pt at 72 ppi, filled proportionally: scale 2, so the
    // content is 128 x 96, centred: x 86..214, the frame shows x 100..200.
    let placed = place(&mut v, &hero, "embedded", "page-1", [100.0, 200.0, 100.0, 96.0], json!({"fit": "fill_proportionally"})).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(placed["page_id"], "page-1", "{placed}");
    assert_eq!((placed["content_width_pt"].as_f64(), placed["content_height_pt"].as_f64()), (Some(64.0), Some(48.0)), "{placed}");
    assert_eq!((placed["pixel_width"].as_u64(), placed["pixel_height"].as_u64()), (Some(64), Some(48)), "{placed}");
    assert_eq!(placed["ppi_source"], "default", "a PNG states no ppi to the import route: {placed}");
    assert!(undo_labels(&v).last().is_some_and(|l| l.starts_with("asset.place")), "{:?}", undo_labels(&v));
    let frame = frames(&v);
    assert_eq!(frame.len(), 1, "one graphic frame record: {frame:?}");
    assert_eq!(frame[0]["layer_id"], placed["layer_id"]);
    assert_eq!(frame[0]["fit"], "fill_proportionally");

    let pdf = s.0.join("issue.pdf");
    let r = export(&mut v, &pdf).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(r["tiles_drawn"].as_u64().unwrap() > 0, "{r}");
    assert!(!losses(&r).iter().any(|c| c == "raster_unresolved"), "{r}");
    let rows = r["placements"].as_array().expect("the receipt lists the placements");
    assert_eq!(rows.len(), 1, "{r}");
    assert_eq!((rows[0]["layer_id"].clone(), rows[0]["placement_kind"].as_str(), rows[0]["link_state"].as_str()), (placed["layer_id"].clone(), Some("embedded"), Some("embedded")), "{r}");
    assert!(rows[0]["content_digest"].as_str().is_some_and(|d| d.starts_with("sha256:")), "{r}");

    let img = render(&mut v, &pdf, 0, &s.0.join("p1.png"));
    for (x, y) in [(102, 202), (150, 248), (198, 294)] {
        assert!(near(at(&img, x, y), RED), "inside the frame at ({x}, {y}): {:?}", at(&img, x, y));
    }
    // Content would be at x 86..100 and 200..214: the frame clips it.
    for (x, y) in [(92, 248), (206, 248), (150, 197), (150, 299)] {
        assert!(white(at(&img, x, y)), "outside the frame at ({x}, {y}): {:?}", at(&img, x, y));
    }

    let again = s.0.join("again.pdf");
    let r2 = export(&mut v, &again).unwrap();
    assert_eq!(r2["sha256"], r["sha256"], "two exports are byte-identical");
    let package = s.0.join("placed.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    let r3 = export(&mut w, &s.0.join("reopened.pdf")).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(r3["sha256"], r["sha256"], "save, reopen, export: the same bytes");
}

/// A frame on the right page of a two-page spread is drawn on that page, not on the left one (frame
/// bounds are page coordinates; the layer's parent is the spread).
#[test]
fn the_right_page_of_a_spread_draws_its_own_frame() {
    let s = Scratch::new("layout-place-spread");
    let mut v = layout(2);
    let spreads = v.run().snapshot().unwrap().document().page_spreads.clone();
    assert_eq!((spreads.len(), spreads[0].pages.len()), (1, 2), "one spread of two pages");
    let page = spreads[0].pages[1].page_id.clone();
    let hero = write(&s.0.join("hero.png"), &png(64, 48, BLUE));
    place(&mut v, &hero, "embedded", &page, [50.0, 50.0, 64.0, 48.0], json!({"fit": "content_to_frame"})).unwrap_or_else(|e| panic!("{e:?}"));
    let pdf = s.0.join("spread.pdf");
    export(&mut v, &pdf).unwrap_or_else(|e| panic!("{e:?}"));
    let right = render(&mut v, &pdf, 1, &s.0.join("right.png"));
    assert!(near(at(&right, 80, 74), BLUE), "{:?}", at(&right, 80, 74));
    assert!(white(at(&right, 45, 74)) && white(at(&right, 120, 74)), "{:?} {:?}", at(&right, 45, 74), at(&right, 120, 74));
    let left = render(&mut v, &pdf, 0, &s.0.join("left.png"));
    assert_eq!(left.2["non_white_pixels"], 0, "the left page is blank: {}", left.2);
}

/// One `asset.place` is ONE undo step: undo removes the layer, the frame record and the raster records
/// entry together; redo restores all three byte for byte.
#[test]
fn one_place_is_one_undo_step_and_redo_restores_it_byte_for_byte() {
    let s = Scratch::new("layout-place-undo");
    let mut v = layout(1);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    let before = state(&v);
    let steps = undo_labels(&v).len();
    place(&mut v, &hero, "embedded", "page-1", [10.0, 10.0, 64.0, 48.0], json!({})).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(undo_labels(&v).len(), steps + 1, "one step: {:?}", undo_labels(&v));
    let after = state(&v);
    assert!(after.2.is_some(), "the raster records entry holds the placement");
    assert_eq!(frames(&v).len(), 1);
    v.undo().unwrap();
    let undone = state(&v);
    assert_eq!(layers(&v), 0, "the layer is gone");
    assert_eq!(frames(&v).len(), 0, "the frame record is gone");
    assert_eq!((&undone.1, &undone.2), (&before.1, &before.2), "both entries are back to before");
    v.redo().unwrap();
    let redone = state(&v);
    assert_eq!((&redone.1, &redone.2), (&after.1, &after.2), "redo restores both entries byte for byte");
    let strip = |d: &Value| {
        let mut d = d.clone();
        d["revision"] = Value::Null;
        d
    };
    assert_eq!(strip(&redone.0), strip(&after.0), "redo restores the document");
    // The next layout step after a redo is not refused stale.
    ok(&mut v, "layout.draw_frame", json!({"page_id": "page-1", "x": 300.0, "y": 300.0, "w": 100.0, "h": 40.0}));
    assert_eq!(frames(&v).len(), 1, "a layout edit keeps the graphic frame");
}

/// STU-LAY-029 / STU-LAY-131 through `asset.fit`: frame to content resizes the frame record at its
/// anchor, the stored rule is re-applied, and the refusals are typed and change nothing.
#[test]
fn fit_refits_a_graphic_frame_in_one_step() {
    let s = Scratch::new("layout-place-fit");
    let mut v = layout(1);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    let placed = place(&mut v, &hero, "embedded", "page-1", [100.0, 200.0, 100.0, 96.0], json!({"fit": "proportionally"})).unwrap_or_else(|e| panic!("{e:?}"));
    let layer = placed["layer_id"].as_str().unwrap().to_owned();
    let steps = undo_labels(&v).len();
    let fitted = ok(&mut v, "asset.fit", json!({"layer": layer, "fit": "frame_to_content"}));
    assert_eq!(undo_labels(&v).len(), steps + 1);
    assert!(undo_labels(&v).last().is_some_and(|l| l.starts_with("asset.fit")), "{:?}", undo_labels(&v));
    // Centre anchor: the 64 x 48 frame keeps the old frame's centre (150, 248).
    let b = &frames(&v)[0]["bounds"];
    let n = |k: &str| b[k]["value"].as_f64().unwrap();
    assert_eq!((n("x"), n("y"), n("width"), n("height")), (118.0, 224.0, 64.0, 48.0), "{fitted}");
    // A stored rule is re-applied; without one the fit refuses.
    let before = state(&v);
    assert_eq!(refused(&mut v, "asset.fit", json!({"layer": layer, "fit": "apply_frame_fitting_options"})), "no_stored_fitting");
    assert_eq!(refused(&mut v, "asset.fit", json!({"layer": layer, "fit": "content_aware"})), "content_aware_unavailable");
    assert_eq!(refused(&mut v, "asset.fit", json!({"layer": layer, "fit": "honour_existing_style"})), "fit_not_legal_here");
    assert_eq!(refused(&mut v, "asset.fit", json!({"layer": layer, "fit": "proportionally", "crop_left": 40.0, "crop_right": 30.0})), "crop_empties_content");
    assert_eq!(refused(&mut v, "asset.fit", json!({"layer": "SLYR-019abcde-0000-7000-8000-00000000dead", "fit": "proportionally"})), "layer_not_graphic_frame");
    assert_eq!(state(&v), before, "refusals change nothing");
    ok(&mut v, "asset.fit", json!({"layer": layer, "fit": "fill_proportionally", "frame_fitting": "center_content", "anchor": "top_left"}));
    ok(&mut v, "asset.fit", json!({"layer": layer, "fit": "apply_frame_fitting_options"}));
    let f = &frames(&v)[0];
    assert_eq!((f["fit"].as_str(), f["frame_fitting"].as_str(), f["anchor"].as_str()), (Some("center_content"), Some("center_content"), Some("top_left")), "{f}");
    v.undo().unwrap();
    assert_eq!(frames(&v)[0]["fit"], "fill_proportionally", "undo takes back one fit");
}

/// STU-ASSET-007 / STU-ASSET-009 on layout frames: a linked picture's state is listed with its frame and
/// resolutions; an export with a modified or missing link fails closed and writes nothing; Update All
/// Links re-reads the file at source resolution and keeps the frame.
#[test]
fn linked_layout_pictures_list_their_state_and_export_fails_closed() {
    let s = Scratch::new("layout-place-linked");
    let mut v = saved_layout(&s);
    let stall = write(&s.0.join("stall.png"), &png(64, 48, RED));
    let placed = place(&mut v, &stall, "linked", "page-1", [100.0, 200.0, 128.0, 96.0], json!({"fit": "proportionally"})).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!((placed["placement_kind"].as_str(), placed["uri"].as_str()), (Some("linked"), Some("stall.png")), "{placed}");
    let rows = ok(&mut v, "asset.list_placements", json!({}))["placements"].as_array().cloned().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!((row["page_id"].as_str(), row["link_state"].as_str(), row["fit"].as_str()), (Some("page-1"), Some("resolved"), Some("proportionally")), "{row}");
    assert_eq!(row["frame"], json!({"x": 100.0, "y": 200.0, "width": 128.0, "height": 96.0}), "{row}");
    // 64 px over 64 pt = 72 ppi actual; scaled 2x into the frame, 36 ppi effective.
    assert_eq!((row["actual_ppi"][0].as_f64(), row["effective_ppi"][0].as_f64()), (Some(72.0), Some(36.0)), "{row}");
    let pdf = s.0.join("linked.pdf");
    let r = export(&mut v, &pdf).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!((r["placements"][0]["link_state"].as_str(), r["placements"][0]["uri"].as_str()), (Some("resolved"), Some("stall.png")), "{r}");

    // Modified: the export refuses and writes nothing.
    write(&stall, &png(64, 48, BLUE));
    let modified = s.0.join("modified.pdf");
    let (code, _) = export(&mut v, &modified).unwrap_err();
    assert_eq!(code, "placed_link_modified");
    assert!(!modified.exists(), "a refused export writes nothing");
    // Update All Links: the new bytes at source resolution, the frame kept.
    let frame_before = frames(&v)[0].clone();
    let updated = ok(&mut v, "asset.update_all_modified", json!({}));
    assert_eq!(updated["changed"], true, "{updated}");
    assert_eq!(frames(&v)[0], frame_before, "the frame is kept");
    let r = export(&mut v, &modified).unwrap_or_else(|e| panic!("{e:?}"));
    let img = render(&mut v, &modified, 0, &s.0.join("m.png"));
    assert!(near(at(&img, 164, 248), BLUE), "the updated bytes are drawn: {:?} {r}", at(&img, 164, 248));
    // Missing: refused, nothing written.
    std::fs::remove_file(&stall).unwrap();
    let missing = s.0.join("missing.pdf");
    assert_eq!(export(&mut v, &missing).unwrap_err().0, "placed_link_missing");
    assert!(!missing.exists());
}

/// Moving the pixels of a graphic-frame layer while its frame record stays would draw them wrong: the
/// transform rows refuse typed.
#[test]
fn transforms_of_a_graphic_frame_layer_refuse_typed() {
    let s = Scratch::new("layout-place-transform");
    let mut v = layout(1);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    let layer = place(&mut v, &hero, "embedded", "page-1", [10.0, 10.0, 64.0, 48.0], json!({})).unwrap_or_else(|e| panic!("{e:?}"))["layer_id"].as_str().unwrap().to_owned();
    let before = state(&v);
    assert_eq!(refused(&mut v, "transform.move", json!({"layer": layer, "dx": 1.0, "dy": 1.0})), "graphic_frame_transform_unsupported");
    assert_eq!(refused(&mut v, "transform.free_transform", json!({"layer": layer, "scale_x": 200.0})), "graphic_frame_transform_unsupported");
    assert_eq!(refused(&mut v, "transform.reset", json!({"layer": layer})), "graphic_frame_transform_unsupported");
    assert_eq!(state(&v), before);
}

/// V4-01/02: hostile frames and sources are typed refusals that change nothing.
#[test]
fn hostile_frames_and_sources_refuse_typed_and_change_nothing() {
    let s = Scratch::new("layout-place-hostile");
    let mut v = saved_layout(&s);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    let before = state(&v);
    let p = hero.to_str().unwrap();
    let base = json!({"path": p, "placement_kind": "embedded"});
    let with = |more: Value| {
        let mut b = base.clone();
        b.as_object_mut().unwrap().extend(params(more));
        b
    };
    let frame = |x: f64, y: f64, w: f64, h: f64| with(json!({"page": "page-1", "frame_x": x, "frame_y": y, "frame_width": w, "frame_height": h}));
    let cases = [
        (base.clone(), "frame_required"),
        (with(json!({"page": "page-1", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0})), "frame_required"),
        (frame(0.0, 0.0, 0.0, 10.0), "frame_out_of_range"),
        (frame(0.0, 0.0, -5.0, 10.0), "frame_out_of_range"),
        (frame(0.0, 0.0, 14_401.0, 10.0), "frame_out_of_range"),
        (frame(1.0e9, 0.0, 10.0, 10.0), "frame_out_of_range"),
        (with(json!({"page": "page-99", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0, "frame_height": 10.0})), "unknown_page"),
        (with(json!({"page": "page-1", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0, "frame_height": 10.0, "shape": "path"})), "frame_shape_unsupported"),
        (with(json!({"page": "page-1", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0, "frame_height": 10.0, "fit": "content_aware"})), "content_aware_unavailable"),
        (with(json!({"page": "page-1", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0, "frame_height": 10.0, "fit": "honour_existing_style"})), "fit_not_legal_here"),
        (with(json!({"page": "page-1", "frame_x": 1.0, "frame_y": 1.0, "frame_width": 10.0, "frame_height": 10.0, "x": 5.0})), "parameter_not_for_frame"),
    ];
    for (p, code) in cases {
        assert_eq!(cmd(&mut v, "asset.place", p.clone()).map_err(|e| e.0), Err(code.to_owned()), "{p}");
    }
    // A header that declares more pixels than a placement holds refuses before the decoder runs (the
    // body is junk: a decode would refuse with the decoder's own code).
    let mut huge = png(1, 1, RED);
    huge[16..20].copy_from_slice(&9000u32.to_be_bytes());
    huge[20..24].copy_from_slice(&9000u32.to_be_bytes());
    let huge = write(&s.0.join("huge.png"), &huge);
    assert_eq!(place(&mut v, &huge, "embedded", "page-1", [1.0, 1.0, 10.0, 10.0], json!({})).map_err(|e| e.0), Err("budget_exceeded".to_owned()));
    // A link outside the document folder.
    let outside = Scratch::new("layout-place-outside");
    let far = write(&outside.0.join("far.png"), &png(8, 8, RED));
    assert_eq!(place(&mut v, &far, "linked", "page-1", [1.0, 1.0, 10.0, 10.0], json!({})).map_err(|e| e.0), Err("link_outside_document_folder".to_owned()));
    assert_eq!(state(&v), before, "every refusal changed nothing");
}

/// The content size at 100 % comes from the source's ppi when the format states one (a TIFF at 144 ppi:
/// 64 x 48 px is 32 x 24 pt).
#[test]
fn a_stated_source_resolution_sets_the_content_size() {
    let s = Scratch::new("layout-place-ppi");
    let mut v = layout(1);
    let samples = hsk_studio_interop_tiff::Samples::U8((0..64 * 48).flat_map(|_| RED).collect());
    let tiff = hsk_studio_interop_tiff::encode(&hsk_studio_interop_tiff::EncodeRequest {
        width: 64,
        height: 48,
        color: hsk_studio_interop_tiff::ColorModel::Rgb,
        alpha: hsk_studio_interop_tiff::Alpha::None,
        bit_depth: 8,
        samples: &samples,
        icc: None,
        compression: hsk_studio_interop_tiff::Compression::None,
        predictor: hsk_studio_interop_tiff::Predictor::None,
        big_tiff: false,
        resolution: Some(hsk_studio_interop_tiff::Resolution { x: (144, 1), y: (144, 1), unit: 2 }),
        orientation: None,
    })
    .unwrap();
    let path = write(&s.0.join("hero.tif"), &tiff);
    let placed = place(&mut v, &path, "embedded", "page-1", [10.0, 10.0, 64.0, 48.0], json!({})).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!((placed["content_width_pt"].as_f64(), placed["content_height_pt"].as_f64(), placed["ppi_source"].as_str()), (Some(32.0), Some(24.0), Some("decoder")), "{placed}");
}

// ---- the process: no Rust call ------------------------------------------------------------------

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
fn session(dir: &Path, package: &Path, input: &[Value]) -> (i32, Vec<Value>) {
    use std::io::Write as _;
    let mut command = std::process::Command::new(binary());
    command
        .args(["command", package.to_str().unwrap(), "--actor", "layout-place-model", "--session", "s-1", "--stdin"])
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

/// The scenario steps as a model runs them: place a picture into a frame, export the layout PDF, look at
/// the page, save; the placement is one named step attributed to the caller and the saved package draws
/// the same PDF.
#[test]
fn a_model_places_a_layout_picture_and_exports_it_through_the_process() {
    let s = Scratch::new("layout-place-process");
    let mut v = layout(1);
    let package = s.0.join("deck.handshake");
    v.save(&package, false).unwrap();
    drop(v);
    let hero = write(&s.0.join("hero.png"), &png(64, 48, RED));
    let (pdf, png_out, out) = (s.0.join("deck.pdf"), s.0.join("deck-1.png"), s.0.join("placed.handshake"));
    let call = |id: &str, parameters: Value| json!({"command": "command", "command_id": id, "parameters": parameters});
    let input = [
        call("asset.place", json!({"path": hero.to_str().unwrap(), "placement_kind": "embedded", "page": "page-1", "frame_x": 100, "frame_y": 200, "frame_width": 128, "frame_height": 96, "fit": "proportionally"})),
        call("layout.export_pdf", json!({"path": pdf.to_str().unwrap()})),
        call("layout.render_pdf_page", json!({"source": pdf.to_str().unwrap(), "page": 0, "px_per_pt": 1.0, "path": png_out.to_str().unwrap()})),
        call("asset.place", json!({"path": hero.to_str().unwrap(), "placement_kind": "embedded"})),
        call("document.save", json!({"path": out.to_str().unwrap()})),
    ];
    let (exit, r) = session(&s.0, &package, &input);
    assert_eq!(r.len(), input.len() + 2, "open, one reply per line, end: {r:?}");
    for n in [1, 2, 3, 5] {
        assert_eq!(r[n]["ok"], true, "line {n}: {}", r[n]);
    }
    assert!(r[1]["detail"]["undo_name"].as_str().is_some_and(|n| n.starts_with("asset.place")), "{}", r[1]);
    assert_eq!(r[1]["attribution"]["actor"]["account_id"], "layout-place-model", "{}", r[1]["attribution"]);
    let export = &r[2]["detail"]["result"];
    assert!(export["tiles_drawn"].as_u64().unwrap_or(0) > 0, "{export}");
    assert!(r[3]["detail"]["result"]["non_white_pixels"].as_u64().unwrap_or(0) > 0, "{}", r[3]);
    assert_eq!(r[4]["error"], "frame_required", "{}", r[4]);
    assert_eq!(exit, 1, "the refused line makes the exit 1");
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&out).unwrap_or_else(|e| panic!("{e}"));
    let again = export_sha(&mut w, &s.0.join("again.pdf"));
    assert_eq!(Some(again.as_str()), export["sha256"].as_str(), "the saved package draws the same PDF");
}

fn export_sha(v: &mut Viewer, pdf: &Path) -> String {
    export(v, pdf).unwrap_or_else(|e| panic!("{e:?}"))["sha256"].as_str().unwrap().to_owned()
}
