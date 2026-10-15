//! Typed node-tree tests (lane SMB-INTEROP-FIG-NODES). Real-file assertions use the three
//! Operator-authored Figma desktop 126.10.7 fixtures (tests/fixtures, provenance.json); expected
//! names, geometry, colours and text were read from the fixtures' PNG references and the decoded
//! values. Hostile-input tests use synthetic Kiwi schemas/messages. Decoding the node tree is not
//! editable import: FigImport::editable stays NOT_PROVEN.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_fig::*;
use std::sync::Arc;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name} must be checked in: {e}"))
}

fn import(name: &str) -> FigImport {
    match import_fig(
        &fixture(name),
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &CancellationToken::default(),
    )
    .unwrap()
    {
        FigOutcome::Imported(i) => *i,
        FigOutcome::OpaqueOnly { error, .. } => panic!("{name}: {error}"),
    }
}

fn doc(name: &str) -> (FigImport, FigDocument) {
    let imp = import(name);
    let d = decode_document(&imp, &Limits::default(), &CancellationToken::default())
        .unwrap_or_else(|e| panic!("{name} decodes: {e}"));
    (imp, d)
}

fn one<'a>(d: &'a FigDocument, name: &'a str) -> &'a FigNode {
    let mut it = d.named(name);
    let n = it.next().unwrap_or_else(|| panic!("node {name}"));
    assert!(it.next().is_none(), "{name} is unique");
    n
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn color(c: Option<Color>, r: f32, g: f32, b: f32, a: f32) {
    let c = c.expect("colour");
    assert!(
        close(c.r, r) && close(c.g, g) && close(c.b, b) && close(c.a, a),
        "{c:?} != ({r},{g},{b},{a})"
    );
}

fn solid(p: &Paint) -> Option<Color> {
    match &p.kind {
        PaintKind::Solid { color } => *color,
        other => panic!("solid paint expected, got {other:?}"),
    }
}

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

const FIXTURES: [&str; 3] = [
    "fig-basic-shapes.fig",
    "fig-components-autolayout.fig",
    "fig-prototype-pages.fig",
];

#[test]
fn real_node_counts_match_census_and_tree_is_ordered() {
    for name in FIXTURES {
        let (imp, d) = doc(name);
        let census = imp.census.as_ref().unwrap();
        assert_eq!(
            d.kind_histogram(),
            census.node_type_histogram,
            "{name} kinds"
        );
        assert_eq!(d.nodes.len() as u32, census.node_count, "{name} count");
        assert_eq!(d.roots.len(), 1, "{name} one root");
        let root = &d.nodes[d.roots[0]];
        assert_eq!((root.id, root.type_name.as_str()), (sid(0, 0), "DOCUMENT"));
        for code in [
            "unreachable_node",
            "parent_missing",
            "duplicate_guid",
            "node_without_guid",
        ] {
            assert!(d.loss.iter().all(|l| l.code != code), "{name}: {code}");
        }
        // Every non-root node is somebody's child exactly once.
        let mut child_refs = vec![0u32; d.nodes.len()];
        for n in &d.nodes {
            for &c in &n.children {
                child_refs[c] += 1;
                assert_eq!(d.nodes[c].parent, Some(n.id));
            }
        }
        for (i, refs) in child_refs.iter().enumerate() {
            assert_eq!(*refs, u32::from(!d.roots.contains(&i)), "{name} node {i}");
        }
        assert_eq!(imp.editable, EditableStatus::NotProven);
    }
    let (_, shapes) = doc("fig-basic-shapes.fig");
    let board = one(&shapes, "basic-shapes-artboard");
    let order: Vec<&str> = board
        .children
        .iter()
        .map(|&c| shapes.nodes[c].name.as_deref().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "rounded-solid-shadow",
            "ellipse-gradient",
            "line-outline",
            "star-half-opacity",
            "pen-freeform-bezier",
            "boolean-union",
            "group-multiply",
            "layer-blur",
            "mask-group",
            "mixed-style-text"
        ]
    );
    let (_, proto) = doc("fig-prototype-pages.fig");
    let pages: Vec<&str> = proto.nodes[proto.roots[0]]
        .children
        .iter()
        .map(|&c| proto.nodes[c].name.as_deref().unwrap())
        .collect();
    assert_eq!(
        pages,
        ["prototype-flow", "Internal Only Canvas", "scrolling-page"]
    );
}

