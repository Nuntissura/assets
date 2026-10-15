//! Lane SMB-INTEROP-FIG-INSTANCES-FRAMES: component instances expand (overrides mapped onto the
//! expanded nodes), frame fills become background layers and frame clipping a clip record, text
//! layers become text outputs with caller-granted fonts (Type `FontResource`), and the Operator
//! fixtures render through composite + render-cpu against the Operator PNGs at measured
//! tolerances. Hostile instance graphs are refused before any layer is created.
mod common;
use common::*;
use hsk_studio_folio::{Budget, NodeKind, Parent, Payload, StudioAppearanceRow, StudioPaint};
use hsk_studio_interop_fig::*;
use std::collections::BTreeMap;

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

fn loss(p: &NativeProposal, path: &str, code: &str) -> u32 {
    p.mapping_loss.iter().filter(|l| l.path == path && l.code == code).map(|l| l.count).sum()
}

fn expansion_text(p: &NativeProposal, instance: SourceId) -> &FigTextLayout {
    let node = p
        .expanded
        .iter()
        .find(|e| e.instance == instance && layer(p, &e.node.node_id).kind == "text")
        .expect("expanded text layer");
    p.text_layouts.iter().find(|t| t.layer_id == node.node.node_id).expect("text layout")
}

fn grant_inter<'a>(f: &'a FontFile, style: &'a str) -> FigFontGrant<'a> {
    FigFontGrant { family: "Inter", style, resource: f.resource() }
}

/// Bounding box of pixels whose colour is within `tol` of `rgb` (0..=255), over `bg`, in a region.
fn ink_box(r: &Render, region: [usize; 4], rgb: [f64; 3], tol: f64, bg: [u8; 3]) -> Option<[usize; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for y in region[1]..region[3] {
        for x in region[0]..region[2] {
            let p = over(r, x, y, bg);
            if (0..3).all(|c| (p[c] - rgb[c]).abs() <= tol) {
                b = Some(match b {
                    None => [x, y, x, y],
                    Some(k) => [k[0].min(x), k[1].min(y), k[2].max(x), k[3].max(y)],
                });
            }
        }
    }
    b
}

fn png_box(png: &(usize, usize, Vec<u8>), region: [usize; 4], rgb: [f64; 3], tol: f64) -> Option<[usize; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for y in region[1]..region[3] {
        for x in region[0]..region[2] {
            let o = (y * png.0 + x) * 4;
            if (0..3).all(|c| (f64::from(png.2[o + c]) - rgb[c]).abs() <= tol) {
                b = Some(match b {
                    None => [x, y, x, y],
                    Some(k) => [k[0].min(x), k[1].min(y), k[2].max(x), k[3].max(y)],
                });
            }
        }
    }
    b
}

