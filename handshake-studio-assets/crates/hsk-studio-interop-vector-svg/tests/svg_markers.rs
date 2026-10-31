//! Lane SMB-AI-ARROWHEADS-2 slice 2: SVG `marker-start` / `marker-end` import as Folio
//! `stroke_markers` (STU-VEC-146) when the referenced `<marker>` is one recognised shape with
//! `orient="auto"` / `auto-start-reverse`, and SVG export writes each stroke marker as a `<marker>`
//! definition holding nib's marker outline. Everything else stays a typed loss. The recognition
//! table and the scale mapping are Studio readings; SVG-viewer parity is UNVERIFIED (D5).
//! Synthetic SVGs only; every expected number is computed by hand in the comments.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioArrowAlignment, StudioStrokeMarkers};
use hsk_studio_interop_vector_core::{ExportOptions, ImportPolicy, SequentialIdMint, VectorProposal};
use hsk_studio_interop_vector_svg::{export_proposal_with_loss, import_svg};

fn import(svg: &str) -> VectorProposal {
    import_svg(svg.as_bytes(), &ImportPolicy::default(), &mut SequentialIdMint::new(7), &CancellationToken::default()).unwrap()
}

fn codes(p: &VectorProposal, code: &str) -> usize {
    p.loss.iter().filter(|l| l.code == code).count()
}

/// The markers without their minted style ids: (row, start, end, start scale, end scale, alignment).
fn shape(m: &[StudioStrokeMarkers]) -> Vec<(u32, String, String, f64, f64, StudioArrowAlignment)> {
    let mut v: Vec<_> = m.iter().map(|m| (m.row, m.start.clone(), m.end.clone(), m.start_scale, m.end_scale, m.alignment)).collect();
    v.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    v
}

const INSIDE: StudioArrowAlignment = StudioArrowAlignment::InsidePath;
const OUTSIDE: StudioArrowAlignment = StudioArrowAlignment::OutsidePath;

#[test]
fn triangle_marker_end_maps_to_arrow_triangle_with_the_hand_computed_scale() {
    // viewBox 10 x 10 into markerWidth 6 (markerUnits strokeWidth): 0.6 stroke widths per unit.
    // The triangle (0,0) (10,5) (0,10) is 10 units long = 6 stroke widths, half width 5 / length
    // 10 = 0.5 like nib's arrow.triangle (length 3 marker sizes): scale = 100 * 6 / 3 = 200 %.
    // refX/refY sit on the tip: the tip is at the path end (inside_path).
    // The circle marker: r 5 in viewBox 10 into markerWidth 3: diameter 3 stroke widths = nib's
    // circle (diameter 3 marker sizes): 100 %; refX 10 is its front: inside_path; at the start
    // with auto-start-reverse. A circle with fill none is arrow.circle, filled arrow.circle_solid.
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <marker id="a" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="6" markerHeight="6" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="c" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto-start-reverse"><circle cx="5" cy="5" r="5"/></marker>
    <marker id="h" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto-start-reverse"><circle cx="5" cy="5" r="4.5" fill="none" stroke="black" stroke-width="1"/></marker>
  </defs>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#000" stroke-width="2" marker-end="url(#a)"/>
  <line x1="10" y1="50" x2="90" y2="50" stroke="#000" stroke-width="2" marker-start="url(#c)" marker-end="url(#h)"/>
</svg>"##,
    );
    assert_eq!(codes(&p, "unsupported_marker"), 0, "{:?}", p.loss);
    assert_eq!(
        shape(&p.vector_records.stroke_markers),
        shape(&[
            StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.none".into(), end: "arrow.triangle".into(), start_scale: 100.0, end_scale: 200.0, alignment: INSIDE },
            StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.circle_solid".into(), end: "arrow.circle".into(), start_scale: 100.0, end_scale: 100.0, alignment: INSIDE },
        ])
    );
    // Each entry binds the stroke row of its own layer's style.
    let styles: Vec<&str> = p.vector_records.stroke_markers.iter().map(|m| m.style_id.as_str()).collect();
    assert_eq!(styles.len(), 2);
    assert_ne!(styles[0], styles[1]);
}

