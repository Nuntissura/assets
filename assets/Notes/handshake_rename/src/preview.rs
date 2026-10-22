//! Workspace-edit preview models and construction (WP-KERNEL-012 MT-048).

use std::ops::Range;

use handshake_text_mechanics::buffer::TextBuffer;

use crate::apply::{apply_text_edits_to_string, byte_range_to_lsp_range, uri_to_path};

/// A half-open LSP-style line/character range (0-based), the wire form an LSP `TextEdit` carries. Kept
/// as its own type (rather than reusing `lsp_types::Range`) so the preview model is transport-agnostic:
/// both the LSP path and the no-LSP tree-sitter fallback produce the SAME [`LspRange`]/[`TextEdit`]
/// shape and the renderer/apply path never branches on origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRange {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
}

impl LspRange {
    /// Build from an `lsp_types::Range`.
    pub fn from_lsp(range: lsp_types::Range) -> Self {
        Self {
            start_line: range.start.line,
            start_char: range.start.character,
            end_line: range.end.line,
            end_char: range.end.character,
        }
    }
}

/// One edit to apply: a line/character RANGE plus the replacement text. The rename produces one of these
/// per occurrence (the new identifier text replacing the old occurrence's range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: LspRange,
    pub new_text: String,
}

/// One before/after diff row shown in the preview (ported visually from the React `CodeSymbolPanel`
/// references list). `line` is the 0-based line the edit touches; `before`/`after` are that whole line
/// before and after the edit so the operator sees the change in context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewHunk {
    pub line: usize,
    pub before: String,
    pub after: String,
}

/// All edits to a single file, plus whether that file is an open in-memory buffer or a to-disk file, and
/// the before/after hunks the preview renders. Built BEFORE any mutation happens (AC-004 — nothing is
/// applied until the operator clicks Apply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEditPreview {
    /// The document URI (or path) the edits apply to.
    pub uri: String,
    /// True when this file is currently an open in-memory buffer (applied to the `TextBuffer`); false
    /// when it is a to-disk file (read -> apply -> atomic write).
    pub is_open_buffer: bool,
    /// The edits for this file, in document order (NOT yet sorted for apply — the apply step sorts
    /// descending per file so the preview can stay in reading order).
    pub edits: Vec<TextEdit>,
    /// The before/after diff rows shown in the preview.
    pub hunks: Vec<PreviewHunk>,
}

/// The whole multi-file rename preview: every changed file, in a stable order. Built from an LSP
/// `WorkspaceEdit` (both shapes — RISK-003) or from the no-LSP single-file fallback. NOTHING is mutated
/// until [`apply_preview`] is called (AC-004).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceEditPreview {
    pub files: Vec<FileEditPreview>,
    /// True when this preview came from the no-LSP single-file fallback path; drives the
    /// `LSP-required for cross-file rename` banner (MC-004 — the operator is never misled about scope).
    pub is_single_file_fallback: bool,
}

