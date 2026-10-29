use handshake_rich_document_model::{
    apply_transaction, apply_transaction_with_trace, BlockNode, Child, NodeKind, SplitSide, Step,
    StepPathTrace, Transaction, UndoManager,
};
use serde_json::json;

fn identified(kind: NodeKind, id: &str, children: Vec<Child>) -> BlockNode {
    let mut node = BlockNode::with_children(kind, children);
    node.attrs.insert("block_id".into(), json!(id));
    node
}

fn paragraph(id: &str) -> BlockNode {
    let mut node = BlockNode::paragraph(id);
    node.attrs.insert("block_id".into(), json!(id));
    node
}

#[test]
fn container_merge_maps_right_descendants_at_actual_left_child_count() {
    let original = BlockNode::doc(vec![
        identified(
            NodeKind::Blockquote,
            "left",
            vec![Child::Block(paragraph("A")), Child::Block(paragraph("B"))],
        ),
        identified(
            NodeKind::Blockquote,
            "right",
            vec![Child::Block(paragraph("C")), Child::Block(paragraph("D"))],
        ),
        paragraph("outside"),
    ]);
    let tx = Transaction::operator(vec![Step::MergeNodes {
        parent_path: vec![],
        index: 1,
    }]);
    let mut ordinary = original.clone();
    let ordinary_receipt = apply_transaction(&mut ordinary, tx.clone()).unwrap();
    let mut doc = original.clone();
    let traced = apply_transaction_with_trace(&mut doc, tx).unwrap();
    assert_eq!(doc, ordinary);
    assert_eq!(traced.receipt, ordinary_receipt);
    assert_eq!(
        traced.trace.steps(),
        vec![StepPathTrace::Merge {
            parent_path: vec![],
            index: 1,
            left_children_before: 2,
            right_children_before: 2,
            coalesced_boundary: false
        }]
    );
    assert_eq!(traced.trace.map_block_path(&[0]), Some(vec![0]));
    assert_eq!(traced.trace.map_block_path(&[1]), None);
    assert_eq!(traced.trace.map_block_path(&[1, 0]), Some(vec![0, 2]));
    assert_eq!(traced.trace.map_block_path(&[1, 1]), Some(vec![0, 3]));
    assert_eq!(traced.trace.map_block_path(&[2]), Some(vec![1]));
    assert_eq!(
        doc.children[0].as_block().unwrap().children[2]
            .as_block()
            .unwrap()
            .attrs["block_id"],
        "C"
    );
    let mut history = UndoManager::new();
    history.push(traced.receipt);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
}

#[test]
fn inserting_a_duplicate_before_original_tracks_origin_instead_of_matching_ids() {
    let original = BlockNode::doc(vec![paragraph("same-id"), paragraph("outside")]);
    let mut doc = original.clone();
    let traced = apply_transaction_with_trace(
        &mut doc,
        Transaction::operator(vec![Step::InsertNode {
            parent_path: vec![],
            index: 0,
            node: paragraph("same-id"),
        }]),
    )
    .unwrap();
    assert_eq!(doc.children[0], doc.children[1]);
    assert_eq!(traced.trace.map_block_path(&[0]), Some(vec![1]));
    assert_eq!(traced.trace.map_block_path(&[1]), Some(vec![2]));
    assert_eq!(traced.trace.map_block_path(&[0, 0]), None);
    assert_eq!(traced.trace.map_block_path(&[]), Some(vec![]));
    let removed = apply_transaction_with_trace(
        &mut doc,
        Transaction::operator(vec![Step::DeleteNode {
            parent_path: vec![],
            index: 1,
        }]),
    )
    .unwrap();
    assert_eq!(removed.trace.map_block_path(&[1]), None);
    assert_eq!(removed.trace.map_block_path(&[0]), Some(vec![0]));
}

#[test]
fn split_exposes_actual_geometry_and_retains_original_head_not_new_tail() {
    let original = BlockNode::doc(vec![paragraph("abcd"), paragraph("outside")]);
    let mut doc = original.clone();
    let traced = apply_transaction_with_trace(
        &mut doc,
        Transaction::operator(vec![Step::SplitNode {
            path: vec![0],
            char_offset: 2,
        }]),
    )
    .unwrap();
    assert_eq!(traced.trace.map_block_path(&[0]), Some(vec![0]));
    assert_eq!(traced.trace.map_block_path(&[1]), Some(vec![2]));
    assert!(
        matches!(&traced.trace.steps()[0],StepPathTrace::Split {path,children_before:1,head_children:1,tail_children:1,block_children} if path==&vec![0] && block_children.is_empty())
    );
    assert_eq!(
        doc.children[0].as_block().unwrap().attrs["block_id"],
        "abcd"
    );
    assert_eq!(
        doc.children[1].as_block().unwrap().attrs["block_id"],
        "abcd"
    );
    let mut history = UndoManager::new();
    history.push(traced.receipt);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
}

