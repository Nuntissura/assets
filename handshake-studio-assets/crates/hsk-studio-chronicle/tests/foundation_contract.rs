use hsk_studio_accord::CancellationToken;
use hsk_studio_chronicle::*;
use hsk_studio_folio::{Inspection, NodeKind, NodeRef, Snapshot, Unresolved};
use hsk_studio_observe::{DeliveryClass, DeliveryError, Outcome, SinkPort};
use serde_json::{Value, json};
fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (s, r) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = s.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match r.recv_timeout(Duration::from_secs(30)) {
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
fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn doc() -> Value {
    json!({"schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],"layers":[{"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",1),"name":"original-private-name","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}})
}
fn snapshot() -> Snapshot {
    match hsk_studio_folio::inspect_bytes(
        &serde_json::to_vec(&doc()).unwrap(),
        Budget::default().folio,
        &CancellationToken::default(),
        &Unresolved,
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        _ => panic!("known doc became unsupported"),
    }
}
fn actor() -> Actor {
    Actor {
        account_id: "account".into(),
        principal_id: "principal".into(),
        owner_account_id: "owner".into(),
        owner_principal_id: "owner-principal".into(),
        access_space_id: "space".into(),
        session_id: "session".into(),
    }
}
fn address(property: Property) -> PropertyAddress {
    PropertyAddress {
        target: NodeRef {
            node_kind: NodeKind::Layer,
            node_id: id("SLYR", 1),
        },
        property,
    }
}
fn vector() -> Vec<RevisionEntry> {
    vec![
        RevisionEntry {
            address: address(Property::Name),
            revision: 0,
        },
        RevisionEntry {
            address: address(Property::Visible),
            revision: 0,
        },
    ]
}
fn patch(property: Property, value: PropertyValue) -> Patch {
    let expected = match property {
        Property::Name => PropertyValue::String("original-private-name".into()),
        Property::Visible => PropertyValue::Bool(true),
        // Layer 1 is a group: identity opacity 100, pass_through (1).
        Property::Opacity => PropertyValue::Number(100.into()),
        Property::BlendMode => PropertyValue::Number(1.into()),
        Property::Tiles => PropertyValue::Tiles(TileList(Vec::new())),
    };
    Patch {
        transport_version: 1,
        document_id: id("SDOC", 1),
        base_revision: 7,
        command_id: "first".into(),
        correlation_id: 1,
        actor: actor(),
        reads: vec![PropertyRead {
            address: address(property),
            expected_revision: 0,
            expected_value: expected,
        }],
        writes: vec![PropertyWrite {
            address: address(property),
            value,
        }],
    }
}
fn run(p: &Patch, s: &Snapshot, v: &[RevisionEntry]) -> std::result::Result<Prepared, Diagnostic> {
    prepare(
        p,
        s,
        v,
        Budget::default(),
        &CancellationToken::default(),
        &Unresolved,
    )
}
fn advance(v: &mut [RevisionEntry], p: &Prepared) {
    for u in p.revision_updates() {
        let entry = v.iter_mut().find(|e| e.address == u.address).unwrap();
        assert_eq!(entry.revision, u.previous_revision);
        entry.revision = u.revision;
    }
}
#[test]
fn actual_apply_inverse_and_typed_wire_roundtrip() {
    bounded_case("actual_apply_inverse_and_typed_wire_roundtrip", || {
        let original = snapshot();
        let bytes = original.encoded_bytes().to_vec();
        let mut v = vector();
        let p = patch(Property::Name, PropertyValue::String("changed".into()));
        let decoded = decode_patch(
            &serde_json::to_vec(&p).unwrap(),
            Budget::default(),
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(decoded, p);
        let result = run(&p, &original, &v).unwrap();
        let successor = result.successor().unwrap();
        assert_eq!(successor.document().layers[0].name, "changed");
        assert_eq!(successor.document().revision, 8);
        assert_eq!(result.revision_updates().len(), 1);
        advance(&mut v, &result);
        let inverse = result
            .inverse()
            .unwrap()
            .invocation("inverse".into(), 2, actor(), 8);
        let restored = run(&inverse, successor, &v).unwrap();
        let mut expected = original.document().clone();
        expected.revision = 9;
        assert_eq!(restored.successor().unwrap().document(), &expected);
        assert_eq!(original.encoded_bytes(), bytes);
        let encoded = serde_json::to_vec(result.wire()).unwrap();
        let decoded: SourceResult = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(&decoded, result.wire());
    });
}
#[test]
fn disjoint_same_node_oldbase_proposals_and_inverse_keep_foreign_property() {
    bounded_case(
        "disjoint_same_node_oldbase_proposals_and_inverse_keep_foreign_property",
        || {
            let base = snapshot();
            let mut v = vector();
            let a = patch(Property::Name, PropertyValue::String("a".into()));
            let b = patch(Property::Visible, PropertyValue::Bool(false));
            assert_eq!(a.base_revision, b.base_revision);
            let first = run(&a, &base, &v).unwrap();
            advance(&mut v, &first);
            assert_ne!(
                b.base_revision,
                first.successor().unwrap().document().revision
            );
            let second = run(&b, first.successor().unwrap(), &v).unwrap();
            advance(&mut v, &second);
            assert_eq!(second.successor().unwrap().document().revision, 9);
            assert_eq!(second.successor().unwrap().document().layers[0].name, "a");
            assert!(!second.successor().unwrap().document().layers[0].visible);
            let inverse = first
                .inverse()
                .unwrap()
                .invocation("undo-a".into(), 3, actor(), 9);
            let undo = run(&inverse, second.successor().unwrap(), &v).unwrap();
            assert_eq!(
                undo.successor().unwrap().document().layers[0].name,
                "original-private-name"
            );
            assert!(!undo.successor().unwrap().document().layers[0].visible);
            assert_eq!(undo.successor().unwrap().document().revision, 10);
            assert_eq!(undo.revision_updates()[0].previous_revision, 1);
            assert_eq!(undo.revision_updates()[0].revision, 2);
            assert_eq!(v[1].revision, 1);
        },
    );
}
#[test]
fn overlapping_and_read_dependent_conflicts_name_dimension_not_private_text() {
    bounded_case(
        "overlapping_and_read_dependent_conflicts_name_dimension_not_private_text",
        || {
            let base = snapshot();
            let mut v = vector();
            let a = patch(Property::Name, PropertyValue::String("private-next".into()));
            let first = run(&a, &base, &v).unwrap();
            advance(&mut v, &first);
            let stale = run(&a, first.successor().unwrap(), &v).unwrap_err();
            assert_eq!(stale.code, Code::RevisionConflict);
            assert_eq!(stale.address, Some(address(Property::Name)));
            assert!(!format!("{stale:?}").contains("private-next"));
            let mut dependent = patch(Property::Visible, PropertyValue::Bool(false));
            dependent.reads.push(a.reads[0].clone());
            assert_eq!(
                run(&dependent, first.successor().unwrap(), &v)
                    .unwrap_err()
                    .code,
                Code::RevisionConflict
            );
            let mut mismatch = a.clone();
            mismatch.reads[0].expected_revision = 1;
            assert_eq!(
                run(&mismatch, first.successor().unwrap(), &v)
                    .unwrap_err()
                    .code,
                Code::ValueConflict
            );
        },
    );
}
#[test]
fn aba_restored_value_still_conflicts_with_old_revision() {
    bounded_case(
        "aba_restored_value_still_conflicts_with_old_revision",
        || {
            let original = snapshot();
            let mut v = vector();
            let p = patch(Property::Name, PropertyValue::String("changed".into()));
            let changed = run(&p, &original, &v).unwrap();
            advance(&mut v, &changed);
            let inverse = changed
                .inverse()
                .unwrap()
                .invocation("undo".into(), 2, actor(), 8);
            let restored = run(&inverse, changed.successor().unwrap(), &v).unwrap();
            advance(&mut v, &restored);
            assert_eq!(
                restored.successor().unwrap().document().layers[0].name,
                original.document().layers[0].name
            );
            assert_eq!(v[0].revision, 2);
            assert_eq!(
                run(&p, restored.successor().unwrap(), &v).unwrap_err().code,
                Code::RevisionConflict
            );
        },
    );
}
#[test]
fn inverse_retains_semantic_extra_reads_and_refuses_foreign_dependency() {
    bounded_case(
        "inverse_retains_semantic_extra_reads_and_refuses_foreign_dependency",
        || {
            let base = snapshot();
            let mut v = vector();
            let mut a = patch(Property::Name, PropertyValue::String("a".into()));
            a.reads.push(PropertyRead {
                address: address(Property::Visible),
                expected_revision: 0,
                expected_value: PropertyValue::Bool(true),
            });
            let first = run(&a, &base, &v).unwrap();
            assert_eq!(first.inverse().unwrap().reads.len(), 2);
            advance(&mut v, &first);
            let b = patch(Property::Visible, PropertyValue::Bool(false));
            let foreign = run(&b, first.successor().unwrap(), &v).unwrap();
            advance(&mut v, &foreign);
            let inverse = first
                .inverse()
                .unwrap()
                .invocation("undo".into(), 3, actor(), 9);
            let bytes = foreign.successor().unwrap().encoded_bytes().to_vec();
            let oldv = v.clone();
            let failure = run(&inverse, foreign.successor().unwrap(), &v).unwrap_err();
            assert_eq!(failure.code, Code::RevisionConflict);
            assert_eq!(failure.address, Some(address(Property::Visible)));
            assert_eq!(foreign.successor().unwrap().encoded_bytes(), bytes);
            assert_eq!(v, oldv);
        },
    );
}
#[test]
fn missing_duplicate_vectors_and_missing_read_coverage() {
    bounded_case(
        "missing_duplicate_vectors_and_missing_read_coverage",
        || {
            let s = snapshot();
            let p = patch(Property::Name, PropertyValue::String("x".into()));
            let mut v = vector();
            v.remove(0);
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::RevisionUnavailable);
            let mut v = vector();
            v.push(v[0].clone());
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::DuplicateAddress);
            let mut p = p.clone();
            p.reads.clear();
            assert_eq!(run(&p, &s, &vector()).unwrap_err().code, Code::MissingRead);
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.reads.push(p.reads[0].clone());
            assert_eq!(
                run(&p, &s, &vector()).unwrap_err().code,
                Code::DuplicateAddress
            );
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.writes.push(p.writes[0].clone());
            assert_eq!(
                run(&p, &s, &vector()).unwrap_err().code,
                Code::DuplicateAddress
            );
        },
    );
}
#[test]
fn wrong_document_target_type_and_atomic_no_partial_writes() {
    bounded_case(
        "wrong_document_target_type_and_atomic_no_partial_writes",
        || {
            let s = snapshot();
            let bytes = s.encoded_bytes().to_vec();
            let v = vector();
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.document_id = id("SDOC", 2);
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::WrongDocument);
            let mut p = patch(Property::Name, PropertyValue::Bool(false));
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::WrongType);
            p = patch(Property::Name, PropertyValue::String("x".into()));
            let mut bad = p.reads[0].clone();
            bad.address.target.node_id = id("SLYR", 99);
            p.reads.push(bad.clone());
            p.writes.push(PropertyWrite {
                address: bad.address,
                value: PropertyValue::String("y".into()),
            });
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::InvalidTarget);
            assert_eq!(s.encoded_bytes(), bytes);
            assert_eq!(v, vector());
            let mut p = patch(Property::Visible, PropertyValue::Bool(false));
            p.reads[0].address.target.node_kind = NodeKind::Artboard;
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::InvalidTarget);
        },
    );
}
#[test]
fn property_and_global_overflow_leave_original_unchanged() {
    bounded_case(
        "property_and_global_overflow_leave_original_unchanged",
        || {
            let s = snapshot();
            let bytes = s.encoded_bytes().to_vec();
            let mut v = vector();
            v[0].revision = u64::MAX;
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.reads[0].expected_revision = u64::MAX;
            assert_eq!(run(&p, &s, &v).unwrap_err().code, Code::RevisionOverflow);
            assert_eq!(s.encoded_bytes(), bytes);
            let mut d = s.document().clone();
            d.revision = u64::MAX;
            let max = hsk_studio_folio::validate_document(
                d,
                Budget::default().folio,
                &CancellationToken::default(),
                &Unresolved,
            )
            .unwrap();
            assert_eq!(
                run(
                    &patch(Property::Name, PropertyValue::String("x".into())),
                    &max,
                    &vector()
                )
                .unwrap_err()
                .code,
                Code::RevisionOverflow
            );
        },
    );
}
#[test]
fn no_change_metadata_and_unchanged_writes_omitted_from_inverse_updates() {
    bounded_case(
        "no_change_metadata_and_unchanged_writes_omitted_from_inverse_updates",
        || {
            let s = snapshot();
            let p = patch(Property::Visible, PropertyValue::Bool(true));
            let result = run(&p, &s, &vector()).unwrap();
            let encoded = serde_json::to_value(result.wire()).unwrap();
            assert_eq!(encoded["disposition"], "no_change");
            for absent in ["successor", "revision_updates", "inverse"] {
                assert!(encoded.get(absent).is_none());
            }
            assert!(result.successor().is_none());
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.reads.push(PropertyRead {
                address: address(Property::Visible),
                expected_revision: 0,
                expected_value: PropertyValue::Bool(true),
            });
            p.writes.push(PropertyWrite {
                address: address(Property::Visible),
                value: PropertyValue::Bool(true),
            });
            let result = run(&p, &s, &vector()).unwrap();
            assert_eq!(result.revision_updates().len(), 1);
            assert_eq!(result.inverse().unwrap().writes.len(), 1);
            assert_eq!(result.inverse().unwrap().reads.len(), 2);
        },
    );
}
#[test]
fn strict_real_decoder_unknown_missing_null_versions_and_duplicate_fields() {
    bounded_case(
        "strict_real_decoder_unknown_missing_null_versions_and_duplicate_fields",
        || {
            let p = patch(Property::Name, PropertyValue::String("x".into()));
            let original = serde_json::to_value(&p).unwrap();
            let t = CancellationToken::default();
            let decode = |v: &Value| {
                decode_patch(&serde_json::to_vec(v).unwrap(), Budget::default(), &t)
                    .unwrap_err()
                    .code
            };
            let mut v = original.clone();
            v["transport_version"] = json!(2);
            assert_eq!(decode(&v), Code::UnsupportedVersion);
            let mut v = original.clone();
            v["unknown"] = json!(true);
            assert_eq!(decode(&v), Code::InvalidInput);
            for key in ["actor", "reads", "writes", "command_id"] {
                let mut v = original.clone();
                v.as_object_mut().unwrap().remove(key);
                assert_eq!(decode(&v), Code::InvalidInput);
            }
            let mut v = original.clone();
            v["actor"] = Value::Null;
            assert_eq!(decode(&v), Code::InvalidInput);
            let mut v = original.clone();
            v["writes"][0]["address"]["property"] = json!("arbitrary/path");
            assert_eq!(decode(&v), Code::InvalidInput);
            let text =
                serde_json::to_string(&p)
                    .unwrap()
                    .replacen('{', "{\"transport_version\":1,", 1);
            assert_eq!(
                decode_patch(text.as_bytes(), Budget::default(), &t)
                    .unwrap_err()
                    .code,
                Code::DuplicateField
            );
            assert_eq!(
                decode_patch(b"{", Budget::default(), &t).unwrap_err().code,
                Code::InvalidInput
            );
        },
    );
}
#[test]
fn bounded_counts_values_decode_and_folio_cancel_preserve_inputs() {
    bounded_case(
        "bounded_counts_values_decode_and_folio_cancel_preserve_inputs",
        || {
            let s = snapshot();
            let bytes = s.encoded_bytes().to_vec();
            let v = vector();
            let oldv = v.clone();
            let p = patch(Property::Name, PropertyValue::String("x".into()));
            let input = serde_json::to_vec(&p).unwrap();
            let token = CancellationToken::default();
            token.cancel();
            assert_eq!(
                prepare(&p, &s, &v, Budget::default(), &token, &Unresolved)
                    .unwrap_err()
                    .code,
                Code::Canceled
            );
            for b in [
                Budget {
                    reads: 0,
                    ..Budget::default()
                },
                Budget {
                    writes: 0,
                    ..Budget::default()
                },
                Budget {
                    value_bytes: 0,
                    ..Budget::default()
                },
                Budget {
                    revisions: 0,
                    ..Budget::default()
                },
            ] {
                assert_eq!(
                    prepare(&p, &s, &v, b, &CancellationToken::default(), &Unresolved)
                        .unwrap_err()
                        .code,
                    Code::BudgetExceeded
                );
            }
            for b in [
                Budget {
                    reads: 0,
                    ..Budget::default()
                },
                Budget {
                    writes: 0,
                    ..Budget::default()
                },
                Budget {
                    value_bytes: 0,
                    ..Budget::default()
                },
                Budget {
                    input_bytes: 0,
                    ..Budget::default()
                },
            ] {
                assert_eq!(
                    decode_patch(&input, b, &CancellationToken::default())
                        .unwrap_err()
                        .code,
                    Code::BudgetExceeded
                );
            }
            let b = Budget {
                folio: hsk_studio_folio::Budget {
                    nodes: 0,
                    ..Budget::default().folio
                },
                ..Budget::default()
            };
            assert_eq!(
                prepare(&p, &s, &v, b, &CancellationToken::default(), &Unresolved)
                    .unwrap_err()
                    .code,
                Code::BudgetExceeded
            );
            assert_eq!(s.encoded_bytes(), bytes);
            assert_eq!(v, oldv);
        },
    );
}
struct Capture {
    failure: Option<DeliveryError>,
    frames: Vec<Vec<u8>>,
}
impl SinkPort for Capture {
    fn try_send(&mut self, _: DeliveryClass, b: &[u8]) -> std::result::Result<(), DeliveryError> {
        if let Some(e) = self.failure {
            return Err(e);
        }
        if !self.frames.is_empty() {
            return Err(DeliveryError::Saturated);
        }
        self.frames.push(b.to_vec());
        Ok(())
    }
}
#[test]
fn observe_delivery_failure_explicit_and_postacceptance_cancel_retains_result() {
    bounded_case(
        "observe_delivery_failure_explicit_and_postacceptance_cancel_retains_result",
        || {
            let s = snapshot();
            let p = patch(
                Property::Name,
                PropertyValue::String("never-telemetry-private".into()),
            );
            let result = run(&p, &s, &vector()).unwrap();
            let accepted = result.successor().unwrap().clone();
            let bytes = accepted.encoded_bytes().to_vec();
            for failure in [
                DeliveryError::Rejected,
                DeliveryError::Unavailable,
                DeliveryError::Indeterminate,
            ] {
                let mut sink = Capture {
                    failure: Some(failure),
                    frames: vec![],
                };
                let report = deliver(
                    &p,
                    8,
                    Outcome::Success,
                    &mut sink,
                    &CancellationToken::default(),
                )
                .unwrap();
                assert!(report.receipt.is_err());
                assert_eq!(report.state.acknowledged, 0);
                assert_eq!(accepted.encoded_bytes(), bytes);
            }
            let token = CancellationToken::default();
            token.cancel();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            let report = deliver(&p, 8, Outcome::Success, &mut sink, &token).unwrap();
            assert_eq!(report.receipt.unwrap().outcome, Outcome::Canceled);
            assert_eq!(
                accepted.document().layers[0].name,
                "never-telemetry-private"
            );
            assert_eq!(accepted.encoded_bytes(), bytes);
            assert_eq!(sink.frames.len(), 1);
            assert!(
                !sink.frames[0]
                    .windows(b"never-telemetry-private".len())
                    .any(|w| w == b"never-telemetry-private")
            );
        },
    );
}
#[test]
fn generated_schema_descriptor_and_unrelated_vector_entry_preserved() {
    bounded_case(
        "generated_schema_descriptor_and_unrelated_vector_entry_preserved",
        || {
            let schema = serde_json::to_value(patch_schema()).unwrap();
            assert_eq!(
                schema["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
            assert_eq!(schema["properties"]["transport_version"]["const"], 1);
            assert_eq!(schema["additionalProperties"], false);
            let required = schema["required"].as_array().unwrap();
            for field in ["actor", "reads", "writes", "base_revision"] {
                assert!(required.contains(&json!(field)));
            }
            let result = serde_json::to_string(&result_schema()).unwrap();
            assert!(result.contains("no_change"));
            assert!(result.contains("hsk.studio.document@1"));
            let d: Value = serde_json::from_str(DESCRIPTOR).unwrap();
            assert!(d["scope"].as_str().unwrap().contains("source-local"));
            let mut v = vector();
            let unrelated = RevisionEntry {
                address: PropertyAddress {
                    target: NodeRef {
                        node_kind: NodeKind::Layer,
                        node_id: id("SLYR", 99),
                    },
                    property: Property::Name,
                },
                revision: 37,
            };
            v.push(unrelated.clone());
            let s = snapshot();
            let prepared = run(
                &patch(Property::Name, PropertyValue::String("x".into())),
                &s,
                &v,
            )
            .unwrap();
            advance(&mut v, &prepared);
            assert_eq!(v[2], unrelated);
        },
    );
}

#[test]
fn cancellation_during_prospective_folio_validation_preserves_every_input() {
    bounded_case(
        "cancellation_during_prospective_folio_validation_preserves_every_input",
        || {
            struct CancelResolver(CancellationToken);
            impl hsk_studio_folio::Resolver for CancelResolver {
                fn primitive(&self, _: &str, _: &str) -> hsk_studio_folio::Resolution {
                    hsk_studio_folio::Resolution::Absent
                }
                fn profile(&self, _: &str) -> hsk_studio_folio::Resolution {
                    self.0.cancel();
                    hsk_studio_folio::Resolution::Available
                }
                fn tile(&self, _: &hsk_studio_folio::TileRef) -> hsk_studio_folio::Resolution {
                    hsk_studio_folio::Resolution::Absent
                }
                fn format_supported(&self, _: &str) -> bool {
                    false
                }
                fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
                    false
                }
            }
            let mut d = snapshot().document().clone();
            d.working_space_rgb = hsk_studio_folio::ProfileBinding::Assigned(id("SCPF", 1));
            let s = hsk_studio_folio::validate_document(
                d,
                Budget::default().folio,
                &CancellationToken::default(),
                &Unresolved,
            )
            .unwrap();
            let bytes = s.encoded_bytes().to_vec();
            let vector = vector();
            let original_vector = vector.clone();
            let token = CancellationToken::default();
            let p = patch(Property::Name, PropertyValue::String("x".into()));
            assert_eq!(
                prepare(
                    &p,
                    &s,
                    &vector,
                    Budget::default(),
                    &token,
                    &CancelResolver(token.clone())
                )
                .unwrap_err()
                .code,
                Code::Canceled
            );
            assert_eq!(s.encoded_bytes(), bytes);
            assert_eq!(vector, original_vector);
        },
    );
}
#[test]
fn unsupported_target_payload_and_invalid_actor_command_refused() {
    bounded_case(
        "unsupported_target_payload_and_invalid_actor_command_refused",
        || {
            let mut d = snapshot().document().clone();
            d.layers[0].kind = "motion_camera".into();
            d.layers[0].payload = hsk_studio_folio::Payload::Unsupported {
                kind: "motion_camera".into(),
                schema_id: "hsk.studio.motion_timeline@1".into(),
                encoded_bytes: vec![0, 255, 1],
                reason: "owner unavailable".into(),
            };
            let s = hsk_studio_folio::validate_document(
                d,
                Budget::default().folio,
                &CancellationToken::default(),
                &Unresolved,
            )
            .unwrap();
            let bytes = s.encoded_bytes().to_vec();
            let p = patch(Property::Name, PropertyValue::String("x".into()));
            assert_eq!(
                run(&p, &s, &vector()).unwrap_err().code,
                Code::InvalidTarget
            );
            assert_eq!(s.encoded_bytes(), bytes);
            let mut p = p.clone();
            p.actor.session_id.clear();
            assert_eq!(
                run(&p, &snapshot(), &vector()).unwrap_err().code,
                Code::InvalidActor
            );
            let mut p = patch(Property::Name, PropertyValue::String("x".into()));
            p.command_id = "control\n".into();
            assert_eq!(
                run(&p, &snapshot(), &vector()).unwrap_err().code,
                Code::InvalidCommand
            );
        },
    );
}

#[test]
fn borrowed_folio_budget_preflight_and_prospective_output_cap() {
    bounded_case(
        "borrowed_folio_budget_preflight_and_prospective_output_cap",
        || {
            let s = snapshot();
            let original = s.encoded_bytes().to_vec();
            let vector = vector();
            for folio in [
                hsk_studio_folio::Budget {
                    depth: 1,
                    ..Budget::default().folio
                },
                hsk_studio_folio::Budget {
                    nodes: 0,
                    ..Budget::default().folio
                },
            ] {
                let b = Budget {
                    folio,
                    ..Budget::default()
                };
                assert_eq!(
                    prepare(
                        &patch(Property::Name, PropertyValue::String("x".into())),
                        &s,
                        &vector,
                        b,
                        &CancellationToken::default(),
                        &Unresolved
                    )
                    .unwrap_err()
                    .code,
                    Code::BudgetExceeded
                );
            }
            let b = Budget {
                folio: hsk_studio_folio::Budget {
                    input_bytes: s.encoded_bytes().len(),
                    ..Budget::default().folio
                },
                ..Budget::default()
            };
            assert_eq!(
                prepare(
                    &patch(Property::Name, PropertyValue::String("x".into())),
                    &s,
                    &vector,
                    Budget {
                        input_bytes: 0,
                        ..Budget::default()
                    },
                    &CancellationToken::default(),
                    &Unresolved
                )
                .unwrap_err()
                .code,
                Code::BudgetExceeded
            );
            let p = patch(Property::Name, PropertyValue::String("\"".repeat(1024)));
            assert_eq!(
                prepare(
                    &p,
                    &s,
                    &vector,
                    b,
                    &CancellationToken::default(),
                    &Unresolved
                )
                .unwrap_err()
                .code,
                Code::BudgetExceeded
            );
            assert_eq!(s.encoded_bytes(), original);
        },
    );
}

// Independent reader of the documented Observe v2 trailer; no producer encode helper.
type DetailFields = (u8, u8, [u32; 5], Option<(u8, u8, String)>);
fn detail_fields(frame: &[u8]) -> DetailFields {
    use std::io::{Cursor, Read};
    assert!(frame.len() <= 2048);
    assert_eq!(&frame[..4], b"HSKO");
    assert_eq!(frame[4], 2);
    let mut reader = Cursor::new(frame);
    reader.set_position(48);
    for _ in 0..7 {
        let mut len = [0; 2];
        reader.read_exact(&mut len).unwrap();
        reader.set_position(reader.position() + u16::from_be_bytes(len) as u64);
    }
    let mut tags = [0; 3];
    reader.read_exact(&mut tags).unwrap();
    let mut counts = [0; 5];
    for count in &mut counts {
        let mut b = [0; 4];
        reader.read_exact(&mut b).unwrap();
        *count = u32::from_be_bytes(b);
    }
    let address = if tags[2] == 1 {
        let mut t = [0; 2];
        reader.read_exact(&mut t).unwrap();
        let mut len = [0; 2];
        reader.read_exact(&mut len).unwrap();
        let mut id = vec![0; u16::from_be_bytes(len) as usize];
        reader.read_exact(&mut id).unwrap();
        Some((t[0], t[1], String::from_utf8(id).unwrap()))
    } else {
        assert_eq!(tags[2], 0);
        None
    };
    assert_eq!(reader.position() as usize, frame.len());
    (tags[0], tags[1], counts, address)
}
#[test]
fn actual_detail_changed_nochange_inverse_conflict_and_privacy() {
    bounded_case(
        "actual_detail_changed_nochange_inverse_conflict_and_privacy",
        || {
            let s = snapshot();
            let v = vector();
            let p = patch(
                Property::Name,
                PropertyValue::String("private-new-value-never-emitted".into()),
            );
            let changed = run(&p, &s, &v).unwrap();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(
                &p,
                8,
                Ok(&changed),
                Budget::default(),
                &mut sink,
                &CancellationToken::default(),
            )
            .unwrap()
            .receipt
            .unwrap();
            assert_eq!(
                detail_fields(&sink.frames[0]),
                (1, 0, [1, 1, 1, 1, 1], None)
            );
            let next = changed.successor().unwrap();
            let mut next_v = v.clone();
            next_v[0].revision = 1;
            let conflict = run(&p, next, &next_v).unwrap_err();
            assert_eq!(conflict.code, Code::RevisionConflict);
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(
                &p,
                8,
                Err(&conflict),
                Budget::default(),
                &mut sink,
                &CancellationToken::default(),
            )
            .unwrap()
            .receipt
            .unwrap();
            assert_eq!(
                detail_fields(&sink.frames[0]),
                (3, 12, [1, 1, 0, 0, 0], Some((1, 1, id("SLYR", 1))))
            );
            for secret in [
                "private-new-value-never-emitted",
                "original-private-name",
                p.command_id.as_str(),
            ] {
                assert!(
                    !sink.frames[0]
                        .windows(secret.len())
                        .any(|w| w == secret.as_bytes())
                );
            }
            let nochange_p = patch(Property::Visible, PropertyValue::Bool(true));
            let nochange = run(&nochange_p, &s, &v).unwrap();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(
                &nochange_p,
                7,
                Ok(&nochange),
                Budget::default(),
                &mut sink,
                &CancellationToken::default(),
            )
            .unwrap()
            .receipt
            .unwrap();
            assert_eq!(
                detail_fields(&sink.frames[0]),
                (2, 0, [1, 1, 1, 1, 0], None)
            );
            let inverse = changed.inverse().unwrap().invocation(
                "private-inverse-command".into(),
                99,
                actor(),
                8,
            );
            let restored = run(&inverse, next, &next_v).unwrap();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(
                &inverse,
                9,
                Ok(&restored),
                Budget::default(),
                &mut sink,
                &CancellationToken::default(),
            )
            .unwrap()
            .receipt
            .unwrap();
            assert_eq!(
                detail_fields(&sink.frames[0]),
                (1, 0, [1, 1, 1, 1, 1], None)
            );
            assert!(
                !sink.frames[0]
                    .windows(b"private-inverse-command".len())
                    .any(|w| w == b"private-inverse-command")
            );
        },
    );
}
#[test]
fn detail_failures_cancel_and_invalid_attribution_preserve_source() {
    bounded_case(
        "detail_failures_cancel_and_invalid_attribution_preserve_source",
        || {
            let s = snapshot();
            let p = patch(Property::Name, PropertyValue::String("private".into()));
            let bytes = s.encoded_bytes().to_vec();
            let v = vector();
            let changed = run(&p, &s, &v).unwrap();
            let accepted = changed.successor().unwrap().encoded_bytes().to_vec();
            for forced in [
                DeliveryError::Rejected,
                DeliveryError::Unavailable,
                DeliveryError::Indeterminate,
            ] {
                let mut sink = Capture {
                    failure: Some(forced),
                    frames: vec![],
                };
                let report = deliver_result(
                    &p,
                    8,
                    Ok(&changed),
                    Budget::default(),
                    &mut sink,
                    &CancellationToken::default(),
                )
                .unwrap();
                assert!(report.receipt.is_err());
                assert_eq!(changed.successor().unwrap().encoded_bytes(), accepted);
            }
            let token = CancellationToken::default();
            token.cancel();
            let error = prepare(&p, &s, &v, Budget::default(), &token, &Unresolved).unwrap_err();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(&p, 7, Err(&error), Budget::default(), &mut sink, &token)
                .unwrap()
                .receipt
                .unwrap();
            assert_eq!(sink.frames[0][5], 4);
            assert_eq!(
                detail_fields(&sink.frames[0]),
                (3, 16, [1, 1, 0, 0, 0], None)
            );
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            deliver_result(&p, 8, Ok(&changed), Budget::default(), &mut sink, &token)
                .unwrap()
                .receipt
                .unwrap();
            assert_eq!(sink.frames[0][5], 4);
            assert_eq!(detail_fields(&sink.frames[0]).0, 1);
            let mut invalid = p.clone();
            invalid.actor.account_id = "bad\nactor".into();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            assert_eq!(
                deliver_result(
                    &invalid,
                    7,
                    Err(&error),
                    Budget::default(),
                    &mut sink,
                    &token
                )
                .unwrap_err()
                .code,
                Code::InvalidActor
            );
            assert!(sink.frames.is_empty());
            assert_eq!(s.encoded_bytes(), bytes);
            assert_eq!(v, vector());
        },
    );
}

#[test]
fn detail_actual_value_budget_and_unavailable_rejections() {
    bounded_case(
        "detail_actual_value_budget_and_unavailable_rejections",
        || {
            let s = snapshot();
            let v = vector();
            let p = patch(
                Property::Name,
                PropertyValue::String("private-value".into()),
            );
            let mut wrong = p.clone();
            wrong.reads[0].expected_value = PropertyValue::String("private-wrong-read".into());
            let value_error = run(&wrong, &s, &v).unwrap_err();
            let unavailable = run(&p, &s, &[]).unwrap_err();
            let budget = Budget {
                reads: 0,
                ..Budget::default()
            };
            let budget_error = prepare(
                &p,
                &s,
                &v,
                budget,
                &CancellationToken::default(),
                &Unresolved,
            )
            .unwrap_err();
            for (patch, error, budget, wire_code) in [
                (&wrong, &value_error, Budget::default(), 13),
                (&p, &unavailable, Budget::default(), 11),
                (&p, &budget_error, budget, 15),
            ] {
                let mut sink = Capture {
                    failure: None,
                    frames: vec![],
                };
                deliver_result(
                    patch,
                    7,
                    Err(error),
                    budget,
                    &mut sink,
                    &CancellationToken::default(),
                )
                .unwrap()
                .receipt
                .unwrap();
                let fields = detail_fields(&sink.frames[0]);
                assert_eq!(
                    (fields.0, fields.1, fields.2),
                    (3, wire_code, [1, 1, 0, 0, 0])
                );
                assert!(
                    !sink.frames[0]
                        .windows(b"private-wrong-read".len())
                        .any(|w| w == b"private-wrong-read")
                );
            }
            let mut invalid = p.clone();
            invalid.document_id = "SDOC-private-raw".into();
            let mut sink = Capture {
                failure: None,
                frames: vec![],
            };
            assert_eq!(
                deliver_result(
                    &invalid,
                    7,
                    Err(&value_error),
                    Budget::default(),
                    &mut sink,
                    &CancellationToken::default()
                )
                .unwrap_err()
                .code,
                Code::WrongDocument
            );
            assert!(sink.frames.is_empty());
        },
    );
}

/// A document with every node kind chronicle can address plus a layer it cannot:
/// layer 1 (group), artboard 1, page spread 1, and layer 3 with an unsupported payload.
fn table_snapshot() -> Snapshot {
    let bounds = json!({"x":{"value":0,"unit":"mm"},"y":{"value":0,"unit":"mm"},"width":{"value":100,"unit":"mm"},"height":{"value":50,"unit":"mm"}});
    let mut d = doc();
    d["artboards"] = json!([{"schema_id":"hsk.studio.artboard@1","artboard_id":id("SART",1),"name":"board","parent":null,"bounds":bounds.clone()}]);
    d["page_spreads"] = json!([{"schema_id":"hsk.studio.page_spread@1","page_spread_id":id("SPGS",1),"name":"spread","parent":null,"bounds":bounds}]);
    d["layers"].as_array_mut().unwrap().push(json!({
        "schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",3),"name":"opaque","parent":null,
        "kind":"vendor-thing","visible":true,
        "payload":{"payload_kind":"unsupported","kind":"vendor-thing","schema_id":"vendor.thing@1","encoded_bytes":[1,2,3],"reason":"preserved"}
    }));
    match hsk_studio_folio::inspect_bytes(
        &serde_json::to_vec(&d).unwrap(),
        Budget::default().folio,
        &CancellationToken::default(),
        &Unresolved,
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        _ => panic!("table fixture became read-only"),
    }
}
fn node(kind: NodeKind, node_id: String, property: Property) -> PropertyAddress {
    PropertyAddress {
        target: NodeRef {
            node_kind: kind,
            node_id,
        },
        property,
    }
}
#[test]
fn exported_table_is_the_table_prepare_enforces() {
    bounded_case("exported_table_is_the_table_prepare_enforces", || {
        // A new Property variant fails to compile here, so PROPERTIES cannot silently lag the enum.
        fn index(p: Property) -> usize {
            match p {
                Property::Name => 0,
                Property::Visible => 1,
                Property::Opacity => 2,
                Property::BlendMode => 3,
                Property::Tiles => 4,
            }
        }
        for (i, p) in PROPERTIES.into_iter().enumerate() {
            assert_eq!(index(p), i);
        }

        let s = table_snapshot();
        let seeded = seed_revisions(s.document());
        // The unsupported layer is read-only; the fixture has no raster layer, so `tiles` (a raster
        // layer's property) is refused on every layer and seeded for none.
        let supported = |a: &PropertyAddress| {
            a.target.node_id != id("SLYR", 3) && a.property != Property::Tiles
        };

        // Every (node, property) pair of every node kind: prepare itself decides acceptance, with
        // a vector that holds only the pair under test (prepare refuses an unaddressable entry).
        let nodes = [
            (NodeKind::Layer, id("SLYR", 1), "original-private-name"),
            (NodeKind::Layer, id("SLYR", 3), "opaque"),
            (NodeKind::Artboard, id("SART", 1), "board"),
            (NodeKind::PageSpread, id("SPGS", 1), "spread"),
        ];
        let mut accepted = 0usize;
        for (kind, node_id, name) in &nodes {
            for property in PROPERTIES {
                let at = node(kind.clone(), node_id.clone(), property);
                let (current, written) = match property {
                    Property::Name => (
                        PropertyValue::String((*name).into()),
                        PropertyValue::String("probe".into()),
                    ),
                    Property::Visible => (PropertyValue::Bool(true), PropertyValue::Bool(false)),
                    Property::Opacity => (PropertyValue::Number(100.into()), PropertyValue::Number(40.into())),
                    Property::BlendMode => (PropertyValue::Number(1.into()), PropertyValue::Number(2.into())),
                    Property::Tiles => (
                        PropertyValue::Tiles(TileList(Vec::new())),
                        PropertyValue::Tiles(TileList(Vec::new())),
                    ),
                };
                let mut p = patch(Property::Name, PropertyValue::String("x".into()));
                p.reads = vec![PropertyRead {
                    address: at.clone(),
                    expected_revision: 0,
                    expected_value: current,
                }];
                p.writes = vec![PropertyWrite {
                    address: at.clone(),
                    value: written,
                }];
                let entry = RevisionEntry {
                    address: at.clone(),
                    revision: 0,
                };
                let verdict = run(&p, &s, std::slice::from_ref(&entry));
                let table_says = addressable(kind, property) && supported(&at);
                assert_eq!(
                    verdict.is_ok(),
                    table_says,
                    "table and prepare disagree on {at:?}: {:?}",
                    verdict.err()
                );
                assert_eq!(
                    seeded.contains(&entry),
                    table_says,
                    "seed and prepare disagree on {at:?}"
                );
                accepted += usize::from(verdict.is_ok());
            }
        }
        // Layer 1 (name, visible, opacity, blend_mode), the artboard and the page spread (name):
        // 6 of 20 pairs.
        assert_eq!((accepted, seeded.len()), (6, 6));
        // The unsupported layer and visibility of artboards and page spreads are refused with the
        // dimension that explains it.
        assert!(!addressable(&NodeKind::Artboard, Property::Visible));
        assert!(!addressable(&NodeKind::PageSpread, Property::Visible));
        assert!(addressable(&NodeKind::Layer, Property::Visible));
        let mut hidden_board = patch(Property::Name, PropertyValue::String("x".into()));
        let at = node(NodeKind::Artboard, id("SART", 1), Property::Visible);
        hidden_board.reads[0].address = at.clone();
        hidden_board.reads[0].expected_value = PropertyValue::Bool(true);
        hidden_board.writes[0].address = at.clone();
        hidden_board.writes[0].value = PropertyValue::Bool(false);
        assert_eq!(
            run(&hidden_board, &s, &[]).unwrap_err().code,
            Code::WrongType
        );

        // The seed is the document-order vector at revision 0, and prepare takes it whole.
        assert_eq!(
            seeded.iter().map(|e| e.revision).collect::<Vec<_>>(),
            [0; 6]
        );
        assert_eq!(
            seeded.iter().map(|e| e.address.clone()).collect::<Vec<_>>(),
            [
                node(NodeKind::Layer, id("SLYR", 1), Property::Name),
                node(NodeKind::Layer, id("SLYR", 1), Property::Visible),
                node(NodeKind::Artboard, id("SART", 1), Property::Name),
                node(NodeKind::PageSpread, id("SPGS", 1), Property::Name),
            ]
        );
        let mut both = patch(Property::Name, PropertyValue::String("renamed".into()));
        let board = node(NodeKind::Artboard, id("SART", 1), Property::Name);
        both.reads.push(PropertyRead {
            address: board.clone(),
            expected_revision: 0,
            expected_value: PropertyValue::String("board".into()),
        });
        both.writes.push(PropertyWrite {
            address: board,
            value: PropertyValue::String("renamed-board".into()),
        });
        let prepared = run(&both, &s, &seeded).unwrap();
        assert_eq!(prepared.revision_updates().len(), 2);

        // mask_property: every seeded address masks; two documents that differ only at those
        // addresses compare equal afterwards; an unaddressable pair or a missing node is
        // refused and leaves the document as it was.
        let mut a = s.document().clone();
        let mut b = s.document().clone();
        b.layers[0].name = "other".into();
        b.layers[0].visible = false;
        b.artboards[0].name = "other-board".into();
        b.page_spreads[0].name = "other-spread".into();
        assert_ne!(a, b);
        for entry in &seeded {
            assert!(mask_property(&mut a, &entry.address), "{:?}", entry.address);
            assert!(mask_property(&mut b, &entry.address), "{:?}", entry.address);
        }
        assert_eq!(a, b);
        assert_eq!(
            (&a.layers[0].name, a.layers[0].visible, &a.artboards[0].name),
            (&String::new(), false, &String::new())
        );
        let before = a.clone();
        for refused in [
            node(NodeKind::Artboard, id("SART", 1), Property::Visible),
            node(NodeKind::PageSpread, id("SPGS", 1), Property::Visible),
            node(NodeKind::Layer, id("SLYR", 99), Property::Name),
            node(NodeKind::Artboard, id("SART", 99), Property::Name),
        ] {
            assert!(!mask_property(&mut a, &refused), "{refused:?}");
            assert_eq!(a, before);
        }
    });
}