#[test]
fn instance_override_renders_overridden_value() {
    let mut doc = document("fig-components-autolayout.fig");
    // Instance 3 (4:20) carries the file's own text override; two more are injected: a red fill on
    // instance 1's label (4:16 -> 4:10) and a hidden label on instance 2 (4:18 -> 4:12).
    let red = Paint {
        kind: PaintKind::Solid { color: Some(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }) },
        opacity: Some(1.0),
        visible: Some(true),
        blend_mode: None,
        color_variable: None,
    };
    for n in &mut doc.nodes {
        if let NodePayload::Instance { instance, .. } = &mut n.payload {
            if n.id == sid(4, 16) {
                instance.overrides.push(InstanceOverride {
                    guid_path: vec![sid(4, 10)],
                    fills: Some(vec![red.clone()]),
                    ..InstanceOverride::default()
                });
            }
            if n.id == sid(4, 18) {
                instance.overrides.push(InstanceOverride {
                    guid_path: vec![sid(4, 12)],
                    visible: Some(false),
                    ..InstanceOverride::default()
                });
            }
        }
    }
    let p = map(&doc, Budget::default());
    // Every instance draws through an expansion group that directly follows its binding layer.
    assert_eq!(p.expansions.len(), 3);
    let layers = &p.snapshot.document().layers;
    for e in &p.expansions {
        let b = layers.iter().position(|l| Some(&l.layer_id) == e.binding_layer_id.as_ref()).unwrap();
        assert_eq!(layers[b].kind, "component_instance");
        assert_eq!(layers[b + 1].layer_id, e.group_layer_id);
        assert_eq!(layers[b + 1].parent, layers[b].parent);
        assert_eq!(e.path, vec![e.instance]);
    }
    // Overridden values on the expanded nodes; the definition keeps its own.
    let t3 = expansion_text(&p, sid(4, 20));
    assert_eq!(story_text(&p, &t3.story_id), "Changed text");
    assert_eq!(t3.runs[0].end, 12);
    let t1 = expansion_text(&p, sid(4, 16));
    assert_eq!(story_text(&p, &t1.story_id), "Button");
    assert_eq!(t1.runs[0].fill_rgba, Some([1.0, 0.0, 0.0, 1.0]));
    let t2 = expansion_text(&p, sid(4, 18));
    assert!(!layer(&p, &t2.layer_id).visible);
    let def = p.id_map.iter().find(|e| e.source == sid(4, 10)).unwrap();
    let def_text = p.text_layouts.iter().find(|t| t.layer_id == def.node.node_id).unwrap();
    assert_eq!(story_text(&p, &def_text.story_id), "Button");
    // Expanded node identity: Figma's guid path from the instance to the definition node.
    assert!(p.expanded.iter().any(|e| e.path == vec![sid(4, 20), sid(4, 10)]));
    // The one override field not mapped (the overridden line metadata) is a typed loss.
    assert_eq!(loss(&p, "native.instance.expansion.override.textData.lines", "override_field_unsupported"), 1);
    // Instances are no longer graph losses: they composite through their expansions.
    assert_eq!(loss(&p, "native.graph", "layer_kind_not_composited"), 0);

    // Render. Inter Bold is not in the repository: the test grants Inter Regular for the
    // requested Inter/Bold (a caller grant, stated here; glyph shapes differ, positions do not).
    let files = [FontFile::inter_regular()];
    let out = resolve_text_outputs(&p, &[grant_inter(&files[0], "Bold")]);
    assert!(out.missing_fonts.is_empty(), "{:?}", out.missing_fonts);
    let text = prepare_outputs(&p, &out.outputs, &files);
    let r = render(&p, Some(&text));
    eprintln!("components: outputs {} rendered {} refused {:?}", r.outputs, r.rendered, r.refused);
    // The PNG is the instances artboard at (100,320); instance boxes (canvas) from the file:
    // x 144..237, 255..348, 366..512; y 364..407 (probed 6 px / 4 px inside: the white stack
    // background shows at the rounded corners).
    let png = decode_png(&fixture("fig-components-autolayout.png"));
    let bg = [png.2[0], png.2[1], png.2[2]];
    let white = [255.0, 255.0, 255.0];
    // Instance 1: the label is red over the instance fill, no white label.
    let red_box = ink_box(&r, [150, 368, 231, 403], [255.0, 0.0, 0.0], 60.0, bg);
    assert!(red_box.is_some(), "red label drawn");
    assert!(ink_box(&r, [150, 368, 231, 403], white, 40.0, bg).is_none());
    // Instance 2: the hidden label draws nothing; only the instance fill (PNG colour there).
    assert!(ink_box(&r, [261, 368, 342, 403], white, 40.0, bg).is_none());
    // Instance 3: "Changed text" in white, spanning (left, right) like the PNG's label.
    let ours = ink_box(&r, [372, 368, 506, 403], white, 40.0, bg).expect("instance 3 label");
    let theirs = png_box(&png, [272, 48, 406, 83], white, 40.0).expect("PNG label");
    let (ol, or) = (ours[0] as i64 - 100, ours[2] as i64 - 100);
    let (tl, tr) = (theirs[0] as i64, theirs[2] as i64);
    eprintln!("instance 3 label x: ours {ol}..{or} png {tl}..{tr}; red box {red_box:?}");
    // Measured: left edge equal (287), right edge 4 px short (386 vs 390: the Regular stand-in is
    // narrower than Bold); the definition's "Button" would end near 335.
    assert!(ol == tl && or <= tr && tr - or <= 4, "label extent ours {ol}..{or} png {tl}..{tr}");
    // The instance fill itself (button blue) at a point of instance 3 left of the label.
    let p3 = over(&r, 372, 385, bg);
    let o = ((385 - 320) * png.0 + (372 - 100)) * 4;
    for (c, v) in p3.iter().enumerate() {
        assert!((v - f64::from(png.2[o + c])).abs() <= 2.0, "instance 3 fill channel {c}");
    }
}

