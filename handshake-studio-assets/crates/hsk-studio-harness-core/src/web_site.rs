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
//!   Actions: `export` for the exports and publish, `read` for `web.site_report` (lane SMB-WEB-SITE-2 added the
//!   session's read class), `edit` for `web.move_file`, `web.change_link`, `web.template_update` and
//!   `web.library_item_update`, which are admitted the same way (on the site's id). Refusals: `overloaded` (`target` `retry_after_ms=N`,
//!   `detail.retry_after_ms`), `canceled` (a token canceled before the command is the lifecycle's
//!   `refused_canceled`; the port is not asked), `stale_revision`. Each admitted job settles `completed`, `canceled`
//!   or `failed` with its code, and an accepted command's result carries `job` (`outcome`, `ticket`,
//!   `bound_revision`, `correlation`, `frame_correlation` when the frame was delivered). `web.publish` checks the
//!   token before each file: a cancel refuses `canceled` with the receipt of the files already transferred.
//!   Deadline: none by nature; ended by cancel (a site walk, export or publish produces the operator's requested
//!   files; HBR-HOST-004).
//! - **The opened package**: `web.move_file` refuses `move_open_package` when `from` or `to` is the file the run's
//!   package was opened from, or a path inside it.
//!
//! # Lane SMB-WEB-SITE-2 (manual)
//!
//! Proven by `hsk-studio-harness/tests/web_site_depth.rs` (each claim below by the test named after it there; the
//! marker rules by `template_markers_are_tokenized_never_substring_matched`, drift by
//! `a_template_update_propagates_and_reports_drift`) and this module's unit tests.
//!
//! - **Spellings.** A reference's path is percent-decoded before it resolves (a decoded separator, `%`, control
//!   character or bad escape never resolves: it stays denied; the engine itself denies `%2E`, `%2F` and `%5C` as
//!   `ambiguous_path` before the site sees them). A move rewrites every reference that reaches the moved
//!   file in any spelling: exact, another letter case (`case_mismatch`) or percent-encoded. A template or library item
//!   cannot be moved yet (`move_template_or_library_item`: instances name them by root path in comments).
//! - **`web.change_link`** `{from, to}` (change-link-site-wide, [STU-WEB-118]): every reference reaching `from` in any
//!   spelling (`from` may be a missing page; a missing reference that folds to it counts) is rewritten to `to`, a file
//!   of the site, in the site's link base, query and fragment kept; parsed and graph-checked before the first write
//!   (`move_rewrite_unverified`), one transaction with a receipt `hsk.studio.web.change_link_receipt@1` (per changed
//!   file every change's byte ranges, line and references), restored on failure (`change_link_failed`). Like a move:
//!   site lock, job (`edit`), `document_unsaved`, `document_changed_on_disk`, `document_unreadable`; not a `web.undo`
//!   step (an open document it rewrote is reopened from disk; the reverse change restores the bytes). Refusals before
//!   admission: `no_site`, `parameter_missing`, `path_invalid`, `move_outside_site`, `nothing_to_change`,
//!   `link_target_reserved`; after: `link_target_missing`, `nothing_to_change` (no reference reaches `from`).
//! - **Templates** ([STU-WEB-090] to [STU-WEB-093]). A template is a `.dwt` file of the site; its markers are HTML
//!   comments the parser found (never text in a script, attribute or style sheet; keywords exact, case-sensitive;
//!   attributes `name="v"` or `name='v'`, no escapes). Spellings follow Dreamweaver's (UNVERIFIED against the
//!   application, lane notes): `TemplateBeginEditable name` / `TemplateEndEditable` (`editable`), `TemplateBeginIf
//!   cond` / `TemplateEndIf` (`optional`; `editable_optional` when it holds an editable region), `TemplateBeginRepeat
//!   name` / `TemplateEndRepeat` (`repeating`; `repeating_table` when the marker's parent is a `table`, `tbody`,
//!   `thead` or `tfoot`), `TemplateParam name type value` (type `text`, `boolean`, `color`, `URL`, `number`), and the
//!   two expression forms of [STU-WEB-092]: `TemplateExpr expr` (evaluated when an instance is generated: the code
//!   must name a parameter, whose value is written as escaped text; any other code refuses
//!   `template_expression_unsupported` and is never evaluated) and `TemplatePassthroughExpr expr` (the code written
//!   verbatim for later server-side evaluation). An instance is a page holding `InstanceBegin template="/<template>"
//!   locked_sha256="<hex>"` after its `html` start tag, `InstanceEnd` before its end tag, `InstanceBeginEditable` /
//!   `InstanceEndEditable` around each editable content, `InstanceParam` per parameter and `InstanceBeginRepeat` /
//!   `InstanceBeginRepeatEntry` / `InstanceEndRepeatEntry` / `InstanceEndRepeat` per repeat; a hidden optional region
//!   is absent. `locked_sha256` is the SHA-256 of the locked text (all but the `InstanceBegin` comment and the
//!   editable contents) as Studio last wrote it. The template's links are re-based from its folder to the instance's.
//!   Limits: 4096 markers per document, 8 KiB per marker, 1024 entries per repeat, 1024 bytes of expression code,
//!   the engine's 8 MiB per generated instance. Not done (declared): nested templates (`template_nested_unsupported`),
//!   repeats inside repeats, the server-language `dwt.*` types, expressions inside attribute values, the
//!   locked-region edit refusal of [STU-WEB-094] and template-safe behaviours ([STU-WEB-095]).
//! - **`web.template_regions`** `{path?}` (a read; default the open document): a template's `regions` (`name` or
//!   `cond`, `kind`, `line`, `span`), `params`, `expressions` (`form` `evaluated` | `pass_through`, `code`, `line`)
//!   and `region_kinds`; for an instance also `instance` (template, digests, params, editable names, repeat entry
//!   counts) and `standing`: `up_to_date`, `template_changed`, or `drifted` (`template_instance_drift`, reason
//!   `locked_changed` | `no_baseline`, remedy `reapply` | `detach`).
//! - **`web.template_instance`** `{template, path, params?}`: a new instance at `path` (a page of the site that does
//!   not exist; its folder must exist for `web.save`) becomes the open document, unsaved, with a fresh history (like
//!   `web.open`); `params` sets parameter values (typed). Refusals: `no_site`, `parameter_invalid`,
//!   `document_unsaved`, `instance_path_exists`, `path_outside_site`, `template_not_found`, `template_param_missing`
//!   and the template's own refusals.
//! - **`web.instance_param`** `{name, value}` and **`web.instance_repeat`** `{region, op: add|remove|move, index?,
//!   to?}`: the open instance regenerated from its template (read from disk) with the parameter set or the repeat's
//!   entries changed, editable contents kept, as one named history step (`Set Instance Parameter`, `Edit Repeating
//!   Region`) carrying the caller's context; refused `template_instance_drift` when its locked text was changed
//!   outside Studio, `not_a_template_instance`, `template_region_missing`, `repeat_entries_over_limit`.
//! - **`web.template_update`** `{template, resolve?: {<instance>: reapply|detach}}` ([STU-WEB-093]): every instance
//!   of the template is regenerated, one transaction (one atomic write) per instance: `updated` (the template changed
//!   and the instance's locked text is what Studio wrote), `up_to_date`, `baseline_refreshed` (its locked text is
//!   current but its digest was not), `drifted` (reported with `template_instance_drift`, never written), `reapplied`
//!   (`resolve` reapply: the template wins over the drift, editable contents kept), `detached` (`resolve` detach:
//!   every instance marker removed, every other byte kept). Receipt `hsk.studio.web.template_receipt@1`: per instance
//!   its outcome and each changed byte range (`before`, `after`, `line`), `updated`, `drifted`, `written`. A write
//!   failure refuses `template_update_failed` with the receipt (the instances already written are complete). Site
//!   lock, job (`edit`), `document_unsaved`, `document_changed_on_disk`, `document_unreadable`; an open instance it
//!   rewrote is reopened from disk; not a `web.undo` step. Refusals: `parameter_invalid`, `template_not_found`,
//!   `not_a_template_instance` (a `resolve` key), `template_markup_invalid` naming `document:line`.
//! - **Library items** ([STU-WEB-096]): a `.lbi` file of the site is a fragment; a document holds it between
//!   `#BeginLibraryItem "/<item>"` and `#EndLibraryItem` comments. A library item is not a template (no regions,
//!   never generated) and holds no library markers itself (`library_item_nested`). **`web.library_item_insert`**
//!   `{item, parent, index?}`: the item's content, its links re-based to the open document's folder, between its
//!   markers, inserted as one named history step (`Insert Library Item`). **`web.library_item_update`** `{item}`:
//!   every reference in every page and template holds the item's current content, re-based per document, in one
//!   transaction with a receipt `hsk.studio.web.library_receipt@1` (restored on failure, `library_update_failed`);
//!   an unbalanced marker anywhere refuses `library_markup_invalid` naming `document:line` before any write. Site
//!   lock, job (`edit`); not a `web.undo` step. The site report counts a template an instance names and an item a
//!   document holds as referenced (never orphaned).
//! - **Snippets** ([STU-WEB-097] to [STU-WEB-099]) are records of the site (saved with it): `{snippet_name,
//!   description, folder_path, top_level_group (the first folder), snippet_kind: block|wrap, preview_mode:
//!   code|design, inserts_before_selection, inserts_after_selection, trigger?}`; insert texts verbatim, at most 16
//!   KiB each; at most 4096 snippets; the record must stay under 1 MiB (`site_record_too_large`). **`web.snippet`**
//!   adds, replaces (`folder_path` + `snippet_name`) or `remove`s one, one named history step; a trigger claimed twice
//!   refuses `snippet_trigger_duplicate` (`detail`: `trigger`, both `snippets`, severity `error`, [STU-WEB-161]).
//!   **`web.snippets`** lists them by `top_level_group` and folder, the trigger map, and load `diagnostics`
//!   (`snippet_unnamed`: an unnamed snippet read back is kept and reported). **`web.snippet_insert`** `{folder_path
//!   + snippet_name | trigger, offset, end?}` (byte offsets of the open source): a `block` snippet inserts both texts
//!   at `offset`, a `wrap` snippet wraps `offset..end`, a `trigger` replaces the trigger word just before `offset`
//!   (`snippet_trigger_not_at_offset` otherwise), one named history step. The shipped 647-snippet baseline is not
//!   loaded (NOT_PROVEN: no corpus).
//! - **`web.synchronize`** `{target_id?, direction: put|get|both, files?, dependents?: prompt|include|exclude,
//!   apply?}` ([STU-WEB-119]; a `local_or_network` target only, else `transport_unsupported`): compares the site and
//!   the target folder file by file (size and modification time; times within 2 s are the same) and lists one action
//!   per file for review (`hsk.studio.web.sync_plan@1`: `put`, `get`, `same`, `conflict` (same time, other size),
//!   `local_newer`, `remote_newer`, `none`, `denied_link` (a path through a link on either side), with both sides'
//!   size and time, counts per action). `files` limits it to those site files; the files they load (images, style
//!   sheets, scripts; not page links) are `dependents`, listed only (`prompt`, the default), added (`include`) or left
//!   out (`exclude`). With `apply` every `put`/`get` is copied as its own atomic write with the source's time kept,
//!   with a per-file receipt (`hsk.studio.web.sync_receipt@1`: path, action, outcome, bytes, SHA-256, transfer mode);
//!   a failed file refuses `sync_failed` with the receipt after the others, a cancel refuses `canceled` with the
//!   receipt so far. No deletion either way (declared). Site lock; a job admitted as `read` for a review, `export` to
//!   apply a put, `edit` to apply a get or both; refused `target_inside_site`, `target_root_missing` (get, both),
//!   `document_unsaved` (a get onto the open document's unsaved edits); an open document a get wrote is reopened.
//! - **Site record** ([STU-WEB-113]): `web.site` also takes `testing_target` (a remote-target record, the target
//!   rules), `server_model_id` (the eight of [STU-WEB-124], `server_model_unknown`), `preprocessor_config`
//!   `{source_root, output_root, output_style, source_maps}`, `framework_pin` `{framework_version: 3.4.1|4.4.1|5.3.8
//!   (framework_version_unsupported), helper_library_version, positioning_helper_version?}`, `linter_overrides`
//!   `{rule: off|info|warning|error}`, `browser_baseline` (ordered, unique profile ids) and `version_control_binding`
//!   `{repository, branch?, credentials_ref?}`; `null` clears one; a key named like a secret anywhere inside refuses
//!   `credential_in_site_record` naming its path; unknown keys refuse `parameter_unknown`. Saved and read back with
//!   the site record; not yet used by publish, lint or preprocessors (recorded only).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    MANIFEST_FILE, MAX_EXPORT_BYTES, MAX_EXPORT_FILES, MAX_RECORD_BYTES, MAX_ROWS, MAX_SEGMENTS, No, P, R, RECEIPT_FILE, Recipe, SIDECAR_SUFFIX, WebHost, actor_json, bounded, checked_path,
    join, minified, no, output_names, portable, read_bounded, resolved, web_no, write_atomic, write_new,
};
use hsk_studio_accord::{CancellationToken, CommandContext, CommandId, DomainId};
use hsk_studio_session::{Action, AdmissionJobPort, Cost, JobAdmissionRefusal, JobKind, JobLifecycle, JobOutcome, JobPort, JobRequest, JobSettlement, ReportStatus};
use crate::action::CommandEntry;
use hsk_studio_web::{
    AssetDecision, AssetOutcome, AssetPort, AssetSource, DenyReason, Limits, Load, Loads, Namespace, NodeId, NodeKind, Quote, SourcePatch, TextEdit, WebDoc, apply, collect_assets, escape_attr, escape_text,
    insert_fragment, parse, sha256_hex,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The commands admitted as jobs (module docs).
pub(super) const ADMITTED: [&str; 8] = ["web.export", "web.publish", "web.site_report", "web.move_file", "web.change_link", "web.template_update", "web.library_item_update", "web.synchronize"];
/// `web.site_report`'s job action: the session's read class (lane SMB-WEB-SITE-2; it was `export` before `read`
/// existed).
pub(super) const REPORT_ACTION: Action = Action::Read;

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
    /// A template document (`.dwt`, [STU-WEB-013] row 23; the server-language `dwt.*` rows are not done).
    Template,
    /// A library item (`.lbi`, [STU-WEB-013] row 24).
    Library,
    Other,
}

fn role(id: &str) -> Role {
    let name = id.rsplit('/').next().unwrap_or(id);
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => Role::Page,
        Some("css") => Role::Sheet,
        Some("dwt") => Role::Template,
        Some("lbi") => Role::Library,
        _ => Role::Other,
    }
}

