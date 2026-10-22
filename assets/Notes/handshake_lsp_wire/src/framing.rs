//! Bounded LSP JSON-RPC framing and routing (WP-KERNEL-012 MT-008).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lsp_types::PublishDiagnosticsParams;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot};

use crate::protocol::{published_diagnostics_from_lsp, JsonRpcResponse, PublishedDiagnostics};

/// Hard framing limits for untrusted language-server stdout. These are checked before any response
/// body allocation, and header lines are read through a bounded byte buffer rather than `read_line`.
pub const MAX_LSP_HEADER_LINE_BYTES: usize = 8 * 1024;
pub const MAX_LSP_HEADER_BYTES: usize = 32 * 1024;
pub const MAX_LSP_CONTENT_BYTES: usize = 16 * 1024 * 1024;

/// TEST HELPER: read exactly one `Content-Length`-framed JSON-RPC message from `reader` and parse it,
/// or `None` on EOF / a malformed frame. Used by `LspClient::read_test_request` so a mock server can
/// observe the client's request before responding. Mirrors the production `read_loop` framing.
pub async fn read_one_frame<R>(reader: &mut R) -> Option<Value>
where
    R: tokio::io::AsyncRead + Unpin,
{
    // Do not create a throwaway `BufReader` here: it may prefetch bytes from the next frame and lose
    // them when dropped, making a back-to-back didClose/didOpen pair look truncated to tests. Reading
    // the small header exactly leaves every following frame in the underlying stream.
    let mut header = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        reader.read_exact(&mut byte).await.ok()?;
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > MAX_LSP_HEADER_BYTES {
            return None;
        }
    }
    let header = std::str::from_utf8(&header).ok()?;
    let content_length = header.split("\r\n").find_map(|line| {
        line.strip_prefix("Content-Length:")
            .and_then(|rest| rest.trim().parse::<usize>().ok())
    });
    let len = content_length?;
    if len > MAX_LSP_CONTENT_BYTES {
        return None;
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

async fn read_header_line_bounded<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    line.clear();
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            return Ok(line.len());
        }
        line.push(byte[0]);
        if line.len() > MAX_LSP_HEADER_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "LSP header line exceeds configured limit",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(line.len());
        }
    }
}

/// Serialize `message` and write it to the server's stdin with the LSP `Content-Length` frame
/// header. Async (the request/notify paths await it). Generic over the writer so the production
/// `ChildStdin` and a test duplex-pipe writer share the SAME framing path.
pub async fn write_message<W>(stdin: &mut W, message: &Value) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + ?Sized,
{
    let body = serde_json::to_vec(message)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await
}

/// The reader loop: own the server's stdout, parse each `Content-Length`-framed JSON-RPC message,
/// and route it. A message with an `id` + `result`/`error` fulfills the pending request; a message
/// without an `id` is a notification (publishDiagnostics is routed to `diagnostics_tx`). A malformed
/// frame / non-JSON body is SKIPPED, never panicked on (RISK-003).
pub async fn read_loop<R>(
    stdout: R,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    diagnostics_tx: broadcast::Sender<PublishedDiagnostics>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stdout);
    loop {
        // Read the header block (lines until a blank line), extracting Content-Length.
        let mut content_length: Option<usize> = None;
        let mut header_line = Vec::with_capacity(64);
        let mut header_bytes = 0usize;
        loop {
            match read_header_line_bounded(&mut reader, &mut header_line).await {
                Ok(0) => return, // EOF: server closed stdout.
                Ok(read) => {
                    header_bytes = match header_bytes.checked_add(read) {
                        Some(total) if total <= MAX_LSP_HEADER_BYTES => total,
                        _ => return,
                    };
                }
                Err(_) => return, // unrecoverable read error.
            }
            let Ok(header_text) = std::str::from_utf8(&header_line) else {
                return;
            };
            let trimmed = header_text.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // end of headers.
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = rest.trim().parse::<usize>().ok();
            }
            // RISK-003: an unexpected non-header line (a server debug print) without a colon is
            // ignored; we keep reading until the blank line / a Content-Length appears.
        }
        let Some(len) = content_length else {
            // A header block ended with no Content-Length (e.g. a stray blank line from a server
            // debug print). Skip it and resync on the next header block (RISK-003 — never panic on
            // malformed framing).
            continue;
        };
        if len > MAX_LSP_CONTENT_BYTES {
            return;
        }
        // Read exactly `len` bytes of body.
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        // Parse the body; a non-JSON / malformed body is SKIPPED (RISK-003).
        let value: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };
        route_message(value, &pending, &diagnostics_tx);
    }
}

/// Route one parsed JSON-RPC message to a pending request or a notification handler.
fn route_message(
    value: Value,
    pending: &Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    diagnostics_tx: &broadcast::Sender<PublishedDiagnostics>,
) {
    // A RESPONSE has an `id`. (Server-to-client REQUESTS also have an id + a `method`; we do not
    // implement server->client requests, so a message with both id+method is treated as a
    // notification-style message we ignore unless it is publishDiagnostics — which has no id.)
    let has_method = value.get("method").is_some();
    if let Some(id) = value.get("id").and_then(numeric_id) {
        if !has_method {
            // It's a response to one of our requests.
            if let Some(tx) = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
            {
                let response = if value.get("error").is_some() {
                    JsonRpcResponse::Error
                } else if let Some(result) = value.get("result").cloned() {
                    JsonRpcResponse::Result(result)
                } else {
                    JsonRpcResponse::Error
                };
                let _ = tx.send(response);
            }
            return;
        }
    }
    // A NOTIFICATION (no id, or id+method we ignore). Route publishDiagnostics.
    if value.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
        if let Some(params) = value.get("params").cloned() {
            if let Ok(parsed) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                let _ = diagnostics_tx.send(published_diagnostics_from_lsp(parsed));
            }
        }
    }
}

/// Parse a JSON-RPC id (number or numeric string) to an i64 (our ids are numeric).
fn numeric_id(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}