#[test]
fn frame_fill_and_clip() {
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    let d = p.snapshot.document();
    // The artboard fill is the artboard's bottom-most child: a vector layer of the artboard
    // bounds painted with the frame fill (245,247,252 = 0.96,0.97,0.99).
    let art = &d.artboards[0];
    let bg = p.frame_backgrounds.iter().find(|b| b.frame.node_id == art.artboard_id).unwrap();
    assert!(bg.painted);
    let first = d
        .layers
        .iter()
        .find(|l| matches!(&l.parent, Parent::Node(n) if n.node_id == art.artboard_id))
        .unwrap();
    assert_eq!(first.layer_id, bg.layer_id);
    let Payload::Vector { path_ids } = &first.payload else { panic!("vector background") };
    let path = &p.paths.iter().find(|x| x.path_id == path_ids[0]).unwrap().geometry;
    let xs: Vec<f64> = path.elements.iter().filter_map(|e| match e {
        hsk_studio_nib::PathElement::MoveTo(q) | hsk_studio_nib::PathElement::LineTo(q) => Some(q.x),
        _ => None,
    }).collect();
    assert_eq!((xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max)), (100.0, 1000.0));
    let design = p.design.nodes.iter().find(|n| n.node.node_id == bg.layer_id).unwrap();
    let style = p.design.paint_styles.iter().find(|s| s.style_id == design.style_refs["fill_style_id"]).unwrap();
    let StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } = &style.rows[0] else { panic!("solid fill") };
    assert_eq!((colour.r, colour.g, colour.b), (0.96, 0.97, 0.99));
    // The artboard itself no longer carries the fill rows.
    assert!(p.design.nodes.iter().all(|n| n.node.node_id != art.artboard_id || !n.style_refs.contains_key("fill_style_id")));
    // Clip content: the artboard clips (frameMaskDisabled false); the region is the background's
    // geometry and every other child layer of the artboard carries the clip.
    let clip_id = bg.clip_id.clone().expect("artboard clip");
    let clip = p.vector_records.clips.iter().find(|c| c.clip_id == clip_id).unwrap();
    assert_eq!(clip.source_layer_id, bg.layer_id);
    let children: Vec<&str> = d
        .layers
        .iter()
        .filter(|l| matches!(&l.parent, Parent::Node(n) if n.node_kind == NodeKind::Artboard) && l.layer_id != bg.layer_id)
        .map(|l| l.layer_id.as_str())
        .collect();
    assert!(!children.is_empty());
    for c in &children {
        let a = p.vector_records.attachments.iter().find(|a| a.layer_id == *c).expect("attachment");
        assert_eq!(a.clip_id.as_deref(), Some(clip_id.as_str()));
    }
    // A clipping frame without fill (group-multiply) gets a hidden clip-source layer.
    let gm = p
        .frame_backgrounds
        .iter()
        .find(|b| b.frame.node_kind == NodeKind::Layer && layer(&p, &b.frame.node_id).name == "group-multiply")
        .unwrap();
    assert!(!gm.painted && !layer(&p, &gm.layer_id).visible && gm.clip_id.is_some());
    assert_eq!(loss(&p, "native.clips_content", "frame_clip_not_mapped"), 0);
    // A frame that does not clip (the prototype SECTION, frameMaskDisabled true) gets no clip.
    let proto = map(&document("fig-prototype-pages.fig"), Budget::default());
    let section = proto.frame_backgrounds.iter().find(|b| b.source == sid(2, 2)).unwrap();
    assert!(section.painted && section.clip_id.is_none());
    // Rendered: the frame fill is drawn where no shape is (it was transparent before this lane).
    let r = render(&p, None);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    for (x, y) in [(352usize, 402usize), (809, 178), (600, 700), (950, 700)] {
        let ours = over(&r, x, y, [0, 0, 0]);
        let o = ((y - 100) * png.0 + (x - 100)) * 4;
        for (c, v) in ours.iter().enumerate() {
            assert!((v - f64::from(png.2[o + c])).abs() <= 1.0, "fill at ({x},{y}) channel {c}");
        }
        assert!(r.px[y * r.w + x][3] >= 0.999);
    }
}