impl Role {
    /// Parsed as HTML (a page, template or library item), not as a style sheet.
    fn markup(self) -> bool {
        matches!(self, Role::Page | Role::Template | Role::Library)
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
        // Lane SMB-WEB-SITE-2: a reference reaching the renamed file in any spelling (another letter case, percent
        // encoding) is planned to reach its new place.
        if let Some((_, to)) = rename.filter(|(from, _)| reaches(self, from)) {
            return (1, to.to_string());
        }
        match self {
            Reach::File(id) | Reach::Case { id, .. } | Reach::Missing(id) => (1, id.clone()),
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

/// Whether a reference reaches `from` in any spelling (lane SMB-WEB-SITE-2): the file itself, another letter case
/// of it (`case_mismatch`), or, with no file at `from`, a missing path that folds to it. A percent-encoded spelling
/// was decoded by the port before it resolved.
fn reaches(reach: &Reach, from: &str) -> bool {
    match reach {
        Reach::File(id) => id == from,
        Reach::Case { actual, .. } => actual == from,
        Reach::Missing(id) => id.eq_ignore_ascii_case(from),
        _ => false,
    }
}

/// A reference path with its percent-encoding decoded (lane SMB-WEB-SITE-2): `None` when a decoded byte would be a
/// separator, a `%`, a control character or not UTF-8, so an encoded separator never resolves (it stays denied).
fn decoded_path(path: &str) -> Option<String> {
    if !path.contains('%') {
        return Some(path.to_string());
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes.get(i.saturating_add(1)..i.saturating_add(3)).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())?;
            if matches!(hex, b'/' | b'\\' | b'%') || hex.is_ascii_control() {
                return None;
            }
            out.push(hex);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8(out).ok()
}

impl SitePort<'_> {
    fn reach(&self, reference: &str, loads: Loads) -> Option<Reach> {
        let path = decoded_path(reference.split(['?', '#']).next().unwrap_or(""))?;
        let path = path.as_str();
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
        (None, r) if r.markup() => match parse(text, 1, &Limits::default(), cancel) {
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
            let live = open.as_ref().filter(|_| role.markup() && set.open_id.as_deref() == Some(id.as_str())).map(|o| o.doc.clone());
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
            // Lane SMB-WEB-SITE-2: the template an instance names and the library items a document holds are
            // referenced (their markers are comments, not references of the engine's carrier table).
            if src.role.markup() {
                referenced.extend(marker_targets(src));
            }
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
        // Lane SMB-WEB-SITE-2: instances name their template and documents their library items by root path in
        // marker comments, which a move does not rewrite yet (declared limit).
        if matches!(role(&from), Role::Template | Role::Library) {
            return no("move_template_or_library_item", &from);
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
        let open_id = self.open_on_disk(&root, &set)?;
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
        let planned = rewrites(&set, &moved_files, &from, &to, site.link_base, true, cancel)?;
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        // The receipt: per changed file, every change with its byte ranges before and after.
        let (changed, mut writes) = receipt_rows(&set, &planned)?;
        let moved_text = writes.iter().position(|(id, _)| *id == from).map(|at| writes.remove(at).1);
        // Lead Q3: moving the recipe's entry document moves `recipe.file` with it.
        let recipe_file = self.recipe.as_ref().filter(|r| r.file == from).map(|_| to.clone());
        let receipt = |complete: bool, restored: &[String], failed: Option<&str>, restore_failed: &[String]| {
            json!({
                "schema": "hsk.studio.web.move_receipt@1", "site_id": site.site_id, "from": from, "to": to, "site_relative_link_base": site.link_base,
                "changed": changed, "complete": complete, "restored": restored, "failed": failed, "restore_failed": restore_failed, "recipe_file": recipe_file,
            })
        };
        // Writes: referring files, the moved file's new bytes, then the rename; a failure restores what was written.
        let from_path = join(&root, &from);
        let to_path = join(&root, &to);
        let mut created: Vec<PathBuf> = Vec::new();
        if let Some(text) = moved_text {
            writes.push((from.clone(), text));
        }
        let (written, mut failure) = write_each(&root, &writes);
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
            let (restored, restore_failed) = restore(&root, &written, &set.disk);
            for dir in created.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(No("move_failed", bounded(&failed), Some(json!({"receipt": receipt(false, &restored, Some(&failed), &restore_failed)}))));
        }
        // An open document that moved or was rewritten is reopened from disk with a fresh history.
        if let Some(id) = &open_id
            && let Some(p) = planned.iter().find(|p| &p.old_id == id)
        {
            self.reopen_written(&p.doc);
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

/// The changes a move makes to one source: every reference to `from` in any spelling (lane SMB-WEB-SITE-2: another
/// letter case, percent encoding), and, when `moving` (the source's text is placed at `to`), every relative
/// reference re-based from its new folder. A template or library item placed into a document is planned as moving
/// from its own path to the document's.
fn plan(src: &Source, from: &str, to: &str, link_base: &str, moving: bool) -> R<Vec<Change>> {
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
            r if reaches(r, from) => spell(link_base, &dir, &split_path(to), false),
            Reach::External | Reach::Inline | Reach::SameDocument => continue,
            // Review F2: in the moved file, every relative reference by path arithmetic on its own bytes.
            _ if moving && !e.rooted && !has_scheme(path) => rebased(link_base, &old_dir, &dir, path, from, to).ok_or_else(not)?,
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

/// One file a site-wide rewrite changes: its id before and after, the planned changes and the parsed result.
struct Planned {
    old_id: String,
    new_id: String,
    changes: Vec<Change>,
    doc: WebDoc,
}

/// Plans every source's rewrite (`moving`: `from`'s file becomes `to`; else references to `from` become references
/// to `to` and no file moves), parses each result and compares its graph with the old one before anything is
/// written (`move_rewrite_unverified`): every reference reaches the same place, `from` read as `to`; a rewritten
/// one reads as planned and every other keeps its text.
fn rewrites(set: &SiteSet, after: &Files, from: &str, to: &str, link_base: &str, moving: bool, cancel: &CancellationToken) -> R<Vec<Planned>> {
    let mut planned = Vec::new();
    for src in &set.sources {
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        let moves = moving && src.id == from;
        let new_id = if moves { to.to_string() } else { src.id.clone() };
        let changes = plan(src, from, to, link_base, moves)?;
        let next = if changes.is_empty() {
            src.doc.clone()
        } else {
            let applied = apply(&src.doc, &SourcePatch::new(&src.doc, changes.iter().map(|c| c.edit.clone()).collect())).map_err(|e| web_no(&e))?;
            parse(&applied.source, applied.revision, src.doc.limits(), cancel).map_err(|e| web_no(&e))?
        };
        let after_src = Source { id: new_id.clone(), role: src.role, doc: next, edges: Vec::new() };
        let edges = edges_of(after, &after_src, cancel).map_err(|e| web_no(&e))?;
        let rename = Some((from, to));
        let (old_dir, new_dir) = (dir_of(&src.id), dir_of(&new_id));
        let differs = |i: usize, n: &Edge, o: &Edge| {
            let expected = changes.iter().find(|c| c.index == i).map_or(o.reference.as_str(), |c| c.after.as_str());
            n.reach.place(None, &n.reference, n.fragment.as_deref(), &new_dir) != o.reach.place(rename, &o.reference, o.fragment.as_deref(), &old_dir) || n.reference != expected
        };
        if edges.len() != src.edges.len() {
            return no("move_rewrite_unverified", &format!("{new_id}:0"));
        }
        if let Some((n, _)) = edges.iter().zip(&src.edges).enumerate().find(|(i, (n, o))| differs(*i, n, o)).map(|(_, pair)| pair) {
            return no("move_rewrite_unverified", &format!("{new_id}:{}", n.line));
        }
        if !changes.is_empty() {
            planned.push(Planned { old_id: src.id.clone(), new_id, changes, doc: after_src.doc });
        }
    }
    Ok(planned)
}

/// The receipt rows of planned rewrites (per changed file, sorted by its new id: every change's byte ranges before
/// and after, line and references) and the writes (old id, the file's own bytes with the edits applied; a style
/// sheet was scanned masked, review F8), in source order.
/// Receipt rows and the (id, new text) writes of planned rewrites.
type RowsAndWrites = (Vec<Value>, Vec<(String, String)>);

fn receipt_rows(set: &SiteSet, planned: &[Planned]) -> R<RowsAndWrites> {
    let mut changed = Vec::new();
    let mut writes = Vec::new();
    for p in planned {
        let head = if role(&p.old_id) == Role::Sheet { SHEET_HEAD.len() } else { 0 };
        let original = set.disk.get(&p.old_id).map_or("", String::as_str);
        let text = applied_text(original, &p.changes, head).ok_or(No("move_rewrite_unverified", bounded(&p.old_id), None))?;
        let mut shift = 0isize;
        let rows: Vec<Value> = p
            .changes
            .iter()
            .map(|c| {
                let start = c.edit.span.start.saturating_sub(head).saturating_add_signed(shift);
                let after = start..start.saturating_add(c.edit.replacement.len());
                shift = shift.saturating_add(isize::try_from(c.edit.replacement.len()).unwrap_or(0)).saturating_sub(isize::try_from(c.edit.span.len()).unwrap_or(0));
                json!({"line": c.line, "before": [c.edit.span.start.saturating_sub(head), c.edit.span.end.saturating_sub(head)], "after": [after.start, after.end], "reference_before": c.before, "reference_after": c.after})
            })
            .collect();
        changed.push((p.new_id.clone(), json!({"document": p.new_id, "changes": rows})));
        writes.push((p.old_id.clone(), text));
    }
    changed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((changed.into_iter().map(|c| c.1).collect(), writes))
}

/// Writes each file through the atomic writer in order and stops at the first failure: the ids written and the one
/// that failed.
fn write_each(root: &Path, writes: &[(String, String)]) -> (Vec<String>, Option<String>) {
    let mut written = Vec::new();
    for (id, text) in writes {
        if write_atomic(&join(root, id), text.as_bytes()).is_err() {
            return (written, Some(id.clone()));
        }
        written.push(id.clone());
    }
    (written, None)
}

/// Restores every written file from the bytes read before the transaction: the ids restored (sorted) and those that
/// could not be.
fn restore(root: &Path, written: &[String], disk: &BTreeMap<String, String>) -> (Vec<String>, Vec<String>) {
    let (mut restored, mut restore_failed) = (Vec::new(), Vec::new());
    for id in written.iter().rev() {
        let original = disk.get(id).map(String::as_bytes).unwrap_or_default();
        if write_atomic(&join(root, id), original).is_ok() {
            restored.push(id.clone());
        } else {
            restore_failed.push(id.clone());
        }
    }
    restored.sort();
    (restored, restore_failed)
}

impl WebHost {
    /// The open document's id in the site when it is one of the site's files; refuses `document_unsaved` while it
    /// has unsaved edits and `document_changed_on_disk` when its file no longer holds what it shows.
    fn open_on_disk(&self, root: &Path, set: &SiteSet) -> R<Option<String>> {
        if let (Some(_), Some(doc)) = (&self.path, &self.doc)
            && self.saved_sha256.as_deref() != Some(doc.sha256_hex())
        {
            return no("document_unsaved", "web.save first");
        }
        let open_id = self.open_view().and_then(|o| std::fs::canonicalize(root).ok().and_then(|c| id_in(&c, o.path))).filter(|id| set.files.files.contains(id));
        if let (Some(id), Some(doc)) = (&open_id, &self.doc)
            && set.disk.get(id).map(|t| sha256_hex(t.as_bytes())).as_deref() != Some(doc.sha256_hex())
        {
            return no("document_changed_on_disk", id);
        }
        Ok(open_id)
    }

    /// The open document after a site-wide rewrite wrote it: the bytes just written as a fresh revision 1 and a
    /// fresh history (the transaction is done: a late cancel must not report it as refused). They parsed before the
    /// first write, so the fallback is never taken.
    fn reopen_written(&mut self, written: &WebDoc) {
        let doc = parse(written.source(), 1, written.limits(), &CancellationToken::default()).unwrap_or_else(|_| written.clone());
        self.saved_sha256 = Some(doc.sha256_hex().to_string());
        self.doc = Some(doc);
        self.undo.clear();
        self.redo.clear();
    }
}

// ----- lane SMB-WEB-SITE-2: the rows this module owns, change-link-site-wide ------------------------------------

impl WebHost {
    /// The `web.*` rows of lane SMB-WEB-SITE-2 (module docs). A row this module does not own ends in the same typed
    /// `unknown_command`, so another dispatcher can be chained before or after this one (lead ruling 2026-11-13).
    pub(super) fn site_command(&mut self, row: &'static CommandEntry, p: &P<'_>, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let label = row.display_name;
        match row.command_id {
            "web.change_link" => {
                let (from, to) = self.change_link_checks(p)?;
                self.admitted(Action::Edit, JobOn::Site, ctx, cancel, |host| host.change_link(&from, &to, ctx, cancel))
            }
            "web.template_regions" => self.template_regions(p, cancel),
            "web.template_instance" => self.template_instance(p, cancel),
            "web.instance_param" => {
                let (name, value) = (p.text("name")?.to_string(), p.text("value")?.to_string());
                self.instance_edit(label, ctx, cancel, |t, st| {
                    let value = param_value(t, &name, &value)?;
                    st.params.insert(name.clone(), value);
                    Ok(())
                })
            }
            "web.instance_repeat" => {
                let region = p.text("region")?.to_string();
                let op = p.text("op")?.to_string();
                let index = opt_index(p, "index")?;
                let to = opt_index(p, "to")?;
                self.instance_edit(label, ctx, cancel, |t, st| repeat_op(t, st, &region, &op, index, to))
            }
            "web.template_update" => {
                let (template, resolve) = self.template_update_checks(p)?;
                self.admitted(Action::Edit, JobOn::Site, ctx, cancel, |host| host.template_update(&template, &resolve, ctx, cancel))
            }
            "web.library_item_insert" => self.library_item_insert(label, p, ctx, cancel),
            "web.library_item_update" => {
                self.site_root()?;
                let item = library_id(p)?;
                self.admitted(Action::Edit, JobOn::Site, ctx, cancel, |host| host.library_item_update(&item, ctx, cancel))
            }
            "web.snippet" => self.snippet(label, p, ctx),
            "web.snippets" => self.snippets(),
            "web.snippet_insert" => self.snippet_insert(label, p, ctx, cancel),
            "web.synchronize" => {
                let plan = self.sync_checks(p)?;
                // A review only reads; applying writes the target (put) or the site too (get, both).
                let action = match (plan.apply, plan.direction) {
                    (false, _) => Action::Read,
                    (true, "put") => Action::Export,
                    (true, _) => Action::Edit,
                };
                self.admitted(action, JobOn::Site, ctx, cancel, |host| host.synchronize(&plan, ctx, cancel))
            }
            _ => no("unknown_command", row.command_id),
        }
    }

    /// `web.change_link`'s checks before admission (no I/O): `from` and `to` as root-relative ids.
    fn change_link_checks(&self, p: &P<'_>) -> R<(String, String)> {
        self.site_root()?;
        let from = site_path(p.text("from")?)?;
        let to = site_path(p.text("to")?)?;
        if from == to {
            return no("nothing_to_change", &to);
        }
        if reserved(&to) {
            return no("link_target_reserved", &to);
        }
        Ok((from, to))
    }

    /// `web.change_link` (module docs): every reference that reaches `from`, in any spelling, now reaches `to`.
    fn change_link(&mut self, from: &str, to: &str, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let _lock = site_lock(&site.local_root, ctx)?;
        let set = SiteSet::load(&root, None, cancel)?;
        let open_id = self.open_on_disk(&root, &set)?;
        unreadable(&set)?;
        if !set.files.files.contains(to) {
            return no("link_target_missing", to);
        }
        let planned = rewrites(&set, &set.files, from, to, site.link_base, false, cancel)?;
        if planned.is_empty() {
            return no("nothing_to_change", from);
        }
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        let (changed, writes) = receipt_rows(&set, &planned)?;
        let receipt = |complete: bool, restored: &[String], failed: Option<&str>, restore_failed: &[String]| {
            json!({
                "schema": "hsk.studio.web.change_link_receipt@1", "site_id": site.site_id, "from": from, "to": to, "site_relative_link_base": site.link_base,
                "changed": changed, "complete": complete, "restored": restored, "failed": failed, "restore_failed": restore_failed,
            })
        };
        let (written, failure) = write_each(&root, &writes);
        if let Some(failed) = failure {
            let (restored, restore_failed) = restore(&root, &written, &set.disk);
            return Err(No("change_link_failed", bounded(&failed), Some(json!({"receipt": receipt(false, &restored, Some(&failed), &restore_failed)}))));
        }
        if let Some(id) = &open_id
            && let Some(p) = planned.iter().find(|p| &p.old_id == id)
        {
            self.reopen_written(&p.doc);
        }
        Ok(json!({"receipt": receipt(true, &[], None, &[]), "web": self.state_json()}))
    }

    /// The open document's id under the site root: its file's canonical place, else (an instance not saved yet) its
    /// path below `local_root` read lexically.
    fn open_id_in(&self, root: &Path) -> R<String> {
        let path = self.path.as_deref().ok_or(No("document_outside_site_root", String::new(), None))?;
        if let Some(id) = std::fs::canonicalize(root).ok().and_then(|c| id_in(&c, path)) {
            return Ok(id);
        }
        let (Ok(base), Ok(at)) = (std::path::absolute(root), std::path::absolute(path)) else {
            return no("document_outside_site_root", &path.to_string_lossy());
        };
        let rel = at.strip_prefix(&base).map_err(|_| No("document_outside_site_root", bounded(&path.to_string_lossy()), None))?;
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        site_path(&parts.map(|p| p.join("/")).unwrap_or_default()).map_err(|_| No("document_outside_site_root", bounded(&path.to_string_lossy()), None))
    }
}

// ----- templates, instances and library items (lane SMB-WEB-SITE-2; [STU-WEB-090] to [STU-WEB-093], [STU-WEB-096])

/// [STU-WEB-091]: the region kinds, closed, five members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegionKind {
    Editable,
    Optional,
    EditableOptional,
    Repeating,
    RepeatingTable,
}

impl RegionKind {
    const ALL: [RegionKind; 5] = [Self::Editable, Self::Optional, Self::EditableOptional, Self::Repeating, Self::RepeatingTable];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Optional => "optional",
            Self::EditableOptional => "editable_optional",
            Self::Repeating => "repeating",
            Self::RepeatingTable => "repeating_table",
        }
    }
}

/// A marker comment (module docs: the spellings are Dreamweaver's as recorded in the lane notes, UNVERIFIED against
/// the application).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mk {
    TBeginEditable,
    TEndEditable,
    TBeginIf,
    TEndIf,
    TBeginRepeat,
    TEndRepeat,
    TParam,
    TExpr,
    TPass,
    IBegin,
    IEnd,
    IBeginEditable,
    IEndEditable,
    IParam,
    IBeginRepeat,
    IEndRepeat,
    IBeginEntry,
    IEndEntry,
    LBegin,
    LEnd,
}

const MARKERS: [(&str, Mk); 20] = [
    ("TemplateBeginEditable", Mk::TBeginEditable),
    ("TemplateEndEditable", Mk::TEndEditable),
    ("TemplateBeginIf", Mk::TBeginIf),
    ("TemplateEndIf", Mk::TEndIf),
    ("TemplateBeginRepeat", Mk::TBeginRepeat),
    ("TemplateEndRepeat", Mk::TEndRepeat),
    ("TemplateParam", Mk::TParam),
    ("TemplateExpr", Mk::TExpr),
    ("TemplatePassthroughExpr", Mk::TPass),
    ("InstanceBegin", Mk::IBegin),
    ("InstanceEnd", Mk::IEnd),
    ("InstanceBeginEditable", Mk::IBeginEditable),
    ("InstanceEndEditable", Mk::IEndEditable),
    ("InstanceParam", Mk::IParam),
    ("InstanceBeginRepeat", Mk::IBeginRepeat),
    ("InstanceEndRepeat", Mk::IEndRepeat),
    ("InstanceBeginRepeatEntry", Mk::IBeginEntry),
    ("InstanceEndRepeatEntry", Mk::IEndEntry),
    ("#BeginLibraryItem", Mk::LBegin),
    ("#EndLibraryItem", Mk::LEnd),
];

impl Mk {
    fn template(self) -> bool {
        matches!(self, Mk::TBeginEditable | Mk::TEndEditable | Mk::TBeginIf | Mk::TEndIf | Mk::TBeginRepeat | Mk::TEndRepeat | Mk::TParam | Mk::TExpr | Mk::TPass)
    }

    fn instance(self) -> bool {
        matches!(self, Mk::IBegin | Mk::IEnd | Mk::IBeginEditable | Mk::IEndEditable | Mk::IParam | Mk::IBeginRepeat | Mk::IEndRepeat | Mk::IBeginEntry | Mk::IEndEntry)
    }
}

/// Markers of one document, comments of one marker's text, entries of one repeat.
const MAX_MARKERS: usize = 4096;
const MAX_MARKER_TEXT: usize = 8192;
const MAX_REPEAT_ENTRIES: usize = 1024;
/// [STU-WEB-092]: one embedded code string, bounded.
const MAX_CODE: usize = 1024;
/// Template parameter types (Dreamweaver's five, UNVERIFIED).
const PARAM_TYPES: [&str; 5] = ["text", "boolean", "color", "URL", "number"];
/// The digest a generated instance is written with before its own digest is known (same length, so no offset moves).
const DIGEST_HOLE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct Marker {
    kind: Mk,
    span: Range<usize>,
    line: usize,
    /// The local name of the comment's parent element (a repeat inside a table repeats table rows).
    parent: Option<String>,
    attrs: BTreeMap<String, String>,
    /// `#BeginLibraryItem "<path>"`.
    path: Option<String>,
}

/// `name="value"` pairs of a marker (either quote, no escapes, at most 8, no duplicate); `None` when malformed.
fn marker_attrs(text: &str) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut rest = text.trim_start_matches(ASCII_WS);
    while !rest.is_empty() {
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))?;
        let (name, after) = rest.split_at(end);
        let after = after.strip_prefix('=')?;
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value_on = after.get(1..)?;
        let close = value_on.find(quote)?;
        if name.is_empty() || out.insert(name.to_string(), value_on.get(..close)?.to_string()).is_some() || out.len() > 8 {
            return None;
        }
        rest = value_on.get(close.saturating_add(1)..)?.trim_start_matches(ASCII_WS);
    }
    Some(out)
}

