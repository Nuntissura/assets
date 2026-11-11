//! The web site as one link graph (lane SMB-WEB-SITE-1; [STU-WEB-118], [STU-WEB-120], [STU-WEB-121], [STU-WEB-131],
//! [STU-WEB-163], [STU-WEB-164], [STU-ASSET-009]): the site report, export of the whole document set,
//! update-links-on-move, the site lock and job admission of the site-wide `web.*` commands. A child of
//! `web_commands`, so the host's types and file rules are shared, never copied.
//!
//! # Manual (no-context operator or model)
//!
//! - **The site.** `web.site` sets `local_root`. The site's files are a sorted walk of that folder (byte order of
//!   the root-relative path, never the file system's enumeration order): at most 4096 files, 16384 entries and 32
//!   path segments; names that are not portable (a leading dot, a reserved device name, characters outside
//!   `A-Za-z0-9-_.~+@,()`) are `skipped` with `name_not_portable`; a symlink or junction is `skipped` with `link`
//!   and never followed, so nothing outside the root is read and a reference through it is `denied`. Every
//!   `.html`/`.htm` file is a document and every `.css` file a style sheet; both are parsed under the engine's
//!   `Limits` (8 MiB each) with at most 64 MiB of source in the whole site and 262144 references (the engine bounds
//!   one document at 200000, `Limits::max_nodes`). A style sheet is scanned as the body of a `style` element with
//!   each `</style` read as `<_style` (same length, so every span is the file's own; the file is never changed by
//!   it). A file that cannot be read or parsed (not UTF-8, over 8 MiB, an engine limit) is listed in `unparsed` with
//!   its code and `graph_complete` is false. The open document contributes its live source, not its file.
//! - **Declared limit (fail closed, lead Q4):** while any file of the site is unparsed, the site export and every
//!   `web.move_file` refuse `document_unreadable`; `target` is `<file>: <reason>` and `detail` is `{file, reason}`.
//!   Fix or remove that file (or convert it to UTF-8), then repeat the command.
//! - **The graph.** One row per reference the engine's carrier table finds (`collect_assets`), resolved against
//!   the referring file's folder (`/...` against the root): a file of the walk, a folder (navigation only), a
//!   missing file (or one that exists only in another letter case: `case_mismatch`, because a published site on a
//!   case-sensitive server breaks), external (`http(s):`, `//`), denied (the engine's reasons) or inline. A
//!   fragment `page.html#id` (and `#id` in the same page) must name an `id`, or an `a name`, of that page (`#` and
//!   `#top` always resolve). References in style sheets resolve against the style sheet's folder.
//! - **`web.site_report`** (a read; no history step). Rows sorted by (document, line, reference): `broken_links`
//!   (`link_broken`, severity `warning`: referring document, reference, target, line, reason `missing` |
//!   `case_mismatch` | `folder_missing` | `anchor_missing`, load contexts), `orphaned_files` (`file_orphaned`,
//!   `info`: a file no document or style sheet references; documents are never orphans, and the site record
//!   `*.studio-site.json`, the export manifest and the publish receipt are excluded), `external_links`,
//!   `missing_alt` (an HTML `img` without `alt`), `untitled_documents` (no `title`, or only whitespace), `denied`
//!   (local references the engine or the walk refuses), `unparsed`, `skipped`. Each list holds at most 2000 rows;
//!   `truncated` counts the rest. Declared not done (`not_checked`): redundant nested tags, removable empty tags,
//!   lint threshold, checked-out files ([STU-WEB-121]).
//! - **Export of the set** (`web.export_recipe` `documents: site`): every document from its source (minified when
//!   the recipe says so), every file a document or an exported style sheet references, the manifest listing every
//!   document (`documents`, role `document`). The recipe's `file` must name a document of the set. A link to a page
//!   that does not exist is a `link_broken` warning row (in the manifest and the result), in both modes, at most 2000
//!   rows with `link_broken_truncated` counting the rest; any other missing or case-mismatched local reference
//!   refuses `asset_missing` before anything is written. The output must lie outside the site
//!   (`output_inside_site`). A manifest over 1 MiB (what publish and verify read back) refuses
//!   `export_manifest_too_large` before the first write, in both modes.
//! - **`web.move_file`** `{from, to}` (root-relative paths of one file): one transaction with a receipt. Refused
//!   while the open document has unsaved edits (`document_unsaved`) or its file changed on disk
//!   (`document_changed_on_disk`). `to` is compared case-folded with the walk: a folder of `to` that is a link or
//!   junction in any letter case refuses `move_outside_site`, a folder that exists in another letter case is used in
//!   its on-disk spelling (the receipt's `to`, every rewritten reference), and the resolved folder of `to` must lie
//!   under the resolved root. Every reference to `from` is rewritten in the spelling `site_relative_link_base` names
//!   (`document`: relative to the referring file; `site_root`: `/...`). In the moved file every relative reference
//!   (not `/...`, no scheme) is re-based by path arithmetic on its own bytes, whatever it reaches (a denied or missing
//!   one too; percent-encoding, query and fragment kept); one that cannot be spelled from the new folder (`site_root`
//!   above the root, an empty segment) refuses `reference_not_rewritable`. A reference whose bytes are its decoded
//!   text is rewritten in its path bytes only; one whose decoding moved offsets (character references) is rewritten
//!   through its whole attribute value (`escape_attr`, always quoted: an unquoted value gains `"`) when the value
//!   carries only that reference; otherwise `reference_not_rewritable` (with the document and line). The engine
//!   applies at most 1024 edits to one file (`Limits::max_edits`): a file needing more refuses with the engine's
//!   code. Every new source is parsed and its graph compared with the old one before the first write
//!   (`move_rewrite_unverified`): every reference must reach the same place (with `from` read as `to`; a denied one
//!   by its folder-resolved path), each rewritten reference must read as planned and every other keeps its text.
//!   Moving the recipe's `file` updates it (receipt `recipe_file`). Then
//!   the referring files are written through the atomic writer, the moved file's new bytes, and the rename last; a
//!   failed write restores every file already written from the in-memory originals and the refusal `move_failed`
//!   carries the receipt (`restored`, `failed`, `restore_failed`). Receipt: `from`, `to`, and per changed file every
//!   change's `before` and `after` byte range, references and line. Not a `web.undo` step: an open document that
//!   was moved or rewritten is reopened from disk with a fresh history; a reverse move restores the bytes.
//!   Refusals: `move_outside_site` (`..` past the root, or a path through a link in any letter case), `path_invalid`,
//!   `move_target_exists` (case-folded), `move_case_only` (a rename by letter case only), `move_source_missing` (not
//!   a file of the walk), `move_reserved_file` (site record, manifest, receipt), `move_site_record` (a document with
//!   a site record beside it), `move_kind_changes` (a document, style sheet or other file stays its kind),
//!   `document_unreadable`, and before admission (no file read) `no_site`, `parameter_missing`, `path_invalid`,
//!   `nothing_to_change`, `move_case_only`, `move_kind_changes`, `move_reserved_file`. Cancellation is checked per
//!   folder and file of the walk and before the first write; once writing starts the transaction completes or
//!   restores.
//! - **Site lock** ([STU-WEB-131]): `web.site_report`, `web.export` with `documents: site`, `web.move_file`,
//!   `web.publish` and `web.save` of a file inside `local_root` (lead Q2) create `<local_root>/.studio-site.lock`
//!   exclusively with the caller's actor, client session, command id and a nonce of this acquisition, and remove it
//!   on every exit path only while it still holds that nonce (a lock taken after a stale one was deleted by hand is
//!   left to its holder). Contention refuses `site_locked` at once, never waits; `target` names the holder and
//!   `detail.holder` is `{account_id, client_session, command_id}` from the file, never the whole file. There is no
//!   automatic stale-lock breaking: when the holder's process is gone (check the session it names), delete the file
//!   by hand.
//! - **Jobs** (R60 probe j3): `web.export`, `web.publish`, `web.site_report` and `web.move_file` are admitted after
//!   their checks that need no file (typed parameter and state refusals, never `overloaded`) and before any work (no
//!   lock, no read, no file) through the bound job port (`Run::bind_web_jobs`; the Viewer binds its own). A job is
//!   never resource-less (lead ruling 2026-11-08). Resource rule (lead Q1, what the job reads or writes): the open
//!   export and `web.publish` admit on the run's open package document at its head, as the viewer's other exports
//!   (`as_export_job`), and on the site's id when no package is open; the site export, `web.site_report` and
//!   `web.move_file` always admit on the site's id at the web source revision. The site's id is `SDOC-` in the
//!   UUIDv7 shape from the SHA-256 of the site's folder (`local_root`, else the open document's folder) made
//!   absolute and case-folded without touching the file system (`job_resource_invalid` if it ever failed to parse).
//!   Actions: `export` for the exports, publish and `web.site_report` (the session's `Action` has no read class:
//!   `edit`, `save`, `export`), `edit` for `web.move_file`. Refusals: `overloaded` (`target` `retry_after_ms=N`,
//!   `detail.retry_after_ms`), `canceled` (a token canceled before the command is the lifecycle's
//!   `refused_canceled`; the port is not asked), `stale_revision`. Each admitted job settles `completed`, `canceled`
//!   or `failed` with its code, and an accepted command's result carries `job` (`outcome`, `ticket`,
//!   `bound_revision`, `correlation`, `frame_correlation` when the frame was delivered). `web.publish` checks the
//!   token before each file: a cancel refuses `canceled` with the receipt of the files already transferred.
//!   Deadline: none by nature; ended by cancel (a site walk, export or publish produces the operator's requested
//!   files; HBR-HOST-004).
//! - **The opened package**: `web.move_file` refuses `move_open_package` when `from` or `to` is the file the run's
//!   package was opened from, or a path inside it.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    MANIFEST_FILE, MAX_EXPORT_BYTES, MAX_EXPORT_FILES, MAX_RECORD_BYTES, MAX_ROWS, MAX_SEGMENTS, No, P, R, RECEIPT_FILE, Recipe, SIDECAR_SUFFIX, WebHost, actor_json, bounded, checked_path,
    join, minified, no, output_names, portable, read_bounded, resolved, web_no, write_atomic, write_new,
};
use hsk_studio_accord::{CancellationToken, CommandContext, CommandId, DomainId};
use hsk_studio_session::{Action, AdmissionJobPort, Cost, JobAdmissionRefusal, JobKind, JobLifecycle, JobOutcome, JobPort, JobRequest, JobSettlement, ReportStatus};
use hsk_studio_web::{
    AssetDecision, AssetOutcome, AssetPort, AssetSource, DenyReason, Limits, Load, Loads, Namespace, NodeId, Quote, SourcePatch, TextEdit, WebDoc, apply, collect_assets, escape_attr, parse, sha256_hex,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The commands admitted as jobs (module docs).
pub(super) const ADMITTED: [&str; 4] = ["web.export", "web.publish", "web.site_report", "web.move_file"];
/// `web.site_report`'s job action: the session's `Action` has no read class (`edit`, `save`, `export`).
pub(super) const REPORT_ACTION: Action = Action::Export;

/// What a job is admitted on (module docs, lead Q1).
pub(super) enum JobOn {
    /// The open package's document (the open export, publish), else the site.
    Open,
    /// The site's own id, package open or not (the site export, the report, a move).
    Site,
}

/// The site-level advisory lock, in `local_root` ([STU-WEB-131]).
pub const LOCK_FILE: &str = ".studio-site.lock";
/// Source bytes of every document and style sheet of one site.
const MAX_SITE_SOURCE_BYTES: u64 = 64 << 20;
/// References of the whole site.
const MAX_SITE_REFERENCES: usize = 1 << 18;
/// Directory entries the walk reads (skipped ones included).
const MAX_WALK_ENTRIES: usize = 16384;
/// A style sheet is scanned as the body of a `style` element: the engine's CSS tokenizer and carriers, no copy.
const SHEET_HEAD: &str = "<!DOCTYPE html><style>";
const SHEET_TAIL: &str = "</style>";
/// HTML's ASCII whitespace (never Unicode `White_Space`).
const ASCII_WS: [char; 5] = [' ', '\t', '\n', '\r', '\u{c}'];

/// The job port and lifecycle the host admits its site-wide work through (module docs).
pub struct Jobs {
    port: Arc<dyn JobPort>,
    lifecycle: JobLifecycle,
}

impl Default for Jobs {
    fn default() -> Self {
        Self { port: Arc::new(AdmissionJobPort::default()), lifecycle: JobLifecycle::default() }
    }
}

impl Jobs {
    pub(super) fn new(port: Arc<dyn JobPort>, lifecycle: JobLifecycle) -> Self {
        Self { port, lifecycle }
    }
}

/// What a file is to the site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Page,
    Sheet,
    Other,
}

