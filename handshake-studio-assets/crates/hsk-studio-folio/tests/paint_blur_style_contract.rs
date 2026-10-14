//! SMB-FOLIO-PAINT-BLUR-STYLE: blur effect kind, image paint binding, live boolean groups, type-style
//! records and the image-typed text source (lead decision G1). Each new kind survives the wire
//! encoding byte-identically (the package stores `document.json` and record entries verbatim, so
//! wire identity is package identity) and every refusal code is derived from the rule named next to
//! it.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn slyr(n: u32) -> String {
    identity("SLYR", n)
}
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
/// Encode, decode, encode again: equal value and identical bytes (17-digit floats included).
fn wire_identical<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> T {
    let first = serde_json::to_vec(v).expect("encode");
    let back: T = serde_json::from_slice(&first).expect("decode");
    assert_eq!(&back, v);
    assert_eq!(serde_json::to_vec(&back).expect("encode again"), first);
    back
}

const GROUP: u32 = 1;
const RASTER: u32 = 2;
const TEXT: u32 = 3;
const VEC_A: u32 = 4;
const VEC_B: u32 = 5;
const INNER: u32 = 6;
const NESTED: u32 = 7;

fn layer(n: u32, kind: &str, payload: Value, parent: Option<u32>) -> Value {
    let parent = parent.map_or(Value::Null, |p| json!({"node_kind":"layer","node_id":slyr(p)}));
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),
        "parent":parent,"kind":kind,"payload":payload,"visible":true})
}
fn vector() -> Value {
    json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})
}
fn source_op(n: u32, value_type: &str) -> Value {
    json!({"layer_id":slyr(n),"operation_key":"source","operation_kind":"source","inputs":[],
        "outputs":[{"port_key":"out","value_type":value_type,"cardinality":"one","semantic":"source"}]})
}
fn edge(n: u32, ordinal: u64) -> Value {
    json!({"source":{"layer_id":slyr(n),"operation_key":"source","port_key":"out"},
        "target":{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"inputs"},
        "dependency_kind":"data","input_ordinal":ordinal})
}
/// Group 1 holding raster 2, text 3, vectors 4 and 5 and group 6 (with vector 7 inside). The graph
/// composites raster 2 (bottom) and text 3 (top, `text_value_type`) inside group 1.
fn document_json(text_value_type: &str) -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            layer(GROUP, "group", json!({"payload_kind":"group"}), None),
            layer(RASTER, "raster", json!({"payload_kind":"raster","tiles":[]}), Some(GROUP)),
            layer(TEXT, "text", json!({"payload_kind":"text","story_id":identity("STXT",1)}), Some(GROUP)),
            layer(VEC_A, "vector", vector(), Some(GROUP)),
            layer(VEC_B, "vector", vector(), Some(GROUP)),
            layer(INNER, "group", json!({"payload_kind":"group"}), Some(GROUP)),
            layer(NESTED, "vector", vector(), Some(INNER)),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                source_op(RASTER, "image"), source_op(TEXT, text_value_type),
                {"layer_id":slyr(GROUP),"operation_key":"composite","operation_kind":"composite",
                 "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
                 "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[edge(RASTER,0), edge(TEXT,1)],
            "outputs":[{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"out"}]}
    })
}
fn document() -> StudioDocument {
    serde_json::from_value(document_json("image")).expect("document shape")
}
fn validate_doc(v: &Value) -> Result<Snapshot, Diagnostic> {
    let d: StudioDocument = serde_json::from_value(v.clone()).expect("document shape");
    validate_document(d, Budget::default(), &CancellationToken::default(), &Caller)
}