/// The one quoted string of a `#BeginLibraryItem` marker.
fn quoted_only(text: &str) -> Option<String> {
    let text = text.trim_matches(ASCII_WS);
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = text.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.contains(quote)).then(|| inner.to_string())
}

/// Every marker of a parsed document, in source order: the parser's comment nodes only, so marker text inside a
/// script, an attribute value or a style sheet is never a marker (V4-08). Refuses `template_markup_invalid` naming
/// `id:line` for a malformed marker or more than 4096 of them.
fn markers(doc: &WebDoc, id: &str) -> R<Vec<Marker>> {
    let src = doc.source();
    let mut out = Vec::new();
    for n in doc.descendants(doc.root()) {
        let Some(node) = doc.node(n).filter(|x| x.kind == NodeKind::Comment) else { continue };
        let Some(body) = src.get(node.span.clone()).and_then(|raw| raw.strip_prefix("<!--")) else { continue };
        let body = body.strip_suffix("-->").unwrap_or(body).trim_matches(ASCII_WS);
        let keyword = body.split(ASCII_WS).next().unwrap_or("");
        let Some(&(_, kind)) = MARKERS.iter().find(|(k, _)| *k == keyword) else { continue };
        let line = line_of(src, node.span.start);
        let invalid = || No("template_markup_invalid", bounded(&format!("{id}:{line}")), None);
        if body.len() > MAX_MARKER_TEXT || out.len() >= MAX_MARKERS {
            return Err(invalid());
        }
        let rest = body.get(keyword.len()..).unwrap_or("");
        let (attrs, path) = if kind == Mk::LBegin { (BTreeMap::new(), Some(quoted_only(rest).ok_or_else(invalid)?)) } else { (marker_attrs(rest).ok_or_else(invalid)?, None) };
        let parent = node.parent.and_then(|p| doc.local_name(p));
        out.push(Marker { kind, span: node.span.clone(), line, parent, attrs, path });
    }
    out.sort_by_key(|m| m.span.start);
    Ok(out)
}

/// A region or parameter name: 1 to 128 characters, none a control character, quote, `<`, `>` or `-`-pair.
fn marker_name(value: Option<&String>) -> Option<String> {
    value.filter(|v| !v.is_empty() && v.chars().count() <= 128 && !v.contains("--") && !v.chars().any(|c| c.is_control() || matches!(c, '"' | '\'' | '<' | '>'))).cloned()
}

/// A parameter value of `ty` (text at most 1024 characters, never a quote, `--` or control character; boolean
/// `true`/`false`; number finite), else `None`.
fn typed_value(ty: &str, value: &str) -> Option<String> {
    let plain = value.chars().count() <= 1024 && !value.contains("--") && !value.chars().any(|c| c.is_control() || c == '"');
    let ok = plain
        && match ty {
            "boolean" => matches!(value, "true" | "false"),
            "number" => value.parse::<f64>().is_ok_and(f64::is_finite),
            _ => true,
        };
    ok.then(|| value.to_string())
}

/// One template part (module docs).
#[derive(Clone, Debug)]
enum Part {
    Text(Range<usize>),
    Editable { name: String, body: Vec<Part> },
    If { cond: String, body: Vec<Part> },
    Repeat { name: String, body: Vec<Part> },
    Param(usize),
    Expr { code: String, pass: bool },
    /// Where the instance's `InstanceBegin` and `InstanceEnd` go (after the `html` start tag, before its end tag).
    Begin,
    End,
}

#[derive(Clone, Debug, PartialEq)]
struct Param {
    name: String,
    ty: &'static str,
    value: String,
}

/// A parsed template (module docs).
struct Template {
    parts: Vec<Part>,
    params: Vec<Param>,
    regions: Vec<(usize, Value)>,
    expressions: Vec<Value>,
}

impl Template {
    fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    fn repeat(&self, name: &str) -> bool {
        fn find(parts: &[Part], name: &str) -> bool {
            parts.iter().any(|p| match p {
                Part::Repeat { name: n, .. } => n == name,
                Part::Editable { body, .. } | Part::If { body, .. } => find(body, name),
                _ => false,
            })
        }
        find(&self.parts, name)
    }

    fn json(&self, id: &str) -> Value {
        let mut regions = self.regions.clone();
        regions.sort_by_key(|r| r.0);
        json!({
            "kind": "template", "document": id, "region_kinds": RegionKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "regions": regions.into_iter().map(|r| r.1).collect::<Vec<_>>(),
            "params": self.params.iter().map(|p| json!({"name": p.name, "type": p.ty, "value": p.value})).collect::<Vec<_>>(),
            "expressions": self.expressions,
        })
    }
}

/// Where `InstanceBegin` and `InstanceEnd` go: after the `html` start tag (else after the doctype, else at 0) and
/// before the `html` end tag (else at the end).
fn instance_bounds(doc: &WebDoc) -> (usize, usize) {
    let len = doc.source().len();
    let html = doc.find_by_tag("html").into_iter().find_map(|h| doc.node(h).filter(|n| n.namespace == Namespace::Html && n.name_span.is_some()).map(|n| (n.open_span.clone(), n.close_span.clone())));
    let doctype = doc.descendants(doc.root()).find_map(|n| doc.node(n).filter(|x| x.kind == NodeKind::Doctype).map(|x| x.span.end));
    let begin = html.as_ref().and_then(|(open, _)| open.as_ref().map(|o| o.end)).or(doctype).unwrap_or(0);
    let end = html.and_then(|(_, close)| close.map(|c| c.start)).unwrap_or(len);
    (begin, end.max(begin))
}

/// One open region while a template is read.
struct Frame {
    kind: Mk,
    name: String,
    parts: Vec<Part>,
    start: usize,
    line: usize,
    parent: Option<String>,
}

