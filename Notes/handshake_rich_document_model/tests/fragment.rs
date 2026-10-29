use handshake_rich_document_model::{
    replace_selection_with_fragment, ActorKind, BlockNode, Child, DocPosition, DocumentEditState,
    FragmentReplaceError, FragmentReplaceOptions, HsLinkNode, Mark, NodeKind, RichFragment,
    Selection, TextLeaf, UndoManager,
};
use serde_json::json;

fn options() -> FragmentReplaceOptions {
    FragmentReplaceOptions {
        actor_kind: ActorKind::Operator,
        actor_id: "fragment-test".into(),
        split_tail_attrs: None,
    }
}

fn range(a: &[usize], a_offset: usize, b: &[usize], b_offset: usize) -> Selection {
    Selection::text(
        DocPosition::new(a.to_vec(), a_offset),
        DocPosition::new(b.to_vec(), b_offset),
    )
}

fn text(value: &str, marks: Vec<Mark>) -> Child {
    Child::Text(TextLeaf::with_marks(value, marks))
}

fn link() -> Child {
    Child::HsLink(HsLinkNode::new("note", "note-1", "Note"))
}

fn attributed(value: &str, id: &str) -> BlockNode {
    let mut block = BlockNode::paragraph(value);
    block.attrs.insert("block_id".into(), json!(id));
    block
}

fn assert_undo_redo(
    original: &BlockNode,
    edited: &mut BlockNode,
    receipt: handshake_rich_document_model::TransactionReceipt,
) {
    let expected = edited.clone();
    let mut history = UndoManager::new();
    history.push(receipt);
    assert_eq!(history.len(), 1);
    assert!(history.undo(edited).unwrap());
    assert_eq!(edited, original);
    assert!(!history.can_undo());
    assert!(history.redo(edited).unwrap());
    assert_eq!(*edited, expected);
}

#[test]
fn inline_fragment_inserts_at_empty_paragraph_virtual_caret_and_preserves_attrs() {
    let mut paragraph = attributed("", "empty-original");
    paragraph.children.clear();
    let original = BlockNode::doc(vec![paragraph, attributed("outside", "outside")]);
    let mut doc = original.clone();
    let selection = range(&[0, 0], 0, &[0, 0], 0);
    let noop = replace_selection_with_fragment(
        &mut doc,
        &selection,
        RichFragment::Inline(vec![]),
        options(),
    )
    .unwrap();
    assert_eq!(doc, original);
    assert_eq!(noop.selection, selection);
    assert!(noop.receipt.forward.is_empty());
    assert!(noop.receipt.inverse.is_empty());
    let content = vec![
        text("🙂", vec![Mark::Bold]),
        link(),
        text("中", vec![Mark::Italic]),
    ];
    let result = replace_selection_with_fragment(
        &mut doc,
        &selection,
        RichFragment::Inline(content.clone()),
        options(),
    )
    .unwrap();
    let first = doc.children[0].as_block().unwrap();
    assert_eq!(first.children, content);
    assert_eq!(first.attrs, original.children[0].as_block().unwrap().attrs);
    assert_eq!(doc.children[1], original.children[1]);
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 2], 1))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn block_fragment_inserts_at_empty_nested_paragraph_virtual_caret() {
    let original = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::Blockquote,
        vec![
            Child::Block(BlockNode::new(NodeKind::Paragraph)),
            Child::Block(attributed("outside", "outside")),
        ],
    )]);
    let mut doc = original.clone();
    let content = vec![
        BlockNode::heading(2, "heading"),
        attributed("body", "new-body"),
    ];
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0, 0], 0, &[0, 0, 0], 0),
        RichFragment::Blocks(content.clone()),
        options(),
    )
    .unwrap();
    let mut expected_children: Vec<_> = content.into_iter().map(Child::Block).collect();
    expected_children.push(Child::Block(attributed("outside", "outside")));
    assert_eq!(
        doc,
        BlockNode::doc(vec![BlockNode::with_children(
            NodeKind::Blockquote,
            expected_children
        )])
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 1, 0], 4))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn invalid_empty_block_positions_do_not_mutate_the_document() {
    for kind in [
        NodeKind::Paragraph,
        NodeKind::HorizontalRule,
        NodeKind::Blockquote,
    ] {
        let original = BlockNode::doc(vec![BlockNode::new(kind)]);
        for (path, offset) in [(vec![0, 0], 1), (vec![0, 1], 0), (vec![0, 0], usize::MAX)] {
            for fragment in [
                RichFragment::Inline(vec![]),
                RichFragment::Inline(vec![text("x", vec![])]),
                RichFragment::Blocks(vec![BlockNode::paragraph("x")]),
            ] {
                let mut doc = original.clone();
                let selection = Selection::caret(DocPosition::new(path.clone(), offset));
                assert_eq!(
                    replace_selection_with_fragment(&mut doc, &selection, fragment, options()),
                    Err(FragmentReplaceError::InvalidPosition(DocPosition::new(
                        path.clone(),
                        offset
                    )))
                );
                assert_eq!(doc, original);
            }
        }
        if kind != NodeKind::Paragraph {
            let mut doc = original.clone();
            assert!(replace_selection_with_fragment(
                &mut doc,
                &range(&[0, 0], 0, &[0, 0], 0),
                RichFragment::Inline(vec![text("x", vec![])]),
                options()
            )
            .is_err());
            assert_eq!(doc, original);
        }
    }
}

