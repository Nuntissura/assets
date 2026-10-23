//! Outline / symbol tree derived from the tree-sitter parse tree (WP-KERNEL-012 MT-006, E1 — VS Code
//! parity).
//!
//! The outline (Monaco's "Outline" / VS Code's `ED-NAV-004` heading tree) lists the named symbols of a
//! file — functions, methods, classes, structs, enums, constants, modules — with their buffer line and
//! nesting depth, so an operator (or a swarm agent) can jump to a symbol by clicking it. Symbols are
//! identified from tree-sitter node KINDS (the SAME language-aware syntax tree MT-001's highlighter
//! builds and MT-005's folding walks), not from regexes, so the result is grammar-accurate.
//!
//! ## Shared traversal with folding (MC-002 / RISK-002)
//!
//! [`FoldProvider::compute`](super::folding::FoldProvider::compute) and [`OutlineProvider::compute`]
//! both walk the highlighter's cached tree (via the host highlighter tree)
//! with an iterative `tree_sitter::TreeCursor` pre-order DFS — the same walk shape, so they share the
//! traversal *pattern* and the same single parse tree. The panel recomputes BOTH only when the buffer
//! version changes (it never re-parses for the outline: it reuses the tree the highlighter already
//! built). A unified `TreeAnalyzer` that returns `{fold_regions, outline_items}` from one walk is the
//! documented next step (MC-002); this MT keeps the two providers separate but pins them to the SAME
//! tree + cursor pattern so there is no third independent parse.
//!
//! ## Depth / indent (MT step 2)
//!
//! `indent` is the OUTLINE nesting depth, NOT the source's whitespace indentation: it counts how many
//! ANCESTOR outline-symbol nodes enclose this symbol (a method inside an `impl`/`class` is indent 1; a
//! top-level function is indent 0). Tracked by pushing the cursor depth onto a stack when an
//! outline-producing node is entered and popping it when the walk climbs back out, so the depth reflects
//! the symbol hierarchy the tree expresses.

use handshake_text_mechanics::buffer::TextBuffer;

/// Max characters of a symbol name before it is truncated (defensive against a pathological
/// identifier). Real identifiers are far shorter; this only guards a degenerate input.
const MAX_SYMBOL_NAME_LEN: usize = 120;

/// The kind of a symbol in the outline tree. A small, stable, language-agnostic set (the contract's
/// vocabulary) that every grammar's node kinds fold into — so the outline panel renders one consistent
/// icon/label set regardless of language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutlineKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Constant,
    Module,
}

impl OutlineKind {
    /// A short, stable label prefix for the kind (used by the panel + the AccessKit value so a
    /// no-context model can read the symbol kind without a separate lookup).
    pub fn label(self) -> &'static str {
        match self {
            OutlineKind::Function => "fn",
            OutlineKind::Method => "method",
            OutlineKind::Class => "class",
            OutlineKind::Struct => "struct",
            OutlineKind::Enum => "enum",
            OutlineKind::Constant => "const",
            OutlineKind::Module => "mod",
        }
    }
}

/// One symbol in the outline: its kind, display name, 0-based buffer line, and outline nesting depth.
///
/// `line` is the buffer line the symbol's defining node starts on (the line click-to-scroll navigates
/// to). `indent` is the OUTLINE depth (how many ancestor symbols enclose it), NOT source whitespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    /// The symbol kind (function/method/class/struct/enum/constant/module).
    pub kind: OutlineKind,
    /// The symbol's display name (the identifier text from the tree). Empty only when the grammar node
    /// has no resolvable name child (rare — such nodes are skipped, so an emitted item always has a name).
    pub name: String,
    /// 0-based buffer line the symbol starts on (`on_navigate(line)` scrolls here).
    pub line: usize,
    /// Outline nesting depth: 0 for a top-level symbol, 1 for a symbol inside one enclosing symbol, etc.
    pub indent: usize,
}

/// The per-language mapping of tree-sitter node KINDS to an [`OutlineKind`] plus the child field/kind
/// that carries the symbol's NAME. Keyed by the stable language-family id (`"rust"` / `"javascript"`),
/// the same ids the highlighter + folding use, so adding a language is a one-table edit.
///
/// A node kind not in the active language's table produces no outline item (the traversal simply
/// descends into it). The kind strings are the grammars' OWN node-type names (verified against
/// `tree-sitter-rust` 0.24 / `tree-sitter-javascript` 0.25 — the grammars MT-001 bundles), so they
/// match `Node::kind()` exactly.
pub struct OutlineNodeTypes;