#[test]
fn appearance_properties_transact_and_invert_to_the_original_document() {
    bounded_case("appearance_properties_transact_and_invert_to_the_original_document", || {
        let original = snapshot();
        let mut v = seed_revisions(original.document());
        assert_eq!(v.len(), 4, "group layer: name, visible, opacity, blend_mode (no tiles)");
        // Opacity 40 on the group and blend mode normal in one patch.
        let mut p = patch(Property::Opacity, PropertyValue::number(40.0).unwrap());
        p.reads.push(PropertyRead { address: address(Property::BlendMode), expected_revision: 0, expected_value: PropertyValue::Number(1.into()) });
        p.writes.push(PropertyWrite { address: address(Property::BlendMode), value: PropertyValue::Number(2.into()) });
        let wire = serde_json::to_vec(&p).unwrap();
        assert_eq!(decode_patch(&wire, Budget::default(), &CancellationToken::default()).unwrap(), p, "numbers round-trip the typed wire");
        let changed = run(&p, &original, &v).unwrap();
        let next = changed.successor().unwrap().clone();
        let a = next.document().layers[0].appearance.clone().expect("appearance stored once it differs from identity");
        assert_eq!((a.opacity, a.blend_mode, a.isolation), (40.0, 2, hsk_studio_folio::Isolation::Isolated), "a group leaving pass-through is isolated");
        assert_eq!(changed.revision_updates().len(), 2);
        advance(&mut v, &changed);
        // 40 and 40.0 are the same value; the inverse restores the original document exactly
        // (identity appearance is stored absent again), not only the original pixels.
        let inverse = changed.inverse().unwrap().invocation("undo".into(), 2, actor(), next.document().revision);
        assert!(inverse.reads.iter().any(|r| r.expected_value == PropertyValue::Number(40.into())), "whole stored values are written as integers");
        let undone = run(&inverse, &next, &v).unwrap();
        let back = undone.successor().unwrap();
        assert_eq!(back.document().layers, original.document().layers);
        assert!(back.document().layers[0].appearance.is_none());
        // Types: opacity is a finite number, blend_mode an integer; ranges and applicability are folio's.
        let wrong = |property, value| {
            let mut p = patch(property, value);
            p.reads[0].expected_value = match property {
                Property::Opacity => PropertyValue::Number(100.into()),
                _ => PropertyValue::Number(1.into()),
            };
            run(&p, &original, &seed_revisions(original.document())).unwrap_err().code
        };
        assert_eq!(wrong(Property::Opacity, PropertyValue::Bool(true)), Code::WrongType);
        assert_eq!(wrong(Property::BlendMode, PropertyValue::number(2.5).unwrap()), Code::WrongType);
        assert_eq!(wrong(Property::BlendMode, PropertyValue::Number(70000.into())), Code::WrongType);
        assert_eq!(wrong(Property::Opacity, PropertyValue::number(150.0).unwrap()), Code::FolioRejected);
        assert_eq!(wrong(Property::BlendMode, PropertyValue::Number(24.into())), Code::FolioRejected, "tool-only behind");
        assert_eq!(wrong(Property::BlendMode, PropertyValue::Number(36.into())), Code::FolioRejected, "unknown discriminant");
        // A stale read of the old opacity after the change is a value conflict, typed and addressed.
        let stale = run(&p, &next, &v).unwrap_err();
        assert_eq!(stale.code, Code::RevisionConflict);
        assert_eq!(stale.address, Some(address(Property::Opacity)));
        // Artboards and page spreads have no appearance property; tiles exist only on raster layers.
        assert!(!addressable(&NodeKind::Artboard, Property::Opacity));
        assert!(!addressable(&NodeKind::PageSpread, Property::BlendMode));
        let mut t = patch(Property::Tiles, PropertyValue::Tiles(TileList(Vec::new())));
        t.reads[0].expected_revision = 0;
        assert_eq!(run(&t, &original, &[RevisionEntry { address: address(Property::Tiles), revision: 0 }]).unwrap_err().code, Code::InvalidTarget);
    });
}
