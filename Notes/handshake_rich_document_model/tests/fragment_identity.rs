use handshake_rich_document_model::{
    from_json_value, replace_selection_with_fragment, ActorKind, BlockNode, FragmentReplaceOptions,
    RichFragment, Selection, UndoManager,
};
use serde_json::json;

#[test]
fn fragment_insertion_and_one_undo_preserve_embed_and_reference_identities() {
    let original = BlockNode::doc(vec![
        BlockNode::paragraph("replace"),
        BlockNode::paragraph("untouched"),
    ]);
    let template = from_json_value(&json!({"type":"doc","content":[
        {"type":"hsEmbed","attrs":{"block_id":"new-instance","refValue":"original-target-note","target_kind":"block","target_block_id":"original-target-block"}}
    ]})).unwrap();
    let mut doc = original.clone();
    let result = replace_selection_with_fragment(
        &mut doc,
        &Selection::node(vec![0]),
        RichFragment::Blocks(
            template
                .children
                .iter()
                .map(|child| child.as_block().unwrap().clone())
                .collect(),
        ),
        FragmentReplaceOptions {
            actor_kind: ActorKind::Operator,
            actor_id: "identity-test".into(),
            split_tail_attrs: None,
        },
    )
    .unwrap();
    let expected = doc.clone();
    assert_eq!(doc.children[0], template.children[0]);
    assert_eq!(doc.children[1], original.children[1]);
    assert_eq!(result.selection, Selection::node(vec![0]));
    let mut history = UndoManager::new();
    history.push(result.receipt);
    assert!(history.undo(&mut doc).unwrap());
    assert_eq!(doc, original);
    assert!(history.redo(&mut doc).unwrap());
    assert_eq!(doc, expected);
}