fn role(id: &str) -> Role {
    let name = id.rsplit('/').next().unwrap_or(id);
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => Role::Page,
        Some("css") => Role::Sheet,
        _ => Role::Other,
    }
}

/// A name the host writes beside the site's own files: never an orphan, never moved.
fn reserved(id: &str) -> bool {
    let name = id.rsplit('/').next().unwrap_or(id);
    name.ends_with(SIDECAR_SUFFIX) || name == MANIFEST_FILE || name == RECEIPT_FILE || name == LOCK_FILE
}

/// The walked files, folders and skipped links of a site.
#[derive(Clone, Default)]
struct Files {
    files: BTreeSet<String>,
    /// ASCII-lowercased id -> the first id of that spelling (sorted, so deterministic).
    folded: BTreeMap<String, String>,
    dirs: BTreeSet<String>,
    /// ASCII-lowercased folder -> its walked spelling.
    folded_dirs: BTreeMap<String, String>,
    /// Links and junctions, ASCII-lowercased (never followed, in any letter case).
    links: BTreeSet<String>,
}

impl Files {
    fn insert(&mut self, id: String) {
        self.folded.entry(id.to_ascii_lowercase()).or_insert_with(|| id.clone());
        self.files.insert(id);
    }

    fn insert_dir(&mut self, id: String) {
        self.folded_dirs.entry(id.to_ascii_lowercase()).or_insert_with(|| id.clone());
        self.dirs.insert(id);
    }

    fn through_link(&self, segments: &[String]) -> bool {
        (1..=segments.len()).any(|n| segments.get(..n).is_some_and(|s| self.links.contains(&s.join("/").to_ascii_lowercase())))
    }

    /// The set after `from` became `to` (its folders added).
    fn moved(&self, from: &str, to: &str) -> Files {
        let mut next = Files { dirs: self.dirs.clone(), folded_dirs: self.folded_dirs.clone(), links: self.links.clone(), ..Files::default() };
        for id in &self.files {
            next.insert(if id == from { to.to_string() } else { id.clone() });
        }
        let segments: Vec<&str> = to.split('/').collect();
        for n in 1..segments.len() {
            if let Some(dir) = segments.get(..n) {
                next.insert_dir(dir.join("/"));
            }
        }
        next
    }

    /// `to` in the walk's spelling (review F1): a folder that exists in another letter case takes its walked
    /// spelling; a folder that is a link in any letter case refuses `move_outside_site`; a file or folder already
    /// at `to`, or a file at one of its folders, refuses `move_target_exists`.
    fn spelled(&self, to: &str) -> R<String> {
        let requested = split_path(to);
        let mut out: Vec<String> = Vec::with_capacity(requested.len());
        for (i, segment) in requested.iter().enumerate() {
            let prefix = requested.get(..=i).map(|s| s.join("/").to_ascii_lowercase()).unwrap_or_default();
            let last = i.saturating_add(1) == requested.len();
            if self.links.contains(&prefix) {
                return no("move_outside_site", to);
            }
            if self.folded.contains_key(&prefix) || (last && self.folded_dirs.contains_key(&prefix)) {
                return no("move_target_exists", to);
            }
            let walked = self.folded_dirs.get(&prefix).filter(|_| !last).and_then(|d| d.rsplit('/').next());
            out.push(walked.unwrap_or(segment.as_str()).to_string());
        }
        Ok(out.join("/"))
    }
}

/// What a reference reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reach {
    File(String),
    /// No file of that id; `actual` exists in another letter case.
    Case {
        id: String,
        actual: String,
    },
    Missing(String),
    /// A folder (navigation only); "" is the root.
    Folder {
        path: String,
        exists: bool,
    },
    External,
    Denied(&'static str),
    Inline,
    SameDocument,
}

impl Reach {
    /// The place reached, for the graph comparison of a move: a file path (whether it exists or not), a folder, or
    /// the reference's kind.
    fn place(&self, rename: Option<(&str, &str)>, reference: &str, fragment: Option<&str>, dir: &[String]) -> (u8, String) {
        match self {
            Reach::File(id) | Reach::Case { id, .. } | Reach::Missing(id) => (1, rename.filter(|(f, _)| f == id).map_or_else(|| id.clone(), |(_, t)| t.to_string())),
            Reach::Folder { path, .. } => (2, path.clone()),
            Reach::External => (3, reference.to_string()),
            // A denied reference by the place its own bytes name from its folder (review F2), so a move that changes
            // where it points is caught.
            Reach::Denied(code) => (4, denied_place(dir, reference).unwrap_or_else(|| (*code).to_string())),
            Reach::Inline => (5, String::new()),
            Reach::SameDocument => (6, fragment.unwrap_or("").to_string()),
        }
    }
}

/// One reference of the site graph.
#[derive(Clone, Debug)]
struct Edge {
    node: NodeId,
    source: AssetSource,
    reference: String,
    /// In the file's own text (a style sheet's wrapper removed).
    span: Range<usize>,
    line: usize,
    loads: Option<Loads>,
    reach: Reach,
    fragment: Option<String>,
    /// The reference's path starts at the root (`/...`).
    rooted: bool,
}

/// A parsed document or style sheet.
struct Source {
    id: String,
    role: Role,
    doc: WebDoc,
    edges: Vec<Edge>,
}

impl Source {
    fn head(&self) -> usize {
        if self.role == Role::Sheet { SHEET_HEAD.len() } else { 0 }
    }

    fn text(&self) -> &str {
        let src = self.doc.source();
        let end = if self.role == Role::Sheet { src.len().saturating_sub(SHEET_TAIL.len()) } else { src.len() };
        src.get(self.head()..end).unwrap_or("")
    }
}

fn dir_of(id: &str) -> Vec<String> {
    let mut segments: Vec<String> = id.split('/').map(str::to_string).collect();
    segments.pop();
    segments
}