/// How a symbol's NAME is found from its defining node. tree-sitter exposes a named child either by a
/// FIELD name (`child_by_field_name("name")`) or, for grammars without that field, by the first child
/// of a given KIND. Both strategies are tried so the table works across the bundled grammars.
#[derive(Clone, Copy, Debug)]
struct SymbolRule {
    /// The node kind that defines this symbol (matched against `Node::kind()`).
    node_kind: &'static str,
    /// The outline kind the symbol maps to.
    outline_kind: OutlineKind,
    /// The grammar field name carrying the symbol's identifier (most grammars expose `"name"`), tried
    /// first via `child_by_field_name`.
    name_field: &'static str,
}

/// Rust symbol rules (MT contract vocabulary). `function_item` -> Function, `impl_item`/`trait_item`
/// -> Class (the type-grouping construct), `struct_item` -> Struct, `enum_item` -> Enum, `mod_item`
/// -> Module, `const_item`/`static_item` -> Constant. `tree-sitter-rust` exposes the identifier on the
/// `"name"` field for most items; `impl_item` has no `name` field — it exposes the implemented type on
/// the `"type"` field, so its rule's `name_field` is `"type"` (and `symbol_name` also falls back to
/// `"type"` for any rule whose `name_field` is absent).
const RUST_RULES: &[SymbolRule] = &[
    SymbolRule {
        node_kind: "function_item",
        outline_kind: OutlineKind::Function,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "struct_item",
        outline_kind: OutlineKind::Struct,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "enum_item",
        outline_kind: OutlineKind::Enum,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "mod_item",
        outline_kind: OutlineKind::Module,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "trait_item",
        outline_kind: OutlineKind::Class,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "impl_item",
        outline_kind: OutlineKind::Class,
        name_field: "type",
    },
    SymbolRule {
        node_kind: "const_item",
        outline_kind: OutlineKind::Constant,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "static_item",
        outline_kind: OutlineKind::Constant,
        name_field: "name",
    },
];

/// JS/TS symbol rules (MT contract vocabulary). `function_declaration` -> Function,
/// `method_definition` -> Method, `class_declaration` -> Class. `tree-sitter-javascript` exposes the
/// identifier on the `"name"` field for these.
const JS_RULES: &[SymbolRule] = &[
    SymbolRule {
        node_kind: "function_declaration",
        outline_kind: OutlineKind::Function,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "method_definition",
        outline_kind: OutlineKind::Method,
        name_field: "name",
    },
    SymbolRule {
        node_kind: "class_declaration",
        outline_kind: OutlineKind::Class,
        name_field: "name",
    },
];

impl OutlineNodeTypes {
    /// The symbol rules for `language_id` (or an empty slice for an unknown language).
    fn rules_for(language_id: &str) -> &'static [SymbolRule] {
        match language_id {
            "rust" => RUST_RULES,
            "javascript" => JS_RULES,
            _ => &[],
        }
    }
}

/// Extracts the outline symbol list from a tree-sitter parse tree. Stateless; the caller (the panel)
/// re-runs [`compute`](OutlineProvider::compute) only when the buffer version changes (MT step / MC-002),
/// reusing the highlighter's cached tree (no second parse).
pub struct OutlineProvider;

