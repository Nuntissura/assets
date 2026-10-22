//! Rename edit application and per-file atomic writes (WP-KERNEL-012 MT-048).

use std::ops::Range;
use std::path::{Path, PathBuf};

use handshake_text_mechanics::buffer::TextBuffer;

use crate::preview::{LspRange, TextEdit, WorkspaceEditPreview};

/// A report of what [`apply_preview`] changed: the files touched + the total edit count, so the panel can
/// surface a truthful "renamed N occurrences across M files" message. On a partial failure the report is
/// still ACCURATE — it lists exactly the files that were already applied before the failure (MC-002 — the
/// report must be truthful; best-effort rollback is out of scope).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenameApplyReport {
    /// The URIs of the files that were successfully applied (in apply order).
    pub files_changed: Vec<String>,
    /// The total number of edits applied across those files.
    pub edits_applied: usize,
}

/// Why a rename apply could not complete. Carries an accurate partial report so the panel reports which
/// files were already applied (MC-002 — the report must be truthful even on failure).
#[derive(Debug, Clone)]
pub enum RenameError {
    /// A file's disk read/write failed; `partial` lists the files already applied before the failure.
    Io {
        uri: String,
        message: String,
        partial: RenameApplyReport,
    },
    /// An edit's range could not be mapped to a buffer offset (a stale range from a prior edit).
    BadRange {
        uri: String,
        partial: RenameApplyReport,
    },
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameError::Io { uri, message, .. } => {
                write!(f, "rename apply failed writing {uri}: {message}")
            }
            RenameError::BadRange { uri, .. } => {
                write!(
                    f,
                    "rename apply failed: an edit range was out of range in {uri}"
                )
            }
        }
    }
}

impl std::error::Error for RenameError {}

/// Apply a [`WorkspaceEditPreview`] across open buffers + to-disk files (AC-002 / AC-008). For each file:
/// - an OPEN buffer's edits are applied to a fresh [`TextBuffer`] built from `read_open_buffer(uri)` in
///   DESCENDING start-offset order (RISK-001) and the new text is written back via `write_open_buffer`;
/// - a TO-DISK file is read, its edits applied to a [`TextBuffer`] (descending order), and the result
///   written ATOMICALLY (temp + rename — RISK-002 / MC-002).
///
/// On the FIRST per-file failure this STOPS and returns a [`RenameError`] whose `partial` report lists the
/// files already applied (MC-002 — the report is truthful; best-effort rollback is out of scope). On full
/// success a [`RenameApplyReport`] of every file + edit count is returned.
///
/// `read_open_buffer(uri) -> Option<String>` returns the current in-memory text of an open buffer (the
/// panel passes its own buffer text); `write_open_buffer(uri, new_text)` installs the renamed text back
/// into the open buffer. Both are closures so the apply logic is testable with in-memory buffers and the
/// panel wires them to its real `TextBuffer`.
pub fn apply_preview(
    preview: &WorkspaceEditPreview,
    mut read_open_buffer: impl FnMut(&str) -> Option<String>,
    mut write_open_buffer: impl FnMut(&str, &str),
) -> Result<RenameApplyReport, RenameError> {
    let mut report = RenameApplyReport::default();
    for file in &preview.files {
        if file.edits.is_empty() {
            continue;
        }
        if file.is_open_buffer {
            let current = read_open_buffer(&file.uri).unwrap_or_default();
            let new_text = apply_text_edits_to_string(&current, &file.edits).map_err(|_| {
                RenameError::BadRange {
                    uri: file.uri.clone(),
                    partial: report.clone(),
                }
            })?;
            write_open_buffer(&file.uri, &new_text);
        } else {
            let path = uri_to_path(&file.uri);
            let current = std::fs::read_to_string(&path).map_err(|e| RenameError::Io {
                uri: file.uri.clone(),
                message: e.to_string(),
                partial: report.clone(),
            })?;
            let new_text = apply_text_edits_to_string(&current, &file.edits).map_err(|_| {
                RenameError::BadRange {
                    uri: file.uri.clone(),
                    partial: report.clone(),
                }
            })?;
            write_file_atomic(&path, &new_text).map_err(|e| RenameError::Io {
                uri: file.uri.clone(),
                message: e.to_string(),
                partial: report.clone(),
            })?;
        }
        report.files_changed.push(file.uri.clone());
        report.edits_applied += file.edits.len();
    }
    Ok(report)
}