/// The template of a parsed `.dwt` document ([STU-WEB-090] to [STU-WEB-092]; module docs).
fn template_of(doc: &WebDoc, id: &str) -> R<Template> {
    let text = doc.source();
    let ms = markers(doc, id)?;
    let at_line = |line: usize| format!("{id}:{line}");
    if let Some(m) = ms.iter().find(|m| m.kind.instance()) {
        return no("template_nested_unsupported", &at_line(m.line));
    }
    let (begin_at, end_at) = instance_bounds(doc);
    let mut events: Vec<(usize, u8, Option<&Marker>)> = ms.iter().filter(|m| m.kind.template()).map(|m| (m.span.start, 1, Some(m))).collect();
    events.push((begin_at, 0, None));
    events.push((end_at, 2, None));
    events.sort_by_key(|e| (e.0, e.1));
    let mut stack = vec![Frame { kind: Mk::IBegin, name: String::new(), parts: Vec::new(), start: 0, line: 0, parent: None }];
    let mut t = Template { parts: Vec::new(), params: Vec::new(), regions: Vec::new(), expressions: Vec::new() };
    let mut names = BTreeSet::new();
    let mut at = 0usize;
    for (offset, order, marker) in events {
        let depth = stack.len();
        let Some(top) = stack.last_mut() else { return no("template_markup_invalid", id) };
        if offset > at {
            top.parts.push(Part::Text(at..offset));
            at = offset;
        }
        let Some(m) = marker else {
            if depth != 1 {
                return no("template_markup_invalid", &format!("{id}: the html tags lie inside a region"));
            }
            top.parts.push(if order == 0 { Part::Begin } else { Part::End });
            continue;
        };
        let invalid = || No("template_markup_invalid", bounded(&at_line(m.line)), None);
        match m.kind {
            Mk::TBeginEditable | Mk::TBeginIf | Mk::TBeginRepeat => {
                let name = marker_name(m.attrs.get(if m.kind == Mk::TBeginIf { "cond" } else { "name" })).ok_or_else(invalid)?;
                if m.kind == Mk::TBeginEditable && stack.iter().any(|f| f.kind == Mk::TBeginEditable) {
                    return Err(invalid());
                }
                if m.kind == Mk::TBeginRepeat && stack.iter().any(|f| f.kind == Mk::TBeginRepeat) {
                    return no("template_nested_unsupported", &at_line(m.line));
                }
                if m.kind != Mk::TBeginIf && !names.insert(name.clone()) {
                    return no("template_region_duplicate", &format!("{} {name}", at_line(m.line)));
                }
                stack.push(Frame { kind: m.kind, name, parts: Vec::new(), start: m.span.start, line: m.line, parent: m.parent.clone() });
            }
            Mk::TEndEditable | Mk::TEndIf | Mk::TEndRepeat => {
                let open = match m.kind {
                    Mk::TEndEditable => Mk::TBeginEditable,
                    Mk::TEndIf => Mk::TBeginIf,
                    _ => Mk::TBeginRepeat,
                };
                if stack.len() < 2 || stack.last().map(|f| f.kind) != Some(open) {
                    return Err(invalid());
                }
                let f = stack.pop().ok_or_else(invalid)?;
                let kind = match f.kind {
                    Mk::TBeginEditable => RegionKind::Editable,
                    Mk::TBeginIf if f.parts.iter().any(|p| matches!(p, Part::Editable { .. })) => RegionKind::EditableOptional,
                    Mk::TBeginIf => RegionKind::Optional,
                    _ if matches!(f.parent.as_deref(), Some("table" | "tbody" | "thead" | "tfoot")) => RegionKind::RepeatingTable,
                    _ => RegionKind::Repeating,
                };
                let mut row = json!({"kind": kind.as_str(), "line": f.line, "span": [f.start, m.span.end]});
                row[if f.kind == Mk::TBeginIf { "cond" } else { "name" }] = json!(f.name);
                t.regions.push((f.start, row));
                let part = match f.kind {
                    Mk::TBeginEditable => Part::Editable { name: f.name, body: f.parts },
                    Mk::TBeginIf => Part::If { cond: f.name, body: f.parts },
                    _ => Part::Repeat { name: f.name, body: f.parts },
                };
                stack.last_mut().ok_or_else(invalid)?.parts.push(part);
            }
            Mk::TParam => {
                let name = marker_name(m.attrs.get("name")).ok_or_else(invalid)?;
                let ty = m.attrs.get("type").and_then(|ty| PARAM_TYPES.iter().copied().find(|t| t == ty)).ok_or_else(invalid)?;
                let value = typed_value(ty, m.attrs.get("value").map_or("", String::as_str)).ok_or_else(invalid)?;
                if depth != 1 {
                    return Err(invalid());
                }
                if t.param(&name).is_some() {
                    return no("template_param_duplicate", &format!("{} {name}", at_line(m.line)));
                }
                t.params.push(Param { name, ty, value });
                top.parts.push(Part::Param(t.params.len().saturating_sub(1)));
            }
            _ => {
                // TemplateExpr and TemplatePassthroughExpr ([STU-WEB-092]): one embedded code string.
                let pass = m.kind == Mk::TPass;
                let code = m.attrs.get("expr").filter(|c| !c.is_empty() && c.len() <= MAX_CODE).cloned().ok_or_else(invalid)?;
                t.expressions.push(json!({"form": if pass { "pass_through" } else { "evaluated" }, "code": code, "line": m.line}));
                stack.last_mut().ok_or_else(invalid)?.parts.push(Part::Expr { code, pass });
            }
        }
        at = m.span.end;
    }
    if stack.len() != 1 {
        return no("template_markup_invalid", &format!("{} unclosed", at_line(stack.last().map_or(0, |f| f.line))));
    }
    let mut root = stack.pop().ok_or(No("template_markup_invalid", bounded(id), None))?;
    if text.len() > at {
        root.parts.push(Part::Text(at..text.len()));
    }
    t.parts = root.parts;
    checked_parts(&t, &t.parts, id)?;
    Ok(t)
}

/// Every condition names a boolean parameter and every evaluated expression a parameter ([STU-WEB-092]: the
/// evaluator reads a parameter by name; any other code refuses `template_expression_unsupported`, never evaluated).
fn checked_parts(t: &Template, parts: &[Part], id: &str) -> R<()> {
    for part in parts {
        match part {
            Part::If { cond, body } => {
                if t.param(cond).is_none_or(|p| p.ty != "boolean") {
                    return no("template_param_missing", &format!("{id}: {cond}"));
                }
                checked_parts(t, body, id)?;
            }
            Part::Editable { body, .. } | Part::Repeat { body, .. } => checked_parts(t, body, id)?,
            Part::Expr { code, pass: false } => {
                let identifier = code.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !identifier {
                    return no("template_expression_unsupported", &format!("{id}: {code}"));
                }
                if t.param(code).is_none() {
                    return no("template_param_missing", &format!("{id}: {code}"));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// What an instance holds of its own: parameter values, editable contents (verbatim) and repeat entries.
#[derive(Clone, Debug, Default, PartialEq)]
struct State {
    params: BTreeMap<String, String>,
    editable: BTreeMap<String, String>,
    repeats: BTreeMap<String, Vec<BTreeMap<String, String>>>,
}

/// A parsed instance (module docs).
struct Instance {
    template: String,
    baseline: Option<String>,
    begin: Range<usize>,
    state: State,
    /// SHA-256 of the locked text: everything but the `InstanceBegin` comment and the editable contents.
    locked: String,
    /// Every instance marker comment (detach removes them).
    markers: Vec<Range<usize>>,
}

impl Instance {
    fn json(&self) -> Value {
        json!({
            "template": self.template, "locked_sha256": self.locked, "baseline": self.baseline, "params": self.state.params,
            "editable": self.state.editable.keys().collect::<Vec<_>>(),
            "repeats": self.state.repeats.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect::<Map<String, Value>>(),
        })
    }
}

/// SHA-256 of `text` without the `excluded` ranges (sorted; each gap marked by one NUL byte).
fn locked_digest(text: &str, excluded: &[Range<usize>]) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut at = 0usize;
    for r in excluded {
        bytes.extend_from_slice(text.get(at..r.start.max(at)).unwrap_or("").as_bytes());
        bytes.push(0);
        at = r.end.max(at);
    }
    bytes.extend_from_slice(text.get(at..).unwrap_or("").as_bytes());
    sha256_hex(&bytes)
}

/// One open instance region while an instance is read.
enum IFrame {
    Editable(String, usize),
    Repeat(String, Vec<BTreeMap<String, String>>),
    Entry(BTreeMap<String, String>),
}

/// The instance a parsed document is, `None` when it has no `InstanceBegin` (module docs).
fn instance_of(doc: &WebDoc, id: &str) -> R<Option<Instance>> {
    let text = doc.source();
    let ms = markers(doc, id)?;
    let Some(begin) = ms.iter().find(|m| m.kind == Mk::IBegin) else { return Ok(None) };
    let invalid = |line: usize| No("template_markup_invalid", bounded(&format!("{id}:{line}")), None);
    if let Some(m) = ms.iter().find(|m| m.kind.template()) {
        return no("template_nested_unsupported", &format!("{id}:{}", m.line));
    }
    let template = begin.attrs.get("template").and_then(|t| t.strip_prefix('/')).and_then(|t| site_path(t).ok()).filter(|t| role(t) == Role::Template).ok_or_else(|| invalid(begin.line))?;
    let baseline = begin.attrs.get("locked_sha256").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).cloned();
    let mut st = State::default();
    let mut excluded = vec![begin.span.clone()];
    let mut instance_markers = Vec::new();
    let mut stack: Vec<IFrame> = Vec::new();
    let mut ended = false;
    for m in ms.iter().filter(|m| m.kind.instance()) {
        instance_markers.push(m.span.clone());
        if ended || (m.kind == Mk::IBegin && m.span != begin.span) {
            return Err(invalid(m.line));
        }
        let name = || marker_name(m.attrs.get("name")).ok_or_else(|| invalid(m.line));
        match m.kind {
            Mk::IEnd if stack.is_empty() => ended = true,
            Mk::IBeginEditable if !stack.iter().any(|f| matches!(f, IFrame::Editable(..))) => stack.push(IFrame::Editable(name()?, m.span.end)),
            Mk::IEndEditable => {
                let Some(IFrame::Editable(n, start)) = stack.pop() else { return Err(invalid(m.line)) };
                let content = text.get(start..m.span.start).unwrap_or("").to_string();
                excluded.push(start..m.span.start);
                let scope = match stack.last_mut() {
                    None => &mut st.editable,
                    Some(IFrame::Entry(map)) => map,
                    Some(_) => return Err(invalid(m.line)),
                };
                if scope.insert(n, content).is_some() {
                    return Err(invalid(m.line));
                }
            }
            Mk::IBeginRepeat if stack.is_empty() => stack.push(IFrame::Repeat(name()?, Vec::new())),
            Mk::IBeginEntry if matches!(stack.last(), Some(IFrame::Repeat(_, entries)) if entries.len() < MAX_REPEAT_ENTRIES) => stack.push(IFrame::Entry(BTreeMap::new())),
            Mk::IEndEntry => {
                let (Some(IFrame::Entry(map)), Some(IFrame::Repeat(_, entries))) = (stack.pop(), stack.last_mut()) else { return Err(invalid(m.line)) };
                entries.push(map);
            }
            Mk::IEndRepeat => {
                let Some(IFrame::Repeat(n, entries)) = stack.pop() else { return Err(invalid(m.line)) };
                if !stack.is_empty() || st.repeats.insert(n, entries).is_some() {
                    return Err(invalid(m.line));
                }
            }
            Mk::IParam if stack.is_empty() => {
                let value = m.attrs.get("value").cloned().unwrap_or_default();
                if st.params.insert(name()?, value).is_some() {
                    return Err(invalid(m.line));
                }
            }
            Mk::IBegin => {}
            _ => return Err(invalid(m.line)),
        }
    }
    if !stack.is_empty() || !ended {
        return Err(invalid(begin.line));
    }
    excluded.sort_by_key(|r| r.start);
    let locked = locked_digest(text, &excluded);
    Ok(Some(Instance { template, baseline, begin: begin.span.clone(), state: st, locked, markers: instance_markers }))
}

/// Writes one instance's text from its template (module docs).
struct Gen<'a> {
    t: &'a Template,
    text: &'a str,
    template_id: &'a str,
    params: BTreeMap<&'a str, &'a str>,
    st: &'a State,
    limit: usize,
}

impl Gen<'_> {
    fn emit(&self, parts: &[Part], scope: &BTreeMap<String, String>, out: &mut String) -> R<()> {
        for part in parts {
            match part {
                Part::Text(r) => out.push_str(self.text.get(r.clone()).unwrap_or("")),
                Part::Begin => out.push_str(&format!("<!-- InstanceBegin template=\"/{}\" locked_sha256=\"{DIGEST_HOLE}\" -->", self.template_id)),
                Part::End => out.push_str("<!-- InstanceEnd -->"),
                Part::Param(i) => {
                    if let Some(p) = self.t.params.get(*i) {
                        let value = self.params.get(p.name.as_str()).copied().unwrap_or("");
                        out.push_str(&format!("<!-- InstanceParam name=\"{}\" type=\"{}\" value=\"{value}\" -->", p.name, p.ty));
                    }
                }
                // [STU-WEB-092]: the pass-through code verbatim, for later server-side evaluation.
                Part::Expr { code, pass: true } => out.push_str(code),
                Part::Expr { code, pass: false } => out.push_str(&escape_text(self.params.get(code.as_str()).copied().unwrap_or(""))),
                Part::Editable { name, body } => {
                    out.push_str(&format!("<!-- InstanceBeginEditable name=\"{name}\" -->"));
                    match scope.get(name) {
                        Some(content) => out.push_str(content),
                        None => self.emit(body, scope, out)?,
                    }
                    out.push_str("<!-- InstanceEndEditable -->");
                }
                Part::If { cond, body } => {
                    if self.params.get(cond.as_str()) == Some(&"true") {
                        self.emit(body, scope, out)?;
                    }
                }
                Part::Repeat { name, body } => {
                    out.push_str(&format!("<!-- InstanceBeginRepeat name=\"{name}\" -->"));
                    let default = [BTreeMap::new()];
                    let entries = self.st.repeats.get(name).map_or(&default[..], Vec::as_slice);
                    for entry in entries {
                        out.push_str("<!-- InstanceBeginRepeatEntry -->");
                        self.emit(body, entry, out)?;
                        out.push_str("<!-- InstanceEndRepeatEntry -->");
                    }
                    out.push_str("<!-- InstanceEndRepeat -->");
                }
            }
            if out.len() > self.limit {
                return no("template_instance_too_large", &out.len().to_string());
            }
        }
        Ok(())
    }
}

/// A generated instance: its text, parsed, and its locked digest (written into its `InstanceBegin`).
struct Generated {
    text: String,
    doc: WebDoc,
    instance: Instance,
}

/// The instance `at` of template `template_id` with state `st` (module docs): generated, parsed, its digest filled in.
fn generated(t: &Template, text: &str, template_id: &str, st: &State, at: &str, cancel: &CancellationToken) -> R<Generated> {
    let params = t.params.iter().map(|p| (p.name.as_str(), st.params.get(&p.name).map_or(p.value.as_str(), String::as_str))).collect();
    let limits = Limits::default();
    let g = Gen { t, text, template_id, params, st, limit: limits.max_bytes };
    let mut out = String::new();
    g.emit(&t.parts, &st.editable, &mut out)?;
    let first = parse(&out, 1, &limits, cancel).map_err(|e| web_no(&e))?;
    let inst = instance_of(&first, at)?.ok_or(No("template_markup_invalid", bounded(at), None))?;
    let hole = out.get(inst.begin.clone()).and_then(|b| b.find(DIGEST_HOLE)).map(|h| inst.begin.start.saturating_add(h)).ok_or(No("template_markup_invalid", bounded(at), None))?;
    out.replace_range(hole..hole.saturating_add(DIGEST_HOLE.len()), &inst.locked);
    let doc = parse(&out, 1, &limits, cancel).map_err(|e| web_no(&e))?;
    let instance = instance_of(&doc, at)?.ok_or(No("template_markup_invalid", bounded(at), None))?;
    Ok(Generated { text: out, doc, instance })
}

/// An instance's new text, its instance markers and (when generated) the parsed document.
type Regenerated = (String, Vec<Range<usize>>, Option<WebDoc>);

/// How an instance stands against its template ([STU-WEB-093]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// Its locked text is what the template generates now.
    UpToDate,
    /// Its locked text is what Studio last wrote; the template changed since.
    TemplateChanged,
    /// Its locked text was changed outside Studio (`locked_changed`) or Studio never wrote it (`no_baseline`).
    Drifted(&'static str),
}

fn standing(inst: &Instance, generated_locked: &str) -> Standing {
    if inst.locked == generated_locked {
        Standing::UpToDate
    } else if inst.baseline.as_deref() == Some(inst.locked.as_str()) {
        Standing::TemplateChanged
    } else {
        Standing::Drifted(if inst.baseline.is_none() { "no_baseline" } else { "locked_changed" })
    }
}

/// One file read on its own (no site walk): bounded, UTF-8, under the root, parsed with its references (each local
/// one unresolved, which re-basing does not need).
fn lone_source(root: &Path, id: &str, cancel: &CancellationToken) -> R<Source> {
    let path = join(root, id);
    if !resolved(&path).starts_with(resolved(root)) {
        return no("path_outside_site", id);
    }
    let limit = u64::try_from(Limits::default().max_bytes).unwrap_or(u64::MAX);
    let bytes = read_bounded(&path, limit)?;
    let text = String::from_utf8(bytes).map_err(|_| No("source_not_utf8", bounded(id), None))?;
    match source(&Files::default(), id, role(id), None, &text, cancel)? {
        Ok(src) => Ok(src),
        Err(code) => no(code, id),
    }
}

/// A template or library item's text as a document at `at` sees it: every relative reference re-based from its own
/// folder to `at`'s (the move's path arithmetic, `plan`).
fn placed_text(src: &Source, at: &str, link_base: &str) -> R<String> {
    let changes = plan(src, &src.id, at, link_base, true)?;
    applied_text(src.text(), &changes, 0).ok_or(No("reference_not_rewritable", bounded(&src.id), None))
}

/// The template `template` as the instance at `at` sees it: parsed after re-basing.
fn template_for(root: &Path, template: &str, at: &str, link_base: &str, cancel: &CancellationToken) -> R<(Template, String)> {
    let src = lone_source(root, template, cancel).map_err(|No(code, target, detail)| if code == "file_missing" { No("template_not_found", bounded(template), None) } else { No(code, target, detail) })?;
    template_from(&src, at, link_base, cancel)
}

fn template_from(src: &Source, at: &str, link_base: &str, cancel: &CancellationToken) -> R<(Template, String)> {
    let text = placed_text(src, at, link_base)?;
    let doc = parse(&text, 1, &Limits::default(), cancel).map_err(|e| web_no(&e))?;
    Ok((template_of(&doc, &src.id)?, text))
}

/// A parameter value for `name` of template `t`, typed (`template_param_missing`, `parameter_invalid`).
fn param_value(t: &Template, name: &str, value: &str) -> R<String> {
    let p = t.param(name).ok_or(No("template_param_missing", bounded(name), None))?;
    typed_value(p.ty, value).ok_or(No("parameter_invalid", bounded(&format!("value of {name}")), None))
}

fn opt_index(p: &P<'_>, key: &str) -> R<Option<usize>> {
    match p.0.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).map(Some).ok_or(No("parameter_invalid", key.to_string(), None)),
    }
}