#[test]
fn intermediate_invalid_prefixes_are_not_validated_and_split_lineage_is_recorded() {
    let original = BlockNode::doc(vec![
        identified(
            NodeKind::Blockquote,
            "left",
            vec![Child::Block(paragraph("original-left"))],
        ),
        paragraph("text"),
        identified(
            NodeKind::Blockquote,
            "right",
            vec![Child::Block(paragraph("moved-original"))],
        ),
    ]);
    let steps = vec![
        Step::MergeNodes {
            parent_path: vec![],
            index: 2,
        },
        Step::SplitNode {
            path: vec![1],
            char_offset: 4,
        },
        Step::MergeNodes {
            parent_path: vec![],
            index: 2,
        },
        Step::MergeNodes {
            parent_path: vec![],
            index: 1,
        },
        Step::DeleteInlineChild {
            parent_path: vec![0],
            index: 1,
        },
    ];
    let mut prefix = original.clone();
    assert!(apply_transaction(&mut prefix, Transaction::operator(vec![steps[0].clone()])).is_err());
    assert_eq!(prefix, original);
    let mut ordinary = original.clone();
    let ordinary_receipt =
        apply_transaction(&mut ordinary, Transaction::operator(steps.clone())).unwrap();
    let mut doc = original.clone();
    let traced = apply_transaction_with_trace(&mut doc, Transaction::operator(steps)).unwrap();
    assert_eq!(doc, ordinary);
    assert_eq!(traced.receipt, ordinary_receipt);
    assert_eq!(doc.children.len(), 1);
    assert_eq!(doc.children[0].as_block().unwrap().children.len(), 2);
    assert_eq!(
        doc.children[0].as_block().unwrap().children[1]
            .as_block()
            .unwrap()
            .attrs["block_id"],
        "moved-original"
    );
    assert_eq!(traced.trace.map_block_path(&[2, 0]), Some(vec![0, 1]));
    assert_eq!(traced.trace.map_block_path(&[0, 0]), Some(vec![0, 0]));
    assert_eq!(traced.trace.map_block_path(&[1]), None);
    assert_eq!(traced.trace.map_block_path(&[2]), None);
    assert!(
        matches!(&traced.trace.steps()[1],StepPathTrace::Split {block_children,..} if block_children.len()==1 && block_children[0].old_index==1 && block_children[0].side==SplitSide::Tail && block_children[0].new_index==0)
    );
    let mut history = UndoManager::new();
    history.push(traced.receipt);
    assert_eq!(history.len(), 1);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
    assert!(history.redo(&mut doc).unwrap());
    assert_eq!(doc, ordinary);
}

#[test]
fn merge_trace_observes_actual_inline_boundary_coalescing() {
    let mut doc = BlockNode::doc(vec![paragraph("left"), paragraph("right")]);
    let traced = apply_transaction_with_trace(
        &mut doc,
        Transaction::operator(vec![Step::MergeNodes {
            parent_path: vec![],
            index: 1,
        }]),
    )
    .unwrap();
    assert!(matches!(
        &traced.trace.steps()[0],
        StepPathTrace::Merge {
            left_children_before: 1,
            right_children_before: 1,
            coalesced_boundary: true,
            ..
        }
    ));
    assert_eq!(doc.children[0].as_block().unwrap().children.len(), 1);
    assert_eq!(traced.trace.map_block_path(&[1]), None);
}

#[test]
fn failed_traced_transaction_returns_no_receipt_or_trace_and_restores_document() {
    let original = BlockNode::doc(vec![paragraph("keep")]);
    let mut doc = original.clone();
    let result = apply_transaction_with_trace(
        &mut doc,
        Transaction::operator(vec![
            Step::InsertNode {
                parent_path: vec![],
                index: 0,
                node: paragraph("new"),
            },
            Step::InsertNode {
                parent_path: vec![0],
                index: 0,
                node: paragraph("invalid nested paragraph"),
            },
        ]),
    );
    assert!(result.is_err());
    assert_eq!(doc, original);
}
