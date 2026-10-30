//! Lane SMB-AI-AREA-TYPE-1 slices 3-4: live path text and area text from Folio records
//! (`StudioPathText`, `StudioAreaText`) drawn into the vector layer that holds the path, redrawn
//! from the current story and path on every preparation (nothing baked), and Create Outlines
//! rendering like the live text. Glyphs are synthetic outlines (font units, upem 1000) so every
//! expected pixel is closed-form: a 400-unit square at 10 pt is a 4 x 4 px box above the baseline.
//! Illustrator area-type line placement parity: NOT_PROVEN (no oracle case, D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::vector_text::{ShapedGlyph, ShapedRun, ShapedStory, TEXT_NOT_PROVIDED};
use hsk_studio_composite::*;
use hsk_studio_folio::{StoryTextKind, StudioDesignRecords, StudioImageSource, StudioVectorRecords};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat};
use hsk_studio_type::{ComposeRun, GlyphBreak, GlyphOutline, OutlineCommand, TextAlign};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};

mod common;
use common::*;

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

fn path(points: &[(f64, f64)], closed: bool) -> OwnedPath {
    let mut elements: Vec<PathElement> = points
        .iter()
        .enumerate()
        .map(|(i, (x, y))| if i == 0 { PathElement::MoveTo(PathPoint::new(*x, *y)) } else { PathElement::LineTo(PathPoint::new(*x, *y)) })
        .collect();
    if closed {
        elements.push(PathElement::Close);
    }
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
/// A 400-unit square glyph (4 x 4 pt at 10 pt), or an "O": 600 x 700 outer, 100-unit counter.
fn square() -> GlyphOutline {
    let s = |x: f32, y: f32| [x, y];
    GlyphOutline::synthetic(1000, vec![OutlineCommand::MoveTo(s(0.0, 0.0)), OutlineCommand::LineTo(s(400.0, 0.0)), OutlineCommand::LineTo(s(400.0, 400.0)), OutlineCommand::LineTo(s(0.0, 400.0)), OutlineCommand::Close])
}
fn ring_o() -> GlyphOutline {
    let s = |x: f32, y: f32| [x, y];
    GlyphOutline::synthetic(1000, vec![
        OutlineCommand::MoveTo(s(0.0, 0.0)), OutlineCommand::LineTo(s(600.0, 0.0)), OutlineCommand::LineTo(s(600.0, 700.0)), OutlineCommand::LineTo(s(0.0, 700.0)), OutlineCommand::Close,
        OutlineCommand::MoveTo(s(100.0, 100.0)), OutlineCommand::LineTo(s(100.0, 600.0)), OutlineCommand::LineTo(s(500.0, 600.0)), OutlineCommand::LineTo(s(500.0, 100.0)), OutlineCommand::Close,
    ])
}
/// `words` words of `letters` glyphs (5 pt advance) separated by 5 pt spaces; glyph `i` paints
/// row `i % paints`.
fn shaped(kind: StoryTextKind, words: usize, letters: usize, paints: usize, glyph: fn() -> GlyphOutline) -> ShapedStory {
    let mut glyphs = Vec::new();
    let mut cluster = 0_u64;
    for w in 0..words {
        for _ in 0..letters {
            let i = glyphs.len();
            glyphs.push(ShapedGlyph { advance_pt: 5.0, brk: GlyphBreak::None, cluster, outline: glyph(), paint: i % paints });
            cluster += 1;
        }
        if w + 1 < words {
            glyphs.push(ShapedGlyph { advance_pt: 5.0, brk: GlyphBreak::Space, cluster, outline: GlyphOutline::synthetic(1000, Vec::new()), paint: 0 });
            cluster += 1;
        }
    }
    let metrics = ComposeRun { glyph_count: glyphs.len(), ascent_pt: 8.0, descent_pt: 2.0, leading_pt: 12.0, cap_height_pt: 7.0, x_height_pt: 5.0 };
    ShapedStory {
        text_kind: kind,
        runs: vec![ShapedRun { metrics, font_size_pt: 10.0 }],
        glyphs,
        paints: [RED, GREEN].iter().take(paints).map(|c| FillPaint::Solid(*c)).collect(),
        align: TextAlign::Left,
    }
}

/// Paths: SVPT 1 = the baseline (2, y)-(60, y); SVPT 2 = the container. Stories: STXT 1 (path),
/// STXT 2 (area), editable through the cells.
struct Assets {
    baseline_y: Cell<f64>,
    container: RefCell<OwnedPath>,
    path_story: RefCell<Option<ShapedStory>>,
    area_story: RefCell<Option<ShapedStory>>,
}
impl Assets {
    fn new() -> Self {
        Self {
            baseline_y: Cell::new(20.0),
            container: RefCell::new(path(&[(10.0, 10.0), (60.0, 10.0), (60.0, 40.0), (10.0, 40.0)], true)),
            path_story: RefCell::new(Some(shaped(StoryTextKind::Path, 1, 3, 1, square))),
            area_story: RefCell::new(Some(shaped(StoryTextKind::Area, 4, 2, 2, square))),
        }
    }
}
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        if path_id == identity("SVPT", 1) {
            let y = self.baseline_y.get();
            Some(path(&[(2.0, y), (60.0, y)], false))
        } else if path_id == identity("SVPT", 2) {
            Some(self.container.borrow().clone())
        } else {
            None
        }
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
    fn shaped_story(&self, text_id: &str) -> Option<ShapedStory> {
        if text_id == identity("STXT", 1) {
            self.path_story.borrow().clone()
        } else if text_id == identity("STXT", 2) {
            self.area_story.borrow().clone()
        } else {
            None
        }
    }
}
/// A host that shapes no live text.
struct NoText(Assets);
impl VectorAssets for NoText {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        self.0.path(path_id)
    }
    fn image(&self, s: &StudioImageSource) -> Option<ImageInfo> {
        self.0.image(s)
    }
}