impl OutlineProvider {
    /// Walk `tree` and return the outline symbols for `language_id`, in source order.
    ///
    /// Algorithm (mirrors [`FoldProvider::compute`](super::folding::FoldProvider::compute) — MC-002):
    /// 1. Iterative pre-order DFS with a `tree_sitter::TreeCursor` (`tree.walk()`), NOT recursion, so a
    ///    deeply nested tree cannot overflow the stack.
    /// 2. A node contributes an [`OutlineItem`] iff its kind is in `language_id`'s symbol table AND a
    ///    name can be resolved for it.
    /// 3. `indent` is the count of ANCESTOR outline-symbol nodes on the cursor path (an outline-depth
    ///    stack, pushed on entering a symbol node and popped when the walk climbs out of it).
    /// 4. Every line is CLAMPED to the live buffer (RISK / stale-tree guard) so a tree that lags a fast
    ///    edit by one frame can never index past the buffer.
    ///
    /// `language_id` selects the symbol table; an unknown language yields no items.
    pub fn compute(
        tree: &tree_sitter::Tree,
        buffer: &TextBuffer,
        language_id: &str,
    ) -> Vec<OutlineItem> {
        let rules = OutlineNodeTypes::rules_for(language_id);
        if rules.is_empty() {
            return Vec::new();
        }
        let max_line = buffer.len_lines().saturating_sub(1);
        let source = buffer.to_bytes();
        let mut items: Vec<OutlineItem> = Vec::new();

        // The outline-depth stack: each entry is the cursor `depth()` at which an enclosing outline
        // symbol was entered. The current `indent` is the stack length (how many symbols enclose us).
        // Entries are popped when the walk climbs back to or above their depth (the symbol's subtree is
        // done), which keeps the depth correct as siblings are visited.
        let mut symbol_depths: Vec<usize> = Vec::new();

        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            let cur_depth = cursor.depth() as usize;

            // Pop any enclosing-symbol depths we have climbed back out of (sibling / parent moves).
            while symbol_depths.last().is_some_and(|&d| d >= cur_depth) {
                symbol_depths.pop();
            }

            if let Some(rule) = rules.iter().find(|r| r.node_kind == node.kind()) {
                if let Some(name) = Self::symbol_name(&node, rule, &source) {
                    let line = node.start_position().row.min(max_line);
                    let indent = symbol_depths.len();
                    items.push(OutlineItem {
                        kind: rule.outline_kind,
                        name,
                        line,
                        indent,
                    });
                    // This node encloses any nested symbols found deeper in its subtree.
                    symbol_depths.push(cur_depth);
                }
            }

            // Advance the cursor in pre-order: descend, else next sibling, else climb until a sibling
            // exists; stop when we climb back out of the root.
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return items; // climbed out of the root — traversal complete.
                }
            }
        }
    }

    /// Resolve a symbol's display name from its defining `node`. Tries the rule's `name_field` first
    /// (`child_by_field_name`); for a Rust `impl_item` (which has no `name` field — it names the
    /// implemented TYPE) falls back to the `"type"` field; finally falls back to the first `identifier`/
    /// `type_identifier` child. Returns `None` (so the symbol is skipped) when no name resolves, so an
    /// emitted item always carries a non-empty name. Char-boundary-safe truncation on a pathological
    /// length.
    fn symbol_name(node: &tree_sitter::Node, rule: &SymbolRule, source: &[u8]) -> Option<String> {
        // Primary: the declared name field.
        let name_node = node
            .child_by_field_name(rule.name_field)
            // impl blocks name a type, not a `name` field.
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| {
                // Fallback: the first identifier-like named child. Bind the child to a local so
                // `walker` is not borrowed past the end of the closure expression (the cursor is a
                // temporary that must outlive the returned `Node`).
                let mut walker = node.walk();
                let found = node.children(&mut walker).find(|c| {
                    matches!(
                        c.kind(),
                        "identifier"
                            | "type_identifier"
                            | "property_identifier"
                            | "field_identifier"
                    )
                });
                found
            })?;
        // Read the identifier text from the source by its BYTE range, guarding against a STALE tree
        // whose node spans past the (now shorter) live source — `Node::utf8_text` panics on an
        // out-of-range slice, so clamp the range to the live source and bail if it does not land on a
        // valid UTF-8 boundary (RISK / stale-tree guard, the same discipline FoldProvider applies to
        // its rows). `source` is the LIVE buffer bytes.
        let range = name_node.byte_range();
        if range.start >= source.len() {
            return None; // node is entirely past the live source (stale tree after a fast delete).
        }
        let end = range.end.min(source.len());
        let text = std::str::from_utf8(source.get(range.start..end)?).ok()?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Char-boundary-safe cap (never split a multi-byte glyph — the buffer-layer discipline).
        Some(trimmed.chars().take(MAX_SYMBOL_NAME_LEN).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `src` as Rust (reusing the MT-001 highlighter's grammar so the test exercises the real
    /// grammar, not a stub).
    fn rust_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("rust language set");
        parser.parse(src, None).expect("rust parse")
    }

    fn js_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("js language set");
        parser.parse(src, None).expect("js parse")
    }

    // ── AC-001: a 20-line Rust file with two functions -> 2 Function OutlineItems, correct names+lines ──

    const RUST_TWO_FNS: &str = "\
// a module of two functions
fn alpha(x: i32) -> i32 {
    let y = x + 1;
    y * 2
}

fn beta(input: &str) -> usize {
    let trimmed = input.trim();
    let length = trimmed.len();
    length
}
";

    #[test]
    fn outline_provider_finds_two_functions_with_names_and_lines() {
        let tree = rust_tree(RUST_TWO_FNS);
        let buffer = TextBuffer::new(RUST_TWO_FNS);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");

        let fns: Vec<&OutlineItem> = items
            .iter()
            .filter(|i| i.kind == OutlineKind::Function)
            .collect();
        assert_eq!(
            fns.len(),
            2,
            "AC-001: a file with two functions yields exactly 2 Function items; got {items:?}"
        );
        // Names + lines are correct and in source order.
        assert_eq!(fns[0].name, "alpha", "first function name");
        assert_eq!(
            fns[0].line, 1,
            "first function on line 1 (after the comment line 0)"
        );
        assert_eq!(fns[1].name, "beta", "second function name");
        assert_eq!(fns[1].line, 6, "second function on line 6");
        // Both are top-level -> indent 0.
        assert!(
            fns.iter().all(|f| f.indent == 0),
            "top-level functions are indent 0"
        );
    }

    #[test]
    fn outline_provider_nested_method_has_indent_one() {
        // A struct + an impl block containing two methods: the methods nest under the impl (indent 1).
        let src = "\
struct Widget {
    count: i32,
}