impl WorkspaceEditPreview {
    /// An empty (no-op) preview — the result of a null/empty `WorkspaceEdit` (the no-op rename case).
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when the preview has no edits in any file (a no-op rename — the caller shows "no changes").
    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|f| f.edits.is_empty())
    }

    /// Total number of edits across all files (the apply report's edit count + the preview summary).
    pub fn total_edits(&self) -> usize {
        self.files.iter().map(|f| f.edits.len()).sum()
    }

    /// Build a preview from an `lsp_types::WorkspaceEdit`, handling BOTH the `changes` map form AND the
    /// `document_changes` array form (RISK-003 / MC-003 / AC-007). For each changed file, an `open_lookup`
    /// closure decides whether the file is an open buffer (so the preview marks `is_open_buffer`) and
    /// supplies that file's current text so the before/after hunks can be computed; a file not known to
    /// `open_lookup` is treated as a to-disk file and its content is read from disk for the hunks (a read
    /// failure yields edits with empty before/after context rather than dropping the file).
    ///
    /// `open_lookup(uri)` returns `Some(text)` when the URI maps to an open buffer (the in-memory text),
    /// else `None` (a to-disk file). Files are sorted by URI for a stable preview order. Edits within a
    /// file are sorted DESCENDING by start offset against that file's content (the apply order — RISK-001)
    /// but the preview keeps them in document order for readability via the hunks.
    pub fn from_lsp(
        edit: &lsp_types::WorkspaceEdit,
        mut open_lookup: impl FnMut(&str) -> Option<String>,
    ) -> Self {
        // Collect (uri, Vec<TextEdit>) from whichever shape the server sent. `document_changes` takes
        // precedence when present (it is the richer form a capable server sends); otherwise `changes`.
        let mut per_file: Vec<(String, Vec<TextEdit>)> = Vec::new();

        if let Some(doc_changes) = &edit.document_changes {
            match doc_changes {
                lsp_types::DocumentChanges::Edits(edits) => {
                    for tde in edits {
                        let uri = tde.text_document.uri.to_string();
                        let mut text_edits = Vec::with_capacity(tde.edits.len());
                        for one in &tde.edits {
                            // Unwrap the `OneOf<TextEdit, AnnotatedTextEdit>` — the annotation is ignored;
                            // both carry the same range + new_text.
                            let (range, new_text) = match one {
                                lsp_types::OneOf::Left(te) => (te.range, te.new_text.clone()),
                                lsp_types::OneOf::Right(ate) => {
                                    (ate.text_edit.range, ate.text_edit.new_text.clone())
                                }
                            };
                            text_edits.push(TextEdit {
                                range: LspRange::from_lsp(range),
                                new_text,
                            });
                        }
                        push_or_merge(&mut per_file, uri, text_edits);
                    }
                }
                lsp_types::DocumentChanges::Operations(ops) => {
                    // A rename only ever sends Edit operations; create/rename/delete file operations are
                    // out of scope (a rename does not move files). Extract only the Edit operations'
                    // text-document edits; pure resource ops are skipped (they carry no TextEdits).
                    for op in ops {
                        if let lsp_types::DocumentChangeOperation::Edit(tde) = op {
                            let uri = tde.text_document.uri.to_string();
                            let mut text_edits = Vec::with_capacity(tde.edits.len());
                            for one in &tde.edits {
                                let (range, new_text) = match one {
                                    lsp_types::OneOf::Left(te) => (te.range, te.new_text.clone()),
                                    lsp_types::OneOf::Right(ate) => {
                                        (ate.text_edit.range, ate.text_edit.new_text.clone())
                                    }
                                };
                                text_edits.push(TextEdit {
                                    range: LspRange::from_lsp(range),
                                    new_text,
                                });
                            }
                            push_or_merge(&mut per_file, uri, text_edits);
                        }
                    }
                }
            }
        } else if let Some(changes) = &edit.changes {
            for (url, edits) in changes {
                let uri = url.to_string();
                let text_edits: Vec<TextEdit> = edits
                    .iter()
                    .map(|te| TextEdit {
                        range: LspRange::from_lsp(te.range),
                        new_text: te.new_text.clone(),
                    })
                    .collect();
                push_or_merge(&mut per_file, uri, text_edits);
            }
        }

        // Stable order so two runs (and the changes-map's HashMap iteration order) produce the same
        // preview, and a documentChanges-only and changes-only response over the same edits compare equal.
        per_file.sort_by(|a, b| a.0.cmp(&b.0));

        let files = per_file
            .into_iter()
            .map(|(uri, edits)| {
                let open_text = open_lookup(&uri);
                let is_open_buffer = open_text.is_some();
                // The content used to compute before/after hunks: the open buffer text, else read disk.
                let content = open_text.or_else(|| read_file_for_preview(&uri));
                let hunks = match &content {
                    Some(text) => hunks_for_edits(text, &edits),
                    None => Vec::new(),
                };
                FileEditPreview {
                    uri,
                    is_open_buffer,
                    edits,
                    hunks,
                }
            })
            .collect();

        Self {
            files,
            is_single_file_fallback: false,
        }
    }

    /// Build the SINGLE-FILE no-LSP fallback preview (RISK-004 / MC-004 — AC-003): rename every in-file
    /// occurrence of `original` to `new_name` in the current file's `text`. The occurrence ranges come
    /// from `occurrence_byte_ranges` (the panel computes these from the file's tree-sitter parse tree —
    /// a SAFE local source, NOT a word-scan). `uri` is the current file. `is_open_buffer` marks whether
    /// the current file is an open buffer. Marks the preview as a single-file fallback so the banner is
    /// rendered (the operator is never misled that the rename was project-wide).
    pub fn single_file_fallback(
        uri: impl Into<String>,
        text: &str,
        new_name: &str,
        occurrence_byte_ranges: &[Range<usize>],
        is_open_buffer: bool,
    ) -> Self {
        let buffer = TextBuffer::new(text);
        let mut edits: Vec<TextEdit> = Vec::with_capacity(occurrence_byte_ranges.len());
        for range in occurrence_byte_ranges {
            if let Some(lsp_range) = byte_range_to_lsp_range(&buffer, range.clone()) {
                edits.push(TextEdit {
                    range: lsp_range,
                    new_text: new_name.to_owned(),
                });
            }
        }
        let hunks = hunks_for_edits(text, &edits);
        let file = FileEditPreview {
            uri: uri.into(),
            is_open_buffer,
            edits,
            hunks,
        };
        Self {
            files: vec![file],
            is_single_file_fallback: true,
        }
    }
}