fn line_of(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |t| t.bytes().filter(|b| *b == b'\n').count()).saturating_add(1)
}

/// `path` resolved against `base` (`/...` against the root): the segments and whether it names a folder; `None`
/// when it leaves the root or a segment is not a portable name.
fn segments_of(base: &[String], path: &str) -> Option<(Vec<String>, bool)> {
    if path.len() > 1024 {
        return None;
    }
    let (mut out, rest) = match path.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => (base.to_vec(), path),
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let mut folder = false;
    for (i, part) in parts.iter().enumerate() {
        let last = i.saturating_add(1) == parts.len();
        match *part {
            "." => folder = last,
            ".." => {
                out.pop()?;
                folder = last;
            }
            "" if last => folder = true,
            name if portable(name) => {
                out.push(name.to_string());
                folder = false;
            }
            _ => return None,
        }
    }
    (out.len() <= MAX_SEGMENTS).then_some((out, folder))
}

/// The reference gate's port over the walked set: every decision is recorded in the granted id (load contexts and
/// what the reference reaches), so the graph keeps them; `Denied` only for a path that leaves the root, is not
/// portable, runs through a link, or names a folder outside navigation.
struct SitePort<'a> {
    files: &'a Files,
    dir: Vec<String>,
    me: String,
    url: String,
}

impl AssetPort for SitePort<'_> {
    fn document_url(&self) -> Option<&str> {
        Some(&self.url)
    }

    fn resolve(&self, reference: &str, loads: Loads) -> AssetDecision {
        match self.reach(reference, loads) {
            Some(reach) => AssetDecision::Granted { id: encode(loads, &reach) },
            None => AssetDecision::Denied,
        }
    }
}

impl SitePort<'_> {
    fn reach(&self, reference: &str, loads: Loads) -> Option<Reach> {
        let path = reference.split(['?', '#']).next().unwrap_or("");
        if path.is_empty() {
            return Some(Reach::File(self.me.clone()));
        }
        let (segments, folder) = segments_of(&self.dir, path)?;
        if self.files.through_link(&segments) {
            return None;
        }
        let id = segments.join("/");
        if folder || id.is_empty() || self.files.dirs.contains(&id) {
            return (loads == Loads::of(Load::Document)).then(|| Reach::Folder { exists: id.is_empty() || self.files.dirs.contains(&id), path: id });
        }
        if self.files.files.contains(&id) {
            return Some(Reach::File(id));
        }
        Some(match self.files.folded.get(&id.to_ascii_lowercase()) {
            Some(actual) => Reach::Case { actual: actual.clone(), id },
            None => Reach::Missing(id),
        })
    }
}

fn loads_bits(loads: Loads) -> u16 {
    Load::ALL.iter().enumerate().filter(|(_, l)| loads.contains(**l)).fold(0u16, |bits, (i, _)| bits | 1u16.checked_shl(u32::try_from(i).unwrap_or(15)).unwrap_or(0))
}

fn loads_of(bits: u16) -> Loads {
    Load::ALL.iter().enumerate().filter(|(i, _)| bits & 1u16.checked_shl(u32::try_from(*i).unwrap_or(15)).unwrap_or(0) != 0).fold(Loads::EMPTY, |l, (_, load)| l.and(*load))
}

/// `<4 hex load bits><tag><payload>`: the id is opaque to the engine.
fn encode(loads: Loads, reach: &Reach) -> String {
    let bits = loads_bits(loads);
    match reach {
        Reach::File(id) => format!("{bits:04x}f{id}"),
        Reach::Missing(id) => format!("{bits:04x}m{id}"),
        Reach::Case { id, actual } => format!("{bits:04x}c{id}|{actual}"),
        Reach::Folder { path, exists: true } => format!("{bits:04x}d{path}"),
        Reach::Folder { path, exists: false } => format!("{bits:04x}e{path}"),
        _ => format!("{bits:04x}x"),
    }
}

fn decode(id: &str) -> Option<(Loads, Reach)> {
    let bits = u16::from_str_radix(id.get(..4)?, 16).ok()?;
    let payload = id.get(5..)?.to_string();
    let reach = match id.get(4..5)? {
        "f" => Reach::File(payload),
        "m" => Reach::Missing(payload),
        "c" => {
            let (id, actual) = payload.split_once('|')?;
            Reach::Case { id: id.to_string(), actual: actual.to_string() }
        }
        "d" => Reach::Folder { path: payload, exists: true },
        "e" => Reach::Folder { path: payload, exists: false },
        _ => return None,
    };
    Some((loads_of(bits), reach))
}

/// The site's sorted walk (module docs).
struct Walk {
    files: Files,
    skipped: Vec<(String, &'static str)>,
}

fn walk(root: &Path, cancel: &CancellationToken) -> R<Walk> {
    let mut out = Walk { files: Files::default(), skipped: Vec::new() };
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    let mut seen = 0usize;
    while let Some((dir, prefix)) = stack.pop() {
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        let unreadable = || No("site_unreadable", bounded(&prefix.join("/")), None);
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|_| unreadable())? {
            let entry = entry.map_err(|_| unreadable())?;
            seen = seen.saturating_add(1);
            if seen > MAX_WALK_ENTRIES {
                return no("site_too_many_files", &seen.to_string());
            }
            entries.push((entry.file_name(), entry.file_type().map_err(|_| unreadable())?));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, kind) in entries {
            let Some(name) = name.to_str() else {
                out.skipped.push((bounded(&format!("{}/{}", prefix.join("/"), name.to_string_lossy())), "name_not_utf8"));
                continue;
            };
            if prefix.is_empty() && name == LOCK_FILE {
                continue;
            }
            let mut segments = prefix.clone();
            segments.push(name.to_string());
            let id = segments.join("/");
            if !portable(name) {
                out.skipped.push((bounded(&id), "name_not_portable"));
            } else if kind.is_symlink() {
                out.skipped.push((bounded(&id), "link"));
                out.files.links.insert(id.to_ascii_lowercase());
            } else if kind.is_dir() {
                if segments.len() >= MAX_SEGMENTS {
                    return no("site_too_deep", &id);
                }
                out.files.insert_dir(id);
                stack.push((dir.join(name), segments));
            } else if kind.is_file() {
                if out.files.files.len() >= MAX_EXPORT_FILES {
                    return no("site_too_many_files", &out.files.files.len().saturating_add(1).to_string());
                }
                out.files.insert(id);
            } else {
                out.skipped.push((bounded(&id), "not_a_file"));
            }
        }
    }
    out.skipped.sort();
    Ok(out)
}

/// The open document as the site sees it: its id under the root and its live source.
struct Open<'a> {
    path: &'a Path,
    doc: &'a WebDoc,
}

/// The site's documents, style sheets and graph.
struct SiteSet {
    files: Files,
    sources: Vec<Source>,
    unparsed: Vec<(String, &'static str)>,
    skipped: Vec<(String, &'static str)>,
    /// The open document's id, when it is a file of the site.
    open_id: Option<String>,
    /// Source bytes as read from disk, by id (the move's restore copy).
    disk: BTreeMap<String, String>,
}

fn canceled(e: &hsk_studio_web::WebError) -> No {
    web_no(e)
}

fn id_in(root: &Path, path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    let rel = real.strip_prefix(root).ok()?;
    let segments: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    segments.filter(|s| !s.is_empty()).map(|s| s.join("/"))
}

/// One parsed source and its edges, or the code it is unparsed with.
fn source(files: &Files, id: &str, role: Role, doc: Option<WebDoc>, text: &str, cancel: &CancellationToken) -> R<Result<Source, &'static str>> {
    let doc = match (doc, role) {
        (Some(doc), _) => doc,
        (None, Role::Page) => match parse(text, 1, &Limits::default(), cancel) {
            Ok(doc) => doc,
            Err(e) if e.code() == "canceled" => return Err(canceled(&e)),
            Err(e) => return Ok(Err(e.code())),
        },
        (None, _) => {
            // Review F8: each `</style` read as `<_style` (same length: every span stays the file's own).
            match parse(&format!("{SHEET_HEAD}{}{SHEET_TAIL}", masked_sheet(text)), 1, &Limits::default(), cancel) {
                Ok(doc) => doc,
                Err(e) if e.code() == "canceled" => return Err(canceled(&e)),
                Err(e) => return Ok(Err(e.code())),
            }
        }
    };
    let mut src = Source { id: id.to_string(), role, doc, edges: Vec::new() };
    match edges_of(files, &src, cancel) {
        Ok(edges) => src.edges = edges,
        Err(e) if e.code() == "canceled" => return Err(canceled(&e)),
        Err(e) => return Ok(Err(e.code())),
    }
    Ok(Ok(src))
}

/// A style sheet's text with each `</style` (any letter case) read as `<_style`: the same length, so the scan's spans
/// are the file's own; the file itself is never changed.
fn masked_sheet(text: &str) -> String {
    const END: &[u8] = b"</style";
    let mut bytes = text.as_bytes().to_vec();
    let mut at = 0usize;
    while let Some(window) = bytes.get(at..at.saturating_add(END.len())) {
        if window.eq_ignore_ascii_case(END) {
            if let Some(slash) = bytes.get_mut(at.saturating_add(1)) {
                *slash = b'_';
            }
            at = at.saturating_add(END.len());
        } else {
            at = at.saturating_add(1);
        }
    }
    // Only an ASCII byte was replaced by an ASCII byte, so the text stays UTF-8.
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_string())
}

