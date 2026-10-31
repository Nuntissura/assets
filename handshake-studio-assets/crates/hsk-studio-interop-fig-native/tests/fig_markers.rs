//! Lane SMB-AI-ARROWHEADS-2 slice 1: Figma marker stroke caps import as Folio `stroke_markers`
//! (STU-VEC-146) and the frame export draws them. The nodes are the `fig-basic-shapes.fig` fixture
//! of `hsk-studio-interop-fig` (read from this repository) with their stroke cap patched, the way
//! that crate's own stroke tests patch it. One node-level cap marks both ends of an open path, at
//! scale 100 (`studio_declared`) with the tip at the path end (`arrow_align.inside_path`); the cap
//! itself becomes butt. Figma's own marker rendering is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, NodeKind, NodeRef, Resolution, Resolver, StrokeCap, StudioAppearanceRow, StudioArrowAlignment, StudioStrokeMarkers, TileRef,
};
use hsk_studio_interop_fig_base::{Color, FigDocument, FigOutcome, Limits, Paint, PaintKind, VersionPolicy, decode_document, import_fig};
use hsk_studio_interop_fig_native::{
    FigExportFormat, FigExportPlan, NativeConfig, NativeProposal, SequentialIdMint, export_frame_pdf, render_frame_export, to_native,
};
use hsk_studio_nib::PathElement;

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn document() -> FigDocument {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-interop-fig/tests/fixtures/fig-basic-shapes.fig");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap() else {
        panic!("fixture imports")
    };
    decode_document(&import, &Limits::default(), &cancel).unwrap()
}

fn map(doc: &FigDocument) -> NativeProposal {
    let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
    to_native(doc, &cfg, &mut SequentialIdMint::new(0xA11), &CancellationToken::default()).unwrap()
}

const BLACK: Paint = Paint {
    kind: PaintKind::Solid { color: Some(Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }) },
    opacity: None,
    visible: None,
    blend_mode: None,
    color_variable: None,
};

/// The fixture with node `name` stroked solid black at `weight` and stroke cap `cap`.
fn patched(name: &str, cap: &str, weight: f32) -> FigDocument {
    let mut doc = document();
    let n = doc.nodes.iter_mut().find(|n| n.name.as_deref() == Some(name)).unwrap_or_else(|| panic!("node {name}"));
    n.stroke_cap = Some(cap.into());
    n.stroke_weight = Some(weight);
    n.strokes = vec![BLACK];
    n.stroke_align = Some("CENTER".into());
    doc
}

fn layer_id(p: &NativeProposal, name: &str) -> String {
    p.snapshot.document().layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id.clone()
}

/// The layer's paint style id and the index of its first stroke row.
fn stroke_row(p: &NativeProposal, layer: &str) -> (String, u32, StudioAppearanceRow) {
    let d = p.design.nodes.iter().find(|d| d.node.node_id == layer).expect("node design");
    let id = d.style_refs["fill_style_id"].clone();
    let s = p.design.paint_styles.iter().find(|s| s.style_id == id).unwrap();
    let i = s.rows.iter().position(|r| matches!(r, StudioAppearanceRow::Stroke { .. })).expect("stroke row");
    (id, u32::try_from(i).unwrap(), s.rows[i].clone())
}

fn cap_losses(p: &NativeProposal) -> Vec<(String, &'static str, u32)> {
    p.mapping_loss.iter().filter(|l| l.path.starts_with("native.strokes.cap")).map(|l| (l.path.clone(), l.code, l.count)).collect()
}

