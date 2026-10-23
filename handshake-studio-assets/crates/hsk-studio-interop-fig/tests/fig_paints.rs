//! Lane SMB-INTEROP-FIG-PAINTS: Figma strokes (weight, align, caps, joins, dashes, miter), gradient
//! paints, effect-style shadows and group compositing on the Operator fixtures, rendered through
//! composite (exact nib coverage) + render-cpu and compared with the Operator PNGs at measured
//! tolerances; what Folio cannot hold stays a typed loss.
mod common;
use common::*;
use hsk_studio_folio::{
    Budget, OperationKind, StrokeCap, StudioAppearanceRow, StudioEffectKind, StudioGradientGeometry, StudioPaint,
};
use hsk_studio_interop_fig::*;

fn loss(p: &NativeProposal, path: &str, code: &str) -> u32 {
    p.mapping_loss.iter().filter(|l| l.path == path && l.code == code).map(|l| l.count).sum()
}

fn rows_of<'a>(p: &'a NativeProposal, layer_id: &str) -> &'a [StudioAppearanceRow] {
    let d = p.design.nodes.iter().find(|d| d.node.node_id == layer_id).expect("node design");
    let style = &d.style_refs["fill_style_id"];
    &p.design.paint_styles.iter().find(|s| &s.style_id == style).unwrap().rows
}

fn layer_named<'a>(p: &'a NativeProposal, name: &str) -> &'a hsk_studio_folio::StudioLayer {
    let id = p.id_map.iter().find(|e| {
        p.snapshot.document().layers.iter().any(|l| l.layer_id == e.node.node_id && l.name == name)
    });
    layer(p, &id.expect("mapped layer").node.node_id)
}

