use handshake_rich_document_model::{
    apply_transaction, from_json_value, to_content_json_value, validate_tree, BlockNode, Child,
    DocJsonError, HsLinkNode, NodeKind, ReferenceTargetKind, SchemaError, Step, Transaction,
    TransformError,
};
use serde_json::json;

#[test]
fn heading_and_block_reference_targets_round_trip_without_synthetic_ids() {
    for (kind, typed) in [
        ("note", ReferenceTargetKind::Note),
        ("heading", ReferenceTargetKind::Heading),
        ("block", ReferenceTargetKind::Block),
    ] {
        let wire = json!({"type":"doc","content":[
            {"type":"heading","attrs":{"level":2,"block_id":"heading-persisted"},"content":[{"type":"text","text":"Title"}]},
            {"type":"paragraph","attrs":{"block_id":"paragraph-persisted"},"content":[
                {"type":"hsLink","attrs":{"refKind":"note","refValue":"note-persisted","label":"Target","resolved":true,"target_kind":kind,"target_block_id":"target-persisted","provenance":{"source":"test"}}}
            ]}
        ]});
        let doc = from_json_value(&wire).unwrap();
        validate_tree(&doc).unwrap();
        let link = doc.children[1].as_block().unwrap().children[0]
            .as_hs_link()
            .unwrap();
        assert_eq!(link.target_kind, Some(typed));
        assert_eq!(link.target_block_id.as_deref(), Some("target-persisted"));
        assert_eq!(link.ref_value, "note-persisted");
        assert_eq!(to_content_json_value(&doc), wire);
    }
}

#[test]
fn block_embed_preserves_its_own_identity_and_distinct_target_identity() {
    let wire = json!({"type":"doc","content":[{"type":"hsEmbed","attrs":{
        "block_id":"embed-instance","refKind":"note","refValue":"target-note","label":"Heading embed",
        "resolved":false,"provenance":{"artifact_id":"artifact"},"target_kind":"heading","target_block_id":"target-heading"
    }}]});
    let doc = from_json_value(&wire).unwrap();
    let embed = doc.children[0].as_block().unwrap();
    assert_eq!(embed.kind, NodeKind::HsEmbed);
    assert!(embed.kind.is_atom());
    assert_eq!(embed.attrs["block_id"], "embed-instance");
    assert_eq!(embed.attrs["target_block_id"], "target-heading");
    validate_tree(&doc).unwrap();
    assert_eq!(to_content_json_value(&doc), wire);
}

#[test]
fn legacy_link_constructor_does_not_invent_reference_target_fields() {
    let link = HsLinkNode::new("wp", "WP-1", "Packet");
    assert_eq!(link.target_kind, None);
    assert_eq!(link.target_block_id, None);
    let doc = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::Paragraph,
        vec![Child::HsLink(link)],
    )]);
    let wire = to_content_json_value(&doc);
    let attrs = &wire["content"][0]["content"][0]["attrs"];
    assert!(attrs.get("target_kind").is_none());
    assert!(attrs.get("target_block_id").is_none());
    assert_eq!(from_json_value(&wire).unwrap(), doc);
}

#[test]
fn invalid_reference_target_fields_are_rejected_in_links_and_embeds() {
    assert_eq!(
        from_json_value(&json!({"type":"doc","content":[{"type":"hsEmbed","attrs":{}}]}))
            .unwrap_err(),
        DocJsonError::HsEmbedMissingAttr("refValue")
    );
    for node_type in ["hsLink", "hsEmbed"] {
        for (field, value) in [
            ("target_kind", json!("unknown-kind")),
            ("target_kind", json!(42)),
            ("target_block_id", json!({"id":"wrong-shape"})),
        ] {
            let mut node = json!({"type":node_type,"attrs":{"refValue":"note"}});
            node["attrs"][field] = value;
            let content = if node_type == "hsLink" {
                json!([{"type":"paragraph","content":[node]}])
            } else {
                json!([node])
            };
            let error = from_json_value(&json!({"type":"doc","content":content})).unwrap_err();
            assert_eq!(error, DocJsonError::InvalidReferenceTargetAttr(field));
        }
    }
}

#[test]
fn null_optional_link_targets_remain_absent_in_the_typed_model() {
    let doc = from_json_value(
        &json!({"type":"doc","content":[{"type":"paragraph","content":[
            {"type":"hsLink","attrs":{"refValue":"note","target_kind":null,"target_block_id":null}}
        ]}]}),
    )
    .unwrap();
    let link = doc.children[0].as_block().unwrap().children[0]
        .as_hs_link()
        .unwrap();
    assert_eq!(link.target_kind, None);
    assert_eq!(link.target_block_id, None);
}

#[test]
fn block_embed_rejects_children_and_inline_embedding() {
    let mut embed = BlockNode::new(NodeKind::HsEmbed);
    embed.attrs.insert("refValue".into(), json!("target-note"));
    assert!(validate_tree(&BlockNode::doc(vec![embed.clone()])).is_ok());
    let inline_embed = embed.clone();
    embed
        .children
        .push(Child::Block(BlockNode::paragraph("invalid child")));
    assert!(validate_tree(&BlockNode::doc(vec![embed])).is_err());
    let inline = BlockNode::with_children(NodeKind::Paragraph, vec![Child::Block(inline_embed)]);
    assert!(validate_tree(&BlockNode::doc(vec![inline])).is_err());
}

#[test]
fn malformed_constructed_embeds_fail_schema_and_transactions_without_partial_edits() {
    for (attrs, attribute) in [
        (json!({}), "refValue"),
        (json!({"refValue":42}), "refValue"),
        (
            json!({"refValue":"note", "target_kind":"unknown"}),
            "target_kind",
        ),
        (json!({"refValue":"note", "target_kind":42}), "target_kind"),
        (
            json!({"refValue":"note", "target_block_id":false}),
            "target_block_id",
        ),
    ] {
        let mut embed = BlockNode::new(NodeKind::HsEmbed);
        embed.attrs = attrs
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let expected = SchemaError::InvalidEmbedReference { attribute };
        assert_eq!(
            validate_tree(&BlockNode::doc(vec![embed.clone()])),
            Err(expected.clone())
        );
        let original = BlockNode::doc(vec![BlockNode::paragraph("keep")]);
        let mut doc = original.clone();
        let result = apply_transaction(
            &mut doc,
            Transaction::operator(vec![
                Step::InsertText {
                    path: vec![0, 0],
                    char_offset: 0,
                    text: "must roll back ".into(),
                },
                Step::InsertNode {
                    parent_path: vec![],
                    index: 1,
                    node: embed,
                },
            ]),
        );
        assert_eq!(result, Err(TransformError::Schema(expected)));
        assert_eq!(doc, original);
        assert_eq!(
            from_json_value(&to_content_json_value(&doc)).unwrap(),
            original
        );
    }
}
