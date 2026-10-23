//! `StudioVectorRecords` contract: pattern paint kind (STU-VEC-017/138, STU-COL-195/196), vector
//! clip and mask (STU-VEC-053, STU-VEC-153) and the layer attachment. Every expected code is
//! derived from the rule named next to it.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}
fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn layer(n: u32, kind: &str, payload: Value, parent: Option<u32>) -> Value {
    let parent = parent.map_or(
        Value::Null,
        |p| json!({"node_kind":"layer","node_id":identity("SLYR",p)}),
    );
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
        "parent":parent,"kind":kind,"payload":payload,"visible":true})
}
fn group() -> Value {
    json!({"payload_kind":"group"})
}
fn vector() -> Value {
    json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})
}
fn document_of(layers: Vec<Value>) -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
/// 1 group, 2 vector in 1, 3 vector (clip geometry), 4 raster, 5 group (tile artwork) with 6 vector
/// inside, 7 vector (mask content), 8 vector (live-source tile).
fn document() -> StudioDocument {
    document_of(vec![
        layer(1, "group", group(), None),
        layer(2, "vector", vector(), Some(1)),
        layer(3, "vector", vector(), None),
        layer(
            4,
            "raster",
            json!({"payload_kind":"raster","tiles":[]}),
            None,
        ),
        layer(5, "group", group(), None),
        layer(6, "vector", vector(), Some(5)),
        layer(7, "vector", vector(), None),
        layer(8, "vector", vector(), None),
    ])
}
fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r": r, "g": g, "b": b, "colour_profile_id": null})
}
fn row(kind: &str) -> Value {
    json!({"row_kind": kind, "paint": {"paint_kind": "solid", "colour": colour(0.5, 0.5, 0.5)},
        "opacity": 100.0, "blend_mode": 2, "visible": true})
}
/// Style 1 has two fill rows (stand-ins for the bound patterns); layer 2 uses it.
fn design_value(nodes: Value) -> Value {
    json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),"nodes":nodes,
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),
            "kind":"PAINT","name":"brand/dots",
            "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"brand-dots","is_remote":false},
            "rows":[row("fill"), row("fill")]}]})
}
fn design(nodes: Value) -> StudioDesignRecords {
    serde_json::from_value(design_value(nodes)).expect("design shape")
}
fn base_design() -> StudioDesignRecords {
    design(
        json!([{"node":{"node_kind":"layer","node_id":identity("SLYR",2)},
        "style_refs":{"fill_style_id":identity("SSTY",1)}}]),
    )
}
fn transform() -> Value {
    json!({"rotation":15.0,"scale_factor":50.0,"shear_angle":0.0,"shear_axis":0.0,"shift_angle":0.0,
        "shift_distance":pt(0.0),"reflect":false,"reflect_angle":0.0})
}
/// A full, valid record set (as JSON, so tests can break single fields).
fn records() -> Value {
    json!({
        "schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "patterns":[
            {"pattern_id":"tile-a","name":"brand/dots","tile_type":"brick_by_row",
             "horizontal_spacing":pt(2.0),"vertical_spacing":pt(-1.0),
             "source":{"pattern_model":"definition","content_layer_id":identity("SLYR",5),
                "tile_width":pt(16.0),"tile_height":pt(8.0),"brick_offset":50.0,
                "overlap_order":"left_in_front","move_tile_with_art":true}},
            {"pattern_id":"live-b","name":"live/hex","tile_type":"hex_by_column",
             "horizontal_spacing":pt(0.0),"vertical_spacing":pt(0.0),
             "source":{"pattern_model":"live_source","source_layer_id":identity("SLYR",8),
                "scaling_factor":1.5,"horizontal_alignment":"center"}}],
        "pattern_paints":[
            {"style_id":identity("SSTY",1),"row":0,"pattern_id":"tile-a",
             "placement":{"placement_kind":"parametric","transform":transform()}},
            {"style_id":identity("SSTY",1),"row":1,"pattern_id":"live-b",
             "placement":{"placement_kind":"matrix","matrix":[2.0,0.0,0.0,2.0,5.0,5.0]}}],
        "clips":[{"clip_id":"clip-a","source_layer_id":identity("SLYR",3),"clip_rule":"EVENODD"}],
        "masks":[{"mask_id":"mask-a","source_layer_id":identity("SLYR",7),"mode":"LUMINANCE","mask_clip":true,
            "region":{"x":pt(0.0),"y":pt(0.0),"width":pt(100.0),"height":pt(50.0)}}],
        "attachments":[
            {"layer_id":identity("SLYR",4),"clip_id":"clip-a","mask_id":"mask-a"},
            {"layer_id":identity("SLYR",1),"clip_id":"clip-a"}]
    })
}
fn run(
    v: &Value,
    d: &StudioDocument,
    design: Option<&StudioDesignRecords>,
) -> Result<(), Diagnostic> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_vector_records(
        &r,
        d,
        design,
        Budget::default(),
        &CancellationToken::default(),
    )
}
fn validate(v: &Value) -> Result<(), Diagnostic> {
    run(v, &document(), Some(&base_design()))
}
fn code(v: &Value) -> Code {
    validate(v).expect_err("must be refused").code
}
fn with(path: &str, value: Value) -> Value {
    let mut v = records();
    *v.pointer_mut(path).expect("pointer") = value;
    v
}