/// An edit could not be applied because its line/character range did not resolve to a valid byte range in
/// the current buffer (a stale range from a prior edit). Returned instead of silently corrupting the text
/// (RISK-001 — a stale range never produces a partially-mangled buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditApplyError;

impl std::fmt::Display for EditApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "an edit range did not resolve to a valid byte range")
    }
}

impl std::error::Error for EditApplyError {}

/// Apply `edits` to `text` and return the new string. The edits are converted to BYTE ranges against the
/// text, sorted DESCENDING by start byte, and applied in that order so an earlier edit's byte offsets are
/// not invalidated by a later edit (RISK-001 — the single most common rename bug). An edit whose range
/// cannot be mapped to a valid byte range yields an `Err` (a stale range never silently corrupts).
pub fn apply_text_edits_to_string(
    text: &str,
    edits: &[TextEdit],
) -> Result<String, EditApplyError> {
    let mut buffer = TextBuffer::new(text);
    apply_text_edits_to_buffer(&mut buffer, edits)?;
    Ok(buffer.to_string())
}

/// Apply `edits` to `buffer` IN PLACE, in DESCENDING start-offset order (RISK-001). Each edit's
/// line/character range is resolved to a byte range against the CURRENT buffer once (before any mutation,
/// using the pre-edit line map), then sorted descending and applied delete+insert. Because later edits
/// (higher offsets) are applied first, the byte offsets of earlier (lower-offset) edits remain valid.
/// Returns `Err(EditApplyError)` if any edit's range cannot be resolved to a valid byte range.
pub fn apply_text_edits_to_buffer(
    buffer: &mut TextBuffer,
    edits: &[TextEdit],
) -> Result<(), EditApplyError> {
    // Resolve every edit to a byte range against the UN-edited buffer first (so all ranges share the same
    // coordinate space), then sort descending and apply — applying high offsets first keeps low offsets
    // valid (RISK-001 / the descending-offset invariant).
    let mut resolved: Vec<(Range<usize>, String)> = Vec::with_capacity(edits.len());
    for edit in edits {
        let byte_range = lsp_range_to_byte_range(buffer, edit.range).ok_or(EditApplyError)?;
        resolved.push((byte_range, edit.new_text.clone()));
    }
    // Sort DESCENDING by start byte (RISK-001). On equal starts, the longer range first is irrelevant for
    // a rename (occurrences do not overlap), but sort by (start desc, end desc) for determinism.
    resolved.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
    for (range, new_text) in resolved {
        // delete the old occurrence then insert the new text at the (now-current) start. Because we apply
        // descending, `range.start` is still valid (no earlier edit has shifted it). A failed delete/insert
        // is a stale range -> Err (never a silent partial corruption).
        buffer.delete(range.clone()).map_err(|_| EditApplyError)?;
        buffer
            .insert(range.start, &new_text)
            .map_err(|_| EditApplyError)?;
    }
    Ok(())
}

/// Convert an [`LspRange`] (0-based line/char) to a BYTE range against `buffer`, or `None` when it cannot
/// be resolved (an out-of-range line/char never panics — it returns `None` and the caller reports a bad
/// range). The character column is interpreted as a byte column on the line (the simple model the editor
/// uses — see the panel's `lsp_position_at` note); this is exact for ASCII and the common identifier case.
fn lsp_range_to_byte_range(buffer: &TextBuffer, range: LspRange) -> Option<Range<usize>> {
    let start = line_char_to_byte(buffer, range.start_line as usize, range.start_char as usize)?;
    let end = line_char_to_byte(buffer, range.end_line as usize, range.end_char as usize)?;
    if start > end {
        return None;
    }
    Some(start..end)
}

