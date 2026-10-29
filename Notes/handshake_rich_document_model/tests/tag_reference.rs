use handshake_rich_document_model::{
    apply_transaction, from_json_value, replace_selection_with_fragment, to_content_json_value,
    validate_tree, ActorKind, BlockNode, Child, DocJsonError, DocPosition, FragmentReplaceOptions,
    HsLinkNode, NodeKind, RichFragment, SchemaError, Selection, Step, TagReference,
    TagReferenceKind, TextLeaf, Transaction, TransformError, UndoManager,
};
use serde_json::json;

fn document(link: HsLinkNode) -> BlockNode {
    BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::Paragraph,
        vec![Child::HsLink(link)],
    )])
}

#[test]
fn marked_tag_identity_and_exact_full_label_round_trip() {
    let label = format!("  #Café / 中文 {}  ", "long".repeat(160));
    for link in [
        HsLinkNode::tag_hub_id("opaque/non-uuid-id", label.clone()),
        HsLinkNode::tag_normalized_name("legacy/name", label.clone()),
    ] {
        let doc = document(link.clone());
        validate_tree(&doc).unwrap();
        let wire = to_content_json_value(&doc);
        let attrs = &wire["content"][0]["content"][0]["attrs"];
        assert_eq!(attrs["refValue"], link.ref_value);
        assert_eq!(attrs["label"], label);
        assert_eq!(attrs["tag_ref_kind"], link.tag_ref_kind.unwrap().as_str());
        assert_eq!(from_json_value(&wire).unwrap(), doc);
    }
}

#[test]
fn marked_labels_add_one_syntax_prefix_without_stripping_real_name_characters() {
    for raw in ["rust", "#rust", "##rust", " Café / 中文 "] {
        for link in [
            HsLinkNode::tag_hub_id("id", raw),
            HsLinkNode::tag_normalized_name("canonical", raw),
        ] {
            assert_eq!(link.label, raw);
            assert_eq!(
                link.tag_reference().unwrap().display_label(),
                format!("#{raw}")
            );
        }
    }
}

#[test]
fn unmarked_legacy_and_imported_references_keep_opaque_values_and_labels() {
    for (value, label, resolved, provenance) in [
        ("rust", "#Rust", true, json!(null)),
        ("rust", "Rust", true, json!("manual")),
        (
            "12345678-1234-1234-1234-123456789abc",
            "#Actual name",
            false,
            json!("source"),
        ),
        ("missing-id", "", true, json!(null)),
    ] {
        let wire = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"hsLink","attrs":{
            "refKind":"tag","refValue":value,"label":label,"resolved":resolved,"provenance":provenance
        }}]}]});
        let doc = from_json_value(&wire).unwrap();
        let link = doc.children[0].as_block().unwrap().children[0]
            .as_hs_link()
            .unwrap();
        assert_eq!(
            link.tag_reference(),
            Some(TagReference::Unclassified {
                raw_value: value,
                stored_label: label
            })
        );
        assert_eq!(
            link.tag_reference().unwrap().display_label(),
            if label.is_empty() { value } else { label }
        );
        assert_eq!(to_content_json_value(&doc), wire);
    }
}

#[test]
fn typed_views_do_not_infer_identity_from_syntax_or_resolution_flags() {
    let id = HsLinkNode::tag_hub_id("same-opaque-value", "Full");
    let name = HsLinkNode::tag_normalized_name("same-opaque-value", "Full");
    assert_ne!(id.tag_reference(), name.tag_reference());
    assert_ne!(
        to_content_json_value(&document(id)),
        to_content_json_value(&document(name))
    );
    assert_eq!(
        HsLinkNode::tag_hub_id("rust", "Rust").tag_reference(),
        Some(TagReference::HubId {
            id: "rust",
            raw_label: "Rust"
        })
    );
    assert_eq!(
        HsLinkNode::tag_normalized_name("id-looking-name", "#Full").tag_reference(),
        Some(TagReference::NormalizedName {
            canonical_name: "id-looking-name",
            raw_label: "#Full"
        })
    );
    let mut old = HsLinkNode::new("tag", "12345678-1234-1234-1234-123456789abc", "Full");
    old.provenance = Some(json!("manual"));
    for resolved in [false, true] {
        old.resolved = resolved;
        assert!(matches!(
            old.tag_reference(),
            Some(TagReference::Unclassified { .. })
        ));
    }
    assert_eq!(
        HsLinkNode::new("note", "rust", "Rust").tag_reference(),
        None
    );
}