fn edges_of(files: &Files, src: &Source, cancel: &CancellationToken) -> Result<Vec<Edge>, hsk_studio_web::WebError> {
    let port = SitePort { files, dir: dir_of(&src.id), me: src.id.clone(), url: format!("/{}", src.id) };
    let refs = collect_assets(&src.doc, &port, cancel)?;
    let head = src.head();
    let text = src.text();
    Ok(refs
        .into_iter()
        .map(|r| {
            let (loads, reach) = match &r.outcome {
                AssetOutcome::Granted { id } => decode(id).map_or((None, Reach::Denied("not_granted")), |(l, reach)| (Some(l), reach)),
                AssetOutcome::Unavailable => (None, Reach::Denied("unavailable")),
                AssetOutcome::Denied(DenyReason::ExternalScheme | DenyReason::ProtocolRelative) => (None, Reach::External),
                AssetOutcome::Denied(reason) => (None, Reach::Denied(reason.code())),
                AssetOutcome::Inline => (None, Reach::Inline),
                AssetOutcome::SameDocument => (None, Reach::SameDocument),
            };
            let span = r.span.start.saturating_sub(head)..r.span.end.saturating_sub(head);
            Edge {
                node: r.node,
                line: line_of(text, span.start),
                span,
                loads,
                fragment: r.reference.split_once('#').map(|(_, f)| f.to_string()),
                rooted: r.reference.starts_with('/'),
                reach,
                source: r.source,
                reference: r.reference,
            }
        })
        .collect())
}

impl SiteSet {
    /// Walks and parses the site; `open` contributes its live source.
    fn load(root: &Path, open: Option<Open<'_>>, cancel: &CancellationToken) -> R<SiteSet> {
        let canonical = std::fs::canonicalize(root).ok().filter(|c| c.is_dir()).ok_or(No("site_root_missing", bounded(&root.to_string_lossy()), None))?;
        let walked = walk(root, cancel)?;
        let open_id = open.as_ref().and_then(|o| id_in(&canonical, o.path)).filter(|id| walked.files.files.contains(id));
        let mut set = SiteSet { files: walked.files, sources: Vec::new(), unparsed: Vec::new(), skipped: walked.skipped, open_id, disk: BTreeMap::new() };
        let limit = u64::try_from(Limits::default().max_bytes).unwrap_or(u64::MAX);
        let mut total = 0u64;
        let mut references = 0usize;
        let ids: Vec<String> = set.files.files.iter().filter(|id| role(id) != Role::Other).cloned().collect();
        for id in ids {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            let role = role(&id);
            let sheet_room = if role == Role::Sheet { u64::try_from(SHEET_HEAD.len().saturating_add(SHEET_TAIL.len())).unwrap_or(u64::MAX) } else { 0 };
            let text = match read_bounded(&join(root, &id), limit.saturating_sub(sheet_room)) {
                Ok(bytes) => {
                    total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    if total > MAX_SITE_SOURCE_BYTES {
                        return no("site_too_large", &id);
                    }
                    match String::from_utf8(bytes) {
                        Ok(text) => text,
                        Err(_) => {
                            set.unparsed.push((id, "source_not_utf8"));
                            continue;
                        }
                    }
                }
                Err(No(code, _, _)) => {
                    set.unparsed.push((id, code));
                    continue;
                }
            };
            let live = open.as_ref().filter(|_| role == Role::Page && set.open_id.as_deref() == Some(id.as_str())).map(|o| o.doc.clone());
            let parsed = source(&set.files, &id, role, live, &text, cancel)?;
            set.disk.insert(id.clone(), text);
            match parsed {
                Ok(src) => {
                    references = references.saturating_add(src.edges.len());
                    if references > MAX_SITE_REFERENCES {
                        return no("site_too_many_references", &id);
                    }
                    set.sources.push(src);
                }
                Err(code) => set.unparsed.push((id, code)),
            }
        }
        Ok(set)
    }

    fn page(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id && s.role == Role::Page)
    }

    /// Whether `fragment` names a place in page `doc` (HTML: an `id`, else an `a name`; `#` and `#top` always).
    fn anchored(doc: &WebDoc, fragment: &str) -> bool {
        if fragment.is_empty() || fragment.eq_ignore_ascii_case("top") {
            return true;
        }
        let decoded = percent_decoded(fragment);
        [Some(fragment.to_string()), decoded].into_iter().flatten().any(|f| {
            doc.find_by_html_id(&f).is_some() || doc.find_by_tag("a").into_iter().any(|a| doc.attribute(a, "name").and_then(|n| doc.attr_value(n)).as_deref() == Some(f.as_str()))
        })
    }

    /// The broken reason of an edge, if it is broken.
    fn broken(&self, src: &Source, e: &Edge) -> Option<(&'static str, String)> {
        match &e.reach {
            Reach::Missing(id) => Some(("missing", id.clone())),
            Reach::Case { id, .. } => Some(("case_mismatch", id.clone())),
            Reach::Folder { path, exists: false } => Some(("folder_missing", format!("{path}/"))),
            Reach::File(id) => {
                let fragment = e.fragment.as_deref()?;
                let page = self.page(id)?;
                (!Self::anchored(&page.doc, fragment)).then(|| ("anchor_missing", format!("{id}#{fragment}")))
            }
            Reach::SameDocument if src.role == Role::Page => {
                let fragment = e.fragment.as_deref()?;
                (!Self::anchored(&src.doc, fragment)).then(|| ("anchor_missing", format!("#{fragment}")))
            }
            _ => None,
        }
    }
}

fn percent_decoded(text: &str) -> Option<String> {
    if !text.contains('%') {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        let hex = (b == b'%').then(|| bytes.get(i.saturating_add(1)..i.saturating_add(3))).flatten().and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i = i.saturating_add(3);
            }
            None => {
                out.push(b);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8(out).ok()
}

fn load_tokens(loads: Option<Loads>) -> Vec<String> {
    loads.map_or_else(Vec::new, |l| l.iter().map(|load| format!("{load:?}").to_ascii_lowercase()).collect())
}

/// At most `MAX_ROWS` rows (sorted by the caller); the rest counted.
fn capped(mut rows: Vec<(String, usize, String, Value)>, truncated: &mut BTreeMap<&'static str, usize>, group: &'static str) -> Vec<Value> {
    rows.sort_by(|a, b| (&a.0, a.1, &a.2).cmp(&(&b.0, b.1, &b.2)));
    if rows.len() > MAX_ROWS {
        truncated.insert(group, rows.len().saturating_sub(MAX_ROWS));
    }
    rows.into_iter().take(MAX_ROWS).map(|r| r.3).collect()
}

/// An export's `link_broken` rows, at most `MAX_ROWS` (review F4), and how many were left out.
pub(super) fn capped_rows(mut rows: Vec<Value>) -> (Vec<Value>, usize) {
    let truncated = rows.len().saturating_sub(MAX_ROWS);
    rows.truncate(MAX_ROWS);
    (rows, truncated)
}

/// An export manifest's bytes, refused `export_manifest_too_large` over what publish and verify read back
/// (`MAX_RECORD_BYTES`), before the first write (review F4).
pub(super) fn manifest_bytes(manifest: &Value) -> R<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| No("export_failed", String::new(), None))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
        return no("export_manifest_too_large", &bytes.len().to_string());
    }
    Ok(bytes)
}

/// The refusal while a file of the site is unparsed (lead Q4: fail closed, the file and its reason named).
fn unreadable(set: &SiteSet) -> R<()> {
    match set.unparsed.first() {
        Some((id, reason)) => Err(No("document_unreadable", bounded(&format!("{id}: {reason}")), Some(json!({"file": id, "reason": reason})))),
        None => Ok(()),
    }
}

/// The settlement of an admitted job as JSON (the viewer's `settlement_json` shape).
fn job_json(s: &JobSettlement) -> Value {
    let mut v = json!({"outcome": s.outcome.code(), "ticket": s.ticket.map(|t| t.0), "bound_revision": s.bound_revision, "correlation": s.correlation_id});
    if let ReportStatus::Delivered(receipt) = &s.report {
        v["frame_correlation"] = json!(receipt.correlation_id);
    }
    if let JobOutcome::Failed(code) = s.outcome {
        v["code"] = json!(code);
    }
    v
}

/// Lock acquisitions of this process (part of every nonce).
static LOCKS: AtomicU64 = AtomicU64::new(0);

/// The site lock's guard: the file is removed on every exit path, only while it holds this acquisition's nonce.
pub(super) struct SiteLock {
    path: PathBuf,
    nonce: String,
}