/// G1: text authored over a raster layer inside a group validates when the text source is
/// image-typed; the text-typed source cannot feed the image `inputs` (equal value types,
/// InvalidEdge); a text-typed text source outside such an edge validates as before.
#[test]
fn text_over_raster_in_group_validates() {
    let snapshot = validate_doc(&document_json("image")).expect("text over raster validates");
    let stack: Vec<String> = snapshot
        .ordered_inputs(&PortAddress {
            layer_id: slyr(GROUP),
            operation_key: "composite".into(),
            port_key: "inputs".into(),
        })
        .into_iter()
        .map(|a| a.layer_id.clone())
        .collect();
    assert_eq!(stack, vec![slyr(RASTER), slyr(TEXT)]);
    let reopened: StudioDocument = serde_json::from_slice(snapshot.encoded_bytes()).unwrap();
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), snapshot.encoded_bytes());

    let text_typed = document_json("text");
    let refused = validate_doc(&text_typed).expect_err("text-typed edge into image inputs");
    assert_eq!(refused.code, Code::InvalidEdge);
    let mut unlinked = text_typed;
    unlinked["graph"]["edges"] = json!([edge(RASTER, 0)]);
    assert!(validate_doc(&unlinked).is_ok(), "existing text-typed source form");
    // Only text sources gain the image form: a vector source declared image stays InvalidPort.
    let mut vector_image = document_json("image");
    vector_image["graph"]["operations"][0] = source_op(VEC_A, "image");
    vector_image["graph"]["edges"] = json!([edge(TEXT, 0)]);
    assert_eq!(validate_doc(&vector_image).unwrap_err().code, Code::InvalidPort);
}

fn blur(id: &str, scope: &str, radius: f64) -> Value {
    json!({"instance_id":id,"enabled":true,"visible_in_editor":true,"blend_mode":2,"opacity":100.0,
        "anti_alias":false,"noise":0.0,"angle":0.0,"use_global_light":false,
        "params":{"effect_kind":"blur","render_scope":scope,"radius":radius}})
}
fn raster_records(effects: Value) -> Value {
    json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":slyr(VEC_A),"effects":{"effect_stack_id":identity("SEFX",1),
            "enabled":true,"scale":100.0,"effects":effects}}]})
}
fn check_raster(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioRasterRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_raster_records(
        &r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}

/// Blur kind: layer and backdrop scope, radius `0..=1000` px (D19 (f) size bound), no colour or
/// contour (a blur has neither).
#[test]
fn blur_effect_round_trips_and_is_bounded() {
    let v = raster_records(json!([
        blur("fg", "layer", 12.345678901234567),
        blur("bg", "below", 1000.0)
    ]));
    assert_eq!(check_raster(&v), Ok(vec![]));
    let r: StudioRasterRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    let stack = back.layer(&slyr(VEC_A)).unwrap().effects.as_ref().unwrap();
    assert_eq!(stack.effects[0].params.key(), "blur");
    assert!(matches!(
        stack.effects[1].params,
        StudioEffectKind::Blur { render_scope: BlurScope::Below, radius } if radius == 1000.0
    ));
    assert_eq!(serde_json::to_value(&back).unwrap(), v);

    for radius in [1000.5, -1.0] {
        let bad = raster_records(json!([blur("fg", "layer", radius)]));
        assert_eq!(check_raster(&bad).unwrap_err().code, Code::InvalidField, "{radius}");
    }
    let mut coloured = blur("fg", "layer", 4.0);
    coloured["colour"] = json!({"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null});
    assert_eq!(
        check_raster(&raster_records(json!([coloured]))).unwrap_err(),
        Diagnostic {
            code: Code::InvalidField,
            target: format!("{}/fg/blur/colour_contour", slyr(VEC_A))
        }
    );
    let mut unknown_scope = raster_records(json!([blur("fg", "group", 4.0)]));
    unknown_scope["layers"][0]["effects"]["effects"][0]["params"]["render_scope"] = json!("group");
    assert!(serde_json::from_value::<StudioRasterRecords>(unknown_scope).is_err());
}

fn digest(c: char) -> Value {
    json!({"algorithm":"sha256","digest":c.to_string().repeat(64)})
}
fn design() -> StudioDesignRecords {
    let row = json!({"row_kind":"fill","paint":{"paint_kind":"solid",
        "colour":{"r":0.5,"g":0.5,"b":0.5,"colour_profile_id":null}},
        "opacity":100.0,"blend_mode":2,"visible":true});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),
            "kind":"PAINT","name":"brand/photo",
            "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"photo","is_remote":false},
            "rows":[row.clone(), row]}]}))
    .expect("design shape")
}
fn image_paint(row: u32, image: Value) -> Value {
    json!({"style_id":identity("SSTY",1),"row":row,"image":image,"scale_mode":"CROP",
        "transform":[0.5,0.0,0.0,0.5,0.12345678901234568,0.25],"scaling_factor":1.0,
        "rotation":90.0,"opacity":80.0})
}
fn vector_records() -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "image_paints":[
            image_paint(0, json!({"source_kind":"package_asset","content_digest":digest('a')})),
            image_paint(1, json!({"source_kind":"import_original","entry_digest":digest('b'),
                "part":"images/0f3e","content_digest":digest('c')}))],
        "boolean_groups":[{"layer_id":slyr(GROUP),"operation":"SUBTRACT",
            "operands":[slyr(VEC_A), slyr(VEC_B)],"fill_geometry_path_ids":[identity("SVPT",9)]}]})
}
fn check_vector(v: &Value) -> Result<(), Diagnostic> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_vector_records(
        &r,
        &document(),
        Some(&design()),
        Budget::default(),
        &CancellationToken::default(),
    )
}
fn vector_with(path: &str, value: Value) -> Value {
    let mut v = vector_records();
    *v.pointer_mut(path).expect("pointer") = value;
    v
}