#[test]
fn unrecognised_markers_keep_unsupported_marker() {
    // Two shapes, a group, a fixed angle, and a stroke-less element: none is mapped.
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <marker id="two" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/><circle cx="5" cy="5" r="1"/></marker>
    <marker id="grp" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><g><path d="M0,0 L10,5 L0,10 z"/></g></marker>
    <marker id="fixed" viewBox="0 0 10 10" refX="10" refY="5" orient="45"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="ok" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
  </defs>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#000" marker-end="url(#two)"/>
  <line x1="10" y1="30" x2="90" y2="30" stroke="#000" marker-end="url(#grp)"/>
  <line x1="10" y1="40" x2="90" y2="40" stroke="#000" marker-end="url(#fixed)"/>
  <line x1="10" y1="50" x2="90" y2="50" stroke="#000" marker-end="url(#missing)"/>
  <path d="M10 60 L90 60" fill="none" marker-end="url(#ok)"/>
</svg>"##,
    );
    assert!(p.vector_records.stroke_markers.is_empty(), "{:?}", p.vector_records.stroke_markers);
    // One loss per use, plus the four definitions no use stored.
    assert_eq!(codes(&p, "unsupported_marker"), 9, "{:?}", p.loss);
}

#[test]
fn marker_mid_keeps_its_loss() {
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <marker id="a" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
  <polyline points="10,10 50,50 90,10" fill="none" stroke="#000" marker-mid="url(#a)" marker-end="url(#a)"/>
</svg>"##,
    );
    // marker-end maps (3 stroke widths long: 100 %); marker-mid has no Studio home.
    assert_eq!(shape(&p.vector_records.stroke_markers), shape(&[StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.none".into(), end: "arrow.triangle".into(), start_scale: 100.0, end_scale: 100.0, alignment: INSIDE }]));
    let mid: Vec<_> = p.loss.iter().filter(|l| l.code == "unsupported_marker").map(|l| l.path.as_str()).collect();
    assert_eq!(mid, ["/svg[1]/polyline[1]"], "{:?}", p.loss);
}

#[test]
fn export_then_import_gives_the_same_markers() {
    // Every recognised shape: triangle, open chevron (arrow.simple), filled and hollow circle and
    // square; inside and outside alignment; start and end scales. (R44 F-R44-4, lane
    // SMB-R44-FOLLOWUPS-1: the hollow chevron and circle on the red and blue lines are drawn with
    // `context-stroke`; a black hollow marker there is now a typed loss, its paint not the stroke's.)
    let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200">
  <defs>
    <marker id="t" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="v" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto-start-reverse"><polyline points="0,0 10,5 0,10" fill="none" stroke="context-stroke"/></marker>
    <marker id="d" viewBox="0 0 10 10" refX="0" refY="5" markerWidth="4.5" markerHeight="4.5" orient="auto-start-reverse"><circle cx="5" cy="5" r="5"/></marker>
    <marker id="o" viewBox="0 0 10 10" refX="0" refY="5" markerWidth="4.5" markerHeight="4.5" orient="auto-start-reverse"><circle cx="5" cy="5" r="4" fill="none" stroke="context-stroke" stroke-width="2"/></marker>
    <marker id="s" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto-start-reverse"><rect x="0" y="0" width="10" height="10"/></marker>
    <marker id="q" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="3" markerHeight="3" orient="auto-start-reverse"><rect x="1" y="1" width="8" height="8" fill="none" stroke="black" stroke-width="2"/></marker>
  </defs>
  <line x1="20" y1="20" x2="180" y2="20" stroke="#ff0000" stroke-width="2" marker-start="url(#t)" marker-end="url(#v)"/>
  <line x1="20" y1="60" x2="180" y2="60" stroke="#0000ff" stroke-width="4" marker-start="url(#d)" marker-end="url(#o)"/>
  <line x1="20" y1="100" x2="180" y2="100" stroke="#000000" stroke-width="1" marker-start="url(#s)" marker-end="url(#q)"/>
</svg>"##;
    let first = import(source);
    assert_eq!(codes(&first, "unsupported_marker"), 0, "{:?}", first.loss);
    // t: 6 stroke widths = 200 %; v: 3 = 100 %; d and o: diameter 4.5 = 150 %, refX on the back:
    // outside_path; s and q: 3 = 100 % (the hollow square's outer edge spans the viewBox).
    let want = shape(&[
        StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.triangle".into(), end: "arrow.simple".into(), start_scale: 200.0, end_scale: 100.0, alignment: INSIDE },
        StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.circle_solid".into(), end: "arrow.circle".into(), start_scale: 150.0, end_scale: 150.0, alignment: OUTSIDE },
        StudioStrokeMarkers { style_id: String::new(), row: 0, start: "arrow.square_solid".into(), end: "arrow.square".into(), start_scale: 100.0, end_scale: 100.0, alignment: INSIDE },
    ]);
    assert_eq!(shape(&first.vector_records.stroke_markers), want);
    let out = export_proposal_with_loss(&first, &ExportOptions::default()).unwrap();
    assert!(out.text.contains("<marker "), "{}", out.text);
    assert!(out.text.contains("marker-start=\"url(#") && out.text.contains("marker-end=\"url(#"), "{}", out.text);
    let second = import(&out.text);
    assert_eq!(codes(&second, "unsupported_marker"), 0, "{:?}\n{}", second.loss, out.text);
    assert_eq!(shape(&second.vector_records.stroke_markers), want, "{}", out.text);
    // A connector terminator has no shape to write: a typed export loss, the stroke still written.
    let mut erd = first.clone();
    erd.vector_records.stroke_markers[0].end = "ERD_ONE".into();
    let out = export_proposal_with_loss(&erd, &ExportOptions::default()).unwrap();
    assert!(out.loss.iter().any(|l| l.code == "marker_shape_unbuilt"), "{:?}", out.loss);
}

#[test]
fn hostile_marker_references_are_bounded_and_typed() {
    // A marker whose content references the marker itself, a huge markerWidth (scale beyond 1000 %),
    // a NaN viewBox, a zero markerWidth, and an external reference: nothing is mapped, every use is
    // a typed loss, the import finishes.
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <marker id="self" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" marker-end="url(#self)"/></marker>
    <marker id="huge" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="1e300" markerHeight="1e300" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="nan" viewBox="0 0 NaN 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="zero" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="0" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
  </defs>
  <line x1="10" y1="10" x2="90" y2="10" stroke="#000" marker-end="url(#self)"/>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#000" marker-end="url(#huge)"/>
  <line x1="10" y1="30" x2="90" y2="30" stroke="#000" marker-end="url(#nan)"/>
  <line x1="10" y1="40" x2="90" y2="40" stroke="#000" marker-end="url(#zero)"/>
  <line x1="10" y1="50" x2="90" y2="50" stroke="#000" marker-end="url(other.svg#a)"/>
</svg>"##,
    );
    // The self-referencing marker is still one recognised triangle (its content's own marker
    // property is never followed): mapped once, at 100 % (default markerWidth 3).
    assert_eq!(p.vector_records.stroke_markers.len(), 1, "{:?}", p.vector_records.stroke_markers);
    assert_eq!(p.vector_records.stroke_markers[0].end, "arrow.triangle");
    assert_eq!(codes(&p, "unsupported_marker"), 3 + 3, "{:?}", p.loss);
    assert_eq!(codes(&p, "external_marker_denied"), 1, "{:?}", p.loss);
    // CSS values are tokenised, never substring-matched (V4-08): case and whitespace, an escaped
    // '#', and a url() inside a comment that is a decoy for `none`. (The two marked lines differ in
    // colour: identical appearance stacks share one style and so one marker entry.)
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <marker id="a" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
  <line x1="10" y1="10" x2="90" y2="10" stroke="#000" style="MARKER-END : URL( '#a' )"/>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#111" style="marker-end: url(\23 a)"/>
  <line x1="10" y1="30" x2="90" y2="30" stroke="#000" style="marker-end: /* url(#a) */ none"/>
</svg>"##,
    );
    assert_eq!(p.vector_records.stroke_markers.len(), 2, "{:?} {:?}", p.vector_records.stroke_markers, p.loss);
    assert_eq!(codes(&p, "unsupported_marker"), 0, "{:?}", p.loss);
}

// ---- R44 findings F-R44-4 and F-R44-10 (SMB-R44-FOLLOWUPS-1) -----------------------------------

/// A marker maps only when its own paint is the stroke's (Studio draws markers in the stroke's
/// paint): a red filled arrowhead on a black line is a typed loss; a black filled one maps, so does
/// an unpainted one and a `context-stroke` one; a filled arrowhead with its own outline is a loss;
/// a marker at half opacity on an opaque line is a loss, at the line's own opacity it maps;
/// `currentColor` resolves through the marker's `color`; a `fill` set on the `<marker>` element is
/// inherited by its content (V4-08: case and whitespace in the paint value are tokenised).
#[test]
fn marker_paint_must_be_the_strokes_paint() {
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="200">
  <defs>
    <marker id="red" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="red"/></marker>
    <marker id="black" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill=" #000000 "/></marker>
    <marker id="bare" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
    <marker id="ctx" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" style="fill: CONTEXT-STROKE"/></marker>
    <marker id="outlined" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#000" stroke="#000"/></marker>
    <marker id="half" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#000" fill-opacity="0.5"/></marker>
    <marker id="cur" viewBox="0 0 10 10" refX="10" refY="5" orient="auto" color="#00ff00"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker>
    <marker id="inherit" viewBox="0 0 10 10" refX="10" refY="5" orient="auto" fill="red"><path d="M0,0 L10,5 L0,10 z"/></marker>
  </defs>
  <line x1="10" y1="10" x2="90" y2="10" stroke="#000" marker-end="url(#red)"/>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#000" marker-end="url(#black)"/>
  <line x1="10" y1="30" x2="90" y2="30" stroke="#010101" marker-end="url(#bare)"/>
  <line x1="10" y1="40" x2="90" y2="40" stroke="#020202" marker-end="url(#ctx)"/>
  <line x1="10" y1="50" x2="90" y2="50" stroke="#000" marker-end="url(#outlined)"/>
  <line x1="10" y1="60" x2="90" y2="60" stroke="#000" marker-end="url(#half)"/>
  <line x1="10" y1="70" x2="90" y2="70" stroke="#000" stroke-opacity="0.5" marker-end="url(#half)"/>
  <line x1="10" y1="80" x2="90" y2="80" stroke="#00ff00" marker-end="url(#cur)"/>
  <line x1="10" y1="90" x2="90" y2="90" stroke="#000" marker-end="url(#inherit)"/>
</svg>"##,
    );
    // Mapped: black, bare, ctx, half on the half-opaque line, cur (five distinct strokes).
    assert_eq!(p.vector_records.stroke_markers.len(), 5, "{:?} {:?}", p.vector_records.stroke_markers, p.loss);
    assert!(p.vector_records.stroke_markers.iter().all(|m| m.end == "arrow.triangle"));
    // Lost: red, outlined, half on the opaque line, inherit (red from the marker): 4 uses; the
    // `red`, `outlined` and `inherit` definitions no use stored: 3 more.
    assert_eq!(codes(&p, "unsupported_marker"), 4 + 3, "{:?}", p.loss);
    // A hollow marker's stroke: black on a black line maps, black on a red line is a loss.
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <marker id="v" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><polyline points="0,0 10,5 0,10" fill="none" stroke="black"/></marker>
  <line x1="10" y1="10" x2="90" y2="10" stroke="#000" marker-end="url(#v)"/>
  <line x1="10" y1="20" x2="90" y2="20" stroke="#f00" marker-end="url(#v)"/>
</svg>"##,
    );
    assert_eq!(p.vector_records.stroke_markers.len(), 1, "{:?}", p.loss);
    assert_eq!(p.vector_records.stroke_markers[0].end, "arrow.simple");
    assert_eq!(codes(&p, "unsupported_marker"), 1, "{:?}", p.loss);
}