impl Drop for SiteLock {
    fn drop(&mut self) {
        let held: Option<Value> = read_bounded(&self.path, MAX_RECORD_BYTES).ok().and_then(|b| serde_json::from_slice(&b).ok());
        if held.is_some_and(|h| h["nonce"].as_str() == Some(self.nonce.as_str())) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Takes the site lock or refuses `site_locked` naming the holder (never waits).
pub(super) fn site_lock(local_root: &str, ctx: &CommandContext) -> R<SiteLock> {
    use std::io::Write;
    let root = PathBuf::from(local_root);
    if !root.is_dir() {
        return no("site_root_missing", local_root);
    }
    let path = root.join(LOCK_FILE);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let nonce = format!("{}-{}-{}-{nanos}", std::process::id(), LOCKS.fetch_add(1, Ordering::Relaxed), ctx.correlation().value());
    let holder = json!({
        "schema": "hsk.studio.web.site_lock@1", "actor": actor_json(ctx.actor()), "client_session": ctx.client_session().as_str(), "command_id": ctx.command().map(CommandId::as_str), "nonce": nonce,
    });
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let lock = SiteLock { path, nonce };
            file.write_all(&serde_json::to_vec(&holder).unwrap_or_default()).map_err(|_| No("site_lock_failed", LOCK_FILE.into(), None))?;
            Ok(lock)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let held: Value = read_bounded(&path, MAX_RECORD_BYTES).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
            // Three fields of the holder's record, each bounded; never the whole file (review F7).
            let field = |v: &Value| super::row_text(v.as_str().unwrap_or("?"));
            let holder = json!({"account_id": field(&held["actor"]["account_id"]), "client_session": field(&held["client_session"]), "command_id": field(&held["command_id"])});
            let target = format!("held by {} in session {} running {}", field(&held["actor"]["account_id"]), field(&held["client_session"]), field(&held["command_id"]));
            Err(No(
                "site_locked",
                bounded(&target),
                Some(json!({"holder": holder, "lock_file": LOCK_FILE, "manual_step": "when the holder's session is gone, delete the lock file by hand; Studio never breaks a lock"})),
            ))
        }
        Err(_) => no("site_lock_failed", LOCK_FILE),
    }
}

/// A root-relative path parameter of one file: `None` names why it is refused.
fn site_path(text: &str) -> R<String> {
    let segments = segments_of(&[], text);
    match segments {
        Some((segments, false)) if !segments.is_empty() && segments.join("/") == text => Ok(text.to_string()),
        _ if text.split(['/', '\\']).any(|s| s == "..") && !text.contains('\\') => no("move_outside_site", text),
        _ => no("path_invalid", text),
    }
}

/// The spelling of `target` from a file in `dir` (module docs: `site_relative_link_base`).
fn spell(link_base: &str, dir: &[String], target: &[String], folder: bool) -> String {
    if link_base == "site_root" {
        let mut out = format!("/{}", target.join("/"));
        if folder && !target.is_empty() {
            out.push('/');
        }
        return out;
    }
    let common = dir.iter().zip(target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; dir.len().saturating_sub(common)];
    parts.extend(target.iter().skip(common).map(String::as_str));
    let mut out = parts.join("/");
    if folder {
        if out.is_empty() {
            out.push('.');
        }
        out.push('/');
    }
    out
}

fn split_path(id: &str) -> Vec<String> {
    if id.is_empty() { Vec::new() } else { id.split('/').map(str::to_string).collect() }
}