#[test]
fn text_output_resolves_granted_font_and_types_missing_family() {
    let files = [FontFile::inter_regular()];
    // Prototype pages: every text is Inter Regular; with the grant all 14 layers are outputs.
    let proto = map(&document("fig-prototype-pages.fig"), Budget::default());
    let out = resolve_text_outputs(&proto, &[grant_inter(&files[0], "Regular")]);
    assert_eq!(out.outputs.len(), 14);
    assert!(out.missing_fonts.is_empty() && out.loss.is_empty());
    let start = out.outputs.iter().find(|o| story_text(&proto, &o.story_id) == "Start").unwrap();
    assert_eq!(start.runs[0].font.identity, "Inter-Regular");
    assert_eq!(start.runs[0].size_px, 18.0);
    // Baseline from Figma's stored layout: frame origin (140,170) + text (24,24) + 17.545454.
    assert_eq!(start.lines[0].baseline_px, [164.0, 211.545454]);
    // Each text layer is a graph output (image-typed text source) of the composited document.
    let g = &proto.snapshot.document().graph;
    for o in &out.outputs {
        assert!(g.operations.iter().any(|op| op.layer_id == o.layer_id && op.outputs[0].value_type == hsk_studio_folio::ValueType::Image));
    }
    // Basic shapes: "Regular text + bold text" requests Inter Bold for its second run; without a
    // grant for it the layer is no output and the loss names the requested family and style.
    let shapes = map(&document("fig-basic-shapes.fig"), Budget::default());
    let out = resolve_text_outputs(&shapes, &[grant_inter(&files[0], "Regular")]);
    assert!(out.outputs.is_empty());
    assert_eq!(out.missing_fonts.len(), 1);
    assert_eq!((out.missing_fonts[0].family.as_str(), out.missing_fonts[0].style.as_str()), ("Inter", "Bold"));
    let l: BTreeMap<(&str, &str), u32> = out.loss.iter().map(|l| ((l.path.as_str(), l.code), l.count)).collect();
    assert_eq!(l.get(&("native.text.font=Inter/Bold", "font_not_granted")), Some(&1));
    // No grants: every requested font is missing, named.
    let none = resolve_text_outputs(&proto, &[]);
    assert!(none.outputs.is_empty());
    assert!(none.missing_fonts.iter().all(|m| m.family == "Inter" && m.style == "Regular"));
    assert_eq!(none.missing_fonts.len(), 14);
    // The resolved output draws through Type: the "Start" label ink lies where the PNG's is.
    let one = resolve_text_outputs(&proto, &[grant_inter(&files[0], "Regular")]);
    let text = prepare_outputs(&proto, &one.outputs, &files);
    let plane = text.plane(&start.layer_id).unwrap().placement.unwrap();
    let png = decode_png(&fixture("fig-prototype-pages-start.png"));
    let dark = png_box(&png, [0, 0, 120, 60], [26.0, 38.0, 64.0], 90.0).expect("PNG label ink");
    eprintln!("Start plane {plane:?} png ink {dark:?} (png origin 140,170)");
    assert!((plane.x - (140 + dark[0] as i64)).abs() <= 2);
    assert!((plane.y - (170 + dark[1] as i64)).abs() <= 2);
}

#[test]
fn whole_fixture_render_matches_operator_png() {
    // Prototype pages, first page: section fill, three frame fills, the orbs and 6 Inter Regular
    // labels, compared with the three screen PNGs over their whole area.
    let files = [FontFile::inter_regular()];
    let p = map(&document("fig-prototype-pages.fig"), Budget::default());
    let out = resolve_text_outputs(&p, &[grant_inter(&files[0], "Regular")]);
    let text = prepare_outputs(&p, &out.outputs, &files);
    let r = render(&p, Some(&text));
    eprintln!("prototype: outputs {} rendered {} refused {:?}", r.outputs, r.rendered, r.refused);
    for (frame, origin) in [("start", (140usize, 170usize)), ("detail", (540, 170)), ("animated", (940, 170))] {
        let png = decode_png(&fixture(&format!("fig-prototype-pages-{frame}.png")));
        let d = diffs(&r, &png, origin);
        let n = d.len() as f64;
        let within = |t: f64| d.iter().filter(|v| **v <= t).count() as f64 / n;
        let mean = d.iter().sum::<f64>() / n;
        let p99 = d[(n * 0.99) as usize];
        eprintln!(
            "{frame}: pixels {} mean {mean:.4} p99 {p99:.4} max {:.1} <=2: {:.4} <=8: {:.4} <=32: {:.4}",
            d.len(),
            d[d.len() - 1],
            within(2.0),
            within(8.0),
            within(32.0)
        );
        // Tolerances from the measured run (lane notes): >= 99.2 % of pixels within 8/255 (measured
        // 0.9923 / 0.9931 / 0.9927), mean difference <= 0.72/255 (measured 0.712 / 0.676 /
        // 0.678), p99 <= 0.51/255 (measured 0.5000 at 4 decimals, f32 residue above it); the residue is glyph antialiasing (Inter 3.019 vs Figma's Inter).
        assert!(within(8.0) >= 0.992, "{frame}: {:.4} of pixels within 8/255", within(8.0));
        assert!(mean <= 0.72, "{frame}: mean difference {mean:.3}");
        assert!(p99 <= 0.51, "{frame}: p99 {p99:.4}");
    }
    // Basic shapes, whole artboard: strokes, a gradient, an image paint and the bold run are
    // composite refusals or missing fonts; reported, compared at its own measured level.
    let shapes = map(&document("fig-basic-shapes.fig"), Budget::default());
    let r = render(&shapes, None);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let d = diffs(&r, &png, (100, 100));
    let n = d.len() as f64;
    let within8 = d.iter().filter(|v| **v <= 8.0).count() as f64 / n;
    let mean = d.iter().sum::<f64>() / n;
    eprintln!("basic-shapes: refused {:?} mean {mean:.3} <=8: {within8:.4}", r.refused);
    // Large residue (measured mean 20.251/255, 88.61 % within 8/255): Figma parity of this fixture
    // is UNVERIFIED (strokes, gradient, image paint, bold run and the drop shadow are not drawn).
    // Pinned as a regression floor at the measured values only.
    assert!(within8 >= 0.886, "basic-shapes: {within8:.4} within 8/255");
    assert!(mean <= 20.26, "basic-shapes: mean {mean:.3}");
}