impl Widget {
    fn new() -> Self {
        Widget { count: 0 }
    }
    fn increment(&mut self) {
        self.count += 1;
    }
}
";
        let tree = rust_tree(src);
        let buffer = TextBuffer::new(src);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");

        // The struct is a top-level Struct.
        let strukt = items
            .iter()
            .find(|i| i.kind == OutlineKind::Struct)
            .expect("struct item");
        assert_eq!(strukt.name, "Widget");
        assert_eq!(strukt.indent, 0, "struct is top-level");

        // The impl block is a Class (type-grouping construct) named after the type, indent 0.
        let impl_item = items
            .iter()
            .find(|i| i.kind == OutlineKind::Class && i.name == "Widget")
            .expect("impl block item named after the type");
        assert_eq!(impl_item.indent, 0, "impl block is top-level");

        // The two functions are INSIDE the impl -> indent 1 (one enclosing outline symbol).
        let methods: Vec<&OutlineItem> = items
            .iter()
            .filter(|i| {
                i.kind == OutlineKind::Function && (i.name == "new" || i.name == "increment")
            })
            .collect();
        assert_eq!(
            methods.len(),
            2,
            "two functions inside the impl; got {items:?}"
        );
        assert!(
            methods.iter().all(|m| m.indent == 1),
            "functions inside the impl are indent 1 (nested under the impl); got {items:?}"
        );
    }

    #[test]
    fn outline_provider_struct_enum_const_mod_kinds() {
        let src = "\
mod config {
    const MAX: usize = 10;
}

enum Color {
    Red,
    Green,
}
";
        let tree = rust_tree(src);
        let buffer = TextBuffer::new(src);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");

        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Module && i.name == "config"),
            "mod -> Module; got {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Constant && i.name == "MAX" && i.indent == 1),
            "const inside mod -> Constant at indent 1; got {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Enum && i.name == "Color" && i.indent == 0),
            "enum -> Enum at indent 0; got {items:?}"
        );
    }

    #[test]
    fn outline_provider_js_function_class_method() {
        let src = "\
function greet(name) {
    return \"hi \" + name;
}

class Counter {
    increment() {
        this.count += 1;
    }
}
";
        let tree = js_tree(src);
        let buffer = TextBuffer::new(src);
        let items = OutlineProvider::compute(&tree, &buffer, "javascript");

        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Function && i.name == "greet"),
            "JS function_declaration -> Function; got {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Class && i.name == "Counter"),
            "JS class_declaration -> Class; got {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|i| i.kind == OutlineKind::Method && i.name == "increment" && i.indent == 1),
            "JS method_definition inside a class -> Method at indent 1; got {items:?}"
        );
    }

    #[test]
    fn outline_provider_unknown_language_is_empty() {
        let tree = rust_tree(RUST_TWO_FNS);
        let buffer = TextBuffer::new(RUST_TWO_FNS);
        let items = OutlineProvider::compute(&tree, &buffer, "cobol");
        assert!(
            items.is_empty(),
            "an unregistered language yields no outline items"
        );
    }

    #[test]
    fn outline_provider_empty_buffer_is_empty() {
        let tree = rust_tree("");
        let buffer = TextBuffer::new("");
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        assert!(items.is_empty(), "an empty buffer yields no outline items");
    }

    #[test]
    fn outline_provider_clamps_lines_to_a_shorter_buffer() {
        // Parse a long source, then compute against a SHORTER buffer (a fast delete that shrank the
        // buffer before the tree re-parsed). No item line may exceed the live buffer's last line.
        let tree = rust_tree(RUST_TWO_FNS);
        let short = TextBuffer::new("fn x() {}\n"); // 2 lines incl. trailing
        let max_line = short.len_lines().saturating_sub(1);
        let items = OutlineProvider::compute(&tree, &short, "rust");
        for item in &items {
            assert!(
                item.line <= max_line,
                "line {} clamped to {}",
                item.line,
                max_line
            );
        }
    }

    #[test]
    fn outline_kind_labels_are_stable() {
        assert_eq!(OutlineKind::Function.label(), "fn");
        assert_eq!(OutlineKind::Method.label(), "method");
        assert_eq!(OutlineKind::Class.label(), "class");
        assert_eq!(OutlineKind::Struct.label(), "struct");
        assert_eq!(OutlineKind::Enum.label(), "enum");
        assert_eq!(OutlineKind::Constant.label(), "const");
        assert_eq!(OutlineKind::Module.label(), "mod");
    }

}
