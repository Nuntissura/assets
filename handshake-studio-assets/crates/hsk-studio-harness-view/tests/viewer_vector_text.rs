//! Lane SMB-AI-AREA-TYPE-2 (feature `viewer`): live path text and area text in the window. The
//! viewer shapes the story of every Folio `StudioPathText` / `StudioAreaText` binding for
//! composite (`VectorAssets::shaped_story`) with the text layers' font resolution and shaping, so
//! the rendered frame holds the text inside its container / along its path; a story it cannot
//! shape stays unshaped and composite's `vector_text_not_provided` report stays visible
//! (`Viewer::vector_text_reports`, `state_json()["edit"]["vector"]["live_texts"]`). Oracles: the
//! container and baseline geometry (hand-derived bounds), the brush colour of the story fill, the
//! composite reports and the undo history. Illustrator area-type behaviour is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use hsk_studio_composite::vector_text::TEXT_NOT_PROVIDED;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::vector::{LiveTextKind, ShapeKind, VectorTool};

const W: usize = 96;
const H: usize = 64;

/// White paper; shapes draw no fill and no stroke, pen paths a white stroke (only the live text inks).
fn paper(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("vector-text-{label}"));
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, stroke: [255, 255, 255], ..VectorTool::default() });
    v.text_size_pt = 12.0;
    (scratch, v)
}

/// Pixels of the frame inked by the story fill (the default brush colour [220, 30, 40] over white).
fn inked(v: &mut Viewer) -> Vec<(usize, usize)> {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {:?}", v.render_error()));
    assert_eq!((f.width as usize, f.height as usize), (W, H));
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let (r, g) = (i32::from(f.rgba8[i]), i32::from(f.rgba8[i + 1]));
            if r - g > 60 {
                out.push((x, y));
            }
        }
    }
    out
}

#[test]
fn area_text_renders_inside_its_container_and_undoes() {
    let (_s, mut v) = paper("area");
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    assert!(inked(&mut v).is_empty());
    let story = v.bind_live_text(&container, "Area text wraps inside its frame", LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    assert!(story.starts_with("STXT-"));
    let ink = inked(&mut v);
    assert!(ink.len() > 40, "too little ink: {}", ink.len());
    // Inside the container (1 px of antialiasing slack), and wrapped onto more than one line.
    assert!(ink.iter().all(|&(x, y)| (9..=81).contains(&x) && (9..=55).contains(&y)), "ink outside the container: {ink:?}");
    let (top, bottom) = (ink.iter().map(|p| p.1).min().unwrap(), ink.iter().map(|p| p.1).max().unwrap());
    assert!(bottom - top > 16, "one line only: rows {top}..={bottom}");
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let drawn = reports[0].report.outcome.as_ref().unwrap_or_else(|e| panic!("refused {e}: {:?}", reports[0].unshaped));
    assert_eq!((reports[0].report.text_id.as_str(), reports[0].report.layer_id.as_str()), (story.as_str(), container.as_str()));
    assert!(drawn.placed > 0 && drawn.container.is_some() && reports[0].unshaped.is_none(), "{drawn:?}");
    let state = v.state_json();
    let live = &state["edit"]["vector"]["live_texts"];
    assert_eq!(live[0]["text_id"], story.as_str(), "{live}");
    assert!(live[0]["code"].is_null(), "{live}");
    // The binding is one undo step; redo draws the text again.
    v.undo().unwrap();
    assert!(inked(&mut v).is_empty());
    assert!(v.vector_text_reports().unwrap().is_empty());
    v.redo().unwrap();
    assert_eq!(inked(&mut v), ink);
}

#[test]
fn path_text_renders_along_its_path() {
    let (_s, mut v) = paper("path");
    let line = v.pen_point(6.0, 40.0, None, 1.0).unwrap();
    v.pen_point(90.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    assert!(inked(&mut v).is_empty());
    let story = v.bind_live_text(&line, "Path text", LiveTextKind::Path).unwrap();
    let ink = inked(&mut v);
    assert!(ink.len() > 20, "too little ink: {}", ink.len());
    // Along the baseline from x = 6, above it (rainbow on a horizontal line: glyph y up = -y),
    // within one 12 pt line (1 px antialiasing slack).
    assert!(ink.iter().all(|&(x, y)| x >= 5 && (26..=41).contains(&y)), "ink off the path: {ink:?}");
    assert!(ink.iter().any(|&(x, _)| x < 14) && ink.iter().any(|&(x, _)| x > 30), "{ink:?}");
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].report.text_id, story);
    let drawn = reports[0].report.outcome.as_ref().unwrap_or_else(|e| panic!("refused {e}: {:?}", reports[0].unshaped));
    assert_eq!((drawn.placed, drawn.overset_from), (9, None));
    // A new binding on the same layer replaces the old one (one live text per layer).
    let again = v.bind_live_text(&line, "Path", LiveTextKind::Path).unwrap();
    assert_eq!(again, story);
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].report.outcome.as_ref().map(|d| d.placed), Ok(4));
}

#[test]
fn unshapeable_story_keeps_the_report_and_hostile_bindings_are_refused() {
    let (_s, mut v) = paper("unshaped");
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    // A registered family whose bytes are no font: the binding is stored, Type refuses to shape
    // it, the layer still draws (without text) and the report says why.
    v.add_font("Broken", vec![0_u8; 64]);
    let story = v.bind_live_text_with_font(&container, "No glyphs here", LiveTextKind::Area { inset_pt: 0.0 }, "Broken").unwrap();
    assert!(inked(&mut v).is_empty());
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].report.text_id, story);
    assert_eq!(reports[0].report.outcome, Err(TEXT_NOT_PROVIDED));
    assert!(reports[0].unshaped.as_deref().is_some_and(|e| e.starts_with("text_")), "{:?}", reports[0].unshaped);
    let state = v.state_json();
    assert_eq!(state["edit"]["vector"]["live_texts"][0]["code"], TEXT_NOT_PROVIDED);
    // Hostile bindings: area text on an open path, empty, multi-line or oversized text, an
    // unregistered font, a non-finite inset, a layer that is no vector layer.
    let line = v.pen_point(6.0, 60.0, None, 1.0).unwrap();
    v.pen_point(90.0, 60.0, None, 1.0).unwrap();
    v.pen_end();
    assert!(v.bind_live_text(&line, "x", LiveTextKind::Area { inset_pt: 0.0 }).is_err());
    assert!(v.bind_live_text(&container, "", LiveTextKind::Path).is_err());
    assert!(v.bind_live_text(&container, "a\nb", LiveTextKind::Path).is_err());
    assert!(v.bind_live_text(&container, &"x".repeat(5000), LiveTextKind::Path).is_err());
    assert!(v.bind_live_text_with_font(&container, "x", LiveTextKind::Path, "Nope").is_err());
    assert!(v.bind_live_text(&container, "x", LiveTextKind::Area { inset_pt: f64::NAN }).is_err());
    let raster = v.layers().into_iter().find(|r| r.layer_id != container && r.layer_id != line).map(|r| r.layer_id).unwrap();
    assert!(v.bind_live_text(&raster, "x", LiveTextKind::Path).is_err());
    // The refused bindings changed nothing.
    assert_eq!(v.vector_text_reports().unwrap().len(), 1);
}
