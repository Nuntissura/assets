//! Lane SMB-ARTBOARD-EXPORT-1: the node export settings record (STU-IO-111 to STU-IO-114, STU-IO-117):
//! closed shapes tagged by `format`, the constraint bounds, the suffix collision rule at authoring time,
//! the bounds on hostile input, and the reference-like properties the package guard classifies.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const BOARD: &str = "SART-019abcde-0000-7000-8000-000000000003";

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .unwrap()
}

fn settings(v: Value) -> Vec<StudioNodeRenderSetting> {
    serde_json::from_value(v).unwrap()
}

fn layer() -> NodeRef {
    NodeRef { node_kind: NodeKind::Layer, node_id: LAYER.into() }
}

fn budget() -> Budget {
    Budget { input_bytes: MAX_EXPORT_SETTINGS_INPUT_BYTES, nodes: 1 << 20, payload_bytes: 1 << 22, ..Budget::default() }
}

/// vector-brand step 86 and product-design step 82: PNG at 1x, 2x and 3x, a sized PNG, SVG and PDF on
/// one node; the wire form round-trips, defaults fill what the caller leaves out, and the set reopens.
#[test]
fn a_recipe_set_round_trips_with_its_defaults() {
    let list = settings(json!([
        {"format": "PNG"},
        {"format": "PNG", "suffix": "@2x", "constraint": {"type": "SCALE", "value": 2}},
        {"format": "PNG", "suffix": "@3x", "constraint": {"type": "SCALE", "value": 3}},
        {"format": "JPG", "suffix": "-512", "constraint": {"type": "WIDTH", "value": 512}, "colour_profile": "SRGB"},
        {"format": "SVG", "outline_text": true},
        {"format": "PDF", "contents_only": false},
    ]));
    assert_eq!(list[0], StudioNodeRenderSetting::Png(StudioRasterImageSettings { contents_only: true, use_absolute_bounds: false, suffix: String::new(), constraint: StudioExportConstraint::default(), colour_profile: ExportColourProfile::Document }));
    assert_eq!(list[1].constraint(), Some(StudioExportConstraint { kind: ExportConstraintType::Scale, value: 2.0 }));
    assert_eq!((list[3].format(), list[3].extension(), list[3].colour_profile()), ("JPG", "jpg", ExportColourProfile::Srgb));
    assert_eq!(list[4].constraint(), None);
    assert!(!list[5].contents_only());
    let d = document();
    let set = StudioExportSettings::new(DOC).with_node(&layer(), list.clone());
    validate_export_settings(&set, &d, budget(), &token()).unwrap();
    let bytes = serde_json::to_vec(&set).unwrap();
    let back = parse_export_settings(&bytes, &d, budget(), &token()).unwrap();
    assert_eq!(back, set);
    assert_eq!(back.node(&layer()).unwrap().settings, list);
    // Replace keeps the place, an empty list removes the node, and the empty set has one form.
    let board = NodeRef { node_kind: NodeKind::Artboard, node_id: BOARD.into() };
    let two = set.with_node(&board, settings(json!([{"format": "SVG"}])));
    assert_eq!(two.nodes.len(), 2);
    let replaced = two.with_node(&layer(), settings(json!([{"format": "PDF"}])));
    assert_eq!(replaced.nodes[0].node, layer());
    assert_eq!(replaced.with_node(&layer(), vec![]).with_node(&board, vec![]), StudioExportSettings::new(DOC));
    assert_eq!(serde_json::to_value(StudioExportSettings::new(DOC)).unwrap(), json!({"schema_id": "hsk.studio.export_settings@1", "document_id": DOC}));
}

