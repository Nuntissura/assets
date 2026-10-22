    use super::*;
    use handshake_rich_document_model::node::{Child, HeadingLevel, NodeKind, TextLeaf};
    use handshake_rich_document_model::position::DocPosition;

    /// A test harness owning a doc + undo + selection so a command runs against real
    /// state and the result is asserted on the live tree.
    struct Editor {
        doc: BlockNode,
        undo: UndoManager,
        sel: Selection,
    }

    impl Editor {
        fn new(doc: BlockNode, sel: Selection) -> Self {
            Self {
                doc,
                undo: UndoManager::new(),
                sel,
            }
        }

        fn run(&mut self, cmd: FormattingCommand) -> Result<(), CommandError> {
            let mut ctx = CommandContext::new(&mut self.doc, &mut self.undo, &mut self.sel, "test");
            dispatch(&mut ctx, &cmd)
        }

        /// The top-level block at `idx`.
        fn block(&self, idx: usize) -> &BlockNode {
            self.doc.children[idx].as_block().unwrap()
        }
    }

    /// A doc of one paragraph with a single text leaf, caret at `offset`.
    fn one_para(text: &str, offset: usize) -> Editor {
        let doc = BlockNode::doc(vec![BlockNode::paragraph(text)]);
        Editor::new(doc, Selection::caret(DocPosition::new(vec![0, 0], offset)))
    }

    // ── AC-2: toggle bold idempotent (add then remove) ───────────────────────────────

    #[test]
    fn test_toggle_bold_idempotent() {
        // Selection over the whole "hello" leaf; first toggle adds bold, second removes it.
        let mut ed = Editor::new(
            BlockNode::doc(vec![BlockNode::paragraph("hello")]),
            Selection::text(
                DocPosition::new(vec![0, 0], 0),
                DocPosition::new(vec![0, 0], 5),
            ),
        );
        ed.run(FormattingCommand::ToggleBold).unwrap();
        assert!(
            ed.block(0).children[0]
                .as_text()
                .unwrap()
                .has_mark_type(&Mark::Bold),
            "first toggle adds bold"
        );
        ed.run(FormattingCommand::ToggleBold).unwrap();
        assert!(
            !ed.block(0).children[0]
                .as_text()
                .unwrap()
                .has_mark_type(&Mark::Bold),
            "second toggle removes bold (idempotent round-trip)"
        );
    }

    // ── MC-001 / RISK-1: full-range active check across a 3-leaf selection ─────────────

    #[test]
    fn toggle_bold_multi_leaf_active() {
        // A paragraph with three runs; ONLY the middle run is bold. A selection spanning
        // all three reports active=true, so the toggle REMOVES bold from the middle run.
        let para = BlockNode::with_children(
            NodeKind::Paragraph,
            vec![
                Child::Text(TextLeaf::new("aaa")),
                Child::Text(TextLeaf::with_marks("bbb", vec![Mark::Bold])),
                Child::Text(TextLeaf::new("ccc")),
            ],
        );
        let doc = BlockNode::doc(vec![para]);
        let sel = Selection::text(
            DocPosition::new(vec![0, 0], 0),
            DocPosition::new(vec![0, 2], 3),
        );
        // Active check: ANY overlapping leaf has bold -> true.
        assert!(
            is_mark_active(&doc, &sel, &Mark::Bold),
            "MC-001: a 3-leaf selection where only the middle leaf is bold is ACTIVE"
        );
        let mut ed = Editor::new(doc, sel);
        ed.run(FormattingCommand::ToggleBold).unwrap();
        // Because it was active, the toggle removed bold from every overlapping leaf.
        for i in 0..3 {
            assert!(
                !ed.block(0).children[i]
                    .as_text()
                    .unwrap()
                    .has_mark_type(&Mark::Bold),
                "leaf {i} must have bold removed after the active toggle"
            );
        }
    }

    // ── AC-5: set_heading changes node kind; active state reported ─────────────────────


    // ── MC-002: set_heading guards context (refuses inside a list item) ────────────────

    #[test]
    fn set_heading_refuses_inside_list_item() {
        // doc > bullet_list > list_item > paragraph("x"); caret in the paragraph.
        let para = BlockNode::paragraph("x");
        let item = BlockNode::with_children(NodeKind::ListItem, vec![Child::Block(para)]);
        let list = BlockNode::with_children(NodeKind::BulletList, vec![Child::Block(item)]);
        let doc = BlockNode::doc(vec![list]);
        // caret path: doc[0] list -> [0] item -> [0] para -> [0] text.
        let sel = Selection::caret(DocPosition::new(vec![0, 0, 0, 0], 0));
        let mut ed = Editor::new(doc, sel);
        // The caret's IMMEDIATE block is the paragraph, which IS convertible — but the MC-002
        // corruption case is converting the LIST ITEM. set_heading converts the paragraph in
        // place (a legal child of a list_item), so it succeeds here. The guard that matters is
        // refusing to convert a non-paragraph/heading block — proven by the code_block case.
        let r = ed.run(FormattingCommand::SetHeading(2));
        assert!(
            r.is_ok(),
            "converting a paragraph inside a list item is legal"
        );
        assert_eq!(
            ed.doc.children[0].as_block().unwrap().kind,
            NodeKind::BulletList
        );
    }

    #[test]
    fn set_heading_refuses_code_block() {
        // A caret inside a code block must NOT convert (MC-002 non-paragraph guard).
        let cb = BlockNode::with_children(
            NodeKind::CodeBlock,
            vec![Child::Text(TextLeaf::new("fn main(){}"))],
        );
        let doc = BlockNode::doc(vec![cb]);
        let sel = Selection::caret(DocPosition::new(vec![0, 0], 0));
        let mut ed = Editor::new(doc, sel);
        let err = ed.run(FormattingCommand::SetHeading(1)).unwrap_err();
        assert!(matches!(err, CommandError::InvalidContext { .. }));
        assert_eq!(
            ed.block(0).kind,
            NodeKind::CodeBlock,
            "code block unchanged"
        );
    }

    // ── AC-6: insert_table structure (2 rows x 3 cols) ─────────────────────────────────

    #[test]
    fn test_insert_table_structure() {
        let mut ed = one_para("para", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 3 })
            .unwrap();
        // The table is inserted AFTER the paragraph -> top-level index 1.
        let table = ed.block(1);
        assert_eq!(table.kind, NodeKind::Table);
        assert_eq!(table.children.len(), 2, "exactly 2 table_row children");
        for r in 0..2 {
            let row = table.children[r].as_block().unwrap();
            assert_eq!(row.kind, NodeKind::TableRow);
            assert_eq!(
                row.children.len(),
                3,
                "each row has exactly 3 table_cell children"
            );
            for c in 0..3 {
                let cell = row.children[c].as_block().unwrap();
                assert_eq!(cell.kind, NodeKind::TableCell);
                // Backend-shape anchor: each cell carries colspan/rowspan = 1.
                assert_eq!(
                    cell.attrs.get("colspan").and_then(JsonValue::as_i64),
                    Some(1)
                );
                assert_eq!(
                    cell.attrs.get("rowspan").and_then(JsonValue::as_i64),
                    Some(1)
                );
            }
            // First row cells are header cells (withHeaderRow parity).
            let is_header = row.children[0]
                .as_block()
                .unwrap()
                .attrs
                .get("isHeader")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            assert_eq!(is_header, r == 0, "row {r} header flag");
        }
    }

    // ── MC-003: insert_table refuses nesting ───────────────────────────────────────────

    #[test]
    fn insert_table_refuses_nesting() {
        let mut ed = one_para("para", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap();
        // The caret is now inside the first cell; a second insert_table must be refused.
        let err = ed
            .run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap_err();
        assert!(
            matches!(err, CommandError::InvalidContext { .. }),
            "MC-003: insert_table inside a table is refused"
        );
    }

    // ── AC-7: delete_row removes one row ───────────────────────────────────────────────

    #[test]
    fn delete_row_decrements_row_count() {
        let mut ed = one_para("para", 0);
        ed.run(FormattingCommand::InsertTable { rows: 3, cols: 2 })
            .unwrap();
        assert_eq!(ed.block(1).children.len(), 3);
        // Caret is in the first cell of row 0. delete_row removes that row.
        ed.run(FormattingCommand::DeleteRow).unwrap();
        assert_eq!(ed.block(1).children.len(), 2, "one row removed");
    }

    // ── AC-8: toggle_ordered_list round-trips paragraph <-> list ───────────────────────

    #[test]
    fn toggle_ordered_list_round_trip() {
        let mut ed = one_para("item text", 0);
        ed.run(FormattingCommand::ToggleOrderedList).unwrap();
        // doc[0] is now an ordered_list with one list_item holding the paragraph.
        let list = ed.block(0);
        assert_eq!(list.kind, NodeKind::OrderedList);
        let item = list.children[0].as_block().unwrap();
        assert_eq!(item.kind, NodeKind::ListItem);
        assert_eq!(
            item.children[0].as_block().unwrap().kind,
            NodeKind::Paragraph
        );
        // Toggling again converts back to a paragraph.
        ed.run(FormattingCommand::ToggleOrderedList).unwrap();
        assert_eq!(
            ed.block(0).kind,
            NodeKind::Paragraph,
            "toggled back to paragraph"
        );
        assert_eq!(
            ed.block(0).children[0].as_text().unwrap().text.to_string(),
            "item text"
        );
    }

    // ── AC-9: sink_list_item increases nesting; lift reverses it ───────────────────────

    #[test]
    fn sink_and_lift_list_item() {
        // Build a bullet list with TWO items; caret in the SECOND item so it can sink under
        // the first.
        let item = |t: &str| {
            BlockNode::with_children(
                NodeKind::ListItem,
                vec![Child::Block(BlockNode::paragraph(t))],
            )
        };
        let list = BlockNode::with_children(
            NodeKind::BulletList,
            vec![Child::Block(item("first")), Child::Block(item("second"))],
        );
        let doc = BlockNode::doc(vec![list]);
        // caret: list[0] -> item[1] -> para[0] -> text[0]  == path [0,1,0,0].
        let sel = Selection::caret(DocPosition::new(vec![0, 1, 0, 0], 0));
        let mut ed = Editor::new(doc, sel);

        ed.run(FormattingCommand::SinkListItem).unwrap();
        // After sink: the outer list has ONE item (the first), which now contains a nested
        // sub-list holding the sunk "second" item.
        let outer = ed.block(0);
        assert_eq!(outer.kind, NodeKind::BulletList);
        assert_eq!(outer.children.len(), 1, "the sunk item left the outer list");
        let first_item = outer.children[0].as_block().unwrap();
        // The first item now ends with a nested bullet_list.
        let sub = first_item
            .children
            .iter()
            .filter_map(Child::as_block)
            .find(|b| matches!(b.kind, NodeKind::BulletList));
        assert!(
            sub.is_some(),
            "first item gained a nested sub-list (indent)"
        );
        assert_eq!(
            sub.unwrap().children.len(),
            1,
            "the nested list holds the sunk item"
        );

        // Now lift the nested item back out.
        ed.run(FormattingCommand::LiftListItem).unwrap();
        let outer = ed.block(0);
        assert_eq!(
            outer.children.len(),
            2,
            "lift returned the item to the outer list (2 items again)"
        );
    }

    // ── AC-4: undo restores the previous doc state ─────────────────────────────────────

    #[test]
    fn undo_restores_pre_bold_state() {
        let mut ed = Editor::new(
            BlockNode::doc(vec![BlockNode::paragraph("hello")]),
            Selection::text(
                DocPosition::new(vec![0, 0], 0),
                DocPosition::new(vec![0, 0], 5),
            ),
        );
        let before = ed.doc.clone();
        ed.run(FormattingCommand::ToggleBold).unwrap();
        assert!(ed.block(0).children[0]
            .as_text()
            .unwrap()
            .has_mark_type(&Mark::Bold));
        ed.run(FormattingCommand::Undo).unwrap();
        assert_eq!(ed.doc, before, "AC-4: undo restores the pre-bold doc");
    }

    #[test]
    fn undo_restores_pre_heading_state() {
        let mut ed = one_para("x", 0);
        let before = ed.doc.clone();
        ed.run(FormattingCommand::SetHeading(2)).unwrap();
        assert_eq!(ed.block(0).kind, NodeKind::Heading(HeadingLevel::new(2)));
        ed.run(FormattingCommand::Undo).unwrap();
        assert_eq!(
            ed.doc, before,
            "undo restores the paragraph (heading reverted)"
        );
    }

    // ── scope expansion: Enter split + Backspace merge + undo ──────────────────────────

    #[test]
    fn enter_splits_backspace_merges_undo_restores() {
        // "helloworld" with caret after "hello" (offset 5). Enter splits into two paragraphs.
        let mut ed = one_para("helloworld", 5);
        let before = ed.doc.clone();
        ed.run(FormattingCommand::InsertParagraphBreak).unwrap();
        assert_eq!(ed.doc.children.len(), 2, "Enter split into two blocks");
        assert_eq!(
            ed.block(0).children[0].as_text().unwrap().text.to_string(),
            "hello"
        );
        assert_eq!(
            ed.block(1).children[0].as_text().unwrap().text.to_string(),
            "world"
        );
        // Caret moved to the start of the new (tail) block.
        if let Selection::Text { head, .. } = &ed.sel {
            assert_eq!(head.path, vec![1, 0]);
            assert_eq!(head.char_offset, 0);
        } else {
            panic!("expected a caret after split");
        }

        // Backspace at the start of the second block merges it back.
        ed.run(FormattingCommand::MergeBackward).unwrap();
        assert_eq!(ed.doc.children.len(), 1, "Backspace-at-0 merged the blocks");
        assert_eq!(
            ed.block(0).children[0].as_text().unwrap().text.to_string(),
            "helloworld"
        );
        // Caret at the join point (offset 5 within the merged block).
        if let Selection::Text { head, .. } = &ed.sel {
            assert_eq!(head.char_offset, 5, "caret at the join point");
        }

        // Undo the merge, then undo the split: back to the original single block.
        ed.run(FormattingCommand::Undo).unwrap();
        assert_eq!(
            ed.doc.children.len(),
            2,
            "undo of merge restores two blocks"
        );
        ed.run(FormattingCommand::Undo).unwrap();
        assert_eq!(ed.doc, before, "undo of split restores the original doc");
    }

    #[test]
    fn merge_backward_no_op_when_no_previous_sibling() {
        // Caret at offset 0 of the FIRST block -> no previous sibling -> benign no-op.
        let mut ed = one_para("only", 0);
        let before = ed.doc.clone();
        ed.run(FormattingCommand::MergeBackward).unwrap();
        assert_eq!(ed.doc, before, "merge at the first block is a no-op");
    }

    // ── task list: checked attr round-trips backend shape ──────────────────────────────

    #[test]
    fn toggle_task_list_and_checked_attr() {
        let mut ed = one_para("buy milk", 0);
        ed.run(FormattingCommand::ToggleTaskList).unwrap();
        // doc[0] is a bullet_list holding a task_item with checked=false.
        let list = ed.block(0);
        let item = list.children[0].as_block().unwrap();
        assert_eq!(item.kind, NodeKind::TaskItem);
        assert!(!item.task_checked(), "new task item starts unchecked");
        // Caret should now be inside the task item's paragraph; toggle the checked attr.
        ed.run(FormattingCommand::ToggleTaskItemChecked).unwrap();
        let item = ed.block(0).children[0].as_block().unwrap();
        assert!(
            item.task_checked(),
            "toggle_task_item_checked sets checked=true"
        );
    }

    // ── code block: set_code_block carries language, flattens marks ────────────────────

    #[test]
    fn set_code_block_carries_language() {
        let mut ed = one_para("let x = 1;", 0);
        ed.run(FormattingCommand::SetCodeBlock(Some("rust".to_string())))
            .unwrap();
        let cb = ed.block(0);
        assert_eq!(cb.kind, NodeKind::CodeBlock);
        assert_eq!(
            cb.attrs.get("language").and_then(JsonValue::as_str),
            Some("rust")
        );
        assert_eq!(
            cb.children[0].as_text().unwrap().text.to_string(),
            "let x = 1;"
        );
    }

    // ── blockquote wrap/unwrap ──────────────────────────────────────────────────────────

    #[test]
    fn blockquote_wrap_then_unwrap() {
        let mut ed = one_para("quoted", 0);
        ed.run(FormattingCommand::SetBlockquote).unwrap();
        assert_eq!(ed.block(0).kind, NodeKind::Blockquote);
        assert_eq!(
            ed.block(0).children[0].as_block().unwrap().kind,
            NodeKind::Paragraph
        );
        ed.run(FormattingCommand::SetBlockquote).unwrap();
        assert_eq!(
            ed.block(0).kind,
            NodeKind::Paragraph,
            "unwrapped back to paragraph"
        );
    }

    // ── horizontal rule + DocJson round-trip of a built table (backend shape) ──────────

    #[test]
    fn insert_table_round_trips_doc_json() {
        use handshake_rich_document_model::doc_json::{from_json_string, to_json_string};
        let mut ed = one_para("p", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap();
        // The built table must serialize and deserialize without an UnknownNodeType error
        // (backend node-shape anchor: table/tableRow/tableCell are known Tiptap types).
        let json = to_json_string(&ed.doc).expect("table serializes to content_json");
        let back = from_json_string(&json).expect("table round-trips from content_json");
        assert_eq!(
            ed.doc, back,
            "the inserted table round-trips through DocJson"
        );
    }

    #[test]
    fn insert_horizontal_rule_adds_atom_block() {
        let mut ed = one_para("above", 5);
        ed.run(FormattingCommand::InsertHorizontalRule).unwrap();
        assert_eq!(ed.doc.children.len(), 2);
        assert_eq!(ed.block(1).kind, NodeKind::HorizontalRule);
    }

    // ── add row before/after + add/delete column ───────────────────────────────────────

    #[test]
    fn add_row_and_column_operations() {
        let mut ed = one_para("p", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap();
        // Caret in first cell. Add a row after -> 3 rows.
        ed.run(FormattingCommand::AddRowAfter).unwrap();
        assert_eq!(ed.block(1).children.len(), 3);
        // Add a column after -> each row has 3 cells.
        ed.run(FormattingCommand::AddColAfter).unwrap();
        for r in 0..ed.block(1).children.len() {
            assert_eq!(
                ed.block(1).children[r].as_block().unwrap().children.len(),
                3,
                "row {r} now has 3 columns"
            );
        }
        // Delete a column -> back to 2 cells per row.
        ed.run(FormattingCommand::DeleteCol).unwrap();
        for r in 0..ed.block(1).children.len() {
            assert_eq!(
                ed.block(1).children[r].as_block().unwrap().children.len(),
                2,
                "row {r} back to 2 columns"
            );
        }
    }

    #[test]
    fn delete_table_removes_it() {
        let mut ed = one_para("p", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap();
        assert_eq!(ed.doc.children.len(), 2);
        ed.run(FormattingCommand::DeleteTable).unwrap();
        // Only the original paragraph remains.
        assert_eq!(ed.doc.children.len(), 1);
        assert_eq!(ed.block(0).kind, NodeKind::Paragraph);
    }

    #[test]
    fn toggle_header_row_flips_first_row_cells() {
        let mut ed = one_para("p", 0);
        ed.run(FormattingCommand::InsertTable { rows: 2, cols: 2 })
            .unwrap();
        // First row starts as header (withHeaderRow). Toggle off -> no isHeader.
        ed.run(FormattingCommand::ToggleHeaderRow).unwrap();
        let first_row = ed.block(1).children[0].as_block().unwrap();
        for c in 0..first_row.children.len() {
            assert!(
                !first_row.children[c]
                    .as_block()
                    .unwrap()
                    .attrs
                    .contains_key("isHeader"),
                "header flag cleared on cell {c}"
            );
        }
    }

    #[test]
    fn table_edit_outside_table_errors() {
        let mut ed = one_para("not a table", 0);
        assert!(matches!(
            ed.run(FormattingCommand::DeleteRow).unwrap_err(),
            CommandError::InvalidContext { .. }
        ));
        assert!(matches!(
            ed.run(FormattingCommand::AddRowAfter).unwrap_err(),
            CommandError::InvalidContext { .. }
        ));
    }