/// `web.instance_repeat`'s change of the entries of repeat `region` (module docs).
fn repeat_op(t: &Template, st: &mut State, region: &str, op: &str, index: Option<usize>, to: Option<usize>) -> R<()> {
    if !t.repeat(region) {
        return no("template_region_missing", region);
    }
    let entries = st.repeats.entry(region.to_string()).or_insert_with(|| vec![BTreeMap::new()]);
    let len = entries.len();
    match (op, index, to) {
        ("add", at, None) => {
            let at = at.unwrap_or(len);
            if at > len {
                return no("parameter_invalid", "index");
            }
            if len >= MAX_REPEAT_ENTRIES {
                return no("repeat_entries_over_limit", &len.to_string());
            }
            entries.insert(at, BTreeMap::new());
        }
        ("remove", Some(at), None) if at < len => {
            entries.remove(at);
        }
        ("move", Some(at), Some(to)) if at < len && to < len => {
            let entry = entries.remove(at);
            entries.insert(to, entry);
        }
        ("add" | "remove" | "move", ..) => return no("parameter_invalid", "index"),
        _ => return no("parameter_invalid", "op"),
    }
    Ok(())
}

/// The one edit that turns `old` into `new` (their common prefix and suffix kept), `None` when they are equal.
fn diff_edit(old: &str, new: &str) -> Option<TextEdit> {
    if old == new {
        return None;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !(old.is_char_boundary(p) && new.is_char_boundary(p)) {
        p = p.saturating_sub(1);
    }
    let room = a.len().min(b.len()).saturating_sub(p);
    let mut s = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| x == y).count();
    while !(old.is_char_boundary(a.len().saturating_sub(s)) && new.is_char_boundary(b.len().saturating_sub(s))) {
        s = s.saturating_sub(1);
    }
    Some(TextEdit { span: p..a.len().saturating_sub(s), replacement: new.get(p..b.len().saturating_sub(s))?.to_string() })
}

/// The edits turning instance text `old` into `new`, one per changed stretch between instance markers when both
/// hold the same markers, else one edit (the receipt names each changed byte range, [STU-WEB-093]).
fn segment_edits(old: &str, old_markers: &[Range<usize>], new: &str, new_markers: &[Range<usize>]) -> Vec<TextEdit> {
    let cuts = |len: usize, ms: &[Range<usize>]| {
        let mut c = vec![0usize];
        for m in ms {
            c.push(m.start);
            c.push(m.end);
        }
        c.push(len);
        c
    };
    let (a, b) = (cuts(old.len(), old_markers), cuts(new.len(), new_markers));
    let whole = || -> Vec<TextEdit> { diff_edit(old, new).into_iter().collect() };
    if a.len() != b.len() || a.windows(2).any(|w| w[0] > w[1]) || b.windows(2).any(|w| w[0] > w[1]) {
        return whole();
    }
    let mut edits = Vec::new();
    for (wa, wb) in a.windows(2).zip(b.windows(2)) {
        let (Some(sa), Some(sb)) = (old.get(wa[0]..wa[1]), new.get(wb[0]..wb[1])) else { return whole() };
        if let Some(e) = diff_edit(sa, sb) {
            edits.push(TextEdit { span: e.span.start.saturating_add(wa[0])..e.span.end.saturating_add(wa[0]), replacement: e.replacement });
        }
    }
    edits
}

/// The receipt rows of edits against one text (sorted, not overlapping): each change's byte range before and after.
fn edit_rows(text: &str, edits: &[TextEdit]) -> Vec<Value> {
    let mut shift = 0isize;
    edits
        .iter()
        .map(|e| {
            let start = e.span.start.saturating_add_signed(shift);
            shift = shift.saturating_add(isize::try_from(e.replacement.len()).unwrap_or(0)).saturating_sub(isize::try_from(e.span.len()).unwrap_or(0));
            json!({"line": line_of(text, e.span.start), "before": [e.span.start, e.span.end], "after": [start, start.saturating_add(e.replacement.len())]})
        })
        .collect()
}

/// `text` with `edits` applied (sorted, not overlapping).
fn with_edits(text: &str, edits: &[TextEdit]) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for e in edits {
        out.push_str(text.get(at..e.span.start)?);
        out.push_str(&e.replacement);
        at = e.span.end;
    }
    out.push_str(text.get(at..)?);
    Some(out)
}

/// The site ids a document's markers name: the template of an instance and every library item it holds.
fn marker_targets(src: &Source) -> Vec<String> {
    let Ok(ms) = markers(&src.doc, &src.id) else { return Vec::new() };
    let dir = dir_of(&src.id);
    ms.iter()
        .filter_map(|m| match m.kind {
            Mk::IBegin => m.attrs.get("template").and_then(|t| t.strip_prefix('/')).map(str::to_string),
            Mk::LBegin => m.path.as_deref().and_then(|p| library_target(&dir, p)),
            _ => None,
        })
        .collect()
}

/// The site id a library marker's path names (`/...` from the root, else from the document's folder).
fn library_target(dir: &[String], path: &str) -> Option<String> {
    segments_of(dir, path).filter(|(s, folder)| !folder && !s.is_empty()).map(|(s, _)| s.join("/"))
}

/// `item`: a root-relative `.lbi` path.
fn library_id(p: &P<'_>) -> R<String> {
    let item = site_path(p.text("item")?)?;
    if role(&item) != Role::Library {
        return no("parameter_invalid", "item");
    }
    Ok(item)
}

/// The text a library item is placed with into the document at `at` (its content re-based) between its markers.
fn library_block(item: &str, content: &str) -> String {
    format!("<!-- #BeginLibraryItem \"/{item}\" -->{content}<!-- #EndLibraryItem -->")
}