/// Layer 1 holds the baseline (no fill style); layer 2 holds the container (blue fill).
fn doc() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),
                  layer(2,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",2)]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(1,"vector"),source(2,"vector")],"edges":[],
            "outputs":[address(1,"source","out"),address(2,"source","out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn design() -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",2)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
            "name":"fill","publishing":{"description":"","description_markdown":"","documentation_links":[],
            "publish_key":"fill","is_remote":false},
            "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":0.0,"g":0.0,"b":1.0,"colour_profile_id":null}},
                "opacity":100.0,"blend_mode":2,"visible":true}]}]}))
    .expect("design shape")
}
fn records(area: Value) -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "path_texts":[{"story_id":identity("STXT",1),"path_layer_id":identity("SLYR",1),"path_id":identity("SVPT",1),"start_t":0.0,"end_t":1.0}],
        "area_texts":[area]}))
    .expect("vector records shape")
}
fn area(auto_size: &str) -> Value {
    json!({"text_id":identity("STXT",2),"container_layer_id":identity("SLYR",2),"container_id":identity("SVPT",2),"auto_size":auto_size})
}
fn prepare(r: &StudioVectorRecords, assets: &dyn VectorAssets) -> PreparedVectors {
    let snap = open(&doc());
    prepare_vectors(&snap, &design(), r, assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
/// Coverage of text row `row` of `layer` at absolute pixel (x, y); 0 outside the plane.
fn text_px(v: &PreparedVectors, layer: u8, row: usize, x: i64, y: i64) -> u8 {
    let plane = v.plane(&identity("SLYR", layer)).expect("plane");
    let m = plane.stroke_mask(row).expect("text row mask");
    at(m.bounds, m.bytes, x, y)
}
fn at(r: Rect, bytes: &[u8], x: i64, y: i64) -> u8 {
    if x < r.x || y < r.y || x >= r.x + i64::from(r.width) || y >= r.y + i64::from(r.height) {
        return 0;
    }
    bytes[((y - r.y) as usize) * r.width as usize + (x - r.x) as usize]
}
/// Every pixel of text row `row` that has ink: (x, y, coverage).
fn inked(v: &PreparedVectors, layer: u8, row: usize) -> Vec<(i64, i64, u8)> {
    let plane = v.plane(&identity("SLYR", layer)).expect("plane");
    let m = plane.stroke_mask(row).expect("text row mask");
    let w = m.bounds.width as usize;
    m.bytes.iter().enumerate().filter(|(_, c)| **c > 0).map(|(i, c)| (m.bounds.x + (i % w) as i64, m.bounds.y + (i / w) as i64, *c)).collect()
}
fn target(extent: Rect) -> OutputTarget {
    OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded } }
}