#[test]
fn empty_inline_at_valid_collapsed_caret_is_a_zero_step_edit() {
    let mut paragraph = attributed("abc", "keep-id");
    paragraph.children = vec![text("abc", vec![Mark::Bold])];
    let original = BlockNode::doc(vec![paragraph]);
    for offset in [0, 1, 3] {
        let selection = range(&[0, 0], offset, &[0, 0], offset);
        let mut state = DocumentEditState::new(original.clone());
        let commit = state
            .edit(0, |doc, history| {
                let result = replace_selection_with_fragment(
                    doc,
                    &selection,
                    RichFragment::Inline(vec![]),
                    options(),
                )?;
                assert!(result.receipt.forward.is_empty());
                assert!(result.receipt.inverse.is_empty());
                assert_eq!(result.receipt.actor_kind, ActorKind::Operator);
                assert_eq!(result.receipt.actor_id, "fragment-test");
                if !result.receipt.forward.is_empty() {
                    history.push(result.receipt);
                }
                Ok::<_, FragmentReplaceError>(result.selection)
            })
            .unwrap();
        assert_eq!(state.document(), &original);
        assert_eq!(commit.output, selection);
        assert!(!commit.content_changed);
        assert_eq!(commit.edit_revision, 0);
        assert_eq!(state.history().len(), 0);
    }
    let mut doc = original.clone();
    let invalid = range(&[0, 0], 99, &[0, 0], 99);
    assert!(replace_selection_with_fragment(
        &mut doc,
        &invalid,
        RichFragment::Inline(vec![]),
        options(),
    )
    .is_err());
    assert_eq!(doc, original);
}