// ---------- hostile instance graphs (synthetic) ----------

fn node(id: SourceId, parent: Option<SourceId>, ty: &str, payload: NodePayload) -> FigNode {
    FigNode {
        id,
        parent,
        position: None,
        children: Vec::new(),
        type_name: ty.into(),
        name: Some(format!("{ty}-{id}")),
        visible: Some(true),
        opacity: Some(1.0),
        blend_mode: None,
        transform: None,
        size: Some(Vec2 { x: 10.0, y: 10.0 }),
        mask: None,
        fills: Vec::new(),
        strokes: Vec::new(),
        stroke_weight: None,
        stroke_align: None,
        stroke_join: None,
        effects: Vec::new(),
        fill_geometry: Vec::new(),
        stroke_geometry: Vec::new(),
        interactions: Vec::new(),
        flow_start: None,
        payload,
    }
}

fn instance_of(id: SourceId, parent: SourceId, symbol: SourceId, overrides: Vec<InstanceOverride>) -> FigNode {
    node(
        id,
        Some(parent),
        "INSTANCE",
        NodePayload::Instance {
            frame: FramePayload::default(),
            instance: InstancePayload { symbol_id: Some(symbol), overrides, uniform_scale: Some(1.0), derived: Vec::new() },
        },
    )
}

fn symbol(id: SourceId, parent: SourceId) -> FigNode {
    node(
        id,
        Some(parent),
        "SYMBOL",
        NodePayload::Symbol { frame: FramePayload::default(), variant: Vec::new(), component_key: None },
    )
}

/// DOCUMENT 0:0 > CANVAS 0:1 > the given nodes (parents by id); children linked in list order.
fn synthetic(mut nodes: Vec<FigNode>) -> FigDocument {
    let mut all = vec![node(sid(0, 0), None, "DOCUMENT", NodePayload::None), node(sid(0, 1), Some(sid(0, 0)), "CANVAS", NodePayload::Canvas { background: None, background_enabled: None, internal_only: None })];
    all.append(&mut nodes);
    let index: BTreeMap<SourceId, usize> = all.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    for i in 0..all.len() {
        if let Some(p) = all[i].parent {
            let pi = index[&p];
            all[pi].children.push(i);
        }
    }
    FigDocument {
        nodes: all,
        roots: vec![0],
        blob_count: 0,
        loss: Vec::new(),
        decoded: BTreeMap::new(),
        source_sha256: None,
        image_parts: BTreeMap::new(),
    }
}

fn rect(id: SourceId, parent: SourceId) -> FigNode {
    node(id, Some(parent), "RECTANGLE", NodePayload::Rectangle { corner_radius: None, corner_radii: None })
}

