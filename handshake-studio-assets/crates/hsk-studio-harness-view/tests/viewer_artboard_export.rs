//! Lane SMB-ARTBOARD-EXPORT-1 (feature `viewer`; STU-VEC-028, STU-IO-010, STU-IO-111 to STU-IO-114,
//! STU-IO-117): a named board target for the vector exports, per-board and per-node export, and the
//! node export settings record with SCALE, WIDTH and HEIGHT constraints writing many named files in one
//! run. Scenarios vector-brand steps 85, 86, 88, 90; product-design 53, 82, 85; board-slides-web 79.
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::doc_export::{DocExportRecipe, DocFormat};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::vector::{ShapeKind, VectorTool};
use std::path::PathBuf;

fn vector_doc(w: f64, h: f64) -> Viewer {
    let mut v = support_context::viewer(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((w, h))), None).unwrap();
    v
}

fn black() -> VectorTool {
    VectorTool { fill_on: true, fill: [0, 0, 0], stroke_on: false, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
}

/// The four numbers of the root `viewBox`.
fn view_box(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").unwrap_or_else(|| panic!("no viewBox: {}", &svg[..svg.len().min(300)])) + 9;
    let end = start + svg[start..].find('"').unwrap();
    let n: Vec<f64> = svg[start..end].split(' ').map(|t| t.parse().unwrap()).collect();
    [n[0], n[1], n[2], n[3]]
}

fn near(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// vector-brand step 88: a board off the document origin exports as SVG framed on that board, so its
/// art lies inside the `viewBox` (the board's origin and size in document pixels), not outside it.
#[test]
fn svg_frames_the_chosen_board_at_its_origin() {
    let mut v = vector_doc(128.0, 128.0);
    let wide = v.add_artboard("wide", 200.0, 64.0, None).unwrap();
    v.set_vector_tool(black());
    v.go_to_artboard(&wide).unwrap();
    v.draw_shape(ShapeKind::Rectangle, [200.0, 8.0], [260.0, 56.0], false).unwrap();
    let (svg, _) = v.vector_svg().unwrap();
    assert!(near(view_box(&svg), [192.0, 0.0, 200.0, 64.0]), "the board of x 192: {}", &svg[..svg.len().min(300)]);
    // The first board keeps the document origin.
    let first = v.state_json()["containers"]["artboards"][0]["id"].as_str().unwrap().to_owned();
    v.go_to_artboard(&first).unwrap();
    assert!(near(view_box(&v.vector_svg().unwrap().0), [0.0, 0.0, 128.0, 128.0]));
}

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-artboard-export-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// V4-06 sibling site: the document export recipe frames the chosen board, as the canvas and the PNG
/// export do (it rendered the first board whatever Go to Artboard chose).
#[test]
fn export_document_frames_the_chosen_board() {
    let scratch = Scratch::new("doc-export");
    let mut v = vector_doc(128.0, 128.0);
    let wide = v.add_artboard("wide", 200.0, 64.0, None).unwrap();
    v.go_to_artboard(&wide).unwrap();
    let report = v.export_document(&scratch.0.join("wide.png"), &DocExportRecipe::of_format(DocFormat::Png { depth: 8 }), false).unwrap();
    assert_eq!((report["width"].as_u64(), report["height"].as_u64()), (Some(200), Some(64)), "{report}");
}