#[test]
fn reversed_multi_run_selection_preserves_unicode_marks_atoms_and_siblings() {
    let original = BlockNode::doc(vec![
        BlockNode::with_children(
            NodeKind::Paragraph,
            vec![
                text("前🙂ab", vec![Mark::Bold]),
                link(),
                text("cd後", vec![Mark::Italic]),
            ],
        ),
        attributed("untouched", "sibling"),
    ]);
    let mut doc = original.clone();
    let inserted = vec![
        text("é", vec![Mark::Underline]),
        link(),
        text("中", vec![Mark::Strike]),
    ];
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 2], 2, &[0, 0], 2),
        RichFragment::Inline(inserted.clone()),
        options(),
    )
    .unwrap();
    let mut expected = vec![text("前🙂", vec![Mark::Bold])];
    expected.extend(inserted);
    expected.push(text("後", vec![Mark::Italic]));
    assert_eq!(doc.children[0].as_block().unwrap().children, expected);
    assert_eq!(doc.children[1], original.children[1]);
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 3], 1))
    );
    assert_eq!(result.receipt.actor_id, "fragment-test");
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn inline_atom_at_caret_returns_a_position_after_the_atom() {
    let original = BlockNode::doc(vec![BlockNode::paragraph("ab")]);
    let mut doc = original.clone();
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0], 1, &[0, 0], 1),
        RichFragment::Inline(vec![link()]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc.children[0].as_block().unwrap().children,
        vec![
            text("a", vec![]),
            link(),
            text("", vec![]),
            text("b", vec![]),
        ]
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 2], 0))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn block_fragment_splits_a_block_with_explicit_new_suffix_identity() {
    let original = BlockNode::doc(vec![
        attributed("beforeSELECTafter", "original"),
        attributed("outside", "outside"),
    ]);
    let mut doc = original.clone();
    let mut opts = options();
    opts.split_tail_attrs = Some(attributed("", "fresh-tail").attrs);
    let heading = BlockNode::heading(2, "template");
    let quote = BlockNode::with_children(
        NodeKind::Blockquote,
        vec![Child::Block(attributed("nested", "fresh-inner"))],
    );
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0], 6, &[0, 0], 12),
        RichFragment::Blocks(vec![heading.clone(), quote.clone()]),
        opts,
    )
    .unwrap();
    assert_eq!(
        doc,
        BlockNode::doc(vec![
            attributed("before", "original"),
            heading,
            quote,
            attributed("after", "fresh-tail"),
            attributed("outside", "outside"),
        ])
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![2, 0, 0], 6))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn sibling_block_selection_preserves_boundary_attrs_and_nested_template() {
    let original = BlockNode::doc(vec![
        attributed("left xx", "left"),
        BlockNode::paragraph("selected"),
        attributed("yy right", "right"),
    ]);
    let mut doc = original.clone();
    let template = BlockNode::with_children(
        NodeKind::BulletList,
        vec![Child::Block(BlockNode::with_children(
            NodeKind::ListItem,
            vec![Child::Block(BlockNode::paragraph("item"))],
        ))],
    );
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[2, 0], 3, &[0, 0], 5),
        RichFragment::Blocks(vec![template.clone()]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc,
        BlockNode::doc(vec![
            attributed("left ", "left"),
            template,
            attributed("right", "right")
        ])
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![1, 0, 0, 0], 4))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn inline_fragment_can_replace_across_matching_sibling_blocks() {
    let original = BlockNode::doc(vec![
        BlockNode::paragraph("abc"),
        BlockNode::paragraph("def"),
    ]);
    let mut doc = original.clone();
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0], 1, &[1, 0], 2),
        RichFragment::Inline(vec![text("X", vec![Mark::Bold])]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc,
        BlockNode::doc(vec![BlockNode::with_children(
            NodeKind::Paragraph,
            vec![
                text("a", vec![]),
                text("X", vec![Mark::Bold]),
                text("f", vec![]),
            ]
        )])
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 1], 1))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn empty_inline_fragment_deletes_the_range_in_one_edit() {
    let original = BlockNode::doc(vec![BlockNode::paragraph("abcd")]);
    let mut state = DocumentEditState::new(original.clone());
    let commit = state
        .edit(0, |doc, history| {
            let result = replace_selection_with_fragment(
                doc,
                &range(&[0, 0], 1, &[0, 0], 3),
                RichFragment::Inline(vec![]),
                options(),
            )?;
            history.push(result.receipt);
            Ok::<_, FragmentReplaceError>(result.selection)
        })
        .unwrap();
    assert!(commit.content_changed);
    assert_eq!(commit.edit_revision, 1);
    assert_eq!(state.history().len(), 1);
    assert_eq!(
        commit.output,
        Selection::caret(DocPosition::new(vec![0, 0], 1))
    );
    assert_eq!(
        state.document().children[0].as_block().unwrap().children,
        vec![text("a", vec![]), text("d", vec![])]
    );
    state.edit(1, |doc, history| history.undo(doc)).unwrap();
    assert_eq!(state.document(), &original);
}

#[test]
fn whole_node_replacement_and_atom_selection_use_the_exact_inserted_path() {
    let original = BlockNode::doc(vec![
        BlockNode::paragraph("first"),
        BlockNode::paragraph("replace"),
        BlockNode::paragraph("last"),
    ]);
    let mut doc = original.clone();
    let atom = BlockNode::new(NodeKind::HorizontalRule);
    let result = replace_selection_with_fragment(
        &mut doc,
        &Selection::node(vec![1]),
        RichFragment::Blocks(vec![BlockNode::heading(1, "heading"), atom.clone()]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc,
        BlockNode::doc(vec![
            BlockNode::paragraph("first"),
            BlockNode::heading(1, "heading"),
            atom,
            BlockNode::paragraph("last")
        ])
    );
    assert_eq!(result.selection, Selection::node(vec![2]));
    assert_undo_redo(&original, &mut doc, result.receipt);
}

#[test]
fn trailing_inline_atom_in_block_fragment_does_not_place_caret_before_it() {
    let mut doc = BlockNode::doc(vec![BlockNode::paragraph("replace")]);
    let result = replace_selection_with_fragment(
        &mut doc,
        &Selection::node(vec![0]),
        RichFragment::Blocks(vec![BlockNode::with_children(
            NodeKind::Paragraph,
            vec![text("a", vec![]), link()],
        )]),
        options(),
    )
    .unwrap();
    assert_eq!(result.selection, Selection::node(vec![0]));
}

#[test]
fn block_fragment_at_start_does_not_require_or_replace_surviving_suffix_identity() {
    let mut doc = BlockNode::doc(vec![attributed("keep", "original")]);
    replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0], 0, &[0, 0], 0),
        RichFragment::Blocks(vec![
            BlockNode::paragraph("one"),
            BlockNode::paragraph("two"),
        ]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc.children[2],
        Child::Block(attributed("keep", "original"))
    );
}