#[test]
fn real_basic_shapes_geometry_colour_text() {
    let (imp, d) = doc("fig-basic-shapes.fig");
    // Artboard 900x640 at (100,100) on the page (PNG reference is 900x640).
    let board = one(&d, "basic-shapes-artboard");
    assert_eq!(board.size, Some(Vec2 { x: 900.0, y: 640.0 }));
    assert_eq!(
        (board.transform.unwrap().m02, board.transform.unwrap().m12),
        (100.0, 100.0)
    );

    // Blue rounded rectangle, dark stroke, drop shadow (PNG: top-left shape).
    let r = one(&d, "rounded-solid-shadow");
    assert_eq!(r.type_name, "ROUNDED_RECTANGLE");
    assert_eq!(r.size, Some(Vec2 { x: 170.0, y: 110.0 }));
    assert_eq!(
        (r.transform.unwrap().m02, r.transform.unwrap().m12),
        (40.0, 90.0)
    );
    assert_eq!(
        r.payload,
        NodePayload::Rectangle {
            corner_radius: Some(18.0),
            corner_radii: Some([18.0; 4])
        }
    );
    color(solid(&r.fills[0]), 0.16, 0.38, 0.90, 1.0);
    color(solid(&r.strokes[0]), 0.05, 0.13, 0.35, 1.0);
    assert_eq!(
        (r.stroke_weight, r.stroke_align.as_deref()),
        (Some(3.0), Some("INSIDE"))
    );
    let shadow = &r.effects[0];
    assert_eq!(shadow.kind, "DROP_SHADOW");
    assert_eq!(
        (shadow.offset, shadow.radius),
        (Some(Vec2 { x: 4.0, y: 6.0 }), Some(10.0))
    );
    color(shadow.color, 0.0, 0.0, 0.0, 0.25);
    // Fill geometry decodes to commands spanning exactly the node box.
    let cmds = r.fill_geometry[0].commands.as_ref().unwrap();
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for c in cmds {
        if let PathCommand::MoveTo { x, y }
        | PathCommand::LineTo { x, y }
        | PathCommand::CubicTo { x, y, .. } = c
        {
            max_x = max_x.max(*x);
            max_y = max_y.max(*y);
        }
    }
    assert_eq!((max_x, max_y), (170.0, 110.0));
    assert_eq!(cmds.last(), Some(&PathCommand::Close));

    // Linear gradient ellipse (red -> yellow).
    let e = one(&d, "ellipse-gradient");
    let PaintKind::Gradient {
        gradient, stops, ..
    } = &e.fills[0].kind
    else {
        panic!("gradient")
    };
    assert_eq!(gradient, "GRADIENT_LINEAR");
    assert_eq!(stops.len(), 2);
    color(Some(stops[0].color), 1.0, 0.35, 0.30, 1.0);
    color(Some(stops[1].color), 1.0, 0.85, 0.20, 1.0);
    assert!(matches!(
        e.payload,
        NodePayload::Ellipse {
            start_angle: Some(0.0),
            ..
        }
    ));
    assert_eq!(
        e.fill_geometry[0].commands.as_ref().unwrap()[0],
        PathCommand::MoveTo { x: 150.0, y: 55.0 }
    );

    let line = one(&d, "line-outline");
    assert_eq!(
        (line.type_name.as_str(), line.stroke_weight),
        ("LINE", Some(5.0))
    );
    assert!(line.stroke_geometry[0].commands.is_some());

    let star = one(&d, "star-half-opacity");
    assert_eq!(star.opacity, Some(0.5));
    let NodePayload::Star { count, inner_scale } = star.payload else {
        panic!("star")
    };
    assert_eq!(count, Some(5));
    assert!(close(inner_scale.unwrap(), 0.381_966));

    // Pen path: vector network of 5 vertices, 5 segments, one region with one 5-segment loop.
    let v = one(&d, "pen-freeform-bezier");
    let NodePayload::Vector {
        network: Some(net),
        normalized_size,
    } = &v.payload
    else {
        panic!("vector network")
    };
    assert_eq!(*normalized_size, Some(Vec2 { x: 160.0, y: 100.0 }));
    assert_eq!(
        (net.vertices.len(), net.segments.len(), net.regions.len()),
        (5, 5, 1)
    );
    assert_eq!(net.regions[0].loops, vec![vec![0, 1, 2, 3, 4]]);
    for vx in &net.vertices {
        assert!((0.0..=160.0).contains(&vx.x) && (0.0..=100.0).contains(&vx.y));
    }

    let b = one(&d, "boolean-union");
    assert_eq!(
        b.payload,
        NodePayload::Boolean {
            operation: Some("UNION".into())
        }
    );
    let kids: Vec<_> = b
        .children
        .iter()
        .map(|&c| d.nodes[c].name.clone().unwrap())
        .collect();
    assert_eq!(kids, ["union-rectangle", "union-ellipse"]);

    assert_eq!(
        one(&d, "multiply-overlay").blend_mode.as_deref(),
        Some("MULTIPLY")
    );
    let blur = &one(&d, "layer-blur").effects[0];
    assert_eq!(
        (blur.kind.as_str(), blur.radius),
        ("FOREGROUND_BLUR", Some(6.0))
    );
    assert_eq!(one(&d, "mask-ellipse").mask, Some(true));

    // Image fill -> images/<hex(hash)> part of the container.
    let img = one(&d, "masked-gradient");
    let PaintKind::Image {
        hash,
        part,
        scale_mode,
        original_size,
        ..
    } = &img.fills[0].kind
    else {
        panic!("image paint")
    };
    assert_eq!(hash.len(), 20);
    let part = part.as_deref().unwrap();
    assert_eq!(part, "images/7607597e993245032beac09a29395c2e824070d2");
    assert!(imp.container.part(part).is_some());
    assert_eq!(
        (scale_mode.as_deref(), *original_size),
        (Some("FILL"), Some((480, 320)))
    );

    // Mixed-style text: regular run + bold run.
    let t = one(&d, "mixed-style-text");
    let NodePayload::Text(text) = &t.payload else {
        panic!("text")
    };
    assert_eq!(text.characters, "Regular text + bold text");
    assert_eq!(text.font_size, Some(26.0));
    assert_eq!(text.runs.len(), 2);
    let (a, bold) = (&text.runs[0], &text.runs[1]);
    assert_eq!(
        (a.start, a.end, a.font_style.as_deref()),
        (0, 15, Some("Regular"))
    );
    assert_eq!(
        (bold.start, bold.end, bold.font_style.as_deref()),
        (15, 24, Some("Bold"))
    );
    assert_eq!(bold.font_family.as_deref(), Some("Inter"));
    color(solid(&bold.fills[0]), 0.10, 0.15, 0.25, 1.0);

    // Glyph outlines (derivedTextData, not mapped) still decode as path blobs, quadratics included.
    let tree =
        decode_message_tree(&imp, &Limits::default(), &CancellationToken::default()).unwrap();
    let blobs = tree.field("blobs").unwrap().as_array().unwrap();
    let (mut quads, mut decoded) = (0, 0);
    for n in tree.field("nodeChanges").unwrap().as_array().unwrap() {
        let Some(glyphs) = n.field("derivedTextData").and_then(|t| t.field("glyphs")) else {
            continue;
        };
        for g in glyphs.as_array().unwrap() {
            let i = g.field("commandsBlob").unwrap().as_u32().unwrap() as usize;
            let bytes = blobs[i].field("bytes").unwrap().as_bytes().unwrap();
            let cmds = decode_path_blob(bytes, &Limits::default()).unwrap();
            quads += cmds
                .iter()
                .filter(|c| matches!(c, PathCommand::QuadTo { .. }))
                .count();
            decoded += 1;
        }
    }
    assert_eq!(decoded, 24);
    assert!(quads > 0, "glyph outlines use quadratic commands");
}