/// Markers on a `polygon` or on a `path` holding a closed subpath are typed losses at import (SVG
/// viewers draw them, Studio does not); an open `path` with the same marker maps.
#[test]
fn markers_on_closed_shapes_are_typed_losses() {
    let p = import(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <marker id="a" viewBox="0 0 10 10" refX="10" refY="5" orient="auto"><path d="M0,0 L10,5 L0,10 z"/></marker>
  <polygon points="10,10 50,10 50,50" fill="none" stroke="#000" marker-end="url(#a)"/>
  <path d="M10 60 L50 60 L50 90 Z" fill="none" stroke="#111" marker-start="url(#a)"/>
  <path d="M60 10 L90 10 M60 20 L90 20 L90 40 z" fill="none" stroke="#222" marker-end="url(#a)"/>
  <path d="M60 60 L90 60" fill="none" stroke="#333" marker-end="url(#a)"/>
</svg>"##,
    );
    assert_eq!(p.vector_records.stroke_markers.len(), 1, "{:?}", p.vector_records.stroke_markers);
    let lost: Vec<&str> = p.loss.iter().filter(|l| l.code == "unsupported_marker").map(|l| l.path.as_str()).collect();
    assert_eq!(lost, ["/svg[1]/polygon[1]", "/svg[1]/path[1]", "/svg[1]/path[2]"], "{:?}", p.loss);
}
