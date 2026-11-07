use handshake_rich_document_model::{
    from_json_value, to_content_json_value, validate_tree, BlockNode, Child, HsLinkNode, NodeKind,
};
use handshake_wikilink_semantics::parser::parse_wikilink;

#[test]
fn legacy_materialization_uses_pinned_model_without_inventing_optional_identity_fields() {
    for (token, kind, value, label, resolved) in [
        (
            "[[note:note-1|#全文 label]]",
            "note",
            "note-1",
            "#全文 label",
            true,
        ),
        (
            "[[future:opaque-id|Raw #Label]]",
            "unknown",
            "opaque-id",
            "Raw #Label",
            false,
        ),
        (
            "[[code:src/main.rs#Thing|Thing]]",
            "code",
            "src/main.rs#Thing",
            "Thing",
            true,
        ),
    ] {
        let link: HsLinkNode = parse_wikilink(token).unwrap().to_hs_link();
        assert_eq!(link.ref_kind, kind);
        assert_eq!(link.ref_value, value);
        assert_eq!(link.label, label);
        assert_eq!(link.resolved, resolved);
        assert_eq!(link.provenance, None);
        assert_eq!(link.target_kind, None);
        assert_eq!(link.target_block_id, None);
        assert_eq!(link.tag_ref_kind, None);
        let doc = BlockNode::doc(vec![BlockNode::with_children(
            NodeKind::Paragraph,
            vec![Child::HsLink(link)],
        )]);
        validate_tree(&doc).unwrap();
        let wire = to_content_json_value(&doc);
        let attrs = &wire["content"][0]["content"][0]["attrs"];
        for field in [
            "provenance",
            "target_kind",
            "target_block_id",
            "tag_ref_kind",
        ] {
            assert!(attrs.get(field).is_none());
        }
        assert_eq!(from_json_value(&wire).unwrap(), doc);
    }
}

#[test]
fn legacy_unknown_tag_prefix_stays_unknown_even_when_value_looks_like_an_id() {
    let value = "12345678-1234-1234-1234-123456789abc";
    let link = parse_wikilink(&format!("[[tag:{value}|#Stored label]]"))
        .unwrap()
        .to_hs_link();
    assert_eq!(link.ref_kind, "unknown");
    assert_eq!(link.ref_value, value);
    assert_eq!(link.label, "#Stored label");
    assert!(!link.resolved);
    assert_eq!(link.tag_ref_kind, None);
    assert_eq!(link.tag_reference(), None);
}