/// Whether a reference's path starts with a URL scheme (`mailto:`, `data:`, a drive letter): not a site path.
fn has_scheme(path: &str) -> bool {
    path.split_once(':').is_some_and(|(scheme, _)| {
        scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// A relative path's bytes resolved against `dir` without decoding them: the levels above the root, the segments
/// below and whether it names a folder; `None` for an empty segment inside the path.
fn lexical(dir: &[String], path: &str) -> Option<(usize, Vec<String>, bool)> {
    let (mut at, rest) = match path.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => (dir.to_vec(), path),
    };
    let mut up = 0usize;
    let mut folder = false;
    let parts: Vec<&str> = rest.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let last = i.saturating_add(1) == parts.len();
        match *part {
            "." => folder = last,
            ".." => {
                if at.pop().is_none() {
                    up = up.saturating_add(1);
                }
                folder = last;
            }
            "" if last => folder = true,
            "" => return None,
            name => {
                at.push(name.to_string());
                folder = false;
            }
        }
    }
    Some((up, at, folder))
}

/// Where a denied reference's own bytes point from `dir` (review F2): `up:segments[/]`, or `None` for a scheme or a
/// path that cannot be resolved.
fn denied_place(dir: &[String], reference: &str) -> Option<String> {
    let path = reference.split(['?', '#']).next().unwrap_or("");
    if path.is_empty() || has_scheme(path) || path.starts_with("//") {
        return None;
    }
    let (up, at, folder) = lexical(dir, path)?;
    Some(format!("{up}:{}{}", at.join("/"), if folder { "/" } else { "" }))
}

/// A relative reference path of the moved file, re-based by path arithmetic on its own bytes (review F2): the place
/// it names from `old_dir`, spelled from `new_dir` (`from` read as `to`); `None` when that cannot be spelled.
fn rebased(link_base: &str, old_dir: &[String], new_dir: &[String], path: &str, from: &str, to: &str) -> Option<String> {
    let (up, mut at, folder) = lexical(old_dir, path)?;
    if up == 0 && !folder && at.join("/") == from {
        at = split_path(to);
    }
    if up == 0 {
        return Some(spell(link_base, new_dir, &at, folder));
    }
    if link_base == "site_root" {
        return None;
    }
    let mut parts: Vec<&str> = vec![".."; new_dir.len().saturating_add(up)];
    parts.extend(at.iter().map(String::as_str));
    let mut out = parts.join("/");
    if folder {
        out.push('/');
    }
    Some(out)
}

/// `original` with the edits applied (spans in the scanned source, `head` bytes before the file's own text).
fn applied_text(original: &str, changes: &[Change], head: usize) -> Option<String> {
    let mut out = String::with_capacity(original.len());
    let mut at = 0usize;
    for c in changes {
        let start = c.edit.span.start.checked_sub(head)?;
        let end = c.edit.span.end.checked_sub(head)?;
        out.push_str(original.get(at..start)?);
        out.push_str(&c.edit.replacement);
        at = end;
    }
    out.push_str(original.get(at..)?);
    Some(out)
}

/// One planned change of a move.
struct Change {
    /// The changed reference's position among its file's edges.
    index: usize,
    edit: TextEdit,
    line: usize,
    before: String,
    after: String,
}

impl WebHost {
    /// Admits `work` as one job (module docs) and settles it with the work's outcome; an accepted result carries the
    /// settlement as `job`.
    pub(super) fn admitted(&mut self, action: Action, on: JobOn, ctx: &CommandContext, cancel: &CancellationToken, work: impl FnOnce(&mut Self) -> R<Value>) -> R<Value> {
        // Never resource-less (lead ruling 2026-11-08, R60 probe j3); the resource is what the job reads or writes
        // (lead Q1): the open package for the open export and publish, else the site.
        let (resource, head) = match (on, self.job_resource.clone()) {
            (JobOn::Open, Some(id)) => (id, self.job_head),
            _ => (self.site_resource()?, self.revision()),
        };
        let request = JobRequest { resource: Some(resource), action, actor: ctx.actor().clone(), cost: Cost { jobs: 1, bytes: 0 }, bound_revision: head, correlation_id: ctx.correlation().value(), kind: JobKind::Committed };
        let job = self.jobs.lifecycle.admit(&self.jobs.port, request, head, cancel).map_err(|refusal| match refusal {
            JobAdmissionRefusal::Overloaded { retry_after_ms } => No("overloaded", format!("retry_after_ms={retry_after_ms}"), Some(json!({"retry_after_ms": retry_after_ms}))),
            other => No(other.code(), String::new(), None),
        })?;
        let mut result = work(self);
        let settled = job.settle(match &result {
            Ok(_) => JobOutcome::Completed,
            Err(No("canceled", ..)) => JobOutcome::Canceled,
            Err(No(code, ..)) => JobOutcome::Failed(code),
        });
        if let Ok(value) = &mut result
            && value.is_object()
        {
            value["job"] = job_json(&settled);
        }
        result
    }

    /// The site's job resource (module docs): an `SDOC` id in the UUIDv7 shape whose digits are the SHA-256 of the
    /// site's folder (`local_root`, else the open document's folder), made absolute and ASCII case-folded without
    /// touching the file system (review F5: nothing is read before the port is asked), so every job on one site
    /// addresses one resource, the same in every process.
    fn site_resource(&self) -> R<DomainId> {
        let folder = match (&self.site, &self.path) {
            (Some(site), _) => PathBuf::from(&site.local_root),
            (None, Some(path)) => path.parent().map_or_else(PathBuf::new, Path::to_path_buf),
            (None, None) => PathBuf::new(),
        };
        let absolute = std::path::absolute(&folder).unwrap_or(folder);
        let key = absolute.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_ascii_lowercase();
        let hex = sha256_hex(format!("hsk.studio.web.site\0{key}").as_bytes());
        let g = |r: Range<usize>| hex.get(r).unwrap_or("0");
        let variant = match g(16..17) {
            "0" | "4" | "8" | "c" => "8",
            "1" | "5" | "9" | "d" => "9",
            "2" | "6" | "a" | "e" => "a",
            _ => "b",
        };
        let id = format!("SDOC-{}-{}-7{}-{variant}{}-{}", g(0..8), g(8..12), g(13..16), g(17..20), g(20..32));
        DomainId::parse(&id).map_err(|_| No("job_resource_invalid", id, None))
    }

    pub(super) fn site_root(&self) -> R<(super::Site, PathBuf)> {
        let site = self.site.clone().ok_or(No("no_site", String::new(), None))?;
        let root = PathBuf::from(checked_path(&site.local_root)?);
        Ok((site, root))
    }

    fn open_view(&self) -> Option<Open<'_>> {
        Some(Open { path: self.path.as_deref()?, doc: self.doc.as_ref()? })
    }

    /// `web.site_report` (module docs).
    pub(super) fn site_report(&self, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let _lock = site_lock(&site.local_root, ctx)?;
        let set = SiteSet::load(&root, self.open_view(), cancel)?;
        let mut referenced = BTreeSet::new();
        let (mut broken, mut external, mut denied, mut alt, mut untitled) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for src in &set.sources {
            for e in &src.edges {
                if let Reach::File(id) = &e.reach {
                    referenced.insert(id.clone());
                }
                if let Some((reason, target)) = set.broken(src, e) {
                    let row = json!({"code": "link_broken", "severity": "warning", "document": src.id, "line": e.line, "reference": super::row_text(&e.reference), "target": bounded(&target), "reason": reason, "load": load_tokens(e.loads)});
                    broken.push((src.id.clone(), e.line, e.reference.clone(), row));
                }
                match &e.reach {
                    Reach::External => external.push((src.id.clone(), e.line, e.reference.clone(), json!({"document": src.id, "line": e.line, "reference": super::row_text(&e.reference)}))),
                    Reach::Denied(reason) => denied.push((src.id.clone(), e.line, e.reference.clone(), json!({"document": src.id, "line": e.line, "reference": super::row_text(&e.reference), "reason": reason}))),
                    _ => {}
                }
            }
            if src.role != Role::Page {
                continue;
            }
            for img in src.doc.find_by_tag("img") {
                let Some(node) = src.doc.node(img).filter(|n| n.namespace == Namespace::Html) else { continue };
                if src.doc.attribute(img, "alt").is_none() {
                    let line = line_of(src.text(), node.span.start);
                    alt.push((src.id.clone(), line, String::new(), json!({"code": "missing_alt", "severity": "warning", "document": src.id, "line": line})));
                }
            }
            let titled = src.doc.find_by_tag("title").into_iter().any(|t| {
                src.doc.node(t).is_some_and(|n| n.namespace == Namespace::Html) && !src.doc.text_content(t).trim_matches(ASCII_WS).is_empty()
            });
            if !titled {
                untitled.push((src.id.clone(), 0, String::new(), json!({"code": "untitled_document", "severity": "warning", "document": src.id})));
            }
        }
        let orphans: Vec<(String, usize, String, Value)> = set
            .files
            .files
            .iter()
            .filter(|id| role(id) != Role::Page && !reserved(id) && !referenced.contains(*id))
            .map(|id| (id.clone(), 0, String::new(), json!({"code": "file_orphaned", "severity": "info", "file": id})))
            .collect();
        let mut truncated = BTreeMap::new();
        let unparsed: Vec<Value> = set.unparsed.iter().take(MAX_ROWS).map(|(f, c)| json!({"file": f, "code": c})).collect();
        let skipped: Vec<Value> = set.skipped.iter().take(MAX_ROWS).map(|(p, r)| json!({"path": p, "reason": r})).collect();
        for (group, len) in [("unparsed", set.unparsed.len()), ("skipped", set.skipped.len())] {
            if len > MAX_ROWS {
                truncated.insert(group, len.saturating_sub(MAX_ROWS));
            }
        }
        let report = json!({
            "schema": "hsk.studio.web.site_report@1", "site_id": site.site_id,
            "documents": set.sources.iter().filter(|s| s.role == Role::Page).count(), "stylesheets": set.sources.iter().filter(|s| s.role == Role::Sheet).count(),
            "files": set.files.files.len(), "references": set.sources.iter().map(|s| s.edges.len()).fold(0usize, usize::saturating_add),
            "graph_complete": set.unparsed.is_empty(),
            "broken_links": capped(broken, &mut truncated, "broken_links"),
            "orphaned_files": capped(orphans, &mut truncated, "orphaned_files"),
            "external_links": capped(external, &mut truncated, "external_links"),
            "missing_alt": capped(alt, &mut truncated, "missing_alt"),
            "untitled_documents": capped(untitled, &mut truncated, "untitled_documents"),
            "denied": capped(denied, &mut truncated, "denied"),
            "unparsed": unparsed, "skipped": skipped, "truncated": truncated,
            "not_checked": ["redundant_nested_tags", "removable_empty_tags", "lint_threshold", "checked_out_files"],
        });
        Ok(report)
    }

    /// `web.export` with `documents: site` (module docs).
    pub(super) fn export_site(&self, recipe: &Recipe, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let out = PathBuf::from(checked_path(&recipe.output_root)?);
        if resolved(&out).starts_with(resolved(&root)) {
            return no("output_inside_site", &recipe.output_root);
        }
        let _lock = site_lock(&site.local_root, ctx)?;
        let set = SiteSet::load(&root, self.open_view(), cancel)?;
        unreadable(&set)?;
        if set.page(&recipe.file).is_none() {
            return no("entry_document_missing", &recipe.file);
        }
        // The documents, then every file they reach, then what the exported style sheets reach (once each).
        let (mut assets, mut external, mut links, mut broken) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), Vec::new());
        let mut queue: Vec<&Source> = set.sources.iter().filter(|s| s.role == Role::Page).collect();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        while let Some(src) = queue.pop() {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            for e in &src.edges {
                let navigation = e.loads == Some(Loads::of(Load::Document));
                match &e.reach {
                    Reach::File(id) if set.page(id).is_some() => {}
                    Reach::File(id) => {
                        if assets.insert(id.clone())
                            && let Some(sheet) = set.sources.iter().find(|s| &s.id == id && s.role == Role::Sheet)
                            && seen.insert(sheet.id.as_str())
                        {
                            queue.push(sheet);
                        }
                    }
                    Reach::Missing(_) | Reach::Case { .. } if navigation => {
                        broken.push((src.id.clone(), e.line, e.reference.clone()));
                    }
                    Reach::Missing(_) | Reach::Case { .. } => return no("asset_missing", &e.reference),
                    Reach::Folder { .. } => {
                        links.insert(bounded(&e.reference));
                    }
                    Reach::External => {
                        external.insert(bounded(&e.reference));
                    }
                    Reach::Denied(reason) => return no("asset_denied", &format!("{reason}: {}", e.reference)),
                    Reach::Inline | Reach::SameDocument => {}
                }
            }
        }
        broken.sort();
        broken.dedup();
        let pages: Vec<&Source> = set.sources.iter().filter(|s| s.role == Role::Page).collect();
        output_names(assets.iter().chain(pages.iter().map(|p| &p.id)))?;
        if assets.len().saturating_add(pages.len()) > MAX_EXPORT_FILES {
            return no("export_too_many_files", &assets.len().to_string());
        }
        let mut files: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
        let mut total = 0u64;
        let mut documents = Vec::new();
        for page in &pages {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            let doc = if recipe.minify { minified(&page.doc, cancel)? } else { page.doc.clone() };
            total = total.saturating_add(u64::try_from(doc.source().len()).unwrap_or(u64::MAX));
            documents.push(json!({"path": page.id, "source_sha256": page.doc.sha256_hex()}));
            files.push((page.id.clone(), doc.source().as_bytes().to_vec(), "document"));
        }
        for id in &assets {
            let bytes = read_bounded(&join(&root, id), super::MAX_ASSET_BYTES)?;
            total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if total > MAX_EXPORT_BYTES {
                return no("export_too_large", id);
            }
            files.push((id.clone(), bytes, "asset"));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        if out.is_file() {
            return no("output_not_a_folder", &recipe.output_root);
        }
        if std::fs::read_dir(&out).is_ok_and(|mut d| d.next().is_some()) {
            return no("output_not_empty", &recipe.output_root);
        }
        let (link_broken, truncated) = capped_rows(broken.iter().map(|(d, line, r)| json!({"document": d, "line": line, "reference": super::row_text(r)})).collect());
        let listed: Vec<Value> = files.iter().map(|(path, bytes, role)| json!({"path": path, "role": role, "bytes": bytes.len(), "sha256": sha256_hex(bytes)})).collect();
        let mut manifest = json!({
            "schema": "hsk.studio.web.export_manifest@1", "document": recipe.file, "documents": documents,
            "recipe": {"file": recipe.file, "documents": "site", "minify": recipe.minify, "asset_rewriting": "keep", "link_base": recipe.link_base},
            "files": listed, "external": external.iter().collect::<Vec<_>>(), "links": links.iter().collect::<Vec<_>>(), "link_broken": link_broken,
        });
        if truncated > 0 {
            manifest["link_broken_truncated"] = json!(truncated);
        }
        let manifest_bytes = manifest_bytes(&manifest)?;
        for (path, bytes, _) in &files {
            write_new(&join(&out, path), bytes)?;
        }
        write_new(&out.join(MANIFEST_FILE), &manifest_bytes)?;
        let mut result = json!({"output_root": recipe.output_root, "manifest": manifest, "link_broken": link_broken});
        if truncated > 0 {
            result["link_broken_truncated"] = json!(truncated);
        }
        Ok(result)
    }

    /// `web.move_file`'s checks before admission (no I/O, review F5): `from` and `to` as root-relative ids.
    pub(super) fn move_checks(&self, p: &P<'_>) -> R<(String, String)> {
        self.site_root()?;
        let from = site_path(p.text("from")?)?;
        let to = site_path(p.text("to")?)?;
        if reserved(&from) || reserved(&to) {
            return no("move_reserved_file", if reserved(&from) { &from } else { &to });
        }
        if from == to {
            return no("nothing_to_change", &to);
        }
        if from.eq_ignore_ascii_case(&to) {
            return no("move_case_only", &to);
        }
        if role(&from) != role(&to) {
            return no("move_kind_changes", &to);
        }
        Ok((from, to))
    }

    /// `web.move_file` (module docs), after [`Self::move_checks`].
    pub(super) fn move_file(&mut self, from: &str, to: &str, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let (from, to) = (from.to_string(), to.to_string());
        if let Some(package) = self.package.as_deref().map(resolved)
            && let Some(id) = [&from, &to].into_iter().find(|id| {
                let at = resolved(&join(&root, id));
                at == package || at.starts_with(&package)
            })
        {
            return no("move_open_package", id);
        }
        if let (Some(_), Some(doc)) = (&self.path, &self.doc)
            && self.saved_sha256.as_deref() != Some(doc.sha256_hex())
        {
            return no("document_unsaved", "web.save first");
        }
        let _lock = site_lock(&site.local_root, ctx)?;
        // Every source from disk: the open document is saved, and a file changed under it is refused.
        let set = SiteSet::load(&root, None, cancel)?;
        let open_id = self.open_view().and_then(|o| std::fs::canonicalize(&root).ok().and_then(|c| id_in(&c, o.path))).filter(|id| set.files.files.contains(id));
        if let (Some(id), Some(doc)) = (&open_id, &self.doc)
            && set.disk.get(id).map(|t| sha256_hex(t.as_bytes())).as_deref() != Some(doc.sha256_hex())
        {
            return no("document_changed_on_disk", id);
        }
        if !set.files.files.contains(&from) {
            return no("move_source_missing", &from);
        }
        // Review F1: `to` in the walk's spelling, never through a link in any letter case, and its resolved folder
        // under the resolved root (a second guard before anything is written).
        let to = set.files.spelled(&to)?;
        if !resolved(&join(&root, &to)).starts_with(resolved(&root)) {
            return no("move_outside_site", &to);
        }
        if role(&from) == Role::Page && set.files.files.contains(&format!("{from}{SIDECAR_SUFFIX}")) {
            return no("move_site_record", &from);
        }
        unreadable(&set)?;
        // Plan every change, parse every result and compare the graphs before the first write.
        let moved_files = set.files.moved(&from, &to);
        let mut planned: Vec<(String, String, Vec<Change>, WebDoc)> = Vec::new();
        for src in &set.sources {
            let new_id = if src.id == from { to.clone() } else { src.id.clone() };
            let changes = plan(src, &from, &to, site.link_base)?;
            let next = if changes.is_empty() {
                src.doc.clone()
            } else {
                let applied = apply(&src.doc, &SourcePatch::new(&src.doc, changes.iter().map(|c| c.edit.clone()).collect())).map_err(|e| web_no(&e))?;
                parse(&applied.source, applied.revision, src.doc.limits(), cancel).map_err(|e| web_no(&e))?
            };
            let after = Source { id: new_id.clone(), role: src.role, doc: next, edges: Vec::new() };
            let edges = edges_of(&moved_files, &after, cancel).map_err(|e| web_no(&e))?;
            let rename = Some((from.as_str(), to.as_str()));
            let (old_dir, new_dir) = (dir_of(&src.id), dir_of(&new_id));
            // Every reference reaches the same place; a rewritten one reads as planned, every other keeps its text.
            let differs = |i: usize, n: &Edge, o: &Edge| {
                let expected = changes.iter().find(|c| c.index == i).map_or(o.reference.as_str(), |c| c.after.as_str());
                n.reach.place(None, &n.reference, n.fragment.as_deref(), &new_dir) != o.reach.place(rename, &o.reference, o.fragment.as_deref(), &old_dir) || n.reference != expected
            };
            if edges.len() != src.edges.len() {
                return no("move_rewrite_unverified", &format!("{new_id}:0"));
            }
            if let Some((n, _)) = edges.iter().zip(&src.edges).enumerate().find(|(i, (n, o))| differs(*i, *n, *o)).map(|(_, pair)| pair) {
                return no("move_rewrite_unverified", &format!("{new_id}:{}", n.line));
            }
            if !changes.is_empty() {
                planned.push((src.id.clone(), new_id, changes, after.doc));
            }
        }
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        // The receipt: per changed file, every change with its byte ranges before and after.
        let mut changed = Vec::new();
        let mut writes: Vec<(String, String)> = Vec::new();
        let mut moved_text = None;
        for (old_id, new_id, changes, _) in &planned {
            let head = if role(old_id) == Role::Sheet { SHEET_HEAD.len() } else { 0 };
            // The file's own bytes with the edits applied (a style sheet was scanned masked, review F8).
            let original = set.disk.get(old_id).map_or("", String::as_str);
            let text = applied_text(original, changes, head).ok_or(No("move_rewrite_unverified", bounded(old_id), None))?;
            let mut shift = 0isize;
            let rows: Vec<Value> = changes
                .iter()
                .map(|c| {
                    let start = c.edit.span.start.saturating_sub(head).saturating_add_signed(shift);
                    let after = start..start.saturating_add(c.edit.replacement.len());
                    shift = shift.saturating_add(isize::try_from(c.edit.replacement.len()).unwrap_or(0)).saturating_sub(isize::try_from(c.edit.span.len()).unwrap_or(0));
                    json!({"line": c.line, "before": [c.edit.span.start.saturating_sub(head), c.edit.span.end.saturating_sub(head)], "after": [after.start, after.end], "reference_before": c.before, "reference_after": c.after})
                })
                .collect();
            changed.push((new_id.clone(), json!({"document": new_id, "changes": rows})));
            if *old_id == from {
                moved_text = Some(text);
            } else {
                writes.push((old_id.clone(), text));
            }
        }
        changed.sort_by(|a, b| a.0.cmp(&b.0));
        let changed: Vec<Value> = changed.into_iter().map(|c| c.1).collect();
        // Lead Q3: moving the recipe's entry document moves `recipe.file` with it.
        let recipe_file = self.recipe.as_ref().filter(|r| r.file == from).map(|_| to.clone());
        let receipt = |complete: bool, restored: &[String], failed: Option<&str>, restore_failed: &[String]| {
            json!({
                "schema": "hsk.studio.web.move_receipt@1", "site_id": site.site_id, "from": from, "to": to, "site_relative_link_base": site.link_base,
                "changed": changed, "complete": complete, "restored": restored, "failed": failed, "restore_failed": restore_failed, "recipe_file": recipe_file,
            })
        };
        // Writes: referring files, the moved file's new bytes, then the rename; a failure restores what was written.
        let mut written: Vec<String> = Vec::new();
        let from_path = join(&root, &from);
        let to_path = join(&root, &to);
        let mut created: Vec<PathBuf> = Vec::new();
        let mut failure: Option<String> = None;
        if let Some(text) = moved_text {
            writes.push((from.clone(), text));
        }
        for (id, text) in &writes {
            if write_atomic(&join(&root, id), text.as_bytes()).is_err() {
                failure = Some(id.clone());
                break;
            }
            written.push(id.clone());
        }
        if failure.is_none() {
            let mut at = to_path.parent();
            while let Some(dir) = at.filter(|d| !d.exists()) {
                created.push(dir.to_path_buf());
                at = dir.parent();
            }
            created.reverse();
            let renamed = created.last().is_none_or(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::symlink_metadata(&to_path).is_err() && std::fs::rename(&from_path, &to_path).is_ok();
            if !renamed {
                failure = Some(to.clone());
            }
        }
        if let Some(failed) = failure {
            let (mut restored, mut restore_failed) = (Vec::new(), Vec::new());
            for id in written.iter().rev() {
                let original = set.disk.get(id).map(String::as_bytes).unwrap_or_default();
                if write_atomic(&join(&root, id), original).is_ok() {
                    restored.push(id.clone());
                } else {
                    restore_failed.push(id.clone());
                }
            }
            for dir in created.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
            restored.sort();
            return Err(No("move_failed", bounded(&failed), Some(json!({"receipt": receipt(false, &restored, Some(&failed), &restore_failed)}))));
        }
        // An open document that moved or was rewritten is reopened from disk with a fresh history.
        if let Some(id) = &open_id
            && let Some((_, _, _, written_doc)) = planned.iter().find(|(old, ..)| old == id)
        {
            // The bytes just written (the move is done: a late cancel must not report it as refused), as a fresh
            // revision 1; they parsed before the first write, so the fallback is never taken.
            let doc = parse(written_doc.source(), 1, written_doc.limits(), &CancellationToken::default()).unwrap_or_else(|_| written_doc.clone());
            self.saved_sha256 = Some(doc.sha256_hex().to_string());
            self.doc = Some(doc);
            self.undo.clear();
            self.redo.clear();
        }
        if open_id.as_deref() == Some(from.as_str()) {
            self.path = Some(join(&PathBuf::from(&site.local_root), &to));
            self.undo.clear();
            self.redo.clear();
        }
        if let (Some(file), Some(recipe)) = (&recipe_file, self.recipe.as_mut()) {
            recipe.file = file.clone();
        }
        Ok(json!({"receipt": receipt(true, &[], None, &[]), "web": self.state_json()}))
    }
}