#[test]
fn invalid_positions_fragments_and_identity_preconditions_leave_document_unchanged() {
    let original = BlockNode::doc(vec![attributed("abc", "id")]);
    let cases = vec![
        (
            range(&[0, 0], 99, &[0, 0], 99),
            RichFragment::Inline(vec![text("x", vec![])]),
        ),
        (range(&[8, 0], 0, &[8, 0], 0), RichFragment::Inline(vec![])),
        (
            Selection::node(vec![]),
            RichFragment::Blocks(vec![BlockNode::paragraph("x")]),
        ),
        (
            Selection::node(vec![0, 0]),
            RichFragment::Blocks(vec![BlockNode::paragraph("x")]),
        ),
        (Selection::node(vec![0]), RichFragment::Inline(vec![])),
        (Selection::node(vec![0]), RichFragment::Blocks(vec![])),
        (
            Selection::node(vec![0]),
            RichFragment::Blocks(vec![BlockNode::doc(vec![])]),
        ),
        (
            range(&[0, 0], 1, &[0, 0], 2),
            RichFragment::Blocks(vec![BlockNode::paragraph("x")]),
        ),
        (
            range(&[0, 0], 1, &[0, 0], 2),
            RichFragment::Inline(vec![Child::Block(BlockNode::paragraph("invalid inline"))]),
        ),
    ];
    for (selection, fragment) in cases {
        let mut doc = original.clone();
        assert!(
            replace_selection_with_fragment(&mut doc, &selection, fragment, options()).is_err()
        );
        assert_eq!(doc, original);
    }
}

#[test]
fn incompatible_inline_boundaries_and_cross_container_selection_are_rejected() {
    let original = BlockNode::doc(vec![attributed("abc", "a"), attributed("def", "b")]);
    let mut doc = original.clone();
    assert_eq!(
        replace_selection_with_fragment(
            &mut doc,
            &range(&[0, 0], 1, &[1, 0], 1),
            RichFragment::Inline(vec![]),
            options()
        ),
        Err(FragmentReplaceError::IncompatibleInlineBoundaries)
    );
    assert_eq!(doc, original);
    let original = BlockNode::doc(vec![
        BlockNode::paragraph("abc"),
        BlockNode::with_children(
            NodeKind::Blockquote,
            vec![Child::Block(BlockNode::paragraph("nested"))],
        ),
    ]);
    let mut doc = original.clone();
    assert_eq!(
        replace_selection_with_fragment(
            &mut doc,
            &range(&[0, 0], 1, &[1, 0, 0], 1),
            RichFragment::Blocks(vec![BlockNode::paragraph("x")]),
            options()
        ),
        Err(FragmentReplaceError::CrossContainerRange)
    );
    assert_eq!(doc, original);
}

#[test]
fn schema_failure_after_structural_steps_rolls_back_and_records_no_history() {
    let original = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::BulletList,
        vec![Child::Block(BlockNode::with_children(
            NodeKind::ListItem,
            vec![Child::Block(BlockNode::paragraph("keep"))],
        ))],
    )]);
    let mut state = DocumentEditState::new(original.clone());
    let result = state.edit(0, |doc, history| {
        let result = replace_selection_with_fragment(
            doc,
            &Selection::node(vec![0, 0]),
            RichFragment::Blocks(vec![BlockNode::paragraph("not a list item")]),
            options(),
        )?;
        history.push(result.receipt);
        Ok::<_, FragmentReplaceError>(result.selection)
    });
    assert!(result.is_err());
    assert_eq!(state.document(), &original);
    assert_eq!(state.edit_revision(), 0);
    assert_eq!(state.history().len(), 0);
}

#[test]
fn nested_sibling_paragraphs_can_be_replaced_without_removing_their_container() {
    let original = BlockNode::doc(vec![BlockNode::with_children(
        NodeKind::Blockquote,
        vec![
            Child::Block(BlockNode::paragraph("abc")),
            Child::Block(BlockNode::paragraph("def")),
        ],
    )]);
    let mut doc = original.clone();
    let result = replace_selection_with_fragment(
        &mut doc,
        &range(&[0, 0, 0], 1, &[0, 1, 0], 2),
        RichFragment::Blocks(vec![BlockNode::heading(2, "x")]),
        options(),
    )
    .unwrap();
    assert_eq!(
        doc.children[0].as_block().unwrap().kind,
        NodeKind::Blockquote
    );
    assert_eq!(
        result.selection,
        Selection::caret(DocPosition::new(vec![0, 1, 0], 1))
    );
    assert_undo_redo(&original, &mut doc, result.receipt);
}