/// Largest channel difference between our pixel (over the PNG background) and the PNG.
fn diff(r: &Render, png: &(usize, usize, Vec<u8>), at: (usize, usize), origin: (usize, usize)) -> f64 {
    let bg = [png.2[0], png.2[1], png.2[2]];
    let ours = over(r, at.0, at.1, bg);
    let o = ((at.1 - origin.1) * png.0 + (at.0 - origin.0)) * 4;
    (0..3).map(|c| (ours[c] - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max)
}

#[test]
fn strokes_render_and_caps_dashes_miter_map() {
    let mut doc = document("fig-basic-shapes.fig");
    // The real stroke (rounded-solid-shadow: inside border, no cap or dash stored) renders where
    // the PNG has it.
    let p = map(&doc, Budget::default());
    let rect = layer_named(&p, "rounded-solid-shadow");
    let stroke = rows_of(&p, &rect.layer_id)
        .iter()
        .find_map(|r| match r {
            StudioAppearanceRow::Stroke { stroke, .. } => Some(stroke.clone()),
            _ => None,
        })
        .expect("stroke row");
    assert_eq!((stroke.cap, stroke.miter_limit, stroke.dash.len()), (StrokeCap::Butt, 4.0, 0));
    let r = render(&p, None);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let origin = (100usize, 100usize);
    let mut worst: f64 = 0.0;
    // Border pixels along the rectangle's left and top edges (canvas x 140, y 190), mid-edge.
    for at in [(141usize, 245usize), (142, 245), (225, 191), (225, 192)] {
        let d = diff(&r, &png, at, origin);
        eprintln!("stroke probe {at:?}: {d:.2}");
        worst = worst.max(d);
    }
    // Measured worst 0.25/255 over the four border probes (exact nib stroke coverage).
    assert!(worst <= 0.3, "stroke border differs by {worst}");
    // Synthetic caps, dashes and miter on the same node: round cap, an odd dash pattern written
    // twice (Folio needs an even count), miter 10; a marker cap is a typed loss.
    for n in &mut doc.nodes {
        if n.name.as_deref() == Some("rounded-solid-shadow") {
            n.stroke_cap = Some("ROUND".into());
            n.dash_pattern = vec![4.0, 2.0, 1.0];
            n.miter_limit = Some(10.0);
        }
        if n.name.as_deref() == Some("line-outline") {
            n.stroke_cap = Some("ARROW_LINES".into());
        }
    }
    let p = map(&doc, Budget::default());
    let rect = layer_named(&p, "rounded-solid-shadow");
    let stroke = rows_of(&p, &rect.layer_id)
        .iter()
        .find_map(|r| match r {
            StudioAppearanceRow::Stroke { stroke, .. } => Some(stroke.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(stroke.cap, StrokeCap::Round);
    assert_eq!(stroke.miter_limit, 10.0);
    assert_eq!(stroke.dash.iter().map(|d| d.value).collect::<Vec<_>>(), [4.0, 2.0, 1.0, 4.0, 2.0, 1.0]);
    assert_eq!(loss(&p, "native.strokes.cap=ARROW_LINES", "stroke_cap_marker_no_folio_home"), 1);
}

#[test]
fn gradient_renders_and_unmapped_kinds_are_losses() {
    let mut doc = document("fig-basic-shapes.fig");
    let p = map(&doc, Budget::default());
    let e = layer_named(&p, "ellipse-gradient");
    let StudioAppearanceRow::Fill { paint: StudioPaint::Gradient { gradient }, .. } = &rows_of(&p, &e.layer_id)[0] else {
        panic!("gradient fill")
    };
    assert!(matches!(gradient.geometry, StudioGradientGeometry::Linear { .. }));
    let r = render(&p, None);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let mut worst: f64 = 0.0;
    // Along the ellipse's centre row (canvas y 245): red-orange left to yellow right.
    for x in [365usize, 400, 425, 450, 485] {
        let d = diff(&r, &png, (x, 245), (100, 100));
        eprintln!("gradient probe x {x}: {d:.2}");
        worst = worst.max(d);
    }
    // Measured worst 1.42/255 (x 450) once the node's absolute transform is part of the gradient
    // transform (the geometry is baked into document px); before, the whole ellipse was the end stop.
    assert!(worst <= 1.5, "gradient differs by {worst}");
    // Angular and diamond gradients have no Folio geometry: typed loss, never a substitute.
    for n in &mut doc.nodes {
        if n.name.as_deref() == Some("ellipse-gradient")
            && let PaintKind::Gradient { gradient, .. } = &mut n.fills[0].kind
        {
            *gradient = "GRADIENT_ANGULAR".into();
        }
    }
    let p = map(&doc, Budget::default());
    assert_eq!(loss(&p, "native.fills.GRADIENT_ANGULAR", "gradient_kind_no_folio_home"), 1);
}

#[test]
fn effect_style_shadow_renders_under_instances() {
    let p = map(&document("fig-components-autolayout.fig"), Budget::default());
    // The buttons reference the EFFECT style node 4:8 (fixture-shadow); its drop shadow is the
    // effect of the definitions and of each expansion group.
    for e in &p.expansions {
        let rec = p.raster_records.layers.iter().find(|l| l.layer_id == e.group_layer_id).expect("effects record");
        let fx = &rec.effects.as_ref().unwrap().effects[0];
        assert!(matches!(fx.params, StudioEffectKind::DropShadow { .. }));
        // An expansion group with effects composites isolated Normal.
        let g = layer(&p, &e.group_layer_id);
        assert_eq!(g.appearance.as_ref().unwrap().blend_mode, hsk_studio_folio::BLEND_NORMAL);
    }
    assert_eq!(loss(&p, "native.effects.style", "effect_style_not_in_file"), 0);
    let files = [FontFile::inter_bold()];
    let out = resolve_text_outputs(&p, &[FigFontGrant { family: "Inter", style: "Bold", resource: files[0].resource() }]);
    let text = prepare_outputs(&p, &out.outputs, &files);
    let r = render(&p, Some(&text));
    let png = decode_png(&fixture("fig-components-autolayout.png"));
    let origin = (100usize, 320usize);
    // Just below each instance's bottom edge (canvas y 407..), the shadow darkens the white stack.
    let mut worst: f64 = 0.0;
    for at in [(190usize, 409usize), (301, 409), (439, 409), (190, 412)] {
        let d = diff(&r, &png, at, origin);
        eprintln!("shadow probe {at:?}: {d:.2}");
        worst = worst.max(d);
    }
    // Measured worst 1.22/255; the shadow is really drawn there (229/255 grey on the white stack).
    assert!(worst <= 1.3, "shadow differs by {worst}");
    assert!(over(&r, 190, 409, [0, 0, 0])[0] < 235.0, "shadow drawn below instance 1");
}

#[test]
fn vector_groups_composite_as_groups() {
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    let g = &p.snapshot.document().graph;
    // group-multiply is a mix of its children in stacking order (clip source, background, overlay).
    let gm = layer_named(&p, "group-multiply");
    let op = g.operations.iter().find(|o| o.layer_id == gm.layer_id).expect("group operation");
    assert_eq!(op.operation_kind, OperationKind::Composite);
    let mut edges: Vec<_> = g.edges.iter().filter(|e| e.target.layer_id == gm.layer_id).collect();
    edges.sort_by_key(|e| e.input_ordinal);
    let names: Vec<&str> = edges.iter().map(|e| layer(&p, &e.source.layer_id).name.as_str()).collect();
    assert_eq!(names, ["group-multiply", "group-background", "multiply-overlay"]);
    assert_eq!(loss(&p, "native.graph", "group_flattened_vector_not_group_input"), 0);
    // The pass-through group keeps Figma's multiply over the frame fill beyond its background.
    let r = render(&p, None);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    for at in [(710usize, 420usize), (650, 460), (650, 420)] {
        let d = diff(&r, &png, at, (100, 100));
        assert!(d <= 4.0, "multiply at {at:?} differs by {d}");
    }
}