/// Image paint: row binding like the pattern paint (row must exist, one pattern-or-image binding per
/// row), content-addressed source (sha256, safe part name), invertible transform, positive scale,
/// percent opacity.
#[test]
fn image_paint_round_trips_and_refusals() {
    let v = vector_records();
    assert_eq!(check_vector(&v), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
    let crop = back.image_paint(&identity("SSTY", 1), 1).expect("row 1 bound");
    assert_eq!(crop.scale_mode, StudioImageScale::Crop);
    assert!(matches!(&crop.image, StudioImageSource::ImportOriginal { part, .. } if part == "images/0f3e"));
    // A record set without the new members encodes exactly as before (no new keys).
    let empty = StudioVectorRecords::new(identity("SDOC", 1));
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)})
    );

    let invalid = [
        ("/image_paints/0/image/content_digest/digest", json!("A".repeat(64))),
        ("/image_paints/0/image/content_digest/algorithm", json!("md5")),
        ("/image_paints/1/image/part", json!("images/../secret")),
        ("/image_paints/1/image/part", json!("C:/x.png")),
        ("/image_paints/1/image/part", json!("images\\x")),
        ("/image_paints/0/transform", json!([1.0, 2.0, 2.0, 4.0, 0.0, 0.0])),
        ("/image_paints/0/scaling_factor", json!(0.0)),
        ("/image_paints/0/opacity", json!(100.5)),
    ];
    for (path, value) in invalid {
        let bad = vector_with(path, value.clone());
        assert_eq!(check_vector(&bad).unwrap_err().code, Code::InvalidField, "{path} {value}");
    }
    let out_of_range = vector_with("/image_paints/1/row", json!(2));
    assert_eq!(check_vector(&out_of_range).unwrap_err().code, Code::DanglingReference);
    let twice = vector_with("/image_paints/1/row", json!(0));
    assert_eq!(check_vector(&twice).unwrap_err().code, Code::DuplicateIdentity);
    // A row bound to a pattern cannot also carry an image.
    let mut both = vector_records();
    both["patterns"] = json!([{"pattern_id":"p","name":"p","tile_type":"grid",
        "horizontal_spacing":{"value":0.0,"unit":"px"},"vertical_spacing":{"value":0.0,"unit":"px"},
        "source":{"pattern_model":"live_source","source_layer_id":slyr(NESTED),"scaling_factor":1.0,
            "horizontal_alignment":"start"}}]);
    both["pattern_paints"] = json!([{"style_id":identity("SSTY",1),"row":0,"pattern_id":"p",
        "placement":{"placement_kind":"matrix","matrix":[1.0,0.0,0.0,1.0,0.0,0.0]}}]);
    assert_eq!(check_vector(&both).unwrap_err().code, Code::DuplicateIdentity);
    let mut no_design = vector_records();
    no_design["boolean_groups"] = json!([]);
    let r: StudioVectorRecords = serde_json::from_value(no_design).unwrap();
    let d = document();
    let refused = validate_vector_records(&r, &d, None, Budget::default(), &CancellationToken::default());
    assert_eq!(refused.unwrap_err().code, Code::DanglingReference);
}