#[test]
fn full_record_set_validates_and_round_trips() {
    let v = records();
    assert_eq!(validate(&v), Ok(()));
    // The combined sidecars agree: the design records of the same document are valid too.
    struct Caller;
    impl Resolver for Caller {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            true
        }
    }
    assert_eq!(
        validate_design_records(
            &base_design(),
            &document(),
            Budget::default(),
            &CancellationToken::default(),
            &Caller
        ),
        Ok(vec![])
    );
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).unwrap();
    let text = serde_json::to_string(&r).unwrap();
    let again: StudioVectorRecords = serde_json::from_str(&text).unwrap();
    assert_eq!(again, r);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
    // Lookups.
    assert!(r.pattern("live-b").is_some() && r.pattern("nope").is_none());
    assert_eq!(
        r.pattern_paint(&identity("SSTY", 1), 1)
            .map(|p| p.pattern_id.as_str()),
        Some("live-b")
    );
    assert!(r.pattern_paint(&identity("SSTY", 1), 2).is_none());
    assert_eq!(
        r.clip("clip-a").map(|c| c.clip_rule),
        Some(StudioClipRule::EvenOdd)
    );
    assert_eq!(
        r.mask("mask-a").map(|m| m.mode),
        Some(StudioMaskMode::Luminance)
    );
    assert_eq!(
        r.attachment(&identity("SLYR", 1))
            .and_then(|a| a.mask_id.as_deref()),
        None
    );
    // Absent members are not encoded; mask booleans default to false and are omitted then.
    let empty = StudioVectorRecords::new(identity("SDOC", 1));
    let text = serde_json::to_string(&empty).unwrap();
    assert!(!text.contains("patterns") && !text.contains("attachments"));
    let mut plain = records();
    plain["masks"][0]
        .as_object_mut()
        .unwrap()
        .remove("mask_clip");
    let m: StudioVectorRecords = serde_json::from_value(plain).unwrap();
    assert!(!m.masks[0].mask_clip && !m.masks[0].invert_mask);
    let text = serde_json::to_string(&m.masks[0]).unwrap();
    assert!(!text.contains("mask_clip") && !text.contains("invert_mask"));
    // STU-VEC-053 / STU-VEC-110 tokens; STU-VEC-138 captured tile tokens (brick has none).
    assert_eq!(
        [
            StudioMaskMode::Alpha,
            StudioMaskMode::Vector,
            StudioMaskMode::Luminance
        ]
        .map(|m| serde_json::to_string(&m).unwrap()),
        ["\"ALPHA\"", "\"VECTOR\"", "\"LUMINANCE\""]
    );
    assert_eq!(
        [StudioClipRule::NonZero, StudioClipRule::EvenOdd]
            .map(|m| serde_json::to_string(&m).unwrap()),
        ["\"NONZERO\"", "\"EVENODD\""]
    );
    let tokens = [
        StudioTileType::Grid,
        StudioTileType::BrickByRow,
        StudioTileType::BrickByColumn,
        StudioTileType::HexByRow,
        StudioTileType::HexByColumn,
    ]
    .map(StudioTileType::capture_token);
    assert_eq!(
        tokens,
        [
            Some("RECTANGULAR"),
            None,
            None,
            Some("HORIZONTAL_HEXAGONAL"),
            Some("VERTICAL_HEXAGONAL")
        ]
    );
    assert_eq!(
        StudioTileType::from_capture_token("VERTICAL_HEXAGONAL"),
        Some(StudioTileType::HexByColumn)
    );
    assert_eq!(StudioTileType::from_capture_token("BRICK"), None);
    assert_eq!(
        [
            StudioTileAlignment::Start,
            StudioTileAlignment::Center,
            StudioTileAlignment::End
        ]
        .map(StudioTileAlignment::capture_token),
        ["START", "CENTER", "END"]
    );
    let identity_transform = StudioPatternTransform::identity(GeometryUnit::Pt);
    assert_eq!(identity_transform.scale_factor, 100.0);
    // The closed wire shape refuses unknown members, unknown models and lowercase/NONE tokens.
    for (path, bad) in [
        ("/masks/0/opacity", json!(1.0)),
        ("/clips/0/clip_rule", json!("NONE")),
        ("/masks/0/mode", json!("alpha")),
        ("/patterns/0/source/pattern_model", json!("swatch")),
        ("/patterns/0/tile_type", json!("rectangular")),
        ("/pattern_paints/0/placement/placement_kind", json!("shear")),
    ] {
        let mut v = records();
        match v.pointer_mut(path) {
            Some(slot) => *slot = bad,
            None => {
                let (parent, last) = path.rsplit_once('/').unwrap();
                v.pointer_mut(parent)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(last.into(), bad);
            }
        }
        assert!(
            serde_json::from_value::<StudioVectorRecords>(v).is_err(),
            "{path} must be refused"
        );
    }
    assert!(
        vector_records_schema()
            .as_value()
            .to_string()
            .contains("vector_records")
    );
}