impl WebHost {
    /// `web.template_regions` (module docs): a template's regions, parameters and expressions, or an instance's state
    /// and standing against its template.
    fn template_regions(&self, p: &P<'_>, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let (id, doc) = match p.opt_text("path")? {
            Some(path) => {
                let id = site_path(path)?;
                let doc = lone_source(&root, &id, cancel)?.doc;
                (id, doc)
            }
            None => (self.open_id_in(&root)?, self.doc()?.clone()),
        };
        if role(&id) == Role::Template {
            return Ok(template_of(&doc, &id)?.json(&id));
        }
        let inst = instance_of(&doc, &id)?.ok_or(No("not_a_template_instance", bounded(&id), None))?;
        let (t, text) = template_for(&root, &inst.template, &id, site.link_base, cancel)?;
        let g = generated(&t, &text, &inst.template, &inst.state, &id, cancel)?;
        let standing = match standing(&inst, &g.instance.locked) {
            Standing::UpToDate => json!({"status": "up_to_date"}),
            Standing::TemplateChanged => json!({"status": "template_changed"}),
            Standing::Drifted(reason) => json!({"status": "drifted", "code": "template_instance_drift", "reason": reason, "remedy": ["reapply", "detach"]}),
        };
        let mut out = t.json(&inst.template);
        out["kind"] = json!("instance");
        out["document"] = json!(id);
        out["instance"] = inst.json();
        out["standing"] = standing;
        Ok(out)
    }

    /// `web.template_instance` (module docs): a new instance of a template as the open document, not saved yet.
    fn template_instance(&mut self, p: &P<'_>, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let template = site_path(p.text("template")?)?;
        if role(&template) != Role::Template {
            return no("parameter_invalid", "template");
        }
        let path = site_path(p.text("path")?)?;
        if role(&path) != Role::Page {
            return no("parameter_invalid", "path");
        }
        if self.doc.as_ref().is_some_and(|d| self.saved_sha256.as_deref() != Some(d.sha256_hex())) {
            return no("document_unsaved", "web.save first");
        }
        let target = join(&root, &path);
        if std::fs::symlink_metadata(&target).is_ok() {
            return no("instance_path_exists", &path);
        }
        if !resolved(&target).starts_with(resolved(&root)) {
            return no("path_outside_site", &path);
        }
        let (t, text) = template_for(&root, &template, &path, site.link_base, cancel)?;
        let mut st = State::default();
        match p.0.get("params") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for (name, value) in map {
                    let value = value.as_str().ok_or(No("parameter_invalid", bounded(&format!("params.{name}")), None))?;
                    st.params.insert(name.clone(), param_value(&t, name, value)?);
                }
            }
            Some(_) => return no("parameter_invalid", "params"),
        }
        let g = generated(&t, &text, &template, &st, &path, cancel)?;
        self.doc = Some(g.doc);
        self.path = Some(join(&PathBuf::from(&site.local_root), &path));
        self.saved_sha256 = None;
        self.undo.clear();
        self.redo.clear();
        self.published_root = None;
        Ok(json!({"instance": {"document": path, "template": template, "locked_sha256": g.instance.locked}, "web": self.state_json()}))
    }

    /// `web.instance_param` and `web.instance_repeat` (module docs): the open instance regenerated from its template
    /// with `change` applied to its state, as one named history step; refused `template_instance_drift` when its
    /// locked text was changed outside Studio.
    fn instance_edit(&mut self, label: &'static str, ctx: &CommandContext, cancel: &CancellationToken, change: impl FnOnce(&Template, &mut State) -> R<()>) -> R<Value> {
        let (site, root) = self.site_root()?;
        let doc = self.doc()?.clone();
        let id = self.open_id_in(&root)?;
        let inst = instance_of(&doc, &id)?.ok_or(No("not_a_template_instance", bounded(&id), None))?;
        let (t, text) = template_for(&root, &inst.template, &id, site.link_base, cancel)?;
        let now = generated(&t, &text, &inst.template, &inst.state, &id, cancel)?;
        if let Standing::Drifted(reason) = standing(&inst, &now.instance.locked) {
            return Err(No("template_instance_drift", bounded(&id), Some(json!({"document": id, "template": inst.template, "reason": reason, "remedy": ["reapply", "detach"]}))));
        }
        let mut st = inst.state.clone();
        change(&t, &mut st)?;
        let next = generated(&t, &text, &inst.template, &st, &id, cancel)?;
        let Some(edit) = diff_edit(doc.source(), &next.text) else { return no("nothing_to_change", label) };
        self.edit(label, SourcePatch::new(&doc, vec![edit]), ctx, cancel)
    }

    /// `web.template_update`'s checks before admission (no I/O): the template's id and the resolutions.
    fn template_update_checks(&self, p: &P<'_>) -> R<(String, BTreeMap<String, &'static str>)> {
        self.site_root()?;
        let template = site_path(p.text("template")?)?;
        if role(&template) != Role::Template {
            return no("parameter_invalid", "template");
        }
        let mut resolve = BTreeMap::new();
        match p.0.get("resolve") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) if map.len() <= MAX_EXPORT_FILES => {
                for (id, how) in map {
                    let how = match how.as_str() {
                        Some("reapply") => "reapply",
                        Some("detach") => "detach",
                        _ => return no("parameter_invalid", &format!("resolve.{id}")),
                    };
                    resolve.insert(site_path(id)?, how);
                }
            }
            Some(_) => return no("parameter_invalid", "resolve"),
        }
        Ok((template, resolve))
    }

    /// `web.template_update` ([STU-WEB-093], module docs).
    fn template_update(&mut self, template: &str, resolve: &BTreeMap<String, &'static str>, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let _lock = site_lock(&site.local_root, ctx)?;
        let set = SiteSet::load(&root, None, cancel)?;
        let open_id = self.open_on_disk(&root, &set)?;
        unreadable(&set)?;
        let source = set.sources.iter().find(|s| s.id == template && s.role == Role::Template).ok_or(No("template_not_found", bounded(template), None))?;
        let mut instances = Vec::new();
        for src in set.sources.iter().filter(|s| s.role == Role::Page) {
            if let Some(inst) = instance_of(&src.doc, &src.id)?.filter(|i| i.template == template) {
                instances.push((src, inst));
            }
        }
        if let Some(id) = resolve.keys().find(|id| !instances.iter().any(|(s, _)| &s.id == *id)) {
            return no("not_a_template_instance", id);
        }
        // Plan every instance before the first write.
        let mut rows = Vec::new();
        let mut writes: Vec<(String, String, Vec<TextEdit>, Option<WebDoc>)> = Vec::new();
        let (mut updated, mut drifted) = (0usize, 0usize);
        for (src, inst) in &instances {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            let old = src.doc.source();
            let how = resolve.get(&src.id).copied();
            let (outcome, next): (&str, Option<Regenerated>) = if how == Some("detach") {
                let mut text = String::with_capacity(old.len());
                let mut at = 0usize;
                for m in &inst.markers {
                    text.push_str(old.get(at..m.start).unwrap_or(""));
                    at = m.end;
                }
                text.push_str(old.get(at..).unwrap_or(""));
                ("detached", Some((text, Vec::new(), None)))
            } else {
                let (t, text) = template_from(source, &src.id, site.link_base, cancel)?;
                let g = generated(&t, &text, template, &inst.state, &src.id, cancel)?;
                let next = Some((g.text.clone(), g.instance.markers.clone(), Some(g.doc)));
                match (standing(inst, &g.instance.locked), how) {
                    (Standing::UpToDate, _) if inst.baseline.as_deref() == Some(inst.locked.as_str()) => ("up_to_date", None),
                    (Standing::UpToDate, _) => ("baseline_refreshed", next),
                    (Standing::TemplateChanged, _) => ("updated", next),
                    (Standing::Drifted(_), Some(_)) => ("reapplied", next),
                    (Standing::Drifted(reason), None) => {
                        drifted = drifted.saturating_add(1);
                        rows.push(json!({"document": src.id, "outcome": "drifted", "code": "template_instance_drift", "severity": "warning", "reason": reason, "remedy": ["reapply", "detach"], "changes": []}));
                        continue;
                    }
                }
            };
            let Some((text, new_markers, doc)) = next else {
                rows.push(json!({"document": src.id, "outcome": outcome, "changes": []}));
                continue;
            };
            let edits = if outcome == "detached" {
                inst.markers.iter().map(|m| TextEdit { span: m.clone(), replacement: String::new() }).collect()
            } else {
                let edits = segment_edits(old, &inst.markers, &text, &new_markers);
                if with_edits(old, &edits).as_deref() == Some(text.as_str()) { edits } else { diff_edit(old, &text).into_iter().collect() }
            };
            updated = updated.saturating_add(1);
            rows.push(json!({"document": src.id, "outcome": outcome, "changes": edit_rows(old, &edits)}));
            writes.push((src.id.clone(), text, edits, doc));
        }
        // One transaction per instance: each is one atomic write; a failure stops before the next (`written` names
        // the instances whose transaction completed).
        let receipt = |complete: bool, written: &[&str], failed: Option<&str>| {
            json!({
                "schema": "hsk.studio.web.template_receipt@1", "site_id": site.site_id, "template": template, "instances": rows, "updated": updated, "drifted": drifted,
                "complete": complete, "written": written, "failed": failed,
            })
        };
        let mut written: Vec<&str> = Vec::new();
        for (id, text, _, _) in &writes {
            if write_atomic(&join(&root, id), text.as_bytes()).is_err() {
                return Err(No("template_update_failed", bounded(id), Some(json!({"receipt": receipt(false, &written, Some(id))}))));
            }
            written.push(id);
        }
        if let Some(id) = &open_id
            && let Some((_, text, _, doc)) = writes.iter().find(|w| &w.0 == id)
        {
            let doc = match doc {
                Some(doc) => doc.clone(),
                None => parse(text, 1, &Limits::default(), &CancellationToken::default()).map_err(|e| web_no(&e))?,
            };
            self.reopen_written(&doc);
        }
        Ok(json!({"receipt": receipt(true, &written, None), "web": self.state_json()}))
    }

    /// `web.library_item_insert` ([STU-WEB-096], module docs): the item's content, re-based to the open document's
    /// folder, between its markers, as one named history step.
    fn library_item_insert(&mut self, label: &'static str, p: &P<'_>, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let item = library_id(p)?;
        let doc = self.doc()?.clone();
        let at = self.open_id_in(&root)?;
        let src = lone_source(&root, &item, cancel).map_err(|No(code, target, detail)| if code == "file_missing" { No("library_item_not_found", bounded(&item), None) } else { No(code, target, detail) })?;
        if markers(&src.doc, &item)?.iter().any(|m| matches!(m.kind, Mk::LBegin | Mk::LEnd)) {
            return no("library_item_nested", &item);
        }
        let block = library_block(&item, &placed_text(&src, &at, site.link_base)?);
        let parent = super::key(&doc, p.text("parent")?)?;
        let index = super::index(&doc, &parent, p)?;
        let patch = insert_fragment(&doc, &parent, index, &block, cancel).map_err(|e| web_no(&e))?;
        self.edit(label, patch, ctx, cancel)
    }

    /// `web.library_item_update` ([STU-WEB-096], module docs): every reference to the item holds its current content,
    /// re-based per document, in one transaction with a receipt.
    fn library_item_update(&mut self, item: &str, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let _lock = site_lock(&site.local_root, ctx)?;
        let set = SiteSet::load(&root, None, cancel)?;
        let open_id = self.open_on_disk(&root, &set)?;
        unreadable(&set)?;
        let source = set.sources.iter().find(|s| s.id == item && s.role == Role::Library).ok_or(No("library_item_not_found", bounded(item), None))?;
        if markers(&source.doc, item)?.iter().any(|m| matches!(m.kind, Mk::LBegin | Mk::LEnd)) {
            return no("library_item_nested", item);
        }
        let mut changed = Vec::new();
        let mut writes: Vec<(String, String)> = Vec::new();
        let mut docs: Vec<(String, WebDoc)> = Vec::new();
        for src in set.sources.iter().filter(|s| matches!(s.role, Role::Page | Role::Template)) {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            let ms = markers(&src.doc, &src.id)?;
            let dir = dir_of(&src.id);
            let mut open: Option<(&Marker, bool)> = None;
            let mut edits = Vec::new();
            let mut content: Option<String> = None;
            for m in ms.iter().filter(|m| matches!(m.kind, Mk::LBegin | Mk::LEnd)) {
                let invalid = || No("library_markup_invalid", bounded(&format!("{}:{}", src.id, m.line)), None);
                match (m.kind, open.take()) {
                    (Mk::LBegin, None) => open = Some((m, m.path.as_deref().and_then(|p| library_target(&dir, p)).as_deref() == Some(item))),
                    (Mk::LEnd, Some((begin, ours))) => {
                        if ours {
                            if content.is_none() {
                                content = Some(placed_text(source, &src.id, site.link_base)?);
                            }
                            let new = content.clone().unwrap_or_default();
                            if src.doc.source().get(begin.span.end..m.span.start) != Some(new.as_str()) {
                                edits.push(TextEdit { span: begin.span.end..m.span.start, replacement: new });
                            }
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            if let Some((begin, _)) = open {
                return no("library_markup_invalid", &format!("{}:{}", src.id, begin.line));
            }
            if edits.is_empty() {
                continue;
            }
            let applied = apply(&src.doc, &SourcePatch::new(&src.doc, edits.clone())).map_err(|e| web_no(&e))?;
            let next = parse(&applied.source, 1, src.doc.limits(), cancel).map_err(|e| web_no(&e))?;
            changed.push(json!({"document": src.id, "changes": edit_rows(src.doc.source(), &edits)}));
            writes.push((src.id.clone(), applied.source));
            docs.push((src.id.clone(), next));
        }
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        let receipt = |complete: bool, restored: &[String], failed: Option<&str>, restore_failed: &[String]| {
            json!({
                "schema": "hsk.studio.web.library_receipt@1", "site_id": site.site_id, "item": item, "changed": changed,
                "complete": complete, "restored": restored, "failed": failed, "restore_failed": restore_failed,
            })
        };
        let (written, failure) = write_each(&root, &writes);
        if let Some(failed) = failure {
            let (restored, restore_failed) = restore(&root, &written, &set.disk);
            return Err(No("library_update_failed", bounded(&failed), Some(json!({"receipt": receipt(false, &restored, Some(&failed), &restore_failed)}))));
        }
        if let Some(id) = &open_id
            && let Some((_, doc)) = docs.iter().find(|d| &d.0 == id)
        {
            self.reopen_written(doc);
        }
        Ok(json!({"receipt": receipt(true, &[], None, &[]), "web": self.state_json()}))
    }
}

// ----- the site record's optional fields and the snippet library (lane SMB-WEB-SITE-2; [STU-WEB-097] to
// [STU-WEB-099], [STU-WEB-113]) -----------------------------------------------------------------------------------

/// [STU-WEB-124]: the eight shipped server models.
const SERVER_MODELS: [&str; 8] = ["aspnet_csharp", "aspnet_vb", "asp_js", "asp_vbs", "coldfusion", "jsp", "php_mysql", "xslt"];
/// [STU-WEB-085]: the three pinned framework versions of the shipped baseline.
const FRAMEWORK_VERSIONS: [&str; 3] = ["3.4.1", "4.4.1", "5.3.8"];
/// A linter override's severity.
const SEVERITIES: [&str; 4] = ["off", "info", "warning", "error"];
/// Snippets of one site (the shipped baseline is 647, [STU-WEB-098]); bytes of one insert text.
const MAX_SNIPPETS: usize = 4096;
const MAX_SNIPPET_TEXT: usize = 16 << 10;
const MAX_LINT_OVERRIDES: usize = 256;
const MAX_BROWSER_PROFILES: usize = 64;
/// The site record's keys this module reads (each checked for a secret named inside it).
const DEPTH_KEYS: [&str; 8] = ["testing_target", "server_model_id", "preprocessor_config", "framework_pin", "linter_overrides", "browser_baseline", "version_control_binding", "snippets"];

/// [STU-WEB-086]: the site's preprocessor settings.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Preprocessor {
    source_root: String,
    output_root: String,
    output_style: String,
    source_maps: bool,
}

/// [STU-WEB-085]: the pinned framework versions.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FrameworkPin {
    framework_version: &'static str,
    helper_library_version: String,
    positioning_helper_version: Option<String>,
}

/// [STU-WEB-122]: the site's repository binding (a credential is a reference, [STU-WEB-114]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct VcBinding {
    repository: String,
    branch: Option<String>,
    credentials_ref: Option<String>,
}

/// [STU-WEB-097]: one snippet; `trigger` is its keyboard trigger word ([STU-WEB-099]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Snippet {
    snippet_name: String,
    description: String,
    folder_path: String,
    snippet_kind: &'static str,
    preview_mode: &'static str,
    before: String,
    after: String,
    trigger: Option<String>,
}

impl Snippet {
    fn id(&self) -> String {
        format!("{}/{}", self.folder_path, self.snippet_name)
    }

    fn group(&self) -> &str {
        self.folder_path.split('/').next().unwrap_or("")
    }

    fn json(&self) -> Value {
        json!({
            "snippet_name": self.snippet_name, "description": self.description, "folder_path": self.folder_path, "top_level_group": self.group(),
            "snippet_kind": self.snippet_kind, "preview_mode": self.preview_mode, "inserts_before_selection": self.before, "inserts_after_selection": self.after, "trigger": self.trigger,
        })
    }
}

/// The optional fields of [STU-WEB-113] and the snippet library of one site record.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SiteDepth(Box<DepthFields>);

/// The fields behind [`SiteDepth`] (boxed: a site record stays small in every history step).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DepthFields {
    testing_target: Option<super::Target>,
    server_model_id: Option<&'static str>,
    preprocessor_config: Option<Preprocessor>,
    framework_pin: Option<FrameworkPin>,
    linter_overrides: Option<BTreeMap<String, &'static str>>,
    browser_baseline: Option<Vec<String>>,
    version_control_binding: Option<VcBinding>,
    snippets: Vec<Snippet>,
}

impl std::ops::Deref for SiteDepth {
    type Target = DepthFields;

    fn deref(&self) -> &DepthFields {
        &self.0
    }
}

impl std::ops::DerefMut for SiteDepth {
    fn deref_mut(&mut self) -> &mut DepthFields {
        &mut self.0
    }
}

/// The dotted key of the first object key named like a secret inside `value` (depth at most 8).
fn secret_in(value: &Value, at: &str, depth: u8) -> Option<String> {
    match value {
        Value::Object(map) => map.iter().find_map(|(k, v)| {
            let path = format!("{at}.{k}");
            if super::SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str()) || depth >= 8 { Some(path) } else { secret_in(v, &path, depth.saturating_add(1)) }
        }),
        Value::Array(items) => items.iter().find_map(|v| secret_in(v, at, depth.saturating_add(1))),
        _ => None,
    }
}

/// An object parameter with only `allowed` keys (`parameter_unknown` names `key.field`).
fn object<'a>(key: &str, value: &'a Value, allowed: &[&str]) -> R<&'a Map<String, Value>> {
    let Value::Object(map) = value else { return no("parameter_invalid", key) };
    if let Some(k) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return no("parameter_unknown", &format!("{key}.{k}"));
    }
    Ok(map)
}

fn field_text<'a>(map: &'a Map<String, Value>, key: &str, field: &str) -> R<Option<&'a str>> {
    match map.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => no("parameter_invalid", &format!("{key}.{field}")),
    }
}

/// A token `[a-z0-9._-]{1,64}`.
fn token(key: &str, value: &str) -> R<String> {
    if value.is_empty() || value.len() > 64 || !value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')) {
        return no("parameter_invalid", key);
    }
    Ok(value.to_string())
}

/// A version `N.N.N`.
fn version(key: &str, value: &str) -> R<String> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.len() > 6 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return no("parameter_invalid", key);
    }
    Ok(value.to_string())
}

/// A credential-store reference ([STU-WEB-114]: the target's rule).
fn credential_ref(key: &str, value: &str) -> R<String> {
    let ok = !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'));
    if !ok {
        return no("parameter_invalid", key);
    }
    Ok(value.to_string())
}

/// Text of at most `max` characters, no control character (newlines and tabs allowed when `lines`).
fn plain(key: &str, value: &str, max: usize, lines: bool) -> R<String> {
    if value.chars().count() > max || value.chars().any(|c| c.is_control() && !(lines && matches!(c, '\n' | '\t' | '\r'))) {
        return no("parameter_invalid", key);
    }
    Ok(value.to_string())
}

/// A site folder (`a/b`, portable segments).
fn folder(key: &str, value: &str) -> R<String> {
    match segments_of(&[], value) {
        Some((segments, _)) if !segments.is_empty() && segments.join("/") == value.trim_end_matches('/') => Ok(segments.join("/")),
        _ => no("parameter_invalid", key),
    }
}

