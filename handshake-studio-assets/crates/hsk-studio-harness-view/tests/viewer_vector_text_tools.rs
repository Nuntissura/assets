//! Lane SMB-AI-AREA-TYPE-3 (feature `viewer`): area and path type usable end to end in the window.
//! The Path Operations panel binds live text to the selected path (Type on Path / Area Type),
//! converts point text <-> area text (Type's `point_to_area` / `area_to_point`) and runs Create
//! Outlines, each ONE undo step; exports (SVG, PDF, separations) carry the live text the canvas
//! draws and keep the typed report of a story that cannot be shaped; the status line echoes the
//! live-text report. Oracles: the inked pixels of the canvas before and after (bounding boxes and
//! pixel sets), composite's reports, the layer kinds and the undo history. Illustrator behaviour is
//! UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod support_context;

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use hsk_studio_composite::vector_text::TEXT_NOT_PROVIDED;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::hosted::PanelAction;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::vector::separations::VectorSeparationOptions;
use hsk_studio_harness_view::view::vector::{LiveTextKind, ShapeKind, VectorTool};

const W: usize = 96;
const H: usize = 64;

/// White paper; shapes draw no fill and no stroke (only text inks, in the brush colour).
fn paper(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("vector-text-tools-{label}"));
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, stroke: [255, 255, 255], ..VectorTool::default() });
    v.text_size_pt = 12.0;
    (scratch, v)
}

/// Pixels inked by the story fill (the default brush colour [220, 30, 40] over white).
fn inked(v: &mut Viewer) -> Vec<(usize, usize)> {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {:?}", v.render_error()));
    assert_eq!((f.width as usize, f.height as usize), (W, H));
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            if i32::from(f.rgba8[i]) - i32::from(f.rgba8[i + 1]) > 60 {
                out.push((x, y));
            }
        }
    }
    out
}

fn bbox(p: &[(usize, usize)]) -> [usize; 4] {
    assert!(!p.is_empty());
    [p.iter().map(|q| q.0).min().unwrap(), p.iter().map(|q| q.1).min().unwrap(), p.iter().map(|q| q.0).max().unwrap(), p.iter().map(|q| q.1).max().unwrap()]
}

/// Bounding boxes equal within `tol` pixels on every side.
fn near(a: [usize; 4], b: [usize; 4], tol: usize) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

fn kind(v: &Viewer, id: &str) -> Option<String> {
    v.layers().into_iter().find(|r| r.layer_id == id).map(|r| r.kind)
}

/// The status log (the echo is followed by panel and render lines).
fn status(v: &Viewer) -> String {
    v.state_json()["status"].as_array().map(|a| a.iter().filter_map(|l| l.as_str()).collect::<Vec<_>>().join("
")).unwrap_or_default()
}

#[test]
fn point_and_area_text_convert_both_ways_as_one_undo_step() {
    let (_s, mut v) = paper("convert");
    let text = v.place_text(10.0, 30.0, "Point type").unwrap();
    let point_ink = inked(&mut v);
    assert!(point_ink.len() > 20, "too little ink: {}", point_ink.len());
    // Point -> area: a vector container replaces the text layer; the area layout places every glyph.
    let area = v.convert_text_kind(&text).unwrap();
    assert_eq!((kind(&v, &text), kind(&v, &area).as_deref()), (None, Some("vector")));
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].report.layer_id, area);
    let drawn = reports[0].report.outcome.as_ref().unwrap_or_else(|e| panic!("refused {e}: {:?}", reports[0].unshaped));
    assert_eq!((drawn.placed, drawn.overset_count), (10, 0), "{drawn:?}");
    let area_ink = inked(&mut v);
    assert!(near(bbox(&area_ink), bbox(&point_ink), 2), "{:?} vs {:?}", bbox(&area_ink), bbox(&point_ink));
    assert!(status(&v).contains(&format!("live text STXT-{}: 10 glyphs placed", &area[5..])), "{}", status(&v));
    // One undo step: the text layer and its pixels come back exactly; redo converts again.
    v.undo().unwrap();
    assert_eq!((kind(&v, &text).as_deref(), kind(&v, &area)), (Some("text"), None));
    assert_eq!(inked(&mut v), point_ink);
    assert!(v.vector_text_reports().unwrap().is_empty());
    v.redo().unwrap();
    assert_eq!(kind(&v, &area).as_deref(), Some("vector"));
    assert_eq!(inked(&mut v), area_ink);
    // Area -> point: a text layer at the first line's baseline replaces the container.
    let back = v.convert_text_kind(&area).unwrap();
    assert_eq!((kind(&v, &area), kind(&v, &back).as_deref()), (None, Some("text")));
    assert!(v.vector_text_reports().unwrap().is_empty());
    assert!(near(bbox(&inked(&mut v)), bbox(&point_ink), 2));
    v.undo().unwrap();
    assert_eq!((kind(&v, &back), kind(&v, &area).as_deref()), (None, Some("vector")));
    assert_eq!(v.vector_text_reports().unwrap().len(), 1);
    // Refused: a raster layer, a vector layer without area text (nothing changes).
    let raster = v.layers().into_iter().find(|r| r.kind == "raster").map(|r| r.layer_id).unwrap();
    assert!(v.convert_text_kind(&raster).is_err());
    let plain = v.draw_shape(ShapeKind::Rectangle, [70.0, 5.0], [90.0, 20.0], false).unwrap();
    assert!(v.convert_text_kind(&plain).unwrap_err().contains("no area text"));
    assert_eq!(kind(&v, &plain).as_deref(), Some("vector"));
}