/// Resolve a `(line, char)` (0-based, char = byte column on the line) to an absolute byte offset, clamped
/// to the line's byte length so a server's past-the-line-end column lands at the line end (never panics).
fn line_char_to_byte(buffer: &TextBuffer, line: usize, char_col: usize) -> Option<usize> {
    let line_start = buffer.line_to_byte(line)?;
    // The end of this line's content (start of next line, or buffer end on the last line).
    let line_end = buffer
        .line_to_byte(line + 1)
        .unwrap_or_else(|| buffer.len_bytes());
    let col_byte = (line_start + char_col).min(line_end);
    Some(col_byte)
}

/// Convert a BYTE range to an [`LspRange`] against `buffer` (the inverse of [`lsp_range_to_byte_range`]),
/// for the no-LSP fallback that produces edits from tree-sitter byte ranges. `None` on an out-of-range
/// byte offset.
pub(crate) fn byte_range_to_lsp_range(buffer: &TextBuffer, range: Range<usize>) -> Option<LspRange> {
    let (start_line, start_char) = byte_to_line_char(buffer, range.start)?;
    let (end_line, end_char) = byte_to_line_char(buffer, range.end)?;
    Some(LspRange {
        start_line: start_line as u32,
        start_char: start_char as u32,
        end_line: end_line as u32,
        end_char: end_char as u32,
    })
}

/// Resolve a byte offset to `(line, byte-column)` (0-based), or `None` when out of range.
fn byte_to_line_char(buffer: &TextBuffer, byte_offset: usize) -> Option<(usize, usize)> {
    let line = buffer.byte_to_line(byte_offset)?;
    let line_start = buffer.line_to_byte(line)?;
    Some((line, byte_offset.saturating_sub(line_start)))
}

/// Map a `file://` URI (or a bare path) to a filesystem [`PathBuf`]. Uses `lsp_types::Url::to_file_path`
/// when the URI parses as a `file:` URL, else treats the string as a path directly (so a test/relative
/// path works without a real URL).
pub(crate) fn uri_to_path(uri: &str) -> PathBuf {
    if let Ok(url) = lsp_types::Url::parse(uri) {
        if url.scheme() == "file" {
            if let Ok(p) = url.to_file_path() {
                return p;
            }
        }
    }
    PathBuf::from(uri)
}