/// Live boolean group: the host is a `group` layer, operands are distinct direct children (at least
/// one), the cached outline is `SVPT` ids, one record per group.
#[test]
fn boolean_group_round_trips_and_refusals() {
    let r: StudioVectorRecords = serde_json::from_value(vector_records()).unwrap();
    let g = r.boolean_group(&slyr(GROUP)).expect("group record");
    assert_eq!(g.operation, StudioBooleanOperation::Subtract);
    assert_eq!(
        serde_json::to_value(StudioBooleanOperation::Exclude).unwrap(),
        json!("EXCLUDE")
    );
    let refusals = [
        ("/boolean_groups/0/layer_id", json!(slyr(VEC_A)), Code::PayloadMismatch),
        ("/boolean_groups/0/layer_id", json!(slyr(99)), Code::DanglingReference),
        ("/boolean_groups/0/operands", json!([]), Code::InvalidField),
        ("/boolean_groups/0/operands", json!([slyr(VEC_A), slyr(NESTED)]), Code::IllegalParent),
        ("/boolean_groups/0/operands", json!([slyr(VEC_A), slyr(VEC_A)]), Code::DuplicateIdentity),
        ("/boolean_groups/0/fill_geometry_path_ids", json!([identity("SLYR", 9)]), Code::InvalidId),
    ];
    for (path, value, code) in refusals {
        let bad = vector_with(path, value.clone());
        assert_eq!(check_vector(&bad).unwrap_err().code, code, "{path} {value}");
    }
    let mut twice = vector_records();
    let first = twice["boolean_groups"][0].clone();
    twice["boolean_groups"].as_array_mut().unwrap().push(first);
    assert_eq!(check_vector(&twice).unwrap_err().code, Code::DuplicateIdentity);
    // The cached outline is optional and not encoded when empty.
    let mut uncached = vector_records();
    uncached["boolean_groups"][0]
        .as_object_mut()
        .unwrap()
        .remove("fill_geometry_path_ids");
    assert_eq!(check_vector(&uncached), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(uncached.clone()).unwrap();
    assert_eq!(serde_json::to_value(wire_identical(&r)).unwrap(), uncached);
}

fn stys(n: u32) -> String {
    identity("STYS", n)
}
fn type_styles() -> Value {
    json!({"schema_id":"hsk.studio.type_style_records@1","document_id":identity("SDOC",1),
        "styles":[
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(1),"scope":"paragraph","name":"body/base",
             "font_family":"Inter","font_style":"Regular","font_size":{"value":16.0,"unit":"px"},
             "line_height":{"line_height_kind":"percent","value":140.0},"align":"left","next_style":stys(2)},
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(2),"scope":"paragraph","name":"body/lead",
             "based_on":stys(1),"font_size":{"value":20.333333333333332,"unit":"px"},
             "line_height":{"line_height_kind":"auto"}},
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(3),"scope":"character","name":"emphasis",
             "font_style":"Bold Italic","fill":{"r":1.0,"g":0.2,"b":0.0,"a":0.5,"colour_profile_id":null},
             "letter_spacing":{"spacing_kind":"length","length":{"value":-0.5,"unit":"px"}}}]})
}
fn check_styles(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioTypeStyleRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_type_style_records(
        &r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}
fn story(paragraph_style: &str, range_style: &str) -> StudioTextStory {
    serde_json::from_value(json!({"schema_id":"hsk.studio.text_story@1","story_id":identity("STXT",1),
        "text_kind":"area","story_direction":"left_to_right",
        "content":{"content_kind":"inline","text_utf8":"Hi there"},
        "paragraphs":[{"start":0,"end":8,"style_ref":paragraph_style,"paragraph_direction":null}],
        "character_ranges":[
            {"start":0,"end":2,"style_ref":range_style,"applied_language":null},
            {"start":2,"end":8,"style_ref":stys(77),"applied_language":null}]}))
    .expect("story shape")
}

/// Type styles: `STYS` records a story resolves through `TypeStyleResolver`; scope rules
/// (STU-TYP-200), `based_on` / `next_style` references and cycles (STU-TYP-201/203).
#[test]
fn type_styles_resolve_stories_and_refuse() {
    let v = type_styles();
    assert_eq!(check_styles(&v), Ok(vec![]));
    let r: StudioTypeStyleRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    assert_eq!(serde_json::to_value(&back).unwrap(), v);

    // Story styles held by the set resolve; the one id it does not hold (77) goes to the caller.
    let s = story(&stys(1), &stys(3));
    let issues = validate_text_story(
        &s,
        Budget::default(),
        &CancellationToken::default(),
        &TypeStyleResolver { styles: &back, inner: &Unresolved },
    )
    .expect("story validates");
    assert_eq!(
        issues,
        vec![ResolutionIssue {
            target: format!("{}/style/{}", identity("STXT", 1), stys(77)),
            disposition: Resolution::Absent
        }]
    );
    assert_eq!(check_story_style_scopes(&s, &back), Ok(()));
    let swapped = story(&stys(3), &stys(1));
    assert_eq!(
        check_story_style_scopes(&swapped, &back).unwrap_err().code,
        Code::PayloadMismatch
    );

    let mut cycle = type_styles();
    cycle["styles"][0]["based_on"] = json!(stys(2));
    assert_eq!(check_styles(&cycle).unwrap_err().code, Code::RenderCycle);
    let mut cross_scope = type_styles();
    cross_scope["styles"][2]["based_on"] = json!(stys(1));
    assert_eq!(check_styles(&cross_scope).unwrap_err().code, Code::PayloadMismatch);
    let mut dangling = type_styles();
    dangling["styles"][1]["based_on"] = json!(stys(9));
    assert_eq!(check_styles(&dangling).unwrap_err().code, Code::DanglingReference);
    let mut duplicate = type_styles();
    duplicate["styles"][1]["style_id"] = json!(stys(1));
    duplicate["styles"][1].as_object_mut().unwrap().remove("based_on");
    assert_eq!(check_styles(&duplicate).unwrap_err().code, Code::DuplicateIdentity);
    let invalid = [
        ("/styles/2/align", json!("center")),
        ("/styles/0/font_size", json!({"value":0.0,"unit":"px"})),
        ("/styles/0/line_height", json!({"line_height_kind":"percent","value":-5.0})),
        ("/styles/2/fill/a", json!(1.5)),
        ("/styles/0/font_family", json!("")),
        ("/styles/0/name", json!("body//base")),
    ];
    for (path, value) in invalid {
        let mut bad = type_styles();
        let (parent, key) = path.rsplit_once('/').expect("path");
        bad.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("parent object")
            .insert(key.to_owned(), value.clone());
        assert_eq!(check_styles(&bad).unwrap_err().code, Code::InvalidField, "{path} {value}");
    }
    let mut wrong_prefix = type_styles();
    wrong_prefix["styles"][0]["style_id"] = json!(identity("SSTY", 1));
    assert_eq!(check_styles(&wrong_prefix).unwrap_err().code, Code::InvalidId);
}