#[test]
fn real_components_variables_prototypes() {
    let (_, d) = doc("fig-components-autolayout.fig");
    let set = one(&d, "fixture-button");
    let NodePayload::Frame(fp) = &set.payload else {
        panic!("component set frame")
    };
    assert_eq!(fp.is_state_group, Some(true));
    assert_eq!(fp.component_props.len(), 1);
    let prop = &fp.component_props[0];
    assert_eq!(
        (prop.name.as_deref(), prop.kind.as_deref()),
        (Some("state"), Some("VARIANT"))
    );

    let default = one(&d, "state=default");
    let NodePayload::Symbol { frame, variant, .. } = &default.payload else {
        panic!("symbol")
    };
    assert_eq!(variant[0].value.as_deref(), Some("default"));
    assert_eq!(variant[0].prop_def, prop.id);
    let al = frame.auto_layout.as_ref().unwrap();
    assert_eq!((al.mode.as_str(), al.spacing), ("HORIZONTAL", Some(8.0)));
    // The symbol fill is bound to variable button-fill (4:3); its single-mode value resolves.
    assert_eq!(default.fills[0].color_variable, Some(sid(4, 3)));
    let fill_var = one(&d, "button-fill");
    assert_eq!(fill_var.id, sid(4, 3));
    let NodePayload::Variable(var) = &fill_var.payload else {
        panic!("variable")
    };
    assert_eq!(var.set_id, Some(one(&d, "fixture-theme").id));
    assert_eq!(var.scopes, ["FRAME_FILL"]);
    let Some(VariableValue::Color(c)) = var.resolved else {
        panic!("resolved colour")
    };
    color(Some(c), 0.15, 0.35, 0.85, 1.0);
    color(solid(&default.fills[0]), c.r, c.g, c.b, c.a);
    let NodePayload::VariableSet { modes } = &one(&d, "fixture-theme").payload else {
        panic!("set")
    };
    assert_eq!(modes.len(), 1);
    assert_eq!(modes[0].name.as_deref(), Some("light"));

    // Instances (PNG: "Button", darker "Button", "Changed text").
    let inst = |name: &str| match &one(&d, name).payload {
        NodePayload::Instance { instance, .. } => instance.clone(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(inst("button-instance-1").symbol_id, Some(sid(4, 9)));
    assert_eq!(inst("button-instance-2").symbol_id, Some(sid(4, 11)));
    let third = inst("button-instance-3");
    assert_eq!(third.symbol_id, Some(sid(4, 9)));
    let text_ov = third
        .overrides
        .iter()
        .find(|o| o.characters.is_some())
        .unwrap();
    assert_eq!(text_ov.guid_path, [sid(4, 10)]);
    assert_eq!(text_ov.characters.as_deref(), Some("Changed text"));
    assert!(
        third
            .overrides
            .iter()
            .any(|o| o.size == Some(Vec2 { x: 146.0, y: 43.0 }))
    );

    // Prototype: three click interactions with their destinations, a flow start, a scroll frame.
    let (_, p) = doc("fig-prototype-pages.fig");
    let action = |name: &str| {
        let n = one(&p, name);
        assert_eq!(n.interactions.len(), 1);
        let it = &n.interactions[0];
        assert_eq!(it.trigger.as_deref(), Some("ON_CLICK"));
        it.actions[0].clone()
    };
    let nav = action("Click: navigate to details");
    assert_eq!(nav.destination, Some(one(&p, "detail-screen").id));
    assert_eq!(nav.navigation.as_deref(), Some("NAVIGATE"));
    assert_eq!(nav.transition.as_deref(), Some("INSTANT_TRANSITION"));
    let smart = action("Click: smart animate");
    assert_eq!(smart.destination, Some(one(&p, "animated-screen").id));
    assert_eq!(smart.transition.as_deref(), Some("SMART_ANIMATE"));
    assert!(close(smart.duration.unwrap(), 0.35));
    assert_eq!(smart.easing.as_deref(), Some("INOUT_CUBIC"));
    assert_eq!(
        action("Click: back to start").destination,
        Some(one(&p, "start-screen").id)
    );
    assert_eq!(
        one(&p, "start-screen").flow_start.as_deref(),
        Some("sample-flow")
    );
    let NodePayload::Frame(sf) = &one(&p, "vertical-scroll-artboard").payload else {
        panic!("frame")
    };
    assert_eq!(sf.scroll_direction.as_deref(), Some("VERTICAL"));
    assert!(matches!(
        one(&p, "prototype-section").payload,
        NodePayload::Frame(_)
    ));
}

#[test]
fn real_fields_decoded_or_listed_as_loss() {
    for name in FIXTURES {
        let (imp, d) = doc(name);
        let census = imp.census.as_ref().unwrap();
        // Every NodeChange field present on a node was either read or reported: never silent.
        for (field, count) in &census.node_field_histogram {
            let path = format!("NodeChange.{field}");
            let read = d.decoded.get(&path).copied().unwrap_or(0);
            let lost = d.loss_count(&path, "field_not_decoded");
            assert_eq!(
                read + lost,
                *count,
                "{name}: {path} read {read} lost {lost}"
            );
        }
        for f in COMMON_NODE_FIELDS {
            assert_eq!(
                d.loss_count(&format!("NodeChange.{f}"), "field_not_decoded"),
                0,
                "{f}"
            );
        }
        assert!(d.loss_count("NodeChange.editInfo", "field_not_decoded") > 0);
        assert!(d.loss.iter().all(|l| l.count > 0));
        assert!(
            d.loss.iter().all(|l| l.code != "field_type_unexpected"),
            "{name}: {:?}",
            d.loss
                .iter()
                .filter(|l| l.code == "field_type_unexpected")
                .collect::<Vec<_>>()
        );
        // Root Message fields other than nodeChanges/blobs are reported too.
        assert!(d.loss_count("Message.originFileKey", "field_not_decoded") == 1);
    }
    let (_, shapes) = doc("fig-basic-shapes.fig");
    // derivedTextData is decoded for its baselines, glyph sizes and font keys (lane
    // SMB-INTEROP-FIG-INSTANCES-FRAMES); its other members stay field-level losses.
    assert!(shapes.loss_count("NodeChange.derivedTextData", "field_not_decoded") == 0);
    assert!(shapes.loss_count("NodeChange.derivedTextData.glyphs.commandsBlob", "field_not_decoded") > 0);
    assert!(shapes.loss_count("NodeChange.fillPaints.authoredColor", "field_not_decoded") > 0);
    assert!(
        shapes.loss_count(
            "NodeChange.vectorData.styleOverrideTable",
            "field_not_decoded"
        ) == 1
    );
    let (_, comps) = doc("fig-components-autolayout.fig");
    // derivedSymbolData is decoded (guid path, size, transform, derived text) since lane
    // SMB-INTEROP-FIG-INSTANCES-FRAMES.
    assert_eq!(
        comps.loss_count("NodeChange.derivedSymbolData", "field_not_decoded"),
        0
    );
}

// ---------- hostile inputs (synthetic) ----------

fn varuint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 127) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 128);
    }
}