/// Patches `depth` with the keys of `p` this module owns ([STU-WEB-113]; `null` clears an optional field). Called
/// by `site_from` for `web.site` and for a site record read back (the only source of `snippets`).
pub(super) fn depth_into(depth: &mut SiteDepth, p: &P<'_>) -> R<()> {
    for key in DEPTH_KEYS {
        if let Some(path) = p.0.get(key).and_then(|v| secret_in(v, key, 0)) {
            return no("credential_in_site_record", &path);
        }
    }
    if let Some(v) = p.0.get("testing_target") {
        depth.testing_target = match v {
            Value::Null => None,
            Value::Object(map) => {
                let scratch = super::Site {
                    site_id: "site".into(),
                    display_name: String::new(),
                    local_root: String::new(),
                    default_images_folder: None,
                    link_base: "document",
                    cache_enabled: false,
                    targets: Vec::new(),
                    depth: SiteDepth::default(),
                };
                let mut site = super::target_into(&scratch, &P(map)).map_err(|No(code, at, _)| No(code, bounded(&format!("testing_target {at}")), None))?;
                site.targets.pop()
            }
            _ => return no("parameter_invalid", "testing_target"),
        };
    }
    if let Some(v) = p.0.get("server_model_id") {
        depth.server_model_id = match v {
            Value::Null => None,
            Value::String(s) => Some(SERVER_MODELS.iter().copied().find(|m| m == s).ok_or(No("server_model_unknown", bounded(s), None))?),
            _ => return no("parameter_invalid", "server_model_id"),
        };
    }
    if let Some(v) = p.0.get("preprocessor_config") {
        depth.preprocessor_config = match v {
            Value::Null => None,
            v => {
                let key = "preprocessor_config";
                let map = object(key, v, &["source_root", "output_root", "output_style", "source_maps"])?;
                let need = |f: &str| field_text(map, key, f).and_then(|t| t.ok_or(No("parameter_missing", format!("{key}.{f}"), None)));
                Some(Preprocessor {
                    source_root: folder("preprocessor_config.source_root", need("source_root")?)?,
                    output_root: folder("preprocessor_config.output_root", need("output_root")?)?,
                    output_style: token("preprocessor_config.output_style", need("output_style")?)?,
                    source_maps: match map.get("source_maps") {
                        None | Some(Value::Null) => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => return no("parameter_invalid", "preprocessor_config.source_maps"),
                    },
                })
            }
        };
    }
    if let Some(v) = p.0.get("framework_pin") {
        depth.framework_pin = match v {
            Value::Null => None,
            v => {
                let key = "framework_pin";
                let map = object(key, v, &["framework_version", "helper_library_version", "positioning_helper_version"])?;
                let framework = field_text(map, key, "framework_version")?.ok_or(No("parameter_missing", "framework_pin.framework_version".into(), None))?;
                let framework_version = FRAMEWORK_VERSIONS.iter().copied().find(|v| *v == framework).ok_or(No("framework_version_unsupported", bounded(framework), None))?;
                let helper = field_text(map, key, "helper_library_version")?.ok_or(No("parameter_missing", "framework_pin.helper_library_version".into(), None))?;
                let positioning = field_text(map, key, "positioning_helper_version")?.map(|v| version("framework_pin.positioning_helper_version", v)).transpose()?;
                Some(FrameworkPin { framework_version, helper_library_version: version("framework_pin.helper_library_version", helper)?, positioning_helper_version: positioning })
            }
        };
    }
    if let Some(v) = p.0.get("linter_overrides") {
        depth.linter_overrides = match v {
            Value::Null => None,
            Value::Object(map) if map.len() <= MAX_LINT_OVERRIDES => {
                let mut out = BTreeMap::new();
                for (rule, severity) in map {
                    let at = format!("linter_overrides.{rule}");
                    let severity = severity.as_str().and_then(|s| SEVERITIES.iter().copied().find(|x| *x == s)).ok_or(No("parameter_invalid", bounded(&at), None))?;
                    out.insert(token(&at, rule)?, severity);
                }
                Some(out)
            }
            _ => return no("parameter_invalid", "linter_overrides"),
        };
    }
    if let Some(v) = p.0.get("browser_baseline") {
        depth.browser_baseline = match v {
            Value::Null => None,
            Value::Array(items) if !items.is_empty() && items.len() <= MAX_BROWSER_PROFILES => {
                let mut out: Vec<String> = Vec::new();
                for item in items {
                    let profile = token("browser_baseline", item.as_str().unwrap_or(""))?;
                    if out.contains(&profile) {
                        return no("parameter_invalid", &format!("browser_baseline {profile}"));
                    }
                    out.push(profile);
                }
                Some(out)
            }
            _ => return no("parameter_invalid", "browser_baseline"),
        };
    }
    if let Some(v) = p.0.get("version_control_binding") {
        depth.version_control_binding = match v {
            Value::Null => None,
            v => {
                let key = "version_control_binding";
                let map = object(key, v, &["repository", "branch", "credentials_ref"])?;
                let repository = field_text(map, key, "repository")?.ok_or(No("parameter_missing", "version_control_binding.repository".into(), None))?;
                Some(VcBinding {
                    repository: plain("version_control_binding.repository", repository, 1024, false).and_then(|r| if r.is_empty() { no("parameter_invalid", "version_control_binding.repository") } else { Ok(r) })?,
                    branch: field_text(map, key, "branch")?.map(|b| plain("version_control_binding.branch", b, 128, false)).transpose()?,
                    credentials_ref: field_text(map, key, "credentials_ref")?.map(|c| credential_ref("version_control_binding.credentials_ref", c)).transpose()?,
                })
            }
        };
    }
    if let Some(v) = p.0.get("snippets") {
        depth.snippets = loaded_snippets(v)?;
    }
    Ok(())
}

/// The site record's JSON with this module's fields (`site_json`).
pub(super) fn with_depth(depth: &SiteDepth, mut site: Value) -> Value {
    let target = depth.testing_target.as_ref().map(|t| {
        json!({"target_id": t.target_id, "display_name": t.display_name, "transport": t.transport, "host": t.host, "port": t.port, "root_path": t.root_path, "credentials_ref": t.credentials_ref})
    });
    let fields = [
        ("testing_target", json!(target)),
        ("server_model_id", json!(depth.server_model_id)),
        ("preprocessor_config", json!(depth.preprocessor_config.as_ref().map(|c| json!({"source_root": c.source_root, "output_root": c.output_root, "output_style": c.output_style, "source_maps": c.source_maps})))),
        (
            "framework_pin",
            json!(depth.framework_pin.as_ref().map(|f| json!({"framework_version": f.framework_version, "helper_library_version": f.helper_library_version, "positioning_helper_version": f.positioning_helper_version}))),
        ),
        ("linter_overrides", json!(depth.linter_overrides)),
        ("browser_baseline", json!(depth.browser_baseline)),
        ("version_control_binding", json!(depth.version_control_binding.as_ref().map(|b| json!({"repository": b.repository, "branch": b.branch, "credentials_ref": b.credentials_ref})))),
        ("snippets", Value::Array(depth.snippets.iter().map(Snippet::json).collect())),
    ];
    if let Value::Object(map) = &mut site {
        for (key, value) in fields {
            map.insert(key.to_string(), value);
        }
    }
    site
}

/// One snippet from its record fields (`strict`: a command, every rule; read back from a site record, an unnamed
/// snippet is kept and reported, [STU-WEB-098]).
fn snippet_from(map: &Map<String, Value>, current: Option<&Snippet>, strict: bool) -> R<Snippet> {
    let key = "snippet";
    let text = |f: &str| field_text(map, key, f);
    let folder_path = match text("folder_path")? {
        Some(f) => f.split('/').map(|s| plain("folder_path", s, 128, false).and_then(|s| if s.is_empty() || s.contains('"') { no("parameter_invalid", "folder_path") } else { Ok(s) })).collect::<R<Vec<_>>>()?.join("/"),
        None => return no("parameter_missing", "folder_path"),
    };
    if folder_path.split('/').count() > MAX_SEGMENTS {
        return no("parameter_invalid", "folder_path");
    }
    let snippet_name = plain("snippet_name", text("snippet_name")?.unwrap_or(""), 128, false)?;
    if (strict && snippet_name.is_empty()) || snippet_name.contains('/') {
        return no("parameter_invalid", "snippet_name");
    }
    if let Some(group) = text("top_level_group")?
        && group != folder_path.split('/').next().unwrap_or("")
    {
        return no("parameter_invalid", "top_level_group");
    }
    let base = current.cloned().unwrap_or(Snippet { snippet_name: snippet_name.clone(), description: String::new(), folder_path: folder_path.clone(), snippet_kind: "block", preview_mode: "code", before: String::new(), after: String::new(), trigger: None });
    let mut s = Snippet { snippet_name, folder_path, ..base };
    if let Some(d) = text("description")? {
        s.description = plain("description", d, 1024, true)?;
    }
    if let Some(k) = text("snippet_kind")? {
        s.snippet_kind = match k {
            "block" => "block",
            "wrap" => "wrap",
            _ => return no("parameter_invalid", "snippet_kind"),
        };
    }
    if let Some(m) = text("preview_mode")? {
        s.preview_mode = match m {
            "code" => "code",
            "design" => "design",
            _ => return no("parameter_invalid", "preview_mode"),
        };
    }
    // [STU-WEB-097]: insert texts are verbatim; only their size is bounded.
    for (field, slot) in [("inserts_before_selection", &mut s.before), ("inserts_after_selection", &mut s.after)] {
        if let Some(t) = text(field)? {
            if t.len() > MAX_SNIPPET_TEXT {
                return no("snippet_text_too_large", field);
            }
            *slot = t.to_string();
        }
    }
    match text("trigger")? {
        Some("") => s.trigger = None,
        Some(t) if t.len() <= 64 && t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) => s.trigger = Some(t.to_string()),
        Some(_) => return no("parameter_invalid", "trigger"),
        None => {}
    }
    Ok(s)
}

/// [STU-WEB-099]: a trigger claimed by two snippets refuses `snippet_trigger_duplicate` naming both and the
/// trigger ([STU-WEB-161]).
fn unique_triggers(snippets: &[Snippet]) -> R<()> {
    let mut seen: BTreeMap<&str, &Snippet> = BTreeMap::new();
    for s in snippets {
        if let Some(t) = s.trigger.as_deref()
            && let Some(first) = seen.insert(t, s)
        {
            return Err(No("snippet_trigger_duplicate", bounded(&format!("{t}: {}, {}", first.id(), s.id())), Some(json!({"trigger": t, "snippets": [first.id(), s.id()], "severity": "error"}))));
        }
    }
    Ok(())
}

/// The snippet library read back from a site record.
fn loaded_snippets(value: &Value) -> R<Vec<Snippet>> {
    let Value::Array(items) = value else { return no("parameter_invalid", "snippets") };
    if items.len() > MAX_SNIPPETS {
        return no("snippets_over_limit", &items.len().to_string());
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let map = object("snippets", item, &["snippet_name", "description", "folder_path", "top_level_group", "snippet_kind", "preview_mode", "inserts_before_selection", "inserts_after_selection", "trigger"])?;
        out.push(snippet_from(map, None, false)?);
    }
    unique_triggers(&out)?;
    Ok(out)
}

impl WebHost {
    /// `web.snippet` ([STU-WEB-097], [STU-WEB-099]; module docs): adds, replaces or removes one snippet of the site
    /// record, one named history step.
    fn snippet(&mut self, label: &'static str, p: &P<'_>, ctx: &CommandContext) -> R<Value> {
        let current = self.site.clone().ok_or(No("no_site", "web.site first".into(), None))?;
        let mut site = current.clone();
        let folder_path = p.text("folder_path")?;
        let name = p.text("snippet_name")?;
        let at = site.depth.snippets.iter().position(|s| s.folder_path == folder_path && s.snippet_name == name);
        if p.flag("remove")? {
            let at = at.ok_or(No("no_such_snippet", bounded(&format!("{folder_path}/{name}")), None))?;
            site.depth.snippets.remove(at);
        } else {
            let s = snippet_from(p.0, at.and_then(|i| site.depth.snippets.get(i)), true)?;
            match at {
                Some(i) => {
                    if let Some(slot) = site.depth.snippets.get_mut(i) {
                        *slot = s;
                    }
                }
                None if site.depth.snippets.len() >= MAX_SNIPPETS => return no("snippets_over_limit", &MAX_SNIPPETS.to_string()),
                None => site.depth.snippets.push(s),
            }
            unique_triggers(&site.depth.snippets)?;
        }
        // The record must read back (`web.open` reads at most 1 MiB).
        let record = serde_json::to_vec_pretty(&super::sidecar_json(Some(&site), self.recipe.as_ref())).map_err(|_| No("save_failed", String::new(), None))?;
        if u64::try_from(record.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return no("site_record_too_large", &record.len().to_string());
        }
        self.record(label, super::Change::Site { before: Some(current), after: Some(site) }, ctx)
    }

    /// `web.snippets` ([STU-WEB-098]): the library by top-level group and folder, the trigger map and load diagnostics.
    fn snippets(&self) -> R<Value> {
        let site = self.site.as_ref().ok_or(No("no_site", "web.site first".into(), None))?;
        let all = &site.depth.snippets;
        let mut groups: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
        for s in all {
            let g = groups.entry(s.group()).or_insert((0, 0, 0));
            g.0 = g.0.saturating_add(1);
            if s.snippet_kind == "block" {
                g.1 = g.1.saturating_add(1);
            } else {
                g.2 = g.2.saturating_add(1);
            }
        }
        let mut listed: Vec<&Snippet> = all.iter().collect();
        listed.sort_by(|a, b| (&a.folder_path, &a.snippet_name).cmp(&(&b.folder_path, &b.snippet_name)));
        Ok(json!({
            "snippets_total": all.len(),
            "groups": groups.iter().map(|(g, (n, block, wrap))| json!({"top_level_group": g, "count": n, "block": block, "wrap": wrap})).collect::<Vec<_>>(),
            "snippets": listed.iter().take(MAX_ROWS).map(|s| json!({"id": s.id(), "folder_path": s.folder_path, "snippet_name": s.snippet_name, "top_level_group": s.group(), "snippet_kind": s.snippet_kind, "preview_mode": s.preview_mode, "trigger": s.trigger, "description": super::row_text(&s.description)})).collect::<Vec<_>>(),
            "truncated": all.len().saturating_sub(MAX_ROWS),
            "triggers": all.iter().filter_map(|s| s.trigger.as_ref().map(|t| (t.clone(), json!(s.id())))).collect::<Map<String, Value>>(),
            "diagnostics": all.iter().filter(|s| s.snippet_name.is_empty()).map(|s| json!({"code": "snippet_unnamed", "severity": "warning", "folder_path": s.folder_path})).collect::<Vec<_>>(),
        }))
    }

