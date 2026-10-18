//! Canonical LSP response models and protocol normalization.

use lsp_types::{
    HoverContents, MarkedString, Position, PublishDiagnosticsParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url,
};
use serde_json::Value;

/// The completion items a single completion response yields (the popup list). Mirrors the LSP
/// `CompletionItem` but flattened to just the fields the popup renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCompletionItem {
    /// The label shown in the popup (and the text inserted when no `insert_text` is given).
    pub label: String,
    /// The text inserted at the cursor on accept (falls back to `label`).
    pub insert_text: String,
    /// The server-provided detail line (type/signature), if any.
    pub detail: Option<String>,
    /// The numeric LSP `CompletionItemKind` (1..=25), or `None` when the server omitted it.
    pub kind: Option<i32>,
}

/// A hover result: the rendered markdown/plain text the server returned for the cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverResult {
    /// The hover body (markdown when the server sent `MarkupContent`, else the plain string).
    pub value: String,
}

/// One diagnostic from a `textDocument/publishDiagnostics` notification, mapped to the editor's gutter
/// vocabulary. `line` is 0-based (the LSP `range.start.line` is already 0-based, matching the gutter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    /// The 0-based line the diagnostic applies to (LSP `range.start.line`).
    pub line: usize,
    /// The LSP severity (1=Error, 2=Warning, 3=Information, 4=Hint), defaulting to Error when omitted
    /// (the LSP convention: an omitted severity is client-interpreted; we treat it as the strongest).
    pub severity: i32,
    /// The human-readable diagnostic message.
    pub message: String,
}

/// A `textDocument/publishDiagnostics` payload routed to the editor: the document URI + its current
/// diagnostics. The editor maps `uri` to a panel and calls `push_diagnostics` with the markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedDiagnostics {
    /// The document URI the diagnostics apply to (as the server sent it).
    pub uri: String,
    /// Document version the server associated with this complete diagnostic set. A panel uses this to
    /// discard a delayed notification for an older buffer generation. `None` remains valid LSP.
    pub version: Option<i64>,
    /// The diagnostics for that document (replaces the document's previous set — LSP semantics).
    pub diagnostics: Vec<LspDiagnostic>,
}

#[derive(Debug)]
pub enum JsonRpcResponse {
    Result(Value),
    Error,
}

/// Build the `TextDocumentPositionParams` JSON for a request, or `None` if the uri is unparseable.
pub fn position_params(uri: &str, position: Position) -> Option<Value> {
    let url = Url::parse(uri).ok()?;
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: url },
        position,
    };
    serde_json::to_value(params).ok()
}

/// Map an `lsp_types::CompletionItem` to our flattened popup item. `CompletionItemKind` is a
/// `#[serde(transparent)]` newtype over `i32`; round-trip it through serde to read the numeric kind
/// (the only API the crate exposes for the inner value).
pub fn completion_item_from_lsp(item: lsp_types::CompletionItem) -> LspCompletionItem {
    let insert_text = item
        .insert_text
        .clone()
        .unwrap_or_else(|| item.label.clone());
    let kind = item
        .kind
        .and_then(|k| serde_json::to_value(k).ok())
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);
    LspCompletionItem {
        label: item.label,
        insert_text,
        detail: item.detail,
        kind,
    }
}

/// Flatten `HoverContents` (scalar / array / markup) to a single display string (impl note 3).
pub fn hover_contents_to_string(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(s) => marked_string_to_text(s),
        HoverContents::Array(items) => items
            .into_iter()
            .map(marked_string_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(markup) => markup.value,
    }
}

/// Flatten a `MarkedString` (plain or language-tagged code) to display text.
fn marked_string_to_text(s: MarkedString) -> String {
    match s {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(ls) => ls.value,
    }
}

/// Map a `PublishDiagnosticsParams` to the editor's [`PublishedDiagnostics`] (0-based gutter lines).
pub fn published_diagnostics_from_lsp(params: PublishDiagnosticsParams) -> PublishedDiagnostics {
    let diagnostics = params
        .diagnostics
        .into_iter()
        .map(|d| LspDiagnostic {
            line: d.range.start.line as usize,
            // An omitted severity is client-interpreted; treat it as Error (severity 1).
            severity: d.severity.map(severity_to_i32).unwrap_or(1),
            message: d.message,
        })
        .collect();
    PublishedDiagnostics {
        uri: params.uri.to_string(),
        version: params.version.map(i64::from),
        diagnostics,
    }
}

/// Map an `lsp_types::DiagnosticSeverity` to its i32 wire value (the crate stores it opaquely).
fn severity_to_i32(severity: lsp_types::DiagnosticSeverity) -> i32 {
    match severity {
        lsp_types::DiagnosticSeverity::ERROR => 1,
        lsp_types::DiagnosticSeverity::WARNING => 2,
        lsp_types::DiagnosticSeverity::INFORMATION => 3,
        lsp_types::DiagnosticSeverity::HINT => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_diagnostics_map_zero_based_lines_and_severity() {
        use lsp_types::{Diagnostic, DiagnosticSeverity, Range};
        let params = PublishDiagnosticsParams {
            uri: Url::parse("file:///x.rs").unwrap(),
            version: None,
            diagnostics: vec![
                Diagnostic {
                    range: Range {
                        start: Position {
                            line: 4,
                            character: 0,
                        },
                        end: Position {
                            line: 4,
                            character: 3,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: "boom".to_owned(),
                    ..Default::default()
                },
                Diagnostic {
                    range: Range {
                        start: Position {
                            line: 9,
                            character: 2,
                        },
                        end: Position {
                            line: 9,
                            character: 8,
                        },
                    },
                    severity: Some(DiagnosticSeverity::WARNING),
                    message: "careful".to_owned(),
                    ..Default::default()
                },
            ],
        };
        let mapped = published_diagnostics_from_lsp(params);
        assert_eq!(mapped.uri, "file:///x.rs");
        assert_eq!(mapped.diagnostics.len(), 2);
        assert_eq!(mapped.diagnostics[0].line, 4);
        assert_eq!(mapped.diagnostics[0].severity, 1);
        assert_eq!(mapped.diagnostics[0].message, "boom");
        assert_eq!(mapped.diagnostics[1].line, 9);
        assert_eq!(mapped.diagnostics[1].severity, 2);
    }

    #[test]
    fn hover_contents_flatten_markup_and_scalar() {
        let markup = HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "**add**\nAdds".to_owned(),
        });
        assert_eq!(hover_contents_to_string(markup), "**add**\nAdds");
        let scalar = HoverContents::Scalar(MarkedString::String("plain".to_owned()));
        assert_eq!(hover_contents_to_string(scalar), "plain");
    }
}