const T_BYTE: i32 = -2;
const T_UINT: i32 = -4;
const T_FLOAT: i32 = -5;
const T_STRING: i32 = -6;

/// 0 STRUCT Item {uint a}; 1 STRUCT Empty {}; 2 MESSAGE Deep {Deep child=1};
/// 3 MESSAGE Message {Item[] items=1, string s=2, Deep deep=3, Empty[] empties=4, byte[] raw=5,
/// float f=6}.
fn hostile_schema() -> Vec<u8> {
    type Def<'a> = (&'a str, u8, Vec<(&'a str, i32, bool, u32)>);
    let defs: Vec<Def> = vec![
        ("Item", 1, vec![("a", T_UINT, false, 1)]),
        ("Empty", 1, vec![]),
        ("Deep", 2, vec![("child", 2, false, 1)]),
        (
            "Message",
            2,
            vec![
                ("items", 0, true, 1),
                ("s", T_STRING, false, 2),
                ("deep", 2, false, 3),
                ("empties", 1, true, 4),
                ("raw", T_BYTE, true, 5),
                ("f", T_FLOAT, false, 6),
            ],
        ),
    ];
    let mut out = Vec::new();
    varuint(&mut out, defs.len() as u32);
    for (name, kind, fields) in defs {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.push(kind);
        varuint(&mut out, fields.len() as u32);
        for (fname, ty, is_array, value) in fields {
            out.extend_from_slice(fname.as_bytes());
            out.push(0);
            varuint(&mut out, ((ty << 1) ^ (ty >> 31)) as u32);
            out.push(u8::from(is_array));
            varuint(&mut out, value);
        }
    }
    out
}

fn kiwi(msg: &[u8], limits: &Limits) -> Result<KiwiValue> {
    decode_kiwi(
        &hostile_schema(),
        msg,
        "Message",
        limits,
        &CancellationToken::default(),
    )
}

#[test]
fn kiwi_tree_hostile_bounds() {
    let lim = Limits::default();
    // Valid: two items, a string, a float (Kiwi rotated encoding of 1.5), a 3-byte array.
    let mut ok = vec![1, 2, 7, 9, 2];
    ok.extend_from_slice(b"hi\0");
    ok.push(6);
    ok.extend_from_slice(&1.5f32.to_bits().rotate_right(23).to_le_bytes());
    ok.extend_from_slice(&[5, 3, 0xAA, 0xBB, 0xCC, 0]);
    let v = kiwi(&ok, &lim).unwrap();
    assert_eq!(v.field("f").unwrap().as_f32(), Some(1.5));
    assert_eq!(v.field("s").unwrap().as_str(), Some("hi"));
    assert_eq!(
        v.field("raw").unwrap().as_bytes(),
        Some(&[0xAA, 0xBB, 0xCC][..])
    );
    let items = v.field("items").unwrap().as_array().unwrap();
    assert_eq!(items[1].field("a").unwrap().as_u32(), Some(9));

    // dims_u32_max: an array count of u32::MAX is refused before any allocation.
    let mut huge = vec![1];
    varuint(&mut huge, u32::MAX);
    assert_eq!(
        kiwi(&huge, &lim).unwrap_err(),
        FigError::BudgetExceeded("array_len")
    );
    // A count under the cap but larger than the input left.
    let mut long = vec![1];
    varuint(&mut long, 1_000_000);
    long.extend_from_slice(&[1, 2, 3]);
    assert_eq!(
        kiwi(&long, &lim).unwrap_err(),
        FigError::MessageInvalid("array_len_exceeds_input")
    );
    // aggregate_over_budget: zero-width elements cost no input bytes; the aggregate value budget
    // refuses them before the vector is allocated.
    let mut empties = vec![4];
    varuint(&mut empties, 10_000_000);
    empties.push(0);
    let small = Limits {
        max_tree_values: 1000,
        ..Limits::default()
    };
    assert_eq!(
        kiwi(&empties, &small).unwrap_err(),
        FigError::BudgetExceeded("tree_values")
    );
    // Deep nesting: 40 nested messages exceed max_depth 32.
    let mut deep = vec![3];
    deep.extend(std::iter::repeat_n(1u8, 40));
    deep.extend(std::iter::repeat_n(0u8, 42));
    assert_eq!(
        kiwi(&deep, &lim).unwrap_err(),
        FigError::MessageInvalid("depth")
    );
    // String over max_string_bytes, byte array past the end, duplicate wire field, trailing bytes.
    let mut s = vec![2];
    s.extend_from_slice(&[b'x'; 64]);
    s.extend_from_slice(&[0, 0]);
    let tight = Limits {
        max_string_bytes: 8,
        ..Limits::default()
    };
    assert_eq!(
        kiwi(&s, &tight).unwrap_err(),
        FigError::BudgetExceeded("string_bytes")
    );
    assert_eq!(
        kiwi(&[5, 9, 1, 2], &lim).unwrap_err(),
        FigError::MessageInvalid("truncated")
    );
    assert_eq!(
        kiwi(&[2, b'a', 0, 2, b'b', 0, 0], &lim).unwrap_err(),
        FigError::MessageInvalid("duplicate_field_on_wire")
    );
    assert_eq!(
        kiwi(&[0, 0], &lim).unwrap_err(),
        FigError::MessageInvalid("trailing_bytes")
    );
    assert_eq!(
        kiwi(&[2, 0xFF, 0, 0], &lim).unwrap_err(),
        FigError::MessageInvalid("string_encoding")
    );
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert_eq!(
        decode_kiwi(&hostile_schema(), &ok, "Message", &lim, &cancel).unwrap_err(),
        FigError::Canceled
    );
}

fn f(name: &str, value: KiwiValue) -> KiwiField {
    KiwiField {
        name: Arc::from(name),
        id: 0,
        value,
    }
}
fn e(name: &str) -> KiwiValue {
    KiwiValue::Enum {
        value: 0,
        name: Some(Arc::from(name)),
    }
}
fn guid(s: u32, l: u32) -> KiwiValue {
    KiwiValue::Struct(vec![
        f("sessionID", KiwiValue::Uint(s)),
        f("localID", KiwiValue::Uint(l)),
    ])
}
fn node(id: u32, parent: Option<u32>, extra: Vec<KiwiField>) -> KiwiValue {
    let mut fields = vec![
        f("guid", guid(1, id)),
        f(
            "type",
            e(if parent.is_none() {
                "DOCUMENT"
            } else {
                "VECTOR"
            }),
        ),
    ];
    if let Some(p) = parent {
        fields.push(f(
            "parentIndex",
            KiwiValue::Message(vec![
                f("guid", guid(1, p)),
                f("position", KiwiValue::String("!".into())),
            ]),
        ));
    }
    fields.extend(extra);
    KiwiValue::Message(fields)
}
fn geom(blob: u32) -> KiwiField {
    f(
        "fillGeometry",
        KiwiValue::Array(vec![KiwiValue::Message(vec![f(
            "commandsBlob",
            KiwiValue::Uint(blob),
        )])]),
    )
}
fn network(blob: u32) -> KiwiField {
    f(
        "vectorData",
        KiwiValue::Message(vec![f("vectorNetworkBlob", KiwiValue::Uint(blob))]),
    )
}
fn message(nodes: Vec<KiwiValue>, blobs: Vec<Vec<u8>>, extra: Vec<KiwiField>) -> KiwiValue {
    let mut fields = vec![
        f("nodeChanges", KiwiValue::Array(nodes)),
        f(
            "blobs",
            KiwiValue::Array(
                blobs
                    .into_iter()
                    .map(|b| KiwiValue::Message(vec![f("bytes", KiwiValue::Bytes(b))]))
                    .collect(),
            ),
        ),
    ];
    fields.extend(extra);
    KiwiValue::Message(fields)
}

#[test]
fn node_tree_hostile_blobs_and_cycles() {
    let lim = Limits::default();
    let cancel = CancellationToken::default();
    let mut counts_max = Vec::new();
    for v in [u32::MAX, u32::MAX, u32::MAX] {
        counts_max.extend_from_slice(&v.to_le_bytes());
    }
    let msg = message(
        vec![
            node(0, None, vec![]),
            node(1, Some(0), vec![geom(0), network(2)]), // unknown command 9; counts u32::MAX
            node(2, Some(0), vec![geom(1), network(7)]), // truncated move; blob index out of range
            node(3, Some(4), vec![]),                    // 3 <-> 4 parent cycle
            node(4, Some(3), vec![]),
            node(5, Some(99), vec![]), // parent missing
            node(5, Some(0), vec![]),  // duplicate guid
            node(
                6,
                Some(0),
                vec![
                    f("opacity", KiwiValue::String("x".into())),
                    f("visible", e("?")),
                ],
            ),
        ],
        vec![vec![9, 0, 0], vec![1, 0, 0, 128], counts_max],
        vec![],
    );
    let d = document_from_tree(&msg, None, &lim, &cancel).unwrap();
    assert_eq!(
        d.loss_count(
            "NodeChange.fillGeometry.commandsBlob",
            "path_command_unknown"
        ),
        1
    );
    assert_eq!(
        d.loss_count(
            "NodeChange.fillGeometry.commandsBlob",
            "path_blob_truncated"
        ),
        1
    );
    assert_eq!(
        d.loss_count(
            "NodeChange.vectorData.vectorNetworkBlob",
            "vector_network_counts_exceed_blob"
        ),
        1
    );
    assert_eq!(
        d.loss_count(
            "NodeChange.vectorData.vectorNetworkBlob",
            "blob_index_out_of_range"
        ),
        1
    );
    assert_eq!(
        d.loss_count("NodeChange.parentIndex", "unreachable_node"),
        3
    );
    assert_eq!(d.loss_count("NodeChange.parentIndex", "parent_missing"), 1);
    assert_eq!(d.loss_count("NodeChange.guid", "duplicate_guid"), 1);
    assert_eq!(
        d.loss_count("NodeChange.opacity", "field_type_unexpected"),
        1
    );
    assert_eq!(
        d.loss_count("NodeChange.visible", "field_type_unexpected"),
        1
    );
    assert_eq!(d.nodes.len(), 7);
    assert!(d.nodes[1].fill_geometry[0].commands.is_none());

    // Aggregate geometry budget: one blob of 5000 close commands shared by 10 nodes.
    let many = (1..=10).map(|i| node(i, Some(0), vec![geom(0)]));
    let msg = message(
        std::iter::once(node(0, None, vec![])).chain(many).collect(),
        vec![vec![0; 5000]],
        vec![],
    );
    let small = Limits {
        max_tree_values: 20_000,
        ..Limits::default()
    };
    assert_eq!(
        document_from_tree(&msg, None, &small, &cancel).unwrap_err(),
        FigError::BudgetExceeded("tree_values")
    );
    assert!(document_from_tree(&msg, None, &lim, &cancel).is_ok());
    // A non-zero blobBaseIndex (meaning UNVERIFIED) resolves no blob instead of guessing.
    let msg = message(
        vec![node(0, None, vec![]), node(1, Some(0), vec![geom(0)])],
        vec![vec![0]],
        vec![f("blobBaseIndex", KiwiValue::Uint(3))],
    );
    let d = document_from_tree(&msg, None, &lim, &cancel).unwrap();
    assert_eq!(
        d.loss_count("Message.blobBaseIndex", "blob_base_index_unsupported"),
        1
    );
    assert_eq!(
        d.loss_count("NodeChange.fillGeometry.commandsBlob", "blob_unresolvable"),
        1
    );
    // Node count bound and cancellation.
    let few = Limits {
        max_nodes: 1,
        ..Limits::default()
    };
    assert_eq!(
        document_from_tree(&msg, None, &few, &cancel).unwrap_err(),
        FigError::BudgetExceeded("nodes")
    );
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(
        document_from_tree(&msg, None, &lim, &stop).unwrap_err(),
        FigError::Canceled
    );
}