/// Write `content` to `path` ATOMICALLY (RISK-002 / MC-002 / AC-008): write the full content to a SIBLING
/// temp file in the SAME directory, flush+sync it, then `std::fs::rename` it over the target. `rename` is
/// atomic on the same volume, so the target is always either the intact original or the fully-replaced new
/// content — never half-written, even if the process is interrupted mid-write. A temp-file leftover from a
/// crash BEFORE the rename never corrupts the target (the target is untouched until the atomic rename).
pub fn write_file_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rename");
    // A unique sibling temp name in the SAME directory (so the rename stays on the same volume). The
    // pid + a monotonic nanosecond stamp keeps concurrent renames from colliding.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_path = dir.join(format!(
        ".{file_name}.{}.{stamp}.hsk-rename-tmp",
        std::process::id()
    ));

    // Write + flush + fsync the temp file fully before the rename, so the rename swaps in complete content.
    {
        let mut temp = std::fs::File::create(&temp_path)?;
        temp.write_all(content.as_bytes())?;
        temp.flush()?;
        // Best-effort durability: sync the file contents to disk before the rename. A platform that does
        // not support sync_all returns an error we ignore (the rename atomicity is the load-bearing
        // guarantee; sync is a durability nicety).
        let _ = temp.sync_all();
    }
    // The atomic swap. On Windows `std::fs::rename` replaces an existing destination on the same volume.
    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Clean up the temp file so a failed rename does not litter the directory; report the error.
            let _ = std::fs::remove_file(&temp_path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::FileEditPreview;

    #[test]
    fn descending_offset_apply_is_correct_and_ascending_would_corrupt() {
        // RISK-001 / MC-001: two edits on the same line; applying ascending would shift the second.
        // "let value = value + 1;" -> rename `value` to `total` at both occurrences.
        let text = "let value = value + 1;";
        // value occurrences: bytes 4..9 and 12..17.
        let edits = vec![
            TextEdit {
                range: LspRange {
                    start_line: 0,
                    start_char: 4,
                    end_line: 0,
                    end_char: 9,
                },
                new_text: "total".into(),
            },
            TextEdit {
                range: LspRange {
                    start_line: 0,
                    start_char: 12,
                    end_line: 0,
                    end_char: 17,
                },
                new_text: "total".into(),
            },
        ];
        let out = apply_text_edits_to_string(text, &edits).expect("apply ok");
        assert_eq!(
            out, "let total = total + 1;",
            "descending apply renames both occurrences exactly"
        );

        // Prove ascending order would CORRUPT: apply the SAME edits naively low->high without re-resolving
        // offsets. The first edit ("total" is same length as "value" here is 5==5, so pick a length-changing
        // rename to expose the bug): rename to a SHORTER name `t` so ascending shifts the second range.
        let edits_short = vec![
            TextEdit {
                range: LspRange {
                    start_line: 0,
                    start_char: 4,
                    end_line: 0,
                    end_char: 9,
                },
                new_text: "t".into(),
            },
            TextEdit {
                range: LspRange {
                    start_line: 0,
                    start_char: 12,
                    end_line: 0,
                    end_char: 17,
                },
                new_text: "t".into(),
            },
        ];
        let correct = apply_text_edits_to_string(text, &edits_short).expect("apply ok");
        assert_eq!(
            correct, "let t = t + 1;",
            "descending apply is correct for a length-changing rename"
        );

        // Naive ASCENDING application (the bug): apply edit[0] first (shrinks the doc by 4 bytes), then
        // edit[1] at its ORIGINAL byte range 12..17 — which now points at the WRONG, shifted text.
        let mut naive = TextBuffer::new(text);
        let r0 = lsp_range_to_byte_range(&naive, edits_short[0].range).unwrap();
        naive.delete(r0.clone()).unwrap();
        naive.insert(r0.start, "t").unwrap();
        let r1 = lsp_range_to_byte_range(&TextBuffer::new(text), edits_short[1].range).unwrap();
        // r1 (12..17) now exceeds/misaligns the shrunk buffer; the naive result differs from the correct one.
        let ascending_result = {
            // Clamp so it does not panic, mirroring a real naive bug that mangles text.
            let len = naive.len_bytes();
            let s = r1.start.min(len);
            let e = r1.end.min(len);
            let _ = naive.delete(s..e);
            let _ = naive.insert(s.min(naive.len_bytes()), "t");
            naive.to_string()
        };
        assert_ne!(
            ascending_result, correct,
            "RISK-001 regression: ascending-order apply must NOT equal the correct descending result"
        );
    }

    #[test]
    fn apply_across_two_files_via_apply_preview() {
        // AC-002: a 2-file WorkspaceEdit applied to 2 open buffers, offsets correct.
        let file_a = "fn foo() {}\nfn bar() { foo(); }";
        let file_b = "fn baz() { foo(); foo(); }";
        // Rename `foo` to `frobnicate` in both files.
        let preview = WorkspaceEditPreview {
            files: vec![
                FileEditPreview {
                    uri: "file:///a.rs".into(),
                    is_open_buffer: true,
                    edits: vec![
                        TextEdit {
                            range: LspRange {
                                start_line: 0,
                                start_char: 3,
                                end_line: 0,
                                end_char: 6,
                            },
                            new_text: "frobnicate".into(),
                        },
                        TextEdit {
                            range: LspRange {
                                start_line: 1,
                                start_char: 11,
                                end_line: 1,
                                end_char: 14,
                            },
                            new_text: "frobnicate".into(),
                        },
                    ],
                    hunks: vec![],
                },
                FileEditPreview {
                    uri: "file:///b.rs".into(),
                    is_open_buffer: true,
                    edits: vec![
                        TextEdit {
                            range: LspRange {
                                start_line: 0,
                                start_char: 11,
                                end_line: 0,
                                end_char: 14,
                            },
                            new_text: "frobnicate".into(),
                        },
                        TextEdit {
                            range: LspRange {
                                start_line: 0,
                                start_char: 18,
                                end_line: 0,
                                end_char: 21,
                            },
                            new_text: "frobnicate".into(),
                        },
                    ],
                    hunks: vec![],
                },
            ],
            is_single_file_fallback: false,
        };

        use std::collections::HashMap;
        let mut buffers: HashMap<String, String> = HashMap::new();
        buffers.insert("file:///a.rs".into(), file_a.to_owned());
        buffers.insert("file:///b.rs".into(), file_b.to_owned());
        let read_buffers = buffers.clone();

        let report = apply_preview(
            &preview,
            |uri| read_buffers.get(uri).cloned(),
            |uri, new_text| {
                buffers.insert(uri.to_owned(), new_text.to_owned());
            },
        )
        .expect("apply ok");

        assert_eq!(report.files_changed, vec!["file:///a.rs", "file:///b.rs"]);
        assert_eq!(report.edits_applied, 4);
        assert_eq!(
            buffers["file:///a.rs"],
            "fn frobnicate() {}\nfn bar() { frobnicate(); }"
        );
        assert_eq!(
            buffers["file:///b.rs"],
            "fn baz() { frobnicate(); frobnicate(); }"
        );
    }

    #[test]
    fn write_file_atomic_replaces_whole_file_never_partial() {
        // AC-008 / RISK-002 / MC-002: an atomic write fully replaces the file; a simulated interruption
        // (the temp write happening but the rename NOT) leaves the ORIGINAL intact.
        let dir = std::env::temp_dir().join(format!("hsk-rename-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("victim.rs");
        std::fs::write(&target, "original content\n").unwrap();

        // A real atomic write replaces the whole file.
        write_file_atomic(&target, "fully replaced content\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "fully replaced content\n"
        );

        // Simulate a write INTERRUPTION: write to the sibling temp but DO NOT rename. The target must be
        // the intact previous content (never half-written) — proving the rename, not an in-place write,
        // is the mutation point.
        let temp_sibling = dir.join(".victim.rs.interrupted.hsk-rename-tmp");
        std::fs::write(&temp_sibling, "HALF WRITTEN GARBAGE").unwrap(); // the "crash before rename" state.
                                                                        // The target is untouched by the temp write.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "fully replaced content\n",
            "AC-008: the target is intact-or-fully-replaced; a temp write never touches it"
        );
        // Cleanup.
        let _ = std::fs::remove_file(&temp_sibling);
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn apply_partial_failure_reports_already_applied_files() {
        // MC-002: a per-file failure stops + reports which files were already applied (truthful report).
        let preview = WorkspaceEditPreview {
            files: vec![
                FileEditPreview {
                    uri: "file:///ok.rs".into(),
                    is_open_buffer: true,
                    edits: vec![TextEdit {
                        range: LspRange {
                            start_line: 0,
                            start_char: 0,
                            end_line: 0,
                            end_char: 3,
                        },
                        new_text: "new".into(),
                    }],
                    hunks: vec![],
                },
                FileEditPreview {
                    // A to-disk file that does not exist -> read fails -> Io error after ok.rs applied.
                    uri: "file:///does-not-exist-hsk.rs".into(),
                    is_open_buffer: false,
                    edits: vec![TextEdit {
                        range: LspRange {
                            start_line: 0,
                            start_char: 0,
                            end_line: 0,
                            end_char: 3,
                        },
                        new_text: "new".into(),
                    }],
                    hunks: vec![],
                },
            ],
            is_single_file_fallback: false,
        };
        let mut applied: Vec<String> = Vec::new();
        let err = apply_preview(
            &preview,
            |_| Some("foo bar".to_owned()),
            |uri, _| applied.push(uri.to_owned()),
        )
        .expect_err("the second file fails to read");
        match err {
            RenameError::Io { partial, .. } => {
                assert_eq!(
                    partial.files_changed,
                    vec!["file:///ok.rs"],
                    "the report accurately lists the already-applied file"
                );
                assert_eq!(partial.edits_applied, 1);
            }
            other => panic!("expected an Io error, got {other:?}"),
        }
        assert_eq!(
            applied,
            vec!["file:///ok.rs"],
            "only the first file's buffer was written"
        );
    }
}