#[test]
fn spec_ranges_and_document_units_are_enforced() {
    // STU-COL-195: brick offset is a percent 0..=100.
    for bad in [100.5, -0.5] {
        assert_eq!(
            code(&with("/patterns/0/source/brick_offset", json!(bad))),
            Code::InvalidField
        );
    }
    assert_eq!(
        validate(&with("/patterns/0/source/brick_offset", json!(100.0))),
        Ok(())
    );
    // STU-COL-195: tile size hard minimum 0; every Length is in the document unit (pt here).
    assert_eq!(
        validate(&with("/patterns/0/source/tile_width", pt(0.0))),
        Ok(())
    );
    assert_eq!(
        code(&with("/patterns/0/source/tile_width", pt(-1.0))),
        Code::InvalidUnit
    );
    assert_eq!(
        code(&with(
            "/patterns/0/source/tile_height",
            json!({"value":8.0,"unit":"mm"})
        )),
        Code::InvalidUnit
    );
    assert_eq!(
        code(&with(
            "/patterns/0/horizontal_spacing",
            json!({"value":1.0,"unit":"px"})
        )),
        Code::InvalidUnit
    );
    // Spacing states no bound: the fixture's -1 pt vertical spacing is accepted (full set above).
    // Live source: positive scaling factor.
    for bad in [0.0, -2.0] {
        assert_eq!(
            code(&with("/patterns/1/source/scaling_factor", json!(bad))),
            Code::InvalidField
        );
    }
    // STU-VEC-138 placement: 100 percent identity, positive scale, document-unit shift distance.
    let t = |field: &str, value: Value| {
        let mut v = records();
        v["pattern_paints"][0]["placement"]["transform"][field] = value;
        v
    };
    assert_eq!(code(&t("scale_factor", json!(0.0))), Code::InvalidField);
    assert_eq!(code(&t("scale_factor", json!(-100.0))), Code::InvalidField);
    assert_eq!(
        code(&t("shift_distance", json!({"value":1.0,"unit":"in"}))),
        Code::InvalidUnit
    );
    let r: StudioVectorRecords = serde_json::from_value(records()).unwrap();
    type Break = fn(&mut StudioPatternTransform);
    let cases: [(&str, Break); 4] = [
        ("rotation", |p| p.rotation = f64::NAN),
        ("shear_angle", |p| p.shear_angle = f64::INFINITY),
        ("reflect_angle", |p| p.reflect_angle = f64::NEG_INFINITY),
        ("scale_factor", |p| p.scale_factor = f64::NAN),
    ];
    for (case, mutate) in cases {
        let mut broken = r.clone();
        let StudioPatternPlacement::Parametric { transform } =
            &mut broken.pattern_paints[0].placement
        else {
            panic!("fixture placement");
        };
        mutate(transform);
        let e = validate_vector_records(
            &broken,
            &document(),
            Some(&base_design()),
            Budget::default(),
            &CancellationToken::default(),
        );
        assert_eq!(e.expect_err(case).code, Code::InvalidField, "{case}");
    }
    // Matrix: finite and invertible (the last case has finite members but an overflowing
    // determinant, 1e308 * 1e308).
    for bad in [
        json!([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        json!([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]),
        json!([1.0e308, 0.0, 0.0, 1.0e308, 0.0, 0.0]),
    ] {
        assert_eq!(
            code(&with("/pattern_paints/1/placement/matrix", bad.clone())),
            Code::InvalidField,
            "{bad}"
        );
    }
    // Labels and ids.
    assert_eq!(
        code(&with("/patterns/0/name", json!(""))),
        Code::InvalidField
    );
    assert_eq!(
        code(&with("/patterns/0/source/overlap_order", json!(""))),
        Code::InvalidField
    );
    assert_eq!(
        code(&with("/patterns/0/pattern_id", json!("has space"))),
        Code::InvalidId
    );
    assert_eq!(code(&with("/clips/0/clip_id", json!(""))), Code::InvalidId);
    assert_eq!(
        code(&with("/masks/0/mask_id", json!("x".repeat(129)))),
        Code::InvalidId
    );
    // Mask region: document unit, non-negative size.
    assert_eq!(
        code(&with("/masks/0/region/width", pt(-5.0))),
        Code::InvalidUnit
    );
    assert_eq!(
        code(&with("/masks/0/region/x", json!({"value":0.0,"unit":"px"}))),
        Code::InvalidUnit
    );
    let mut no_region = records();
    no_region["masks"][0]
        .as_object_mut()
        .unwrap()
        .remove("region");
    assert_eq!(validate(&no_region), Ok(()));
}

#[test]
fn references_resolve_with_typed_errors() {
    let doc = document();
    // Pattern content and source layers.
    assert_eq!(
        code(&with(
            "/patterns/0/source/content_layer_id",
            json!(identity("SLYR", 99))
        )),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with(
            "/patterns/1/source/source_layer_id",
            json!(identity("SDOC", 8))
        )),
        Code::InvalidId
    );
    // A clip region is geometry: vector or group payload; a raster layer is refused, a group works.
    assert_eq!(
        code(&with(
            "/clips/0/source_layer_id",
            json!(identity("SLYR", 4))
        )),
        Code::PayloadMismatch
    );
    assert_eq!(
        validate(&with(
            "/clips/0/source_layer_id",
            json!(identity("SLYR", 5))
        )),
        Ok(())
    );
    // A mask takes any layer's content: the raster layer 4 masks layer 2. (Attached to layer 4
    // itself, the mask would be a cycle: see `reference_cycles_are_refused`.)
    let mut raster_mask = with("/masks/0/source_layer_id", json!(identity("SLYR", 4)));
    raster_mask["attachments"][0]["layer_id"] = json!(identity("SLYR", 2));
    assert_eq!(validate(&raster_mask), Ok(()));
    assert_eq!(
        code(&with(
            "/masks/0/source_layer_id",
            json!(identity("SLYR", 99))
        )),
        Code::DanglingReference
    );
    // Attachments: known layer, known clip/mask, at least one of them, one entry per layer.
    assert_eq!(
        code(&with(
            "/attachments/0/layer_id",
            json!(identity("SLYR", 99))
        )),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with("/attachments/0/clip_id", json!("clip-z"))),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with("/attachments/0/mask_id", json!("mask-z"))),
        Code::DanglingReference
    );
    let mut v = records();
    v["attachments"][1] = json!({"layer_id":identity("SLYR",1)});
    assert_eq!(code(&v), Code::InvalidField);
    let mut v = records();
    v["attachments"][1]["layer_id"] = json!(identity("SLYR", 4));
    assert_eq!(code(&v), Code::DuplicateIdentity);
    // Duplicate set-local ids.
    let mut v = records();
    let p = v["patterns"][0].clone();
    v["patterns"].as_array_mut().unwrap().push(p);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    let mut v = records();
    let c = v["clips"][0].clone();
    v["clips"].as_array_mut().unwrap().push(c);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    let mut v = records();
    let m = v["masks"][0].clone();
    v["masks"].as_array_mut().unwrap().push(m);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    // Pattern bindings need the design records, an existing style, an existing row, an existing
    // pattern and are unique per row.
    let v = records();
    assert_eq!(
        run(&v, &doc, None).unwrap_err().code,
        Code::DanglingReference
    );
    let other = design(json!([]));
    let mut other_value = design_value(json!([]));
    other_value["paint_styles"][0]["style_id"] = json!(identity("SSTY", 2));
    let other_style: StudioDesignRecords = serde_json::from_value(other_value).unwrap();
    assert_eq!(
        run(&v, &doc, Some(&other_style)).unwrap_err().code,
        Code::DanglingReference
    );
    assert_eq!(run(&v, &doc, Some(&other)), Ok(()));
    assert_eq!(
        code(&with("/pattern_paints/0/row", json!(2))),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with("/pattern_paints/0/row", json!(u32::MAX))),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with("/pattern_paints/0/pattern_id", json!("tile-z"))),
        Code::DanglingReference
    );
    assert_eq!(
        code(&with(
            "/pattern_paints/0/style_id",
            json!(identity("SLYR", 1))
        )),
        Code::InvalidId
    );
    assert_eq!(
        code(&with("/pattern_paints/1/row", json!(0))),
        Code::DuplicateIdentity
    );
    // Records of another document, design records of another document.
    assert_eq!(
        code(&with("/document_id", json!(identity("SDOC", 2)))),
        Code::InvalidField
    );
    let mut foreign = design_value(json!([]));
    foreign["document_id"] = json!(identity("SDOC", 2));
    let foreign: StudioDesignRecords = serde_json::from_value(foreign).unwrap();
    assert_eq!(
        run(&records(), &doc, Some(&foreign)).unwrap_err().code,
        Code::InvalidField
    );
    // An empty set validates without design records.
    let empty = serde_json::to_value(StudioVectorRecords::new(identity("SDOC", 1))).unwrap();
    assert_eq!(run(&empty, &doc, None), Ok(()));
}