#[test]
fn path_text_records_draw_live_and_follow_path_edits() {
    bounded_case("path_text_records_draw_live_and_follow_path_edits", || {
        let assets = Assets::new();
        let r = records(area("off"));
        let v = prepare(&r, &assets);
        // Three 5 pt glyphs from x = 2 on the baseline y = 20 (rainbow on a horizontal line: no
        // rotation): ink boxes [2, 6] x [16, 20], [7, 11] x .., [12, 16] x ..; the layer has no fill
        // of its own, so the text is its only row (row 0).
        let report = v.text(&identity("STXT", 1)).expect("report");
        assert_eq!((report.layer_id.as_str(), report.outcome.as_ref().map(|d| (d.placed, d.overset_count))), (identity("SLYR", 1).as_str(), Ok((3, 0))));
        let ink = inked(&v, 1, 0);
        assert_eq!(ink.len(), 3 * 16, "three 4 x 4 boxes");
        for (x, y, c) in &ink {
            assert_eq!(*c, 255);
            assert!((16..20).contains(y) && [2, 7, 12].iter().any(|g| (*g..*g + 4).contains(x)), "({x}, {y})");
        }
        // The layer lowers through the existing vector route: the red text Solid is in the plan.
        let low = vector_layer_render_plan(&v, &identity("SLYR", 1), 3, target(Rect { x: 0, y: 0, width: 64, height: 64 })).unwrap();
        assert!(low.plan.nodes.iter().any(|n| matches!(n, PlanNode::Solid { rgba, .. } if *rgba == RED)));
        // Edit the path: the baseline moves to y = 40 and the text follows (nothing baked).
        assets.baseline_y.set(40.0);
        let moved = prepare(&r, &assets);
        assert!(inked(&moved, 1, 0).iter().all(|(_, y, _)| (36..40).contains(y)));
        assert_eq!(text_px(&moved, 1, 0, 3, 37), 255);
        // Record options are applied: centre alignment in the 58 pt extent (text 15 pt wide).
        let mut centred = r.clone();
        centred.path_texts[0].align = hsk_studio_folio::StudioPathTextAlign::Center;
        let c = prepare(&centred, &assets);
        // lead (58 - 15) / 2 = 21.5: first box [23.5, 27.5] -> pixels 24..27 full, 23 half.
        assert_eq!(text_px(&c, 1, 0, 24, 37), 255);
        assert!((127..=128).contains(&text_px(&c, 1, 0, 23, 37)));
        // A host without live text: the layer still prepares, the binding is reported.
        let none = prepare(&r, &NoText(Assets::new()));
        assert_eq!(none.text(&identity("STXT", 1)).unwrap().outcome, Err(TEXT_NOT_PROVIDED));
        assert_eq!(none.refusal(&identity("SLYR", 1)), None);
        // A story of the wrong kind refuses the layer, typed.
        *assets.path_story.borrow_mut() = Some(shaped(StoryTextKind::Area, 1, 3, 1, square));
        let wrong = prepare(&r, &assets);
        assert_eq!(wrong.refusal(&identity("SLYR", 1)), Some("vector_text_kind"));
        // A malformed story (paint row out of range) is refused too.
        let mut bad = shaped(StoryTextKind::Path, 1, 3, 1, square);
        bad.glyphs[0].paint = 5;
        *assets.path_story.borrow_mut() = Some(bad);
        assert_eq!(prepare(&r, &assets).refusal(&identity("SLYR", 1)), Some("vector_text_story"));
    });
}