#[test]
fn create_outlines_replaces_live_and_point_text_as_one_undo_step() {
    let (_s, mut v) = paper("outlines");
    // Through the Path Operations panel: the drawn container is selected.
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    v.hosted_action(PanelAction::BindLiveText { text: "Outlined area text".into(), kind: LiveTextKind::Area { inset_pt: 2.0 } }).unwrap();
    assert!(status(&v).contains("glyphs placed"), "{}", status(&v));
    let live = inked(&mut v);
    assert!(live.len() > 40, "too little ink: {}", live.len());
    let said = v.hosted_action(PanelAction::CreateOutlines).unwrap();
    assert!(v.vector_text_reports().unwrap().is_empty(), "the binding stays");
    // The outlines are their own vector layer (a vector layer has one appearance, its first
    // path's: appended to the unfilled container they would draw nothing); the container stays.
    let shapes = v.layers().into_iter().find(|r| said.ends_with(&r.layer_id)).map(|r| (r.layer_id, r.kind));
    assert!(shapes.as_ref().is_some_and(|(id, k)| *id != container && k == "vector"), "{said}: {shapes:?}");
    assert_eq!(kind(&v, &container).as_deref(), Some("vector"));
    let outlined = inked(&mut v);
    assert!(!outlined.is_empty(), "the outlines draw nothing");
    // The same glyph geometry as filled paths: the pixel sets differ by antialiasing only.
    let differ = live.iter().filter(|p| !outlined.contains(p)).count() + outlined.iter().filter(|p| !live.contains(p)).count();
    assert!(differ * 10 <= live.len(), "{differ} of {} pixels differ", live.len());
    assert!(near(bbox(&outlined), bbox(&live), 1));
    // One undo step restores the binding and removes the outline paths together.
    v.undo().unwrap();
    assert_eq!(v.vector_text_reports().unwrap().len(), 1);
    assert_eq!(inked(&mut v), live);
    v.redo().unwrap();
    assert!(v.vector_text_reports().unwrap().is_empty());
    assert_eq!(inked(&mut v), outlined);
    // Nothing left to outline on that layer.
    assert!(v.create_outlines(&container).unwrap_err().contains("no live text"));

    // Point text: a vector layer of glyph outlines replaces the text layer.
    let (_s2, mut w) = paper("outlines-point");
    let text = w.place_text(8.0, 40.0, "Outline me").unwrap();
    let point = inked(&mut w);
    let shapes = w.create_outlines(&text).unwrap();
    assert_eq!((kind(&w, &text), kind(&w, &shapes).as_deref()), (None, Some("vector")));
    assert!(near(bbox(&inked(&mut w)), bbox(&point), 2));
    w.undo().unwrap();
    assert_eq!((kind(&w, &text).as_deref(), kind(&w, &shapes)), (Some("text"), None));
    assert_eq!(inked(&mut w), point);

    // An unshapeable story is refused with composite's code and the reason; nothing changes.
    w.add_font("Broken", vec![0_u8; 64]);
    let frame = w.draw_shape(ShapeKind::Rectangle, [10.0, 5.0], [90.0, 30.0], false).unwrap();
    w.bind_live_text_with_font(&frame, "No glyphs", LiveTextKind::Area { inset_pt: 0.0 }, "Broken").unwrap();
    assert!(status(&w).contains(TEXT_NOT_PROVIDED), "{}", status(&w));
    let e = w.create_outlines(&frame).unwrap_err();
    assert!(e.contains(TEXT_NOT_PROVIDED) && e.contains("text_"), "{e}");
    assert_eq!(w.vector_text_reports().unwrap().len(), 1);
    let raster = w.layers().into_iter().find(|r| r.kind == "raster").map(|r| r.layer_id).unwrap();
    assert!(w.create_outlines(&raster).is_err());
}