#[test]
fn reference_cycles_are_refused() {
    bounded_case("reference_cycles_are_refused", || {
        let target = |layer_n: u32, clip_source: u32| {
            json!({
                "schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
                "clips":[{"clip_id":"c","source_layer_id":identity("SLYR",clip_source),"clip_rule":"NONZERO"}],
                "attachments":[{"layer_id":identity("SLYR",layer_n),"clip_id":"c"}]})
        };
        let doc = document();
        // A layer clipped by its own geometry.
        let e = run(&target(3, 3), &doc, None).unwrap_err();
        assert_eq!((e.code, e.target), (Code::RenderCycle, identity("SLYR", 3)));
        // Layer 2 sits inside group 1; masking it with group 1 makes the group depend on itself.
        assert_eq!(
            run(&target(2, 1), &doc, None).unwrap_err().code,
            Code::RenderCycle
        );
        // The reverse direction is not a cycle: group 1 clipped by its own child 2.
        assert_eq!(run(&target(1, 2), &doc, None), Ok(()));
        // A pattern whose tile artwork (group 5, child 6) paints with the pattern.
        let looped = design(
            json!([{"node":{"node_kind":"layer","node_id":identity("SLYR",6)},
            "style_refs":{"fill_style_id":identity("SSTY",1)}}]),
        );
        let e = validate_with(&records(), &doc, &looped).unwrap_err();
        assert_eq!(e.code, Code::RenderCycle);
        // The same style on a layer outside the artwork (the base design) is fine.
        assert_eq!(validate_with(&records(), &doc, &base_design()), Ok(()));
        // A layer that is masked by a layer which is painted with a pattern whose artwork is the
        // masked layer's group: pattern -> group 5 -> child 6; mask source 7 is painted with the
        // pattern; attach the mask to layer 6.
        let nodes = design(
            json!([{"node":{"node_kind":"layer","node_id":identity("SLYR",7)},
            "style_refs":{"fill_style_id":identity("SSTY",1)}}]),
        );
        let mut v = records();
        v["attachments"] = json!([{"layer_id":identity("SLYR",6),"mask_id":"mask-a"}]);
        let e = validate_with(&v, &doc, &nodes).unwrap_err();
        assert_eq!(e.code, Code::RenderCycle);
    });
}
fn validate_with(
    v: &Value,
    d: &StudioDocument,
    design: &StudioDesignRecords,
) -> Result<(), Diagnostic> {
    run(v, d, Some(design))
}

