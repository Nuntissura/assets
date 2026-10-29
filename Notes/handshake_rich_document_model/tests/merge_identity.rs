use handshake_rich_document_model::{
    apply_transaction, from_json_value, to_content_json_value, BlockNode, Child, HsLinkNode, Mark,
    NodeKind, Step, TextLeaf, Transaction, UndoManager,
};
use serde_json::json;

#[test]
fn merge_undo_restores_distinct_ids_kinds_attrs_and_rich_run_boundaries() {
    for right in [
        BlockNode::heading(2, "right"),
        BlockNode::new(NodeKind::Paragraph),
    ] {
        let mut left = BlockNode::paragraph("left");
        left.attrs.insert("block_id".into(), json!("A"));
        left.attrs.insert("custom".into(), json!({"left":true}));
        left.children
            .push(Child::Text(TextLeaf::new(" adjacent run")));
        left.children
            .push(Child::HsLink(HsLinkNode::new("note", "target", "Actual")));
        let mut right = right;
        right.attrs.insert("block_id".into(), json!("B"));
        right.attrs.insert("custom".into(), json!({"right":[1,2]}));
        right.children.push(Child::Text(TextLeaf::with_marks(
            " marked",
            vec![Mark::Bold],
        )));
        let original = BlockNode::doc(vec![BlockNode::with_children(
            NodeKind::Blockquote,
            vec![Child::Block(left.clone()), Child::Block(right)],
        )]);
        let mut doc = original.clone();
        let receipt = apply_transaction(
            &mut doc,
            Transaction::operator(vec![Step::MergeNodes {
                parent_path: vec![0],
                index: 1,
            }]),
        )
        .unwrap();
        let merged = doc.clone();
        let parent = doc.children[0].as_block().unwrap();
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].as_block().unwrap().kind, left.kind);
        assert_eq!(parent.children[0].as_block().unwrap().attrs, left.attrs);
        let mut history = UndoManager::new();
        history.push(receipt);
        assert_eq!(history.len(), 1);
        assert!(history.undo(&mut doc).unwrap());
        assert_eq!(doc, original);
        assert_eq!(
            from_json_value(&to_content_json_value(&doc)).unwrap(),
            original
        );
        assert!(!history.can_undo());
        assert!(history.redo(&mut doc).unwrap());
        assert_eq!(doc, merged);
    }
}

#[test]
fn compound_merge_inverses_keep_group_order_among_other_steps() {
    let blocks: Vec<_> = ["A", "B", "C"]
        .into_iter()
        .map(|id| {
            let mut block = BlockNode::paragraph(id);
            block.attrs.insert("block_id".into(), json!(id));
            block
        })
        .collect();
    let original = BlockNode::doc(blocks);
    let mut doc = original.clone();
    let receipt = apply_transaction(
        &mut doc,
        Transaction::operator(vec![
            Step::InsertText {
                path: vec![0, 0],
                char_offset: 0,
                text: "before".into(),
            },
            Step::MergeNodes {
                parent_path: vec![],
                index: 1,
            },
            Step::MergeNodes {
                parent_path: vec![],
                index: 1,
            },
            Step::InsertText {
                path: vec![0, 0],
                char_offset: usize::MAX,
                text: "after".into(),
            },
        ]),
    )
    .unwrap();
    assert_eq!(doc.children.len(), 1);
    assert_eq!(
        doc.children[0].as_block().unwrap().children[0]
            .as_text()
            .unwrap()
            .text
            .to_string(),
        "beforeABCafter"
    );
    let merged = doc.clone();
    let mut history = UndoManager::new();
    history.push(receipt);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
    assert!(history.redo(&mut doc).unwrap());
    assert_eq!(doc, merged);
}

#[test]
fn failed_merge_does_not_leave_a_preceding_edit_applied() {
    for index in [0, 2, usize::MAX] {
        let original = BlockNode::doc(vec![BlockNode::paragraph("keep")]);
        let mut doc = original.clone();
        assert!(apply_transaction(
            &mut doc,
            Transaction::operator(vec![
                Step::InsertText {
                    path: vec![0, 0],
                    char_offset: 0,
                    text: "rollback".into()
                },
                Step::MergeNodes {
                    parent_path: vec![],
                    index
                },
            ])
        )
        .is_err());
        assert_eq!(doc, original);
    }
}