#[test]
fn triangle_filled_cap_imports_as_markers_on_both_ends() {
    let p = map(&patched("line-outline", "TRIANGLE_FILLED", 8.0));
    let line = layer_id(&p, "line-outline");
    let (style_id, row, stroke) = stroke_row(&p, &line);
    let StudioAppearanceRow::Stroke { stroke, .. } = stroke else { unreachable!() };
    assert_eq!(stroke.cap, StrokeCap::Butt, "the marker cap itself becomes butt");
    let want = StudioStrokeMarkers {
        style_id,
        row,
        start: "TRIANGLE_FILLED".into(),
        end: "TRIANGLE_FILLED".into(),
        start_scale: 100.0,
        end_scale: 100.0,
        alignment: StudioArrowAlignment::InsidePath,
    };
    assert_eq!(p.vector_records.stroke_markers, vec![want]);
    assert_eq!(cap_losses(&p), Vec::<(String, &'static str, u32)>::new(), "no stroke_cap_marker_no_folio_home loss");
    // Every collaborative marker cap and every connector terminator maps verbatim.
    for cap in ["ARROW_LINES", "ARROW_EQUILATERAL", "DIAMOND_FILLED", "CIRCLE_FILLED", "ERD_ONE", "ERD_ZERO_OR_ONE"] {
        let p = map(&patched("line-outline", cap, 2.0));
        let m = &p.vector_records.stroke_markers;
        assert_eq!(m.len(), 1, "{cap}");
        assert_eq!((m[0].start.as_str(), m[0].end.as_str()), (cap, cap));
        assert!(cap_losses(&p).is_empty(), "{cap}: {:?}", cap_losses(&p));
    }
}

#[test]
fn erd_cap_is_stored_and_its_render_refusal_is_typed() {
    let p = map(&patched("line-outline", "ERD_MANY", 4.0));
    let m = &p.vector_records.stroke_markers;
    assert_eq!(m.len(), 1);
    assert_eq!((m[0].start.as_str(), m[0].end.as_str()), ("ERD_MANY", "ERD_MANY"));
    assert!(cap_losses(&p).is_empty());
    // Stored, not drawn: nib refuses the terminator typed (composite reports the same code).
    let refused = hsk_studio_nib::markers::MarkerShape::from_token("ERD_MANY").unwrap_err();
    assert_eq!(refused.code(), "marker_shape_unbuilt");
    // The frame export draws the stroke and reports the marker as a typed loss.
    let (pdf, _) = export_line(&p);
    assert!(
        pdf.loss.iter().any(|l| l.path == "export.stroke.markers" && l.code == "marker_shape_unbuilt"),
        "{:?}",
        pdf.loss
    );
}

#[test]
fn network_stroke_keeps_its_loss() {
    // A VECTOR node is a vector network: its endpoint markers are not built (STU-VEC-062,
    // composite `vector_marker_network`), so the cap stays the typed loss.
    let p = map(&patched("pen-freeform-bezier", "TRIANGLE_FILLED", 2.0));
    assert!(p.vector_records.stroke_markers.is_empty());
    assert_eq!(cap_losses(&p), vec![("native.strokes.cap=TRIANGLE_FILLED".to_owned(), "stroke_cap_marker_no_folio_home", 1)]);
}

/// The line layer exported as its own frame: bounds around its end point (document px, scale 1).
fn export_line(p: &NativeProposal) -> (hsk_studio_interop_fig_native::FigPdfExport, [f64; 4]) {
    let line = layer_id(p, "line-outline");
    let path = &p.paths.iter().find(|x| x.layer_id == line).expect("line path").geometry;
    let end = match path.elements.last() {
        Some(PathElement::LineTo(q)) => [q.x, q.y],
        other => panic!("line ends with {other:?}"),
    };
    let bounds = [end[0] - 40.0, end[1] - 20.0, 60.0, 40.0];
    let plan = FigExportPlan {
        source: p.id_map.iter().find(|e| e.node.node_id == line).expect("source").source,
        frame: NodeRef { node_kind: NodeKind::Layer, node_id: line },
        file_name: "line.pdf".into(),
        format: FigExportFormat::Pdf,
        scale: 1.0,
        bounds,
        default_setting: false,
    };
    (export_frame_pdf(p, &plan, None, &CancellationToken::default()).unwrap(), bounds)
}

#[test]
fn export_writes_the_marker_cap_back_as_its_outline() {
    let cancel = CancellationToken::default();
    let (marked, b) = export_line(&map(&patched("line-outline", "TRIANGLE_FILLED", 8.0)));
    let (plain, _) = export_line(&map(&patched("line-outline", "NONE", 8.0)));
    assert!(!marked.loss.iter().any(|l| l.path.contains("marker")), "{:?}", marked.loss);
    let (marked, plain) = (render_frame_export(&marked, &cancel).unwrap(), render_frame_export(&plain, &cancel).unwrap());
    // The line runs along +x and ends at (40, 20) of the page. TRIANGLE_FILLED at weight 8: marker
    // size 8, tip at the end, base 16 px behind it, half width 8 (nib table, Studio reading). Pixel
    // (25, 25) spans 14..15 px behind the tip (triangle half width 7..7.5) and 5..6 px off the line
    // (the stroke's half width is 4): inside the marker only.
    let lum = |pg: &hsk_studio_press::RenderedPage, x: u32, y: u32| pg.pixel(x, y).map(|c| c.iter().map(|v| u32::from(*v)).sum::<u32>()).unwrap();
    let (px, py) = (25, 25);
    assert!(lum(&plain, px, py) > 740, "unmarked render is paper there: {}", lum(&plain, px, py));
    assert!(lum(&marked, px, py) < 100, "the marker is drawn: {}", lum(&marked, px, py));
    // On the line far from the end both draw the stroke; beyond the tip neither draws.
    assert!(lum(&plain, 10, 20) < 100 && lum(&marked, 10, 20) < 100);
    assert!(lum(&plain, 45, 20) > 740 && lum(&marked, 45, 20) > 740);
    assert_eq!(b[2], 60.0);
}

#[test]
fn unknown_marker_cap_keeps_the_old_loss() {
    let p = map(&patched("line-outline", "WASHI_TAPE_1", 2.0));
    assert!(p.vector_records.stroke_markers.is_empty());
    assert_eq!(cap_losses(&p), vec![("native.strokes.cap=WASHI_TAPE_1".to_owned(), "stroke_cap_marker_no_folio_home", 1)]);
}

/// R44 finding F-R44-5 (lane SMB-R44-FOLLOWUPS-1): at 50 % stroke opacity the marked stroke is
/// painted ONCE. From the PDF content (press's own reader of the exported bytes): the trimmed
/// stroke and every marker outline are drawn opaque (alpha 1) inside one transparency group whose
/// opacity is 0.5, and nothing of them is drawn at 0.5 outside it. From the render: no pixel is
/// darker than a pixel of the stroke alone (a double application at 50 % black would leave 25 %
/// paper in the overlap, a third of the stroke-only level; 6 of 765 levels allow edge rounding).
/// At full opacity no group is written (the opaque export is unchanged).
#[test]
fn half_opacity_markers_are_painted_once_in_one_group() {
    let cancel = CancellationToken::default();
    let mut doc = patched("line-outline", "TRIANGLE_FILLED", 8.0);
    let n = doc.nodes.iter_mut().find(|n| n.name.as_deref() == Some("line-outline")).unwrap();
    n.strokes = vec![Paint { opacity: Some(0.5), ..BLACK }];
    let p = map(&doc);
    let (_, _, row) = stroke_row(&p, &layer_id(&p, "line-outline"));
    let StudioAppearanceRow::Stroke { opacity, .. } = row else { unreachable!() };
    assert!((opacity - 50.0).abs() < 1e-9, "row opacity {opacity}");
    let (pdf, _) = export_line(&p);
    let pages = hsk_studio_press::read_vector_pages(&pdf.bytes, 1 << 24, &cancel).unwrap();
    let items = &pages[0].items;
    let open = items.iter().position(|i| matches!(i, hsk_studio_press::VectorItem::PushGroup { .. })).expect("one group");
    let close = items.iter().rposition(|i| matches!(i, hsk_studio_press::VectorItem::PopGroup)).expect("group closed");
    let hsk_studio_press::VectorItem::PushGroup { opacity, .. } = &items[open] else { unreachable!() };
    assert!((opacity - 0.5).abs() < 1e-6, "group opacity {opacity}");
    let alpha = |i: &hsk_studio_press::VectorItem| match i {
        hsk_studio_press::VectorItem::Draw { paint: hsk_studio_press::VectorPaint::Solid(c), mode, .. } => Some((c[3], matches!(mode, hsk_studio_press::VectorMode::Stroke(_)))),
        _ => None,
    };
    let inside: Vec<(f32, bool)> = items[open..close].iter().filter_map(alpha).collect();
    assert!(inside.iter().any(|(_, s)| *s) && inside.iter().any(|(_, s)| !*s), "stroke and marker fills in the group: {inside:?}");
    assert!(inside.iter().all(|(a, _)| (*a - 1.0).abs() < 1e-6), "drawn opaque inside the group: {inside:?}");
    let outside: Vec<(f32, bool)> = items[..open].iter().chain(&items[close..]).filter_map(alpha).collect();
    assert!(outside.iter().all(|(a, _)| (*a - 1.0).abs() < 1e-6), "no half-alpha draw outside the group: {outside:?}");
    // Render: the overlap is no darker than the stroke alone.
    let page = render_frame_export(&pdf, &cancel).unwrap();
    let lum = |x: u32, y: u32| page.pixel(x, y).map(|c| c.iter().map(|v| u32::from(*v)).sum::<u32>()).unwrap();
    let stroke_only = lum(10, 20);
    assert!((300..500).contains(&stroke_only), "50 % black over paper: {stroke_only}");
    let darkest = (0..page.width).flat_map(|x| (0..page.height).map(move |y| (x, y))).map(|(x, y)| lum(x, y)).min().unwrap();
    assert!(darkest + 6 >= stroke_only, "a darker overlap ({darkest}) than the stroke alone ({stroke_only})");
    // Opaque: no group.
    let (opaque, _) = export_line(&map(&patched("line-outline", "TRIANGLE_FILLED", 8.0)));
    let pages = hsk_studio_press::read_vector_pages(&opaque.bytes, 1 << 24, &cancel).unwrap();
    assert!(!pages[0].items.iter().any(|i| matches!(i, hsk_studio_press::VectorItem::PushGroup { .. })));
}
