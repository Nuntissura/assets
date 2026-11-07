use handshake_rich_document_model::{
    from_json_value, BlockNode, Child, DocPosition, HsLinkNode, Selection, UndoManager,
};
use handshake_rich_formatting::{
    apply_action, dispatch, ActionHost, CommandContext, EditAction, EditContext, FormattingCommand,
};
use serde_json::json;

struct Host;
impl ActionHost for Host {
    fn merge_backward(&mut self, _: &mut EditContext<'_>) -> bool {
        false
    }
    fn prev_grapheme_boundary_chars(&self, _: &str, offset: usize) -> usize {
        offset.saturating_sub(1)
    }
    fn next_grapheme_boundary_chars(&self, text: &str, offset: usize) -> usize {
        (offset + 1).min(text.chars().count())
    }
}

#[test]
fn public_action_context_accepts_pinned_model_and_preserves_typed_links_and_embeds() {
    let mut doc = from_json_value(&json!({"type":"doc","content":[
        {"type":"paragraph","attrs":{"block_id":"paragraph-id"},"content":[{"type":"text","text":"ab"}]},
        {"type":"hsEmbed","attrs":{"block_id":"embed-id","refValue":"note-id","target_kind":"heading","target_block_id":"heading-id"}}
    ]})).unwrap();
    doc.children[0]
        .as_block_mut()
        .unwrap()
        .children
        .push(Child::HsLink(HsLinkNode::tag_hub_id(
            "hub-id",
            "#Raw full title",
        )));
    let original = doc.clone();
    let mut selection = Selection::caret(DocPosition::new(vec![0, 0], 1));
    let mut undo = UndoManager::new();
    let mut ctx = EditContext {
        doc: &mut doc,
        selection: &mut selection,
        undo: &mut undo,
        actor_id: "compatibility",
    };
    assert!(apply_action(
        &mut ctx,
        EditAction::Insert("X".into()),
        &mut Host
    ));
    assert_eq!(
        ctx.doc.children[0].as_block().unwrap().children[0]
            .as_text()
            .unwrap()
            .text
            .to_string(),
        "aXb"
    );
    assert_eq!(
        ctx.doc.children[0].as_block().unwrap().children[1],
        original.children[0].as_block().unwrap().children[1]
    );
    assert_eq!(ctx.doc.children[1], original.children[1]);
    assert_eq!(ctx.undo.len(), 1);
    let edited = ctx.doc.clone();
    assert!(apply_action(&mut ctx, EditAction::Undo, &mut Host));
    assert_eq!(*ctx.doc, original);
    assert!(apply_action(&mut ctx, EditAction::Redo, &mut Host));
    assert_eq!(*ctx.doc, edited);
}

#[test]
fn public_merge_command_undo_restores_original_block_ids_kinds_and_attrs() {
    let mut left = BlockNode::paragraph("left");
    left.attrs.insert("block_id".into(), json!("A"));
    let mut right = BlockNode::heading(2, "right");
    right.attrs.insert("block_id".into(), json!("B"));
    right.attrs.insert("custom".into(), json!({"keep":true}));
    let original = BlockNode::doc(vec![left, right]);
    let mut doc = original.clone();
    let mut selection = Selection::caret(DocPosition::new(vec![1, 0], 0));
    let mut undo = UndoManager::new();
    let mut ctx = CommandContext::new(&mut doc, &mut undo, &mut selection, "compatibility");
    dispatch(&mut ctx, &FormattingCommand::MergeBackward).unwrap();
    assert_eq!(ctx.doc.children.len(), 1);
    assert_eq!(
        ctx.doc.children[0].as_block().unwrap().attrs["block_id"],
        "A"
    );
    assert_eq!(ctx.history.len(), 1);
    let merged = ctx.doc.clone();
    dispatch(&mut ctx, &FormattingCommand::Undo).unwrap();
    assert_eq!(*ctx.doc, original);
    assert!(!ctx.history.can_undo());
    dispatch(&mut ctx, &FormattingCommand::Redo).unwrap();
    assert_eq!(*ctx.doc, merged);
}