#[test]
fn exports_carry_live_text_and_keep_the_unshaped_report() {
    let (_s, mut v) = paper("exports");
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    let (svg0, _) = v.vector_svg().unwrap();
    let (pdf0, _) = v.vector_pdf_bytes().unwrap();
    let (sep0, _) = v.vector_separations(&VectorSeparationOptions::default()).unwrap();
    v.bind_live_text(&container, "Exported text", LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    let (svg1, svg_losses) = v.vector_svg().unwrap();
    let (pdf1, pdf_losses) = v.vector_pdf_bytes().unwrap();
    let (sep1, sep_losses) = v.vector_separations(&VectorSeparationOptions::default()).unwrap();
    // The text rows are exported as geometry (more paths, more content, one more unseparated RGB row).
    assert!(svg1.matches("<path").count() > svg0.matches("<path").count(), "{svg1}");
    assert!(pdf1.len() > pdf0.len() + 200, "{} vs {}", pdf1.len(), pdf0.len());
    assert!(sep1.unseparated > sep0.unseparated, "{} vs {}", sep1.unseparated, sep0.unseparated);
    for l in [&svg_losses, &pdf_losses, &sep_losses] {
        assert!(!l.iter().any(|x| x.contains(TEXT_NOT_PROVIDED)), "{l:?}");
    }
    // The export colour is the canvas colour: the brush red sRGB-encoded (#dc1e28 within one step
    // of rounding), not its linear-light values written as sRGB (about #b40306).
    let colours: Vec<[u8; 3]> = svg1
        .match_indices('#')
        .filter_map(|(i, _)| svg1.get(i + 1..i + 7))
        .filter_map(|h| {
            let c = |k: usize| u8::from_str_radix(h.get(k..k + 2)?, 16).ok();
            Some([c(0)?, c(2)?, c(4)?])
        })
        .collect();
    assert!(colours.iter().any(|c| c.iter().zip([220_u8, 30, 40]).all(|(a, b)| a.abs_diff(b) <= 1)), "{colours:?}");
    // A story that cannot be shaped: the file is still written and the report stays a typed loss.
    v.add_font("Broken", vec![0_u8; 64]);
    let frame = v.draw_shape(ShapeKind::Rectangle, [10.0, 56.0], [90.0, 63.0], false).unwrap();
    let story = v.bind_live_text_with_font(&frame, "No glyphs", LiveTextKind::Area { inset_pt: 0.0 }, "Broken").unwrap();
    let (_, svg_losses) = v.vector_svg().unwrap();
    let (_, pdf_losses) = v.vector_pdf_bytes().unwrap();
    let (_, sep_losses) = v.vector_separations(&VectorSeparationOptions::default()).unwrap();
    for l in [&svg_losses, &pdf_losses, &sep_losses] {
        assert!(l.iter().any(|x| x.starts_with(&story) && x.contains(TEXT_NOT_PROVIDED) && x.contains("(text_")), "{l:?}");
    }
}

/// A document whose `text` entry does not decode still exports; every live story's loss line
/// carries the decode error (`text_entry_unavailable: <decode error>`, the same reason the canvas
/// reports since SMB-R41-FOLLOWUPS-1).
#[test]
fn exports_name_the_text_entry_decode_error() {
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_harness_core::harness::{Harness, References, Selection};
    let (s, mut v) = paper("exports-decode");
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    let story = v.bind_live_text(&container, "Area text", LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    let saved = s.0.join("live.handshake");
    v.save(&saved, false).unwrap();
    let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = harness.run(crate::support_context::context(), &cancel);
    assert!(run.open_path(&saved));
    run.set_document_entry(hsk_studio_harness_view::view::text::ENTRY, Some(b"{not json".to_vec())).unwrap();
    let corrupt = s.0.join("corrupt.handshake");
    assert!(run.save_to(&corrupt));
    let mut w = crate::support_context::viewer(Prefer::Cpu);
    w.open(&corrupt).unwrap();
    let decode = w.text_entry().unwrap_err();
    let want = format!("{story}: {TEXT_NOT_PROVIDED} (text_entry_unavailable: {decode})");
    let (_, svg_losses) = w.vector_svg().unwrap();
    let (_, pdf_losses) = w.vector_pdf_bytes().unwrap();
    let (_, sep_losses) = w.vector_separations(&VectorSeparationOptions::default()).unwrap();
    for l in [&svg_losses, &pdf_losses, &sep_losses] {
        assert!(l.contains(&want), "{want} not in {l:?}");
    }
}
