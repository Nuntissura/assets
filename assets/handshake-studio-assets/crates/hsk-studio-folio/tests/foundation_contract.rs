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
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}
fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
fn source(n: u8, ty: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":ty,"cardinality":"one","semantic":"source"}]})
}
fn composite(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
fn edge(source: Value, target: Value, n: u64) -> Value {
    json!({"source":source,"target":target,"dependency_kind":"data","input_ordinal":n})
}
fn mixed() -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":identity("SART",1),"name":"board","parent":null,"bounds":{"x":{"value":-1,"unit":"mm"},"y":{"value":0,"unit":"mm"},"width":{"value":100,"unit":"mm"},"height":{"value":50,"unit":"mm"}}}],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(5,"text",json!({"payload_kind":"text","story_id":identity("STXT",1)}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2,"image"),source(3,"image"),source(4,"vector"),source(5,"text"),composite(1)],"edges":[edge(address(3,"source","out"),address(1,"mix","inputs"),1),edge(address(2,"source","out"),address(1,"mix","inputs"),0)],"outputs":[address(1,"mix","out"),address(4,"source","out"),address(5,"source","out")]}
    })
}
struct CallerResolver(Resolution);
impl Resolver for CallerResolver {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        self.0
    }
    fn profile(&self, _: &str) -> Resolution {
        self.0
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        self.0
    }
    fn format_supported(&self, f: &str) -> bool {
        f == "caller-selected-format"
    }
    fn primitive_layer_binding(&self, k: &str, s: &str) -> bool {
        k == "adjustment" && s == "hsk.studio.adjustment@1"
    }
}
fn open(v: &Value) -> Snapshot {
    match inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        Budget::default(),
        &CancellationToken::default(),
        &CallerResolver(Resolution::Available),
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("known typed document became read-only"),
    }
}
fn rejection(v: &Value) -> Diagnostic {
    inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        Budget::default(),
        &CancellationToken::default(),
        &CallerResolver(Resolution::Available),
    )
    .unwrap_err()
}
#[test]
fn mixed_actual_roundtrip_order_and_snapshot_isolation() {
    bounded_case(
        "mixed_actual_roundtrip_order_and_snapshot_isolation",
        || {
            let input = serde_json::to_vec(&mixed()).unwrap();
            let original = open(&mixed());
            assert_eq!(original.encoded_bytes(), input);
            let target: PortAddress = serde_json::from_value(address(1, "mix", "inputs")).unwrap();
            let ordered = original.ordered_inputs(&target);
            assert_eq!(ordered[0].layer_id, identity("SLYR", 2));
            assert_eq!(ordered[1].layer_id, identity("SLYR", 3));
            let new = original
                .rename(
                    &identity("SLYR", 2),
                    "renamed".into(),
                    7,
                    8,
                    Budget::default(),
                    &CancellationToken::default(),
                    &CallerResolver(Resolution::Available),
                )
                .unwrap();
            assert_eq!(new.document().document_id, original.document().document_id);
            assert_eq!(new.document().revision, 8);
            assert_eq!(new.document().layers[1].name, "renamed");
            assert_eq!(original.document().layers[1].name, "layer-2");
            assert_eq!(original.encoded_bytes(), input);
            let decoded: StudioDocument = serde_json::from_slice(new.encoded_bytes()).unwrap();
            assert_eq!(&decoded, new.document());
        },
    );
}
#[test]
fn actual_schema_required_nullable_and_closed_domains() {
    bounded_case("actual_schema_required_nullable_and_closed_domains", || {
        let schema = serde_json::to_value(document_schema()).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        let required = schema["required"].as_array().unwrap();
        for name in [
            "working_space_rgb",
            "working_space_cmyk",
            "working_space_gray",
            "working_space_spot",
            "blending_space",
            "graph",
            "revision",
        ] {
            assert!(required.contains(&json!(name)), "missing {name}");
        }
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["bits_per_channel"]["enum"],
            json!([1, 8, 16, 32])
        );
        assert_eq!(
            schema["properties"]["colour_mode"]["minimum"].as_f64(),
            Some(1.0)
        );
        assert_eq!(
            schema["properties"]["colour_mode"]["maximum"].as_f64(),
            Some(8.0)
        );
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("ordered_composite"));
        assert!(text.contains("composite_result"));
        assert!(text.contains("null"));
        for name in [
            "working_space_rgb",
            "working_space_cmyk",
            "working_space_gray",
            "working_space_spot",
            "blending_space",
        ] {
            let mut v = mixed();
            v.as_object_mut().unwrap().remove(name);
            assert_eq!(rejection(&v).code, Code::MissingField);
        }
        let mut v = mixed();
        v["layers"][0].as_object_mut().unwrap().remove("parent");
        assert_eq!(rejection(&v).code, Code::MissingField);
    });
}
#[test]
fn duplicate_ids_and_dangling_references() {
    bounded_case("duplicate_ids_and_dangling_references", || {
        let mut v = mixed();
        v["layers"][2]["layer_id"] = v["layers"][1]["layer_id"].clone();
        assert_eq!(rejection(&v).code, Code::DuplicateIdentity);
        let mut v = mixed();
        v["graph"]["edges"][0]["source"]["layer_id"] = json!(identity("SLYR", 90));
        assert_eq!(rejection(&v).code, Code::DanglingReference);
        let mut v = mixed();
        v["layers"][1]["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",90)});
        assert_eq!(rejection(&v).code, Code::DanglingReference);
    });
}
#[test]
fn containment_cycles_and_illegal_group_parent() {
    bounded_case("containment_cycles_and_illegal_group_parent", || {
        let mut v = mixed();
        v["layers"][0]["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",1)});
        assert_eq!(rejection(&v).code, Code::ContainmentCycle);
        let mut v = mixed();
        v["layers"][0]["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",2)});
        assert_eq!(rejection(&v).code, Code::IllegalParent);
        let mut v = mixed();
        v["layers"][1]["parent"] = json!({"node_kind":"artboard","node_id":identity("SLYR",1)});
        assert_eq!(rejection(&v).code, Code::IllegalParent);
    });
}
#[test]
fn render_cycles_distinct_from_containment() {
    bounded_case("render_cycles_distinct_from_containment", || {
        let mut v = mixed();
        v["graph"]["edges"].as_array_mut().unwrap().push(edge(
            address(1, "mix", "out"),
            address(1, "mix", "inputs"),
            2,
        ));
        assert_eq!(rejection(&v).code, Code::RenderCycle);
    });
}
#[test]
fn illegal_mixed_units_and_colour_depth() {
    bounded_case("illegal_mixed_units_and_colour_depth", || {
        let mut v = mixed();
        v["artboards"][0]["bounds"]["width"]["unit"] = json!("px");
        assert_eq!(rejection(&v).code, Code::InvalidUnit);
        let mut v = mixed();
        v["artboards"][0]["bounds"]["height"]["value"] = json!(-1);
        assert_eq!(rejection(&v).code, Code::InvalidUnit);
        let mut v = mixed();
        v["bits_per_channel"] = json!(1);
        assert_eq!(rejection(&v).code, Code::InvalidColourDepth);
        for mode in [0, 9] {
            let mut v = mixed();
            v["colour_mode"] = json!(mode);
            assert_eq!(rejection(&v).code, Code::InvalidColourDepth);
        }
        let mut d = open(&mixed()).document().clone();
        d.artboards[0].bounds.width.value = f64::NAN;
        assert_eq!(
            validate_document(
                d,
                Budget::default(),
                &CancellationToken::default(),
                &Unresolved
            )
            .unwrap_err()
            .code,
            Code::InvalidUnit
        );
    });
}
#[test]
fn payload_type_port_and_order_cardinality_mismatch() {
    bounded_case("payload_type_port_and_order_cardinality_mismatch", || {
        let mut v = mixed();
        v["layers"][1]["payload"] = json!({"payload_kind":"group"});
        assert_eq!(rejection(&v).code, Code::PayloadMismatch);
        let mut v = mixed();
        v["graph"]["edges"][0]["source"] = address(4, "source", "out");
        assert_eq!(rejection(&v).code, Code::InvalidEdge);
        let mut v = mixed();
        v["graph"]["operations"][0]["outputs"][0]["value_type"] = json!("vector");
        assert_eq!(rejection(&v).code, Code::InvalidPort);
        let mut v = mixed();
        v["graph"]["edges"][0]["input_ordinal"] = json!(3);
        assert_eq!(rejection(&v).code, Code::Cardinality);
        let mut v = mixed();
        v["graph"]["edges"][0]["input_ordinal"] = json!(0);
        assert_eq!(rejection(&v).code, Code::InvalidEdge);
        let mut v = mixed();
        v["graph"]["operations"][4]["inputs"][0]["cardinality"] = json!("one");
        assert_eq!(rejection(&v).code, Code::InvalidPort);
    });
}
#[test]
fn revision_stale_overflow_successor_reject_original_unchanged() {
    bounded_case(
        "revision_stale_overflow_successor_reject_original_unchanged",
        || {
            let s = open(&mixed());
            let bytes = s.encoded_bytes().to_vec();
            let r = CallerResolver(Resolution::Available);
            let t = CancellationToken::default();
            assert_eq!(
                s.rename(
                    &identity("SLYR", 2),
                    "x".into(),
                    6,
                    7,
                    Budget::default(),
                    &t,
                    &r
                )
                .unwrap_err()
                .code,
                Code::StaleRevision
            );
            assert_eq!(
                s.rename(
                    &identity("SLYR", 2),
                    "x".into(),
                    7,
                    9,
                    Budget::default(),
                    &t,
                    &r
                )
                .unwrap_err()
                .code,
                Code::InvalidSuccessor
            );
            assert_eq!(s.encoded_bytes(), bytes);
            let mut v = mixed();
            v["revision"] = json!(u64::MAX);
            let s = open(&v);
            let bytes = s.encoded_bytes().to_vec();
            assert_eq!(
                s.rename(
                    &identity("SLYR", 2),
                    "x".into(),
                    u64::MAX,
                    0,
                    Budget::default(),
                    &t,
                    &r
                )
                .unwrap_err()
                .code,
                Code::RevisionOverflow
            );
            assert_eq!(s.encoded_bytes(), bytes);
        },
    );
}
#[test]
fn cancellation_and_each_budget_reject_without_mutation() {
    bounded_case(
        "cancellation_and_each_budget_reject_without_mutation",
        || {
            let s = open(&mixed());
            let bytes = s.encoded_bytes().to_vec();
            let r = CallerResolver(Resolution::Available);
            let t = CancellationToken::default();
            t.cancel();
            assert_eq!(
                s.rename(
                    &identity("SLYR", 2),
                    "x".into(),
                    7,
                    8,
                    Budget::default(),
                    &t,
                    &r
                )
                .unwrap_err()
                .code,
                Code::Canceled
            );
            for budget in [
                Budget {
                    nodes: 0,
                    ..Budget::default()
                },
                Budget {
                    edges: 0,
                    ..Budget::default()
                },
                Budget {
                    payload_bytes: 0,
                    ..Budget::default()
                },
                Budget {
                    input_bytes: 1,
                    ..Budget::default()
                },
            ] {
                assert_eq!(
                    s.rename(
                        &identity("SLYR", 2),
                        "x".into(),
                        7,
                        8,
                        budget,
                        &CancellationToken::default(),
                        &r
                    )
                    .unwrap_err()
                    .code,
                    Code::BudgetExceeded
                );
                assert_eq!(s.encoded_bytes(), bytes);
            }
            assert_eq!(
                inspect_bytes(
                    &bytes,
                    Budget {
                        depth: 1,
                        ..Budget::default()
                    },
                    &CancellationToken::default(),
                    &r
                )
                .unwrap_err()
                .code,
                Code::BudgetExceeded
            );
        },
    );
}
#[test]
fn unsupported_richer_payload_retained_and_dependent_mutation_refused() {
    bounded_case(
        "unsupported_richer_payload_retained_and_dependent_mutation_refused",
        || {
            let mut v = mixed();
            let encoded = vec![0, 255, 17, 32, 10];
            v["layers"].as_array_mut().unwrap().push(layer(9,"motion_camera",json!({"payload_kind":"unsupported","kind":"motion_camera","schema_id":"hsk.studio.motion_timeline@1","encoded_bytes":encoded,"reason":"typed owner unavailable"})));
            let s = open(&v);
            let bytes = s.encoded_bytes().to_vec();
            match &s.document().layers[5].payload {
                Payload::Unsupported { encoded_bytes, .. } => assert_eq!(encoded_bytes, &encoded),
                _ => panic!("lost opaque payload"),
            };
            assert!(s.resolution_issues().iter().any(
                |i| i.target == identity("SLYR", 9) && i.disposition == Resolution::Unsupported
            ));
            assert_eq!(
                s.rename(
                    &identity("SLYR", 9),
                    "x".into(),
                    7,
                    8,
                    Budget::default(),
                    &CancellationToken::default(),
                    &Unresolved
                )
                .unwrap_err()
                .code,
                Code::UnsupportedMutation
            );
            assert_eq!(s.encoded_bytes(), bytes);
            let new = s
                .rename(
                    &identity("SLYR", 2),
                    "x".into(),
                    7,
                    8,
                    Budget::default(),
                    &CancellationToken::default(),
                    &CallerResolver(Resolution::Available),
                )
                .unwrap();
            assert_eq!(
                new.document().layers[5].payload,
                s.document().layers[5].payload
            );
        },
    );
}
#[test]
fn complete_unknown_fields_and_schema_retained_read_only() {
    bounded_case(
        "complete_unknown_fields_and_schema_retained_read_only",
        || {
            let mut cases = Vec::new();
            let mut v = mixed();
            v["future_root"] = json!({"secret":[1,2,3]});
            cases.push(v);
            let mut v = mixed();
            v["layers"][0]["rich_transform"] = json!([2, 3, 4]);
            cases.push(v);
            let mut v = mixed();
            v["schema_id"] = json!("hsk.studio.document@2");
            cases.push(v);
            let mut v = mixed();
            v["graph"]["schema_id"] = json!("hsk.studio.layer_graph@2");
            cases.push(v);
            let mut v = mixed();
            v["graph"]["operations"][0]["operation_kind"] = json!("future_source");
            cases.push(v);
            for v in cases {
                let bytes = serde_json::to_vec_pretty(&v).unwrap();
                match inspect_bytes(
                    &bytes,
                    Budget::default(),
                    &CancellationToken::default(),
                    &Unresolved,
                )
                .unwrap()
                {
                    Inspection::ReadOnly(r) => {
                        assert_eq!(r.encoded_bytes(), bytes);
                        assert_eq!(
                            r.refuse_mutation().unwrap_err().code,
                            Code::UnsupportedMutation
                        );
                    }
                    _ => panic!("unsupported input became editable"),
                }
            }
        },
    );
}
#[test]
fn duplicate_json_fields_reject_instead_of_overwrite() {
    bounded_case("duplicate_json_fields_reject_instead_of_overwrite", || {
        let input = serde_json::to_string(&mixed()).unwrap();
        let input = input.replacen('{', "{\"revision\":99,", 1);
        assert_eq!(
            inspect_bytes(
                input.as_bytes(),
                Budget::default(),
                &CancellationToken::default(),
                &Unresolved
            )
            .unwrap_err()
            .code,
            Code::DuplicateField
        );
    });
}
fn tile() -> Value {
    json!({"object_key":"tile-0","layer_id":identity("SLYR",2),"column":-1,"row":0,"width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"caller-selected-format","colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"caller-algorithm","digest":"caller-digest"},"artifact_manifest_id":"caller-owned-existing-handle"})
}
#[test]
fn typed_resolver_dispositions_and_no_default_profile() {
    bounded_case("typed_resolver_dispositions_and_no_default_profile", || {
        let mut v = mixed();
        v["working_space_rgb"] = json!(identity("SCPF", 1));
        v["layers"][1]["payload"]["tiles"] = json!([tile()]);
        let bytes = serde_json::to_vec(&v).unwrap();
        for disposition in [
            Resolution::Absent,
            Resolution::HashMismatch,
            Resolution::Unauthorized,
            Resolution::Unsupported,
        ] {
            let r = CallerResolver(disposition);
            let s =
                match inspect_bytes(&bytes, Budget::default(), &CancellationToken::default(), &r)
                    .unwrap()
                {
                    Inspection::Editable(s) => s,
                    _ => panic!("structurally valid input was lost"),
                };
            assert!(
                s.resolution_issues()
                    .iter()
                    .any(|i| i.disposition == disposition)
            );
            assert_eq!(s.require_available().unwrap_err().code, Code::Unavailable);
        }
        assert_eq!(
            open(&mixed()).require_available().unwrap_err().target,
            "unassigned_profile"
        );
    });
}
#[test]
fn tile_geometry_owner_identity_and_primitive_binding() {
    bounded_case("tile_geometry_owner_identity_and_primitive_binding", || {
        let mut v = mixed();
        v["layers"][1]["payload"]["tiles"] = json!([tile()]);
        v["layers"][1]["payload"]["tiles"][0]["width"]["value"] = json!(0);
        assert_eq!(rejection(&v).code, Code::InvalidUnit);
        let mut v = mixed();
        v["layers"][1]["payload"]["tiles"] = json!([tile()]);
        v["layers"][1]["payload"]["tiles"][0]["layer_id"] = json!(identity("SLYR", 3));
        assert_eq!(rejection(&v).code, Code::PayloadMismatch);
        let mut v = mixed();
        v["layers"].as_array_mut().unwrap().push(layer(10,"adjustment",json!({"payload_kind":"primitive_reference","schema_id":"hsk.studio.adjustment@1","object_id":"caller-owned-PRIM-handle"})));
        assert_eq!(open(&v).document().layers.len(), 6);
        let mut v = mixed();
        v["layers"][3]["payload"]["path_ids"] = json!([identity("STXT", 1)]);
        assert_eq!(rejection(&v).code, Code::InvalidId);
    });
}
#[test]
fn descriptor_exposes_actual_consumer_recovery_and_pending_host() {
    bounded_case(
        "descriptor_exposes_actual_consumer_recovery_and_pending_host",
        || {
            let d: Value = serde_json::from_str(DESCRIPTOR).unwrap();
            assert_eq!(d["owner"], "STUDIO-MODULE-FOLIO");
            assert!(d["consumer"].as_str().unwrap().contains("--schema"));
            assert!(d["authority"].as_str().unwrap().contains("pending"));
            assert!(d["diagnostics"].as_str().unwrap().contains("caller"));
        },
    );
}