#[test]
fn area_text_records_draw_inside_their_container() {
    bounded_case("area_text_records_draw_inside_their_container", || {
        let assets = Assets::new();
        let v = prepare(&records(area("off")), &assets);
        // Container (10, 10)-(60, 40): 50 pt holds 3 words of 2 glyphs (3 x 10 + 2 x 5 = 40);
        // first baseline 10 + 8 = 18, second 30. Glyph i paints row i % 2 (red, green). The layer's
        // blue fill is row 0; the text rows are 1 (red) and 2 (green).
        let report = v.text(&identity("STXT", 2)).unwrap().outcome.clone().unwrap();
        assert_eq!((report.placed, report.overset_from, report.container, report.resized), (11, None, Some([10.0, 10.0, 60.0, 40.0]), false));
        let plane = v.plane(&identity("SLYR", 2)).unwrap();
        assert_eq!(plane.fills.len(), 3);
        // Word 0: glyphs 0 (red) at x 10 and 1 (green) at x 15, ink y [14, 18].
        assert_eq!((text_px(&v, 2, 1, 11, 15), text_px(&v, 2, 2, 11, 15)), (255, 0));
        assert_eq!((text_px(&v, 2, 1, 16, 15), text_px(&v, 2, 2, 16, 15)), (0, 255));
        // Word 3 (glyph 9, green) wraps to the second line at x 10, baseline 30.
        assert_eq!(text_px(&v, 2, 2, 11, 27), 255);
        // Every ink pixel lies inside the container.
        for row in [1, 2] {
            for (x, y, _) in inked(&v, 2, row) {
                assert!((10..60).contains(&x) && (10..40).contains(&y), "row {row} ({x}, {y})");
            }
        }
        // A triangle container (non-rectangular area type): drawn, and every glyph outline vertex
        // lies inside the triangle.
        *assets.container.borrow_mut() = path(&[(10.0, 10.0), (90.0, 10.0), (50.0, 90.0)], true);
        let r = records(area("off"));
        let t = prepare(&r, &assets);
        assert_eq!(t.refusal(&identity("SLYR", 2)), None);
        let placed = t.text(&identity("STXT", 2)).unwrap().outcome.clone().unwrap().placed;
        assert!(placed > 0);
        let inside = |x: f64, y: f64| y >= 10.0 - 1e-9 && x >= 10.0 + (y - 10.0) / 2.0 - 1e-9 && x <= 90.0 - (y - 10.0) / 2.0 + 1e-9;
        let (_, rows, _) = vector_text::create_text_outlines(&r, &identity("STXT", 2), &assets).unwrap();
        for row in &rows {
            for p in &row.paths {
                for e in &p.elements {
                    if let PathElement::MoveTo(q) | PathElement::LineTo(q) = e {
                        assert!(inside(q.x, q.y), "vertex ({}, {}) outside the triangle", q.x, q.y);
                    }
                }
            }
        }
        // Overset is reported: a long story in the rectangle.
        *assets.container.borrow_mut() = path(&[(10.0, 10.0), (60.0, 10.0), (60.0, 40.0), (10.0, 40.0)], true);
        *assets.area_story.borrow_mut() = Some(shaped(StoryTextKind::Area, 30, 2, 2, square));
        let o = prepare(&records(area("off")), &assets).text(&identity("STXT", 2)).unwrap().outcome.clone().unwrap();
        assert_eq!((o.placed, o.overset_from), (18, Some(18)));
        assert!(o.overset_count > 0);
        // Auto-size height_only: the container is reported resized, nothing overset.
        let h = prepare(&records(area("height_only")), &assets).text(&identity("STXT", 2)).unwrap().outcome.clone().unwrap();
        assert!(h.resized && h.overset_from.is_none() && h.container.unwrap()[3] > 40.0, "{h:?}");
        // An open container refuses the layer (Type: area_text_open_container).
        *assets.container.borrow_mut() = path(&[(10.0, 10.0), (60.0, 10.0), (60.0, 40.0)], false);
        assert_eq!(prepare(&records(area("off")), &assets).refusal(&identity("SLYR", 2)), Some("vector_text_layout"));
    });
}