#[test]
fn hostile_instances_refused_before_allocation() {
    let canvas = sid(0, 1);
    let big = Budget { nodes: 1 << 20, edges: 1 << 20, input_bytes: 1 << 28, payload_bytes: 1 << 26, depth: 64 };
    // 1. Cycle: A holds an instance of B, B an instance of A; a document instance of A.
    let (a, b) = (sid(1, 1), sid(1, 2));
    let doc = synthetic(vec![
        symbol(a, canvas),
        instance_of(sid(1, 3), a, b, Vec::new()),
        symbol(b, canvas),
        instance_of(sid(1, 4), b, a, Vec::new()),
        instance_of(sid(1, 5), canvas, a, Vec::new()),
    ]);
    let p = map(&doc, big);
    // The document instance (1:5) and both definition instances are refused whole: no group.
    assert_eq!(p.expansions.len(), 0);
    assert_eq!(loss(&p, "native.instance.expansion", "instance_cycle_refused"), 3);
    assert!(p.expanded.is_empty());

    // 2. Nesting deeper than MAX_INSTANCE_DEPTH: symbol k holds an instance of symbol k+1.
    let depth = MAX_INSTANCE_DEPTH + 1;
    let mut nodes = Vec::new();
    for k in 0..=depth as u32 {
        nodes.push(symbol(sid(2, 2 * k), canvas));
        if (k as usize) < depth {
            nodes.push(instance_of(sid(2, 2 * k + 1), sid(2, 2 * k), sid(2, 2 * k + 2), Vec::new()));
        }
    }
    nodes.push(instance_of(sid(2, 9999), canvas, sid(2, 0), Vec::new()));
    let p = map(&synthetic(nodes), big);
    assert!(loss(&p, "native.instance.expansion", "instance_nesting_over_limit") >= 1);
    // The document instance of the deepest chain is refused before any of its layers exist.
    assert!(p.expansions.iter().all(|e| e.instance != sid(2, 9999)));

    // 3. An override path longer than MAX_OVERRIDE_PATH is refused (typed) and the instance still
    //    expands with its other overrides.
    let s = sid(3, 1);
    let long = InstanceOverride { guid_path: (0..=MAX_OVERRIDE_PATH as u32).map(|i| sid(3, 100 + i)).collect(), visible: Some(false), ..InstanceOverride::default() };
    let ok = InstanceOverride { guid_path: vec![sid(3, 2)], name: Some("renamed".into()), ..InstanceOverride::default() };
    let doc = synthetic(vec![symbol(s, canvas), rect(sid(3, 2), s), instance_of(sid(3, 3), canvas, s, vec![long, ok])]);
    let p = map(&doc, big);
    assert_eq!(loss(&p, "native.instance.expansion.override_path", "override_path_over_limit"), 1);
    assert_eq!(p.expansions.len(), 1);
    let renamed = p.expanded.iter().find(|e| e.path == vec![sid(3, 3), sid(3, 2)]).unwrap();
    assert_eq!(layer(&p, &renamed.node.node_id).name, "renamed");

    // 4. Huge instance count: 2000 instances of a 20-child symbol cost 2000 x 42 nodes, past
    //    MAX_EXPANDED_NODES; expansions stop at the budget and each refused instance creates
    //    nothing.
    let s = sid(4, 1);
    let mut nodes = vec![symbol(s, canvas)];
    for c in 0..20 {
        nodes.push(rect(sid(4, 10 + c), s));
    }
    for i in 0..2000 {
        nodes.push(instance_of(sid(5, i), canvas, s, Vec::new()));
    }
    let p = map(&synthetic(nodes), big);
    let refused = loss(&p, "native.instance.expansion", "instance_expansion_over_budget");
    let per = 2 * 21u64;
    assert_eq!(p.expansions.len() as u64, MAX_EXPANDED_NODES / per);
    assert_eq!(u64::from(refused), 2000 - MAX_EXPANDED_NODES / per);
    assert_eq!(p.expanded.len() as u64, (MAX_EXPANDED_NODES / per) * 20);
    // The caller's Folio budget bounds expansion too: the default budget admits none here.
    let small = synthetic(vec![symbol(s, canvas), rect(sid(4, 10), s), instance_of(sid(6, 1), canvas, s, Vec::new())]);
    let tight = Budget { nodes: 10, ..Budget::default() };
    let p = to_native(&small, &NativeConfig { budget: tight, resolver: &Host }, &mut SequentialIdMint::new(1), &hsk_studio_accord::CancellationToken::default());
    match p {
        Ok(p) => assert!(p.expansions.is_empty()),
        Err(e) => panic!("small document maps: {e:?}"),
    }
}