/// Append `edits` to the file's entry in `per_file`, merging if the URI is already present (a server may
/// list the same file twice). Keeps document order within a file.
fn push_or_merge(per_file: &mut Vec<(String, Vec<TextEdit>)>, uri: String, edits: Vec<TextEdit>) {
    if let Some(entry) = per_file.iter_mut().find(|(u, _)| *u == uri) {
        entry.1.extend(edits);
    } else {
        per_file.push((uri, edits));
    }
}

/// Compute the before/after [`PreviewHunk`]s for `edits` against `text`: for each edit, the whole line it
/// starts on, before the edit and after applying ONLY that file's edits (so the preview shows the final
/// per-file result line-by-line). One hunk per distinct affected line (deduped). Never mutates `text`.
fn hunks_for_edits(text: &str, edits: &[TextEdit]) -> Vec<PreviewHunk> {
    if edits.is_empty() {
        return Vec::new();
    }
    let before_buffer = TextBuffer::new(text);
    // Apply all edits to a copy to get the "after" text (descending order — same as apply).
    let after_text = apply_text_edits_to_string(text, edits).unwrap_or_else(|_| text.to_owned());
    let after_buffer = TextBuffer::new(&after_text);

    // The set of affected lines (the start line of each edit), deduped + sorted.
    let mut lines: Vec<usize> = edits.iter().map(|e| e.range.start_line as usize).collect();
    lines.sort_unstable();
    lines.dedup();

    lines
        .into_iter()
        .map(|line| {
            let before = whole_line(&before_buffer, line);
            // The "after" line index can shift if an edit changed the line count, but a rename replaces an
            // identifier WITHIN a line (no newline change), so the same line index holds. Guard against a
            // shorter after-buffer by clamping.
            let after_line = line.min(after_buffer.len_lines().saturating_sub(1));
            let after = whole_line(&after_buffer, after_line);
            PreviewHunk {
                line,
                before,
                after,
            }
        })
        .collect()
}

/// The whole text of `line` (without the trailing newline), or an empty string when out of range.
fn whole_line(buffer: &TextBuffer, line: usize) -> String {
    let s = buffer.slice_to_string(line..line + 1);
    s.trim_end_matches(['\n', '\r']).to_owned()
}

/// Read a file's content for the preview hunks, mapping a `file://` URI (or a bare path) to a path. A
/// read failure yields `None` (the preview then shows the file with empty hunks rather than dropping it).
fn read_file_for_preview(uri: &str) -> Option<String> {
    std::fs::read_to_string(uri_to_path(uri)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_lsp_both_shapes_produce_the_same_preview() {
        // RISK-003 / MC-003 / AC-007 / PT-006: the `changes` map form and the `documentChanges` array form
        // deserialize to the SAME FileEditPreview set.
        let uri = lsp_types::Url::parse("file:///x.rs").unwrap();
        let range = lsp_types::Range {
            start: lsp_types::Position {
                line: 0,
                character: 3,
            },
            end: lsp_types::Position {
                line: 0,
                character: 6,
            },
        };
        let te = lsp_types::TextEdit {
            range,
            new_text: "renamed".into(),
        };

        // changes map form.
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri.clone(), vec![te.clone()]);
        let edit_changes = lsp_types::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };

        // documentChanges array form.
        let tde = lsp_types::TextDocumentEdit {
            text_document: lsp_types::OptionalVersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: Some(1),
            },
            edits: vec![lsp_types::OneOf::Left(te)],
        };
        let edit_doc = lsp_types::WorkspaceEdit {
            changes: None,
            document_changes: Some(lsp_types::DocumentChanges::Edits(vec![tde])),
            change_annotations: None,
        };

        // No open buffer; no disk read (uri does not exist) -> empty hunks but the edits + uri must match.
        let p_changes = WorkspaceEditPreview::from_lsp(&edit_changes, |_| None);
        let p_doc = WorkspaceEditPreview::from_lsp(&edit_doc, |_| None);
        assert_eq!(p_changes.files.len(), 1);
        assert_eq!(p_doc.files.len(), 1);
        assert_eq!(p_changes.files[0].uri, p_doc.files[0].uri);
        assert_eq!(p_changes.files[0].edits, p_doc.files[0].edits);
        assert_eq!(
            p_changes.files[0].edits[0],
            TextEdit {
                range: LspRange {
                    start_line: 0,
                    start_char: 3,
                    end_line: 0,
                    end_char: 6
                },
                new_text: "renamed".into(),
            }
        );
    }

    #[test]
    fn from_lsp_empty_is_a_noop_preview() {
        let edit = lsp_types::WorkspaceEdit {
            changes: None,
            document_changes: None,
            change_annotations: None,
        };
        let preview = WorkspaceEditPreview::from_lsp(&edit, |_| None);
        assert!(preview.is_empty());
        assert_eq!(preview.total_edits(), 0);
    }
}