#[test]
fn editing_the_story_redraws_without_a_bake() {
    bounded_case("editing_the_story_redraws_without_a_bake", || {
        let assets = Assets::new();
        let r = records(area("off"));
        let bytes = serde_json::to_vec(&r).unwrap();
        let before = prepare(&r, &assets);
        let n0 = inked(&before, 1, 0).len();
        // The story grows to 2 words of 3 glyphs: 6 boxes on the path, same record bytes.
        *assets.path_story.borrow_mut() = Some(shaped(StoryTextKind::Path, 2, 3, 1, square));
        let after = prepare(&r, &assets);
        assert_eq!((n0, inked(&after, 1, 0).len()), (3 * 16, 6 * 16));
        assert_eq!(after.text(&identity("STXT", 1)).unwrap().outcome.as_ref().unwrap().placed, 7);
        assert_eq!(serde_json::to_vec(&r).unwrap(), bytes, "the records hold no baked text");
        // Expanded export carries the live text as outlines of the current story.
        let snap = open(&doc());
        let (layers, refused) = vector::expand_vectors(&snap, &design(), &r, &assets, BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
        assert!(refused.is_empty());
        let l1 = layers.iter().find(|l| l.layer_id == identity("SLYR", 1)).unwrap();
        assert_eq!(l1.rows.len(), 1);
        assert_eq!(l1.rows[0].paths.len(), 6, "one compound path per inked glyph");
    });
}

#[test]
fn outlined_text_renders_like_the_live_text() {
    bounded_case("outlined_text_renders_like_the_live_text", || {
        println!("NOT_PROVEN: Illustrator Create Outlines contour order and area-type line placement (no oracle case, D5)");
        let assets = Assets::new();
        // "O" glyphs with counters at fractional positions (inset 0.3) to exercise partial coverage.
        *assets.area_story.borrow_mut() = Some(shaped(StoryTextKind::Area, 3, 2, 2, ring_o));
        let mut rec = area("off");
        rec["inset"] = json!(0.3);
        let r = records(rec);
        let live = prepare(&r, &assets);
        let (layer, rows, drawn) = vector_text::create_text_outlines(&r, &identity("STXT", 2), &assets).unwrap();
        assert_eq!((layer, rows.len(), drawn.placed), (identity("SLYR", 2), 2, 8));
        assert!(rows.iter().all(|row| row.paths.iter().all(|p| p.elements.iter().filter(|e| matches!(e, PathElement::Close)).count() == 2)), "outer contour + counter per glyph");
        // The outlined proposal as plain vector layers (one per paint row): same coverage as the
        // live text rows, pixel for pixel.
        let specs: Vec<VectorLayerSpec> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| VectorLayerSpec { layer_id: format!("outlined-{i}"), geometry: vector::VectorGeometry::Paths(row.paths.clone()), fills: vec![vector::FillSpec { paint: row.paint.clone(), mode: row.mode, opacity: row.opacity }] })
            .collect();
        let outlined = prepare_vector_layers(&specs, &VectorResolution::default(), &CancellationToken::default()).unwrap();
        for (i, row) in [1_usize, 2].into_iter().enumerate() {
            let o = outlined.plane(&format!("outlined-{i}")).unwrap();
            let m = o.mask().unwrap();
            let mut counter_seen = false;
            for (k, c) in m.bytes.iter().enumerate() {
                let (x, y) = (m.bounds.x + (k % m.bounds.width as usize) as i64, m.bounds.y + (k / m.bounds.width as usize) as i64);
                let l = text_px(&live, 2, row, x, y);
                assert!(l.abs_diff(*c) <= 1, "row {row} pixel ({x}, {y}): live {l} vs outlined {c}");
                counter_seen |= *c == 0;
            }
            assert!(counter_seen, "counters stay holes");
            assert!(m.bytes.iter().any(|c| *c > 0 && *c < 255), "partial coverage exercised");
            let b = m.bounds;
            for (x, y, _) in inked(&live, 2, row) {
                assert!(x >= b.x && y >= b.y && x < b.x + i64::from(b.width) && y < b.y + i64::from(b.height), "live ink ({x}, {y}) outside the outlined bounds");
            }
            assert_eq!(o.fills[0].paint, PreparedPaint::Solid(if row == 1 { RED } else { GREEN }));
        }
        // Unbound or unshaped stories are refused, typed.
        assert_eq!(vector_text::create_text_outlines(&r, &identity("STXT", 9), &assets).unwrap_err(), "vector_text_story");
        assert_eq!(vector_text::create_text_outlines(&r, &identity("STXT", 2), &NoText(Assets::new())).unwrap_err(), TEXT_NOT_PROVIDED);
    });
}