    /// `web.snippet_insert` ([STU-WEB-097], [STU-WEB-099]; module docs): a snippet's texts verbatim at byte `offset`
    /// (a `wrap` snippet around `offset..end`), or its trigger word just before `offset` expanded, as one named
    /// history step.
    fn snippet_insert(&mut self, label: &'static str, p: &P<'_>, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let site = self.site.as_ref().ok_or(No("no_site", "web.site first".into(), None))?;
        let doc = self.doc()?.clone();
        let src = doc.source();
        let at = |key: &str| -> R<Option<usize>> {
            match opt_index(p, key)? {
                Some(n) if n <= src.len() && src.is_char_boundary(n) => Ok(Some(n)),
                Some(_) => no("parameter_invalid", key),
                None => Ok(None),
            }
        };
        let offset = at("offset")?.ok_or(No("parameter_missing", "offset".into(), None))?;
        let end = at("end")?;
        let (snippet, word) = match (p.opt_text("trigger")?, p.opt_text("folder_path")?, p.opt_text("snippet_name")?) {
            (Some(trigger), None, None) => {
                let s = site.depth.snippets.iter().find(|s| s.trigger.as_deref() == Some(trigger)).ok_or(No("no_such_snippet", bounded(trigger), None))?;
                // The word just before the cursor, alone (not the tail of a longer word).
                let start = offset.checked_sub(trigger.len()).filter(|s| src.get(*s..offset) == Some(trigger)).ok_or(No("snippet_trigger_not_at_offset", bounded(trigger), None))?;
                if src.get(..start).and_then(|b| b.chars().next_back()).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                    return no("snippet_trigger_not_at_offset", trigger);
                }
                (s, Some(start))
            }
            (None, Some(folder_path), Some(name)) => (site.depth.snippets.iter().find(|s| s.folder_path == folder_path && s.snippet_name == name).ok_or(No("no_such_snippet", bounded(&format!("{folder_path}/{name}")), None))?, None),
            _ => return no("parameter_invalid", "trigger or folder_path and snippet_name"),
        };
        let edits = match (snippet.snippet_kind, word, end) {
            (_, Some(start), None) => vec![TextEdit { span: start..offset, replacement: format!("{}{}", snippet.before, snippet.after) }],
            ("block", None, None) => vec![TextEdit { span: offset..offset, replacement: format!("{}{}", snippet.before, snippet.after) }],
            ("wrap", None, Some(end)) if end >= offset => vec![TextEdit { span: offset..offset, replacement: snippet.before.clone() }, TextEdit { span: end..end, replacement: snippet.after.clone() }],
            _ => return no("parameter_invalid", "end"),
        };
        self.edit(label, SourcePatch::new(&doc, edits), ctx, cancel)
    }
}

// ----- synchronisation (lane SMB-WEB-SITE-2; [STU-WEB-119]) -------------------------------------------------------

/// Modification times closer than this are the same time (file systems keep times at different granularities).
const SYNC_MTIME_TOLERANCE_MS: u64 = 2000;

/// `web.synchronize`'s request, checked before admission (module docs).
pub(super) struct SyncPlan {
    target_id: String,
    root_path: String,
    direction: &'static str,
    dependents: &'static str,
    files: Option<Vec<String>>,
    apply: bool,
}

/// A file's size and modification time (milliseconds since the epoch).
type Stat = (u64, u64);
/// One compared file: its id, action and both sides' stats.
type SyncRow = (String, &'static str, Option<Stat>, Option<Stat>);

/// A regular file's size and modification time (milliseconds since the epoch).
fn file_stat(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path).ok().filter(std::fs::Metadata::is_file)?;
    let ms = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))?;
    Some((meta.len(), ms))
}

/// The action for one file ([STU-WEB-119]), from size and modification time: `put`, `get`, `same`, `conflict` (the
/// same time, another size), `local_newer` (a get leaves it), `remote_newer` (a put leaves it) or `none`.
fn sync_action(direction: &str, local: Option<(u64, u64)>, remote: Option<(u64, u64)>) -> &'static str {
    let newer = |a: (u64, u64), b: (u64, u64)| a.1 > b.1.saturating_add(SYNC_MTIME_TOLERANCE_MS);
    match (local, remote) {
        (None, None) => "none",
        (Some(_), None) => {
            if direction == "get" {
                "none"
            } else {
                "put"
            }
        }
        (None, Some(_)) => {
            if direction == "put" {
                "none"
            } else {
                "get"
            }
        }
        (Some(l), Some(r)) if newer(l, r) => {
            if direction == "get" {
                "local_newer"
            } else {
                "put"
            }
        }
        (Some(l), Some(r)) if newer(r, l) => {
            if direction == "put" {
                "remote_newer"
            } else {
                "get"
            }
        }
        (Some(l), Some(r)) if l.0 == r.0 => "same",
        _ => "conflict",
    }
}

fn stat_json(stat: Option<(u64, u64)>) -> Value {
    stat.map_or(Value::Null, |(bytes, ms)| json!({"bytes": bytes, "modified_ms": ms}))
}

impl WebHost {
    /// `web.synchronize`'s checks before admission (no I/O): the target and the options.
    fn sync_checks(&self, p: &P<'_>) -> R<SyncPlan> {
        let site = self.site.as_ref().ok_or(No("no_site", String::new(), None))?;
        let target = match p.opt_text("target_id")? {
            Some(id) => site.targets.iter().find(|t| t.target_id == id).ok_or(No("no_such_target", bounded(id), None))?,
            None => {
                let mut local = site.targets.iter().filter(|t| t.transport == "local_or_network");
                match (local.next(), local.next()) {
                    (Some(t), None) => t,
                    (None, _) => return no("no_such_target", "local_or_network"),
                    (Some(_), Some(_)) => return no("parameter_missing", "target_id"),
                }
            }
        };
        if target.transport != "local_or_network" {
            return no("transport_unsupported", target.transport);
        }
        let direction = match p.text("direction")? {
            "put" => "put",
            "get" => "get",
            "both" => "both",
            _ => return no("parameter_invalid", "direction"),
        };
        let dependents = match p.opt_text("dependents")? {
            None | Some("prompt") => "prompt",
            Some("include") => "include",
            Some("exclude") => "exclude",
            Some(_) => return no("parameter_invalid", "dependents"),
        };
        let files = match p.0.get("files") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) if !items.is_empty() && items.len() <= MAX_EXPORT_FILES => Some(items.iter().map(|v| site_path(v.as_str().unwrap_or(""))).collect::<R<Vec<_>>>()?),
            Some(_) => return no("parameter_invalid", "files"),
        };
        Ok(SyncPlan { target_id: target.target_id.clone(), root_path: target.root_path.clone(), direction, dependents, files, apply: p.flag("apply")? })
    }

    /// `web.synchronize` ([STU-WEB-119], module docs): the action list for review, or (`apply`) each action applied
    /// as its own transaction with a per-file receipt.
    fn synchronize(&mut self, plan: &SyncPlan, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        let (site, root) = self.site_root()?;
        let remote = PathBuf::from(checked_path(&plan.root_path)?);
        let (remote_at, root_at) = (resolved(&remote), resolved(&root));
        if remote_at.starts_with(&root_at) || root_at.starts_with(&remote_at) {
            return no("target_inside_site", &plan.root_path);
        }
        let _lock = site_lock(&site.local_root, ctx)?;
        let local = walk(&root, cancel)?.files;
        let far = if remote.is_dir() {
            walk(&remote, cancel)?.files
        } else if plan.direction == "put" {
            Files::default()
        } else {
            return no("target_root_missing", &plan.root_path);
        };
        // The files to compare: the whole site and target, or the chosen files and (dependents) what they load.
        let mut dependents: Vec<String> = Vec::new();
        let selected: BTreeSet<String> = match &plan.files {
            None => local.files.iter().chain(&far.files).cloned().collect(),
            Some(files) => {
                let set = SiteSet::load(&root, None, cancel)?;
                let chosen: BTreeSet<String> = files.iter().cloned().collect();
                let mut deps = BTreeSet::new();
                for src in set.sources.iter().filter(|s| chosen.contains(&s.id)) {
                    for e in &src.edges {
                        if let Reach::File(id) = &e.reach
                            && !chosen.contains(id)
                            && e.loads != Some(Loads::of(Load::Document))
                        {
                            deps.insert(id.clone());
                        }
                    }
                }
                dependents = deps.iter().cloned().collect();
                let mut out = chosen;
                if plan.dependents == "include" {
                    out.extend(deps);
                }
                out
            }
        };
        let mut rows: Vec<SyncRow> = Vec::new();
        for id in selected.iter().filter(|id| !reserved(id)) {
            cancel.check().map_err(|_| No("canceled", String::new(), None))?;
            let segments = split_path(id);
            let l = local.files.contains(id).then(|| file_stat(&join(&root, id))).flatten();
            let r = far.files.contains(id).then(|| file_stat(&join(&remote, id))).flatten();
            // Never through a link or junction on either side.
            let action = if local.through_link(&segments) || far.through_link(&segments) { "denied_link" } else { sync_action(plan.direction, l, r) };
            rows.push((id.clone(), action, l, r));
        }
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, action, ..) in &rows {
            let n = counts.entry(*action).or_insert(0);
            *n = n.saturating_add(1);
        }
        let listed: Vec<Value> = rows.iter().take(MAX_ROWS).map(|(id, action, l, r)| json!({"path": id, "action": action, "local": stat_json(*l), "remote": stat_json(*r)})).collect();
        if !plan.apply {
            return Ok(json!({
                "schema": "hsk.studio.web.sync_plan@1", "site_id": site.site_id, "target_id": plan.target_id, "direction": plan.direction, "applied": false,
                "actions": listed, "truncated": rows.len().saturating_sub(MAX_ROWS), "counts": counts,
                "dependents": dependents, "dependents_option": plan.dependents,
            }));
        }
        // A get never overwrites the open document's unsaved edits.
        let open_id = self.open_view().and_then(|o| std::fs::canonicalize(&root).ok().and_then(|c| id_in(&c, o.path)));
        let dirty = self.doc.as_ref().is_some_and(|d| self.saved_sha256.as_deref() != Some(d.sha256_hex()));
        if let Some(id) = &open_id
            && dirty
            && rows.iter().any(|(p, action, ..)| p == id && *action == "get")
        {
            return no("document_unsaved", "web.save first");
        }
        let mut files = Vec::new();
        let (mut transferred, mut failed, mut total) = (0usize, 0usize, 0u64);
        let mut first_failure: Option<String> = None;
        let mut reopened = None;
        for (id, action, ..) in rows.iter().filter(|r| matches!(r.1, "put" | "get")) {
            if cancel.check().is_err() {
                let receipt = json!({"schema": "hsk.studio.web.sync_receipt@1", "site_id": site.site_id, "target_id": plan.target_id, "files": files, "complete": false});
                return Err(No("canceled", String::new(), Some(json!({"receipt": receipt}))));
            }
            let (src, dst) = if *action == "put" { (join(&root, id), join(&remote, id)) } else { (join(&remote, id), join(&root, id)) };
            let modified = std::fs::symlink_metadata(&src).and_then(|m| m.modified()).ok();
            let result = read_bounded(&src, super::MAX_ASSET_BYTES).and_then(|bytes| {
                total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                if total > MAX_EXPORT_BYTES {
                    return no("sync_too_large", id);
                }
                super::write_replacing(&dst, &bytes)?;
                Ok(bytes)
            });
            match result {
                Ok(bytes) => {
                    // The source's time on the copy, so the next comparison reads them as the same.
                    let kept = modified.is_some_and(|t| std::fs::File::options().write(true).open(&dst).and_then(|f| f.set_modified(t)).is_ok());
                    transferred = transferred.saturating_add(1);
                    if *action == "get" && open_id.as_deref() == Some(id.as_str()) {
                        reopened = String::from_utf8(bytes.clone()).ok();
                    }
                    files.push(json!({"path": id, "action": action, "outcome": "transferred", "bytes": bytes.len(), "sha256": sha256_hex(&bytes), "transfer_mode": super::transfer_mode(id).0, "time_kept": kept}));
                }
                Err(No(code, ..)) => {
                    failed = failed.saturating_add(1);
                    first_failure.get_or_insert_with(|| id.clone());
                    files.push(json!({"path": id, "action": action, "outcome": "failed", "code": code}));
                }
            }
        }
        if let Some(text) = reopened
            && let Ok(doc) = parse(&text, 1, &Limits::default(), &CancellationToken::default())
        {
            self.reopen_written(&doc);
        }
        let receipt = json!({
            "schema": "hsk.studio.web.sync_receipt@1", "site_id": site.site_id, "target_id": plan.target_id, "direction": plan.direction,
            "files": files, "transferred": transferred, "failed": failed, "complete": failed == 0, "counts": counts, "dependents": dependents,
        });
        if let Some(path) = first_failure {
            return Err(No("sync_failed", bounded(&path), Some(json!({"receipt": receipt}))));
        }
        Ok(json!({"receipt": receipt, "web": self.state_json()}))
    }
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

    /// Lane SMB-WEB-SITE-2 (ledger 2026-11-12T15:08:00Z): the site report is admitted as the session's read class.
    #[test]
    fn the_report_is_admitted_as_a_read() {
        assert_eq!(REPORT_ACTION.as_str(), "read");
    }

    /// [STU-WEB-091]: closed, five members, the spec's tokens.
    #[test]
    fn region_kinds_are_a_closed_set_of_five() {
        let tokens: Vec<&str> = RegionKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(tokens, ["editable", "optional", "editable_optional", "repeating", "repeating_table"]);
    }

    /// A percent-encoded spelling of a portable path resolves; an encoded separator, `%`, control character, bad
    /// escape or non-UTF-8 byte never does.
    #[test]
    fn an_encoded_separator_never_resolves() {
        assert_eq!(decoded_path("img/her%6F.png").as_deref(), Some("img/hero.png"));
        assert_eq!(decoded_path("plain.html").as_deref(), Some("plain.html"));
        for hostile in ["a%2Fb", "a%2fb", "a%5Cb", "a%25b", "a%0Ab", "a%zz", "a%2", "a%E2%82"] {
            assert_eq!(decoded_path(hostile), None, "{hostile}");
        }
    }

    /// A repeat holds at most 1024 entries; removing below the cap works again.
    #[test]
    fn repeat_entries_are_bounded() {
        let source = "<!DOCTYPE html><html><body><ul><!-- TemplateBeginRepeat name=\"r\" --><li>x</li><!-- TemplateEndRepeat --></ul></body></html>";
        let doc = parse(source, 1, &Limits::default(), &CancellationToken::default()).unwrap();
        let t = template_of(&doc, "t.dwt").ok().unwrap();
        let mut st = State::default();
        st.repeats.insert("r".into(), vec![BTreeMap::new(); MAX_REPEAT_ENTRIES]);
        assert!(matches!(repeat_op(&t, &mut st, "r", "add", None, None), Err(No("repeat_entries_over_limit", ..))));
        assert!(repeat_op(&t, &mut st, "r", "remove", Some(0), None).is_ok());
        assert!(repeat_op(&t, &mut st, "r", "add", Some(0), None).is_ok());
    }

    /// The one edit between two texts never splits a character.
    #[test]
    fn a_minimal_edit_keeps_characters_whole() {
        let e = diff_edit("a\u{e9} b", "a\u{e8} b").unwrap();
        assert_eq!((e.span, e.replacement.as_str()), (1..3, "\u{e8}"));
        assert!(diff_edit("same", "same").is_none());
        let e = diff_edit("ab", "aXb").unwrap();
        assert_eq!((e.span, e.replacement.as_str()), (1..1, "X"));
    }

    #[test]
    fn decisions_round_trip_through_the_granted_id() {
        let loads = Loads::of(Load::Document).and(Load::Image);
        for reach in [Reach::File("a/b.html".into()), Reach::Missing("x".into()), Reach::Case { id: "A.png".into(), actual: "a.png".into() }, Reach::Folder { path: String::new(), exists: true }, Reach::Folder { path: "d".into(), exists: false }] {
            assert_eq!(decode(&encode(loads, &reach)), Some((loads, reach)));
        }
    }
}
