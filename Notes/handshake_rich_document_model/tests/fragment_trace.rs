use handshake_rich_document_model::{
    replace_selection_with_fragment, replace_selection_with_fragment_with_trace, ActorKind,
    BlockNode, Child, DocPosition, FragmentReplaceOptions, NodeKind, RichFragment, Selection,
    TextLeaf, UndoManager,
};
use serde_json::json;

fn options() -> FragmentReplaceOptions {
    FragmentReplaceOptions {
        actor_kind: ActorKind::Operator,
        actor_id: "fragment-trace".into(),
        split_tail_attrs: None,
    }
}

fn paragraph(id: &str) -> BlockNode {
    let mut node = BlockNode::paragraph(id);
    node.attrs.insert("block_id".into(), json!(id));
    node
}

#[test]
fn inline_replacement_traced_and_ordinary_results_match_in_one_undo_entry() {
    let original = BlockNode::doc(vec![paragraph("before"), paragraph("outside")]);
    let selection = Selection::text(
        DocPosition::new(vec![0, 0], 1),
        DocPosition::new(vec![0, 0], 3),
    );
    let fragment = RichFragment::Inline(vec![Child::Text(TextLeaf::new("中"))]);
    let mut ordinary = original.clone();
    let plain =
        replace_selection_with_fragment(&mut ordinary, &selection, fragment.clone(), options())
            .unwrap();
    let mut doc = original.clone();
    let traced =
        replace_selection_with_fragment_with_trace(&mut doc, &selection, fragment, options())
            .unwrap();
    assert_eq!(doc, ordinary);
    assert_eq!(traced.receipt, plain.receipt);
    assert_eq!(traced.selection, plain.selection);
    assert_eq!(traced.trace.steps().len(), traced.receipt.forward.len());
    assert_eq!(traced.trace.map_block_path(&[0]), Some(vec![0]));
    assert_eq!(traced.trace.map_block_path(&[1]), Some(vec![1]));
    let mut history = UndoManager::new();
    history.push(traced.receipt);
    assert_eq!(history.len(), 1);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
    assert!(history.redo(&mut doc).unwrap());
    assert_eq!(doc, ordinary);
}

#[test]
fn actual_block_fragment_trace_distinguishes_inserted_duplicate_from_surviving_original() {
    let original = BlockNode::doc(vec![paragraph("replace"), paragraph("same-id")]);
    let fragment = RichFragment::Blocks(vec![paragraph("same-id"), paragraph("new")]);
    let selection = Selection::node(vec![0]);
    let mut ordinary = original.clone();
    let plain =
        replace_selection_with_fragment(&mut ordinary, &selection, fragment.clone(), options())
            .unwrap();
    let mut doc = original.clone();
    let traced =
        replace_selection_with_fragment_with_trace(&mut doc, &selection, fragment, options())
            .unwrap();
    assert_eq!(doc, ordinary);
    assert_eq!(traced.receipt, plain.receipt);
    assert_eq!(traced.selection, plain.selection);
    assert_eq!(doc.children[0], doc.children[2]);
    assert_eq!(traced.trace.map_block_path(&[0]), None);
    assert_eq!(traced.trace.map_block_path(&[1]), Some(vec![2]));
    assert_eq!(traced.trace.steps().len(), traced.receipt.forward.len());
    let mut history = UndoManager::new();
    history.push(traced.receipt);
    assert_eq!(history.len(), 1);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
}

#[test]
fn empty_noop_returns_the_actual_empty_trace_without_changing_selection_or_document() {
    for block in [BlockNode::new(NodeKind::Paragraph), paragraph("text")] {
        let original = BlockNode::doc(vec![block, paragraph("outside")]);
        let selection = Selection::caret(DocPosition::new(vec![0, 0], 0));
        let mut ordinary = original.clone();
        let plain = replace_selection_with_fragment(
            &mut ordinary,
            &selection,
            RichFragment::Inline(vec![]),
            options(),
        )
        .unwrap();
        let mut doc = original.clone();
        let traced = replace_selection_with_fragment_with_trace(
            &mut doc,
            &selection,
            RichFragment::Inline(vec![]),
            options(),
        )
        .unwrap();
        assert_eq!(doc, original);
        assert_eq!(ordinary, original);
        assert_eq!(traced.receipt, plain.receipt);
        assert_eq!(traced.selection, selection);
        assert_eq!(traced.selection, plain.selection);
        assert!(traced.receipt.forward.is_empty());
        assert!(traced.receipt.inverse.is_empty());
        assert!(traced.trace.steps().is_empty());
        assert_eq!(traced.trace.map_block_path(&[0]), Some(vec![0]));
        assert_eq!(traced.trace.map_block_path(&[1]), Some(vec![1]));
    }
}

#[test]
fn failed_traced_replacement_matches_ordinary_error_and_preserves_the_original() {
    let list = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::BulletList,
        vec![Child::Block(BlockNode::with_children(
            NodeKind::ListItem,
            vec![Child::Block(paragraph("keep"))],
        ))],
    )]);
    for (original, selection, fragment) in [
        (
            list,
            Selection::node(vec![0, 0]),
            RichFragment::Blocks(vec![paragraph("invalid list child")]),
        ),
        (
            BlockNode::doc(vec![paragraph("keep")]),
            Selection::caret(DocPosition::new(vec![0, 0], 999)),
            RichFragment::Inline(vec![]),
        ),
    ] {
        let mut ordinary = original.clone();
        let plain =
            replace_selection_with_fragment(&mut ordinary, &selection, fragment.clone(), options())
                .unwrap_err();
        let mut doc = original.clone();
        let traced =
            replace_selection_with_fragment_with_trace(&mut doc, &selection, fragment, options())
                .unwrap_err();
        assert_eq!(traced, plain);
        assert_eq!(doc, original);
        assert_eq!(ordinary, original);
    }
}