/// The changes a move makes to one source: every reference to `from`, and, in the moved file, every relative
/// reference re-based from its new folder.
fn plan(src: &Source, from: &str, to: &str, link_base: &str) -> R<Vec<Change>> {
    let moving = src.id == from;
    let old_dir = dir_of(&src.id);
    let dir = dir_of(if moving { to } else { &src.id });
    let text = src.text();
    let head = src.head();
    let mut out = Vec::new();
    for (index, e) in src.edges.iter().enumerate() {
        let path_len = e.reference.find(['?', '#']).unwrap_or(e.reference.len());
        if path_len == 0 {
            continue;
        }
        let not = || No("reference_not_rewritable", bounded(&format!("{}:{}", src.id, e.line)), None);
        let path = e.reference.get(..path_len).unwrap_or("");
        let new_path = match &e.reach {
            Reach::File(id) if id == from && !moving => spell(link_base, &dir, &split_path(to), false),
            Reach::External | Reach::Inline | Reach::SameDocument => continue,
            // Review F2: in the moved file, every relative reference by path arithmetic on its own bytes.
            _ if moving && !e.rooted && !has_scheme(path) => rebased(link_base, &old_dir, &dir, path, from, to).ok_or_else(not)?,
            Reach::File(id) if id == from => spell(link_base, &dir, &split_path(to), false),
            _ => continue,
        };
        let new_ref = format!("{new_path}{}", e.reference.get(path_len..).unwrap_or(""));
        if new_ref == e.reference {
            continue;
        }
        let raw = text.get(e.span.clone()).unwrap_or("");
        let at = |r: Range<usize>| r.start.saturating_add(head)..r.end.saturating_add(head);
        let edit = if raw == e.reference {
            TextEdit { span: at(e.span.start..e.span.start.saturating_add(path_len)), replacement: new_path }
        } else {
            // Decoding moved offsets: the whole attribute value, when it carries only this reference.
            let AssetSource::Attribute(name) = &e.source else { return Err(not()) };
            let alone = src.edges.iter().filter(|o| o.node == e.node && o.span == e.span).count() == 1;
            let attr = src.doc.attribute(e.node, name).filter(|a| a.value_span.as_ref().map(|v| v.start..v.end) == Some(at(e.span.clone())));
            let (Some(attr), true) = (attr, alone) else { return Err(not()) };
            let decoded = src.doc.attr_value(attr).unwrap_or_default();
            let lead = decoded.len().saturating_sub(decoded.trim_start_matches(ASCII_WS).len());
            if decoded.get(lead..lead.saturating_add(e.reference.len())) != Some(e.reference.as_str()) {
                return Err(not());
            }
            let value = format!("{}{new_ref}{}", decoded.get(..lead).unwrap_or(""), decoded.get(lead.saturating_add(e.reference.len())..).unwrap_or(""));
            // Review F3: an unquoted value is written double-quoted, so decoded whitespace or `>` cannot end it.
            let replacement = match attr.quote {
                Quote::Single => escape_attr(&value, '\''),
                Quote::Double => escape_attr(&value, '"'),
                Quote::Unquoted | Quote::Absent => format!("\"{}\"", escape_attr(&value, '"')),
            };
            TextEdit { span: at(e.span.clone()), replacement }
        };
        out.push(Change { index, edit, line: e.line, before: e.reference.clone(), after: new_ref });
    }
    out.sort_by_key(|c| c.edit.span.start);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::row;
    use super::*;
    use crate::context::RunContext;
    use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
    use hsk_studio_session::Admission;

    /// This crate's own unit-test context (as `harness.rs` tests build theirs; no feature needed).
    fn context() -> RunContext {
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        RunContext::new(actor, ClientSessionId::parse("web-site-unit").unwrap(), Arc::new(TransientCorrelation::new()))
    }

    /// A host whose job port admits `capacity` jobs (retry hint 9 ms); the port is what admits, never the
    /// lifecycle's own admission (jobs are never resource-less).
    fn host_with(capacity: u32) -> (WebHost, Arc<AdmissionJobPort>) {
        let port = Arc::new(AdmissionJobPort::new(Admission::new(capacity, 1 << 20, 9).unwrap()));
        (WebHost { jobs: Jobs { port: port.clone(), lifecycle: JobLifecycle::default() }, ..WebHost::default() }, port)
    }

    fn run(host: &mut WebHost, id: &str, params: Value, cancel: &CancellationToken) -> R<Value> {
        let ctx = context().command(Some(id), host.revision(), None);
        host.execute(row(id).unwrap(), &params, &ctx, cancel)
    }

    #[test]
    fn site_jobs_refuse_overloaded_and_canceled_before_any_work() {
        let dir = std::env::temp_dir().join(format!("hsk-web-site-unit-{}-jobs", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut host, port) = host_with(1);
        let cancel = CancellationToken::default();
        run(&mut host, "web.site", json!({"display_name": "s", "local_root": dir.to_string_lossy()}), &cancel).ok().unwrap();
        run(&mut host, "web.remote_target", json!({"target_id": "t", "root_path": dir.join("target").to_string_lossy()}), &cancel).ok().unwrap();
        run(&mut host, "web.export_recipe", json!({"output_root": dir.join("out").to_string_lossy(), "documents": "site"}), &cancel).ok().unwrap();
        let held = port.admission().try_acquire(0).unwrap();
        for (id, params) in [("web.site_report", json!({})), ("web.export", json!({})), ("web.publish", json!({})), ("web.move_file", json!({"from": "a.png", "to": "b.png"}))] {
            let Err(No(code, at, detail)) = run(&mut host, id, params, &cancel) else { panic!("{id} ran while the admission was full") };
            assert_eq!((code, at.as_str(), detail), ("overloaded", "retry_after_ms=9", Some(json!({"retry_after_ms": 9}))), "{id}");
        }
        assert!(!dir.join(LOCK_FILE).exists(), "refused before any work: no lock");
        assert_eq!(host.jobs.lifecycle.counts().refused_overloaded, 4);
        drop(held);
        assert!(run(&mut host, "web.site_report", json!({}), &cancel).is_ok());
        assert_eq!(host.jobs.lifecycle.counts().admitted, 1);
        assert_eq!(port.tickets_held(), 0, "the ticket is released at settlement");
        cancel.cancel();
        assert!(matches!(run(&mut host, "web.site_report", json!({}), &cancel), Err(No("canceled", ..))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review F7: a guard removes the lock file only while it still holds the guard's own nonce, so a lock taken after
    /// a stale one was deleted by hand survives the stale holder's exit.
    #[test]
    fn a_lock_guard_removes_only_its_own_lock() {
        let dir = std::env::temp_dir().join(format!("hsk-web-site-unit-{}-nonce", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = context().command(Some("web.move_file"), 0, None);
        let guard = site_lock(&dir.to_string_lossy(), &ctx).ok().unwrap();
        let first: Value = serde_json::from_slice(&std::fs::read(dir.join(LOCK_FILE)).unwrap()).unwrap();
        assert!(first["nonce"].as_str().is_some_and(|n| !n.is_empty()), "{first}");
        // The operator deletes the stale lock; another holder takes it.
        std::fs::remove_file(dir.join(LOCK_FILE)).unwrap();
        let other = site_lock(&dir.to_string_lossy(), &ctx).ok().unwrap();
        drop(guard);
        assert!(dir.join(LOCK_FILE).exists(), "the stale guard left the new holder's lock alone");
        drop(other);
        assert!(!dir.join(LOCK_FILE).exists(), "its own holder removes it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spelling_follows_the_link_base() {
        let d = |s: &str| split_path(s);
        assert_eq!(spell("document", &d("pages"), &d("index.html"), false), "../index.html");
        assert_eq!(spell("document", &d(""), &d("pages/thanks.html"), false), "pages/thanks.html");
        assert_eq!(spell("document", &d("a/b"), &d("a/c/x.png"), false), "../c/x.png");
        assert_eq!(spell("document", &d("a"), &d("a"), true), "./");
        assert_eq!(spell("document", &d("a"), &d(""), true), "../");
        assert_eq!(spell("site_root", &d("a"), &d("x/y.html"), false), "/x/y.html");
        assert_eq!(spell("site_root", &d("a"), &d(""), true), "/");
        assert_eq!(segments_of(&d("a"), "../../x"), None, "never above the root");
        assert_eq!(segments_of(&d("a"), "./"), Some((d("a"), true)));
    }

    #[test]
    fn decisions_round_trip_through_the_granted_id() {
        let loads = Loads::of(Load::Document).and(Load::Image);
        for reach in [Reach::File("a/b.html".into()), Reach::Missing("x".into()), Reach::Case { id: "A.png".into(), actual: "a.png".into() }, Reach::Folder { path: String::new(), exists: true }, Reach::Folder { path: "d".into(), exists: false }] {
            assert_eq!(decode(&encode(loads, &reach)), Some((loads, reach)));
        }
    }
}
