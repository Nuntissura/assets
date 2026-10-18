---
file_id: handshake-lsp-wire-readme
file_kind: module-manual
updated_at: 2026-10-18
---

<topic id="api-and-io">

# LSP wire

`handshake_lsp_wire` preserves the existing LSP JSON-RPC framing, response routing and protocol normalization from WP-KERNEL-012. Public modules are `framing` and `protocol`.

Await `framing::write_message(&mut writer, &message)` to serialize a `serde_json::Value`, write its `Content-Length` header and body, then flush. The writer implements Tokio `AsyncWrite + Unpin`; I/O failures return `std::io::Result` errors.

Await `framing::read_loop(reader, pending, diagnostics_tx)` to read framed messages from a Tokio `AsyncRead + Unpin` reader. The caller supplies an `Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>` for pending numeric request IDs and a broadcast sender for `PublishedDiagnostics`. Responses remove their pending entry and deliver a result or error. The loop routes `textDocument/publishDiagnostics` notifications through the broadcast sender. It does not implement server-to-client requests.

Production framing limits are 8 KiB per header line, 32 KiB per header block and 16 MiB per content body. Missing content lengths and malformed JSON bodies are skipped. Oversized frames, invalid UTF-8 headers, I/O errors and truncated bodies end the reader loop. `read_one_frame` is the existing test helper; it reads one frame without prefetching subsequent frames and returns `None` on failure. Its header handling differs from the production loop.

`protocol::position_params` builds position-request JSON and returns `None` for an invalid URI. `completion_item_from_lsp` normalizes completion fields with the label as fallback insertion text. `hover_contents_to_string` flattens hover content. `published_diagnostics_from_lsp` retains the URI and optional version, maps zero-based diagnostic lines, and defaults omitted severity to error.

</topic>

<topic id="dependencies-and-recovery">

## Dependencies and recovery

The manifest remains unchanged: `lsp-types = "0.95"`, `serde_json = "1"`, and `tokio = "1"` with `io-util` and `sync`. This package does not enable Tokio runtime, process, network or macro features and does not start a runtime or language server. Its caller provides execution, streams, pending-request lifecycle, cancellation, restart policy and diagnostics consumption. It has no TextBuffer dependency or native/Core backdependency.

When a reader loop ends, the host must decide how to settle pending requests and recover its connection. Inspect the supplied stream and frame limits before reconnecting. Inspect the original JSON-RPC response at the host when an error needs more detail: `JsonRpcResponse::Error` intentionally preserves the existing unit variant.

The manifest and all three Rust files are copied byte-for-byte from the assigned product source, including its two protocol tests. No framing tests were invented or moved from the host. No compiler, tests or runtime has run for this extraction. Standalone locked proof, publication and independent canonical remote verification must precede separate pinned host adoption.

</topic>