/// STU-IO-114: SCALE above 0 and at most 100; WIDTH and HEIGHT whole pixels 1..=100000; the output
/// size keeps the aspect ratio and is at least 1 pixel.
#[test]
fn the_constraint_bounds_and_sizes() {
    let c = |kind, value| StudioExportConstraint { kind, value };
    use ExportConstraintType::{Height, Scale, Width};
    for ok in [c(Scale, 0.01), c(Scale, 100.0), c(Width, 1.0), c(Height, 100_000.0)] {
        check_constraint(&ok).unwrap_or_else(|e| panic!("{ok:?}: {e:?}"));
    }
    for bad in [c(Scale, 0.0), c(Scale, -1.0), c(Scale, 100.5), c(Scale, f64::NAN), c(Scale, f64::INFINITY), c(Width, 0.0), c(Width, 16.5), c(Height, 100_001.0), c(Height, f64::NAN)] {
        assert_eq!(check_constraint(&bad).unwrap_err().target, "export_settings/constraint", "{bad:?}");
        assert_eq!(bad.output_size(10, 10), None, "{bad:?}");
    }
    assert_eq!(c(Scale, 2.0).output_size(200, 64), Some((400, 128)));
    assert_eq!(c(Scale, 0.25).output_size(3, 3), Some((1, 1)));
    assert_eq!(c(Width, 16.0).output_size(1024, 512), Some((16, 8)));
    assert_eq!(c(Height, 180.0).output_size(1024, 1024), Some((180, 180)));
    assert_eq!(c(Width, 1.0).output_size(1000, 1), Some((1, 1)), "never below 1");
    assert_eq!(c(Scale, 1.0).output_size(0, 5), None);
}