/// `n` nested groups; clip `i` takes layer `i + 1` as geometry and is attached to layer `i`.
fn chain(n: u32, close: bool) -> (StudioDocument, Value) {
    let layers = (1..=n).map(|i| layer(i, "group", group(), None)).collect();
    let clips: Vec<Value> = (1..=n)
        .map(|i| {
            let source = if i == n { if close { 1 } else { n } } else { i + 1 };
            json!({"clip_id":format!("c{i}"),"source_layer_id":identity("SLYR",source),"clip_rule":"NONZERO"})
        })
        .collect();
    let attachments: Vec<Value> = (1..n + u32::from(close))
        .map(|i| json!({"layer_id":identity("SLYR",i),"clip_id":format!("c{i}")}))
        .collect();
    (
        document_of(layers),
        json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
            "clips":clips,"attachments":attachments}),
    )
}

#[test]
fn hostile_sizes_are_bounded_by_the_budget_and_do_not_recurse() {
    bounded_case(
        "hostile_sizes_are_bounded_by_the_budget_and_do_not_recurse",
        || {
            // A 1500-deep reference chain (1500 clips + 1499 attachments) neither overflows the stack
            // nor trips the default budget (4096), and closing the chain is a cycle.
            let (doc, open) = chain(1500, false);
            assert_eq!(run(&open, &doc, None), Ok(()));
            let (doc, closed) = chain(1500, true);
            assert_eq!(
                run(&closed, &doc, None).unwrap_err().code,
                Code::RenderCycle
            );
            // Aggregate over budget: the same chain with a node budget below the record count.
            let r: StudioVectorRecords = serde_json::from_value(open.clone()).unwrap();
            let (doc, _) = chain(1500, false);
            let tight = Budget {
                nodes: 2000,
                ..Budget::default()
            };
            let e = validate_vector_records(&r, &doc, None, tight, &CancellationToken::default());
            assert_eq!(e.unwrap_err().code, Code::BudgetExceeded);
            let exact = Budget {
                nodes: 1500 + 1499,
                ..Budget::default()
            };
            assert_eq!(
                validate_vector_records(&r, &doc, None, exact, &CancellationToken::default()),
                Ok(())
            );
            // A small budget refuses the full fixture on the first collection that exceeds it.
            let full: StudioVectorRecords = serde_json::from_value(records()).unwrap();
            let small = Budget {
                nodes: 2,
                ..Budget::default()
            };
            let e = validate_vector_records(
                &full,
                &document(),
                Some(&base_design()),
                small,
                &CancellationToken::default(),
            );
            assert_eq!(e.unwrap_err().code, Code::BudgetExceeded);
            // Cancellation is typed.
            let token = CancellationToken::default();
            token.cancel();
            let e = validate_vector_records(
                &full,
                &document(),
                Some(&base_design()),
                Budget::default(),
                &token,
            );
            assert_eq!(e.unwrap_err().code, Code::Canceled);
        },
    );
}
