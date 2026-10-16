//! SMB-AI-TYPE-PATH-RECOLOR (feature `viewer`): recolour artwork as one undo step with global
//! swatch propagation (slice 2), gradient meshes created from a shape, edited and rasterised
//! (slice 3), and the separations preview of a vector document through Press (slice 4). Expected
//! values are hand-derived from the geometry (document px; 1 px = 0.75 pt on the page) and from
//! the swatch components. Illustrator parity is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::path_text::PathTextSpec;
use hsk_studio_harness::view::vector::VectorTool;
use hsk_studio_type::{PathFlip, PathTextOptions};

const W: usize = 96;
const H: usize = 64;
const RED: [u8; 3] = [255, 0, 0];


fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, ..VectorTool::default() });
    v
}
fn frame_px(v: &mut Viewer, x: usize, y: usize) -> [u8; 3] {
    v.render_if_dirty();
    let f = &v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8;
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

/// Rows of the frame (y) holding pure tool-red pixels in columns `x0..x1`.
fn red_rows(v: &mut Viewer, x0: usize, x1: usize) -> Vec<usize> {
    (0..H).filter(|y| (x0..x1).any(|x| frame_px(v, x, *y) == RED)).collect()
}

#[test]
fn ai_type_on_a_path_lays_out_and_converts_to_outlines() {
    let scratch = Scratch::new("ai-path-text");
    let mut v = paper(&scratch);
    // An open horizontal baseline from (10, 40) to (90, 40).
    let line = v.pen_point(10.0, 40.0, None, 1.0).unwrap();
    v.pen_point(90.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    let spec = |options| PathTextSpec { text: "HI".into(), family: "Ubuntu-Light".into(), size_pt: 40.0, options };
    let l = v.path_text_layout(&line, &spec(PathTextOptions::default())).unwrap();
    assert_eq!((l.glyphs.len(), l.overset_from, l.path_length_pt), (2, None, 80.0));
    // Rainbow on a horizontal line: glyph 0 at the path start, y up = document -y; glyph 1 one
    // advance on, at its arc start.
    assert!(l.glyphs[0].transform.iter().zip([1.0, 0.0, 0.0, -1.0, 10.0, 40.0]).all(|(a, b)| (a - b).abs() < 1e-9), "{:?}", l.glyphs[0].transform);
    assert!((l.glyphs[1].transform[4] - (10.0 + l.glyphs[1].arc_start_pt)).abs() < 1e-9 && l.glyphs[1].arc_start_pt > 0.0);
    // Converted to outlines: one new layer, ink above the baseline only (rows < 40), one undo step.
    let before = red_rows(&mut v, 0, W);
    assert!(before.is_empty(), "{before:?}");
    let (outlines, _) = v.text_on_path_to_outlines(&line, &spec(PathTextOptions::default())).unwrap();
    assert!(outlines.starts_with("SLYR-"));
    let rows = red_rows(&mut v, 0, W);
    assert!(!rows.is_empty() && rows.iter().all(|y| *y < 40), "{rows:?}");
    v.undo().unwrap();
    assert!(red_rows(&mut v, 0, W).is_empty());
    // Flipped: the text runs back along the line, on its other side (ink below the baseline).
    v.text_on_path_to_outlines(&line, &spec(PathTextOptions { flipped: PathFlip::Flipped, ..PathTextOptions::default() })).unwrap();
    let rows = red_rows(&mut v, 0, W);
    assert!(!rows.is_empty() && rows.iter().all(|y| *y >= 40), "{rows:?}");
    // A 5 px baseline holds no glyph: everything overset is reported, and nothing is drawn.
    let short = v.pen_point(10.0, 60.0, None, 1.0).unwrap();
    v.pen_point(15.0, 60.0, None, 1.0).unwrap();
    v.pen_end();
    let o = v.path_text_layout(&short, &spec(PathTextOptions::default())).unwrap();
    assert_eq!((o.glyphs.len(), o.overset_from, o.overset_count), (0, Some(0), 2));
    assert!(v.text_on_path_to_outlines(&short, &spec(PathTextOptions::default())).is_err());
    // Hostile: unknown font, empty or oversized text, bad size, undefined flip.
    assert!(v.path_text_layout(&line, &PathTextSpec { family: "Nope".into(), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { text: String::new(), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { text: "x".repeat(5000), ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &PathTextSpec { size_pt: f64::NAN, ..spec(PathTextOptions::default()) }).is_err());
    assert!(v.path_text_layout(&line, &spec(PathTextOptions { flipped: PathFlip::Undefined, ..PathTextOptions::default() })).is_err());
}