/// STU-IO-117 at authoring time, suffix grammar, closed shapes and hostile sizes: typed, never a panic.
#[test]
fn collisions_suffixes_shapes_and_bounds_are_refused() {
    let d = document();
    let set = |v: Value| StudioExportSettings::new(DOC).with_node(&layer(), settings(v));
    let refused = |s: &StudioExportSettings| validate_export_settings(s, &d, budget(), &token()).unwrap_err();
    // Same suffix and extension on one node collide; another extension or suffix does not.
    let clash = refused(&set(json!([{"format": "PNG", "suffix": "@2x"}, {"format": "PNG", "suffix": "@2x", "constraint": {"type": "SCALE", "value": 3}}])));
    assert_eq!((clash.code, clash.target.as_str()), (Code::DuplicateIdentity, "export_settings/suffix"));
    validate_export_settings(&set(json!([{"format": "PNG"}, {"format": "JPG"}, {"format": "SVG"}, {"format": "PNG", "suffix": "@2x"}])), &d, budget(), &token()).unwrap();
    // Review F5 (b): suffixes compare as the file system compares names, without case (NTFS), so
    // '@2x' and '@2X' name one file and cannot both be stored; a final sigma folds with sigma.
    for (a, b) in [("@2x", "@2X"), ("-σ", "-ς"), ("-Σ", "-ς")] {
        let folded = refused(&set(json!([{"format": "PNG", "suffix": a}, {"format": "PNG", "suffix": b}])));
        assert_eq!((folded.code, folded.target.as_str()), (Code::DuplicateIdentity, "export_settings/suffix"), "{a} {b}");
    }
    assert_eq!(file_name_fold("aς.PNG"), file_name_fold("Aσ.png"));
    assert_ne!(file_name_fold("straße"), file_name_fold("STRASSE"), "a one-to-many upper case is not folded (NTFS simple mapping)");
    for bad in ["a/b", "a\\b", "x:y", "q?", "\u{7}", "<", "|"] {
        assert_eq!(refused(&set(json!([{"format": "PNG", "suffix": bad}]))).target, "export_settings/suffix", "{bad:?}");
    }
    assert_eq!(refused(&set(json!([{"format": "PNG", "suffix": "x".repeat(MAX_EXPORT_SUFFIX_BYTES + 1)}]))).target, "export_settings/suffix");
    // Closed shapes: an unknown format, a field of another shape, a JSON_TREE (not admitted) are not records.
    for wire in [json!({"format": "GIF"}), json!({"format": "SVG", "constraint": {"type": "SCALE", "value": 2}}), json!({"format": "PNG", "outline_text": true}), json!({"format": "JSON_TREE"}), json!({"suffix": ""})] {
        assert!(serde_json::from_value::<StudioNodeRenderSetting>(wire.clone()).is_err(), "{wire}");
    }
    // A page spread is not an export node; a malformed id is typed; a node appears once.
    let spread = StudioExportSettings::new(DOC).with_node(&NodeRef { node_kind: NodeKind::PageSpread, node_id: "SPSP-019abcde-0000-7000-8000-000000000009".into() }, settings(json!([{"format": "PNG"}])));
    assert_eq!(refused(&spread).target, "export_settings/node_kind");
    let bad_id = StudioExportSettings::new(DOC).with_node(&NodeRef { node_kind: NodeKind::Layer, node_id: BOARD.into() }, settings(json!([{"format": "PNG"}])));
    assert_eq!(refused(&bad_id).code, Code::InvalidId);
    let mut twice = set(json!([{"format": "PNG"}]));
    twice.nodes.push(twice.nodes[0].clone());
    assert_eq!(refused(&twice).code, Code::DuplicateIdentity);
    // Empty lists, too many settings, too many nodes, another document, oversized wire bytes.
    let mut empty = set(json!([{"format": "PNG"}]));
    empty.nodes[0].settings.clear();
    assert_eq!(refused(&empty).target, "export_settings/settings");
    let many: Vec<Value> = (0..=MAX_SETTINGS_PER_NODE).map(|i| json!({"format": "PNG", "suffix": format!("-{i}")})).collect();
    assert_eq!(refused(&set(Value::Array(many))).code, Code::BudgetExceeded);
    let mut crowd = StudioExportSettings::new(DOC);
    crowd.nodes = (0..=MAX_EXPORT_NODES).map(|i| StudioNodeExportSettings { node: NodeRef { node_kind: NodeKind::Layer, node_id: format!("SLYR-019abcde-0000-7000-8000-{i:012x}") }, settings: settings(json!([{"format": "PNG"}])) }).collect();
    assert_eq!(refused(&crowd).code, Code::BudgetExceeded);
    let mut other = set(json!([{"format": "PNG"}]));
    other.document_id = "SDOC-019abcde-0000-7000-8000-0000000000ff".into();
    assert_eq!(refused(&other).target, "document_id");
    assert_eq!(parse_export_settings(&vec![b' '; MAX_EXPORT_SETTINGS_INPUT_BYTES + 1], &d, budget(), &token()).unwrap_err().code, Code::BudgetExceeded);
    assert_eq!(parse_export_settings(br#"{"schema_id":"hsk.studio.export_settings@1","document_id":"x","document_id":"y"}"#, &d, budget(), &token()).unwrap_err().code, Code::DuplicateField);
}

/// The reference-like property names of this schema (the package's guard vocabulary), as the package's
/// `externals.rs` classifies them: `colour_profile`, an output choice that names no input, and
/// `use_absolute_bounds`, a flag whose name holds `lut`. The shapes are inlined into the tagging enum.
#[test]
fn the_schema_names_colour_profile_as_its_only_reference_like_property() {
    let schema = serde_json::to_value(export_settings_schema()).unwrap();
    let vocab = ["digest", "artifact", "manifest", "asset", "media", "audio", "font", "lut", "profile", "path", "uri", "url", "file", "link", "source", "catalog", "image", "story", "object_id", "working_space", "blending_space", "external"];
    let mut found = std::collections::BTreeSet::new();
    let defs = schema.get("$defs").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut roots: Vec<(String, Value)> = vec![(schema["title"].as_str().unwrap_or("root").to_string(), schema.clone())];
    roots.extend(defs.into_iter());
    for (def, body) in roots {
        let mut stack = vec![body];
        while let Some(v) = stack.pop() {
            match v {
                Value::Object(map) => {
                    if let Some(Value::Object(props)) = map.get("properties") {
                        for p in props.keys().filter(|p| vocab.iter().any(|w| p.contains(w))) {
                            found.insert(format!("{def}.{p}"));
                        }
                    }
                    stack.extend(map.into_iter().filter(|(k, _)| k != "$defs").map(|(_, v)| v));
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    }
    let found: Vec<String> = found.into_iter().collect();
    assert_eq!(found, ["StudioNodeRenderSetting.colour_profile", "StudioNodeRenderSetting.use_absolute_bounds"], "{schema}");
}
