//! Lane SMB-AI-AREA-TYPE-2 (feature `viewer`): live path text and area text in the window. The
//! viewer shapes the story of every Folio `StudioPathText` / `StudioAreaText` binding for
//! composite (`VectorAssets::shaped_story`) with the text layers' font resolution and shaping, so
//! the rendered frame holds the text inside its container / along its path; a story it cannot
//! shape stays unshaped and composite's `vector_text_not_provided` report stays visible
//! (`Viewer::vector_text_reports`, `state_json()["edit"]["vector"]["live_texts"]`). Oracles: the
//! container and baseline geometry (hand-derived bounds), the brush colour of the story fill, the
//! composite reports and the undo history. Illustrator area-type behaviour is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod support_context;

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
    let mut v = crate::support_context::viewer(Prefer::Cpu);
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

// ---- Lane SMB-R54-FOLLOWUPS-1 (R55 F-R55-2): a forced line break U+2028 in the Viewer paths. Type keeps
// it outside shaping under the current token, so no glyph carries it: area text classes the coverage
// entry as a forced break; a path that holds one line refuses it, typed. ----

/// First and last inked row of `text` as area text in a tall container.
fn area_rows(v: &mut Viewer, container: &str, text: &str) -> (usize, usize) {
    v.bind_live_text(container, text, LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    let reports = v.vector_text_reports().unwrap();
    let drawn = reports[0].report.outcome.as_ref().unwrap_or_else(|e| panic!("{text:?} refused {e}: {:?}", reports[0].unshaped));
    assert!(reports[0].unshaped.is_none() && drawn.overset_from.is_none(), "{text:?}: {drawn:?}");
    let ink = inked(v);
    (ink.iter().map(|p| p.1).min().unwrap(), ink.iter().map(|p| p.1).max().unwrap())
}

#[test]
fn mt_31008_area_text_breaks_the_line_at_a_forced_line_break() {
    const LS: char = '\u{2028}';
    let (_s, mut v) = paper("area-forced-break");
    let container = v.draw_shape(ShapeKind::Rectangle, [6.0, 2.0], [90.0, 62.0], false).unwrap();
    // 12 pt text with the viewer's 120 % leading: 14.4 pt from line to line (1 pt = 1 px here).
    let (top, bottom) = area_rows(&mut v, &container, "HH");
    let near = |got: usize, want: usize| got.abs_diff(want) <= 1;
    // One break: the second line one leading below the first, nothing else moves.
    let (t, b) = area_rows(&mut v, &container, &format!("HH{LS}HH"));
    assert!(t == top && near(b, bottom + 14), "one break: rows {t}..={b}, control {top}..={bottom}");
    // Two breaks: a blank line between them.
    let (t, b) = area_rows(&mut v, &container, &format!("HH{LS}{LS}HH"));
    assert!(t == top && near(b, bottom + 29), "two breaks: rows {t}..={b}, control {top}..={bottom}");
    // Before the first glyph: a blank first line.
    let (t, b) = area_rows(&mut v, &container, &format!("{LS}HH"));
    assert!(near(t, top + 14) && near(b, bottom + 14), "leading break: rows {t}..={b}, control {top}..={bottom}");
    // After the last glyph: no further line and no ink change.
    assert_eq!(area_rows(&mut v, &container, &format!("HH{LS}")), (top, bottom));
    // Area to point text cannot keep the break (a text layer holds one line): refused, typed.
    area_rows(&mut v, &container, &format!("HH{LS}HH"));
    let refused = v.convert_text_kind(&container).unwrap_err();
    assert!(refused.contains("text_forced_line_break_unsupported"), "{refused}");
}

#[test]
fn mt_31008_paths_of_one_line_refuse_a_forced_line_break() {
    use hsk_studio_folio::StoryContent;
    use hsk_studio_harness_view::view::text::{self, DEFAULT_FONT, FontFile};
    use hsk_studio_harness_view::view::vector::path_text::PathTextSpec;
    const REFUSAL: &str = "text_forced_line_break_unsupported";
    let (_s, mut v) = paper("forced-break-refused");
    // Live path text: the binding is kept, unshaped, and the report says why.
    let line = v.pen_point(6.0, 40.0, None, 1.0).unwrap();
    v.pen_point(90.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    let story = v.bind_live_text(&line, "ab\u{2028}cd", LiveTextKind::Path).unwrap();
    assert!(inked(&mut v).is_empty());
    let reports = v.vector_text_reports().unwrap();
    assert_eq!(reports[0].report.text_id, story);
    assert_eq!(reports[0].report.outcome, Err(TEXT_NOT_PROVIDED));
    assert!(reports[0].unshaped.as_deref().is_some_and(|e| e.starts_with(REFUSAL)), "{:?}", reports[0].unshaped);
    // The path-text layout of the vector tool.
    let spec = PathTextSpec { text: "ab\u{2028}cd".into(), family: DEFAULT_FONT.into(), size_pt: 12.0, options: Default::default() };
    assert!(v.path_text_layout(&line, &spec).unwrap_err().starts_with(REFUSAL));
    // The text tool holds one line.
    assert!(v.place_text(10.0, 20.0, "ab\u{2028}cd").unwrap_err().starts_with("text_content"));
    // A text layer whose stored story holds the break draws no line at all rather than a wrong one.
    let id = v.place_text(10.0, 20.0, "abcd").unwrap();
    let mut entry = v.text_entry().unwrap();
    let held = "ab\u{2028}cd";
    let story = entry.stories.iter_mut().find(|s| entry.layers.iter().any(|l| l.layer_id == id && l.story_id == s.story_id)).unwrap();
    story.content = StoryContent::Inline { text_utf8: held.into() };
    story.paragraphs[0].end = held.len() as u64;
    story.character_ranges[0].end = held.len() as u64;
    let snapshot = v.run().snapshot().unwrap();
    let fonts = [FontFile::bundled().unwrap()];
    let refused = text::prepare(snapshot.document(), &entry, &fonts, v.run().actor(), &Default::default()).err().unwrap();
    assert!(refused.starts_with(REFUSAL), "{refused}");
}