#[test]
fn malformed_or_misplaced_wire_discriminators_are_rejected() {
    for label in [json!(null), json!(42)] {
        let wire = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"hsLink","attrs":{"refKind":"tag","refValue":"value","tag_ref_kind":"hub_id","label":label}}]}]});
        assert_eq!(from_json_value(&wire), Err(DocJsonError::InvalidTagLabel));
    }
    for marker in [
        json!("unknown"),
        json!(42),
        json!(true),
        json!({"kind":"hub_id"}),
    ] {
        let wire = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"hsLink","attrs":{"refKind":"tag","refValue":"value","tag_ref_kind":marker}}]}]});
        assert_eq!(
            from_json_value(&wire),
            Err(DocJsonError::InvalidTagReferenceKind)
        );
    }
    for node_type in ["hsLink", "hsEmbed", "paragraph"] {
        let node = json!({"type":node_type,"attrs":{"refKind":"note","refValue":"value","tag_ref_kind":"hub_id"}});
        let content = if node_type == "hsLink" {
            json!([{"type":"paragraph","content":[node]}])
        } else {
            json!([node])
        };
        assert_eq!(
            from_json_value(&json!({"type":"doc","content":content})),
            Err(DocJsonError::InvalidTagReferenceKind)
        );
    }
}

#[test]
fn text_and_transclusion_reject_reserved_tag_markers_without_losing_valid_content() {
    for node in [
        json!({"type":"text","text":"kept 中文"}),
        json!({"type":"loomTransclusion","attrs":{"refValue":"target-block"}}),
    ] {
        let valid = json!({"type":"doc","content":[{"type":"paragraph","content":[node.clone()]}]});
        let doc = from_json_value(&valid).unwrap();
        validate_tree(&doc).unwrap();
        assert_eq!(to_content_json_value(&doc), valid);
        for marker in [
            json!("hub_id"),
            json!("normalized_name"),
            json!(true),
            json!(42),
            json!({"kind":"hub_id"}),
        ] {
            let mut marked = node.clone();
            marked
                .as_object_mut()
                .unwrap()
                .entry("attrs")
                .or_insert(json!({}))["tag_ref_kind"] = marker;
            let wire = json!({"type":"doc","content":[{"type":"paragraph","content":[marked]}]});
            assert_eq!(
                from_json_value(&wire),
                Err(DocJsonError::InvalidTagReferenceKind)
            );
        }
    }
}

#[test]
fn directly_misclassified_reference_rolls_back_an_entire_transaction() {
    let mut invalid = HsLinkNode::tag_hub_id("id", "Raw");
    invalid.ref_kind = "note".into();
    assert_eq!(
        validate_tree(&document(invalid.clone())),
        Err(SchemaError::InvalidTagReferenceKind)
    );
    let original = BlockNode::doc(vec![BlockNode::paragraph("keep")]);
    let mut doc = original.clone();
    assert_eq!(
        apply_transaction(
            &mut doc,
            Transaction::operator(vec![
                Step::InsertText {
                    path: vec![0, 0],
                    char_offset: 0,
                    text: "rollback".into()
                },
                Step::InsertInlineChild {
                    parent_path: vec![0],
                    index: 1,
                    child: Child::HsLink(invalid)
                },
            ])
        ),
        Err(TransformError::Schema(SchemaError::InvalidTagReferenceKind))
    );
    assert_eq!(doc, original);
    let mut embed = BlockNode::new(NodeKind::HsEmbed);
    embed.attrs.insert("refValue".into(), json!("id"));
    embed.attrs.insert("tag_ref_kind".into(), json!("hub_id"));
    assert_eq!(
        validate_tree(&BlockNode::doc(vec![embed])),
        Err(SchemaError::InvalidTagReferenceKind)
    );
}

#[test]
fn explicit_caller_resolution_upgrades_a_reference_with_one_undo_and_exact_raw_title() {
    let original = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::Paragraph,
        vec![
            Child::Text(TextLeaf::new("before")),
            Child::HsLink(HsLinkNode::new("tag", "legacy-name", "#old label")),
            Child::Text(TextLeaf::new("after")),
        ],
    )]);
    let mut doc = original.clone();
    // The host has explicitly selected this authoritative row. The model does no
    // name/ID lookup and does not derive its title by stripping the old label.
    let resolved = HsLinkNode::tag_hub_id("selected-hub", "#Actual full title");
    let result = replace_selection_with_fragment(
        &mut doc,
        &Selection::text(
            DocPosition::new(vec![0, 0], 6),
            DocPosition::new(vec![0, 2], 0),
        ),
        RichFragment::Inline(vec![Child::HsLink(resolved.clone())]),
        FragmentReplaceOptions {
            actor_kind: ActorKind::Operator,
            actor_id: "explicit-resolution".into(),
            split_tail_attrs: None,
        },
    )
    .unwrap();
    assert_eq!(
        doc.children[0].as_block().unwrap().children[1],
        Child::HsLink(resolved)
    );
    let edited = doc.clone();
    assert_eq!(from_json_value(&to_content_json_value(&doc)).unwrap(), doc);
    let mut history = UndoManager::new();
    history.push(result.receipt);
    assert_eq!(history.len(), 1);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
    assert!(history.redo(&mut doc).unwrap());
    assert_eq!(doc, edited);
    assert_eq!(TagReferenceKind::HubId.as_str(), "hub_id");
}
