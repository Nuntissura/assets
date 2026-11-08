//! The web document host (lane SMB-WEB-HOST-1; [STU-WEB-003], [STU-WEB-004], [STU-WEB-113] to [STU-WEB-120],
//! [STU-WEB-127], [STU-WEB-131]): the `web.*` rows of `RUN_COMMANDS`, run through [`Run::web_command`]. The first
//! dependent of the web crates: a model opens HTML source, edits it through the engine's source patches, undoes and
//! redoes, saves, defines a site with a remote target, sets an export recipe, exports a static tree, publishes it to
//! a folder with a receipt and verifies the published folder. No rendering.
//!
//! Text is authority ([STU-WEB-003]): the host holds one source revision and its parsed projection; every edit is a
//! `hsk_studio_web` source patch, so untouched bytes stay identical ([STU-WEB-004]). One command is one op record,
//! one terminal observe frame (the run's own, codes and counters only) and, when it changes the document, the site or
//! the recipe, one named step of the host's history carrying the caller's command context (actor, client session,
//! command id, base revision = the web source revision, correlation). A refusal changes nothing and records a typed
//! `error` and a bounded `target`.
//!
//! Parameters: `web.open` takes `path` or `source` (one of them) and `doctype` (`html_5`, the only member here).
//! A node `key`/`parent` is a node path (`/0:html/1:body`, `WebDoc::key_of`) or `#<id attribute>`. `index` is the
//! child position (default: append). `attrs` is a JSON object of decoded values, written in key order.
//! `web.site` and `web.export_recipe` patch the current record (the first call needs `display_name` and
//! `local_root`, or `output_root`). `web.remote_target` adds, replaces or (`remove`) drops one target: transport one
//! of the six of [STU-WEB-115]; `local_or_network` needs `root_path`, every other transport `host`, `root_path` and
//! `credentials_ref` (a reference, [STU-WEB-114]). A parameter named like a secret (`password`, `token`,
//! `private_key`, `secret`, `passphrase`) refuses `credential_in_site_record` naming the field only ([STU-WEB-167]).
//!
//! Persistence: `web.save` writes the source text, and the site, targets and recipe beside it as
//! `<file>.studio-site.json` when any is set; `web.open` of that file reads them back. The history is in-process
//! only (the drive's own limit: undo lives in one process).
//!
//! Export ([STU-WEB-120], a recipe, never a private writer elsewhere): the document (minified when asked: comments
//! other than conditional ones and whitespace-only text at the document skeleton level removed, both through spans,
//! never re-serialised) plus every local asset the engine's reference gate grants (`collect_assets`), into an empty
//! `output_root`, with `studio-export-manifest.json` (paths, sizes and SHA-256, sorted; no time, no enumeration
//! order). A missing or denied local reference fails closed before anything is written ([STU-ASSET-009]);
//! `http(s):` and `//` references are listed as external and not copied.
//!
//! Publish ([STU-WEB-127]): `local_or_network` only (every other transport refuses `transport_unsupported`): the
//! exported files, re-hashed against the manifest, copied to the target's `root_path`; the receipt names the site,
//! target, transport, each file with its outcome and transfer mode ([STU-WEB-117]; an unlisted extension is binary and
//! reported once), and the elapsed time, never a credential; a failure keeps the files transferred before it in the
//! receipt. The receipt is the op record's detail and `studio-publish-receipt.json` in the export root.
//!
//! Folder links (`/`, `docs/`, and a link naming an existing folder) and links to the document itself are listed,
//! never copied or refused; a granted file must resolve inside the root (no junction or symlink escape); every output
//! name is checked case-folded before the first write; publish refuses a target inside or around the export and the
//! site's own folder.
//!
//! Page links ([STU-WEB-163], lane SMB-WEB-SITE-1): a navigation reference (`a`, `area`, `form`, refresh, `iframe`;
//! `Load::Document` only) to a file that does not exist is a `link_broken` warning row `{document, line, reference}`
//! (the line of the source, also when minified) in the manifest's `link_broken` and the result's, never a refusal;
//! the list is absent when empty, so an export without broken links is byte-identical to before. Every other missing
//! local reference still fails closed (`asset_missing`, [STU-ASSET-009]).
//!
//! The site (module `web_site`, its manual): `web.export_recipe` `documents` is `open` (default: the open document,
//! as above) or `site` (every document of the site's walk from its source, the files they and their style sheets
//! reach, a `documents` list in the manifest); `web.site_report` reports broken links, orphaned files, external links,
//! missing alternative text and untitled documents; `web.move_file` moves one file and rewrites every reference to it
//! as one receipted transaction. `web.site_report`, the site export, `web.move_file` and `web.publish` hold the
//! site-level lock `<local_root>/.studio-site.lock` (contention refuses `site_locked` naming the holder, never waits);
//! `web.export`, `web.publish`, `web.site_report` and `web.move_file` are admitted as jobs (`overloaded`, `canceled`).
//!
//! Declared limits: the open export's document set is the open document and its folder must be the site's
//! `local_root`; no source maps, no preprocessors, assets keep their paths (`asset_rewriting: keep`); site and recipe
//! are not Folio records (no web token exists among the session's document entries), so the web history is the
//! host's own, not the run's (`history.undo` refuses `newest_step_is_web` while a web step is newest); no business
//! event; the web site is not a session document, so its jobs are resource-less (the lifecycle's own admission, no
//! host port); no controls registry ids.
#![deny(clippy::arithmetic_side_effects)]
use super::{Run, actor_json};
use crate::action::{self, CommandEntry};
use hsk_studio_accord::{CancellationToken, CausationId, CommandContext, CommandId};
use hsk_studio_session::Action;
use hsk_studio_web::responsive::document_media_blocks;
use hsk_studio_web::{
    AssetDecision, AssetOutcome, AssetPort, AssetSource, DenyReason, Limits, Load, Loads, NodeId, NodeKey, NodeKind, SourcePatch, TextEdit, WebDoc, WebError, apply, collect_assets, insert_element, insert_fragment,
    parse, preview_tree, remove_attribute, remove_node, set_attribute, set_text, sha256_hex,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Longest refusal target, key, name or id text.
const MAX_TEXT: usize = 256;
/// Longest path parameter.
const MAX_PATH: usize = 4096;
/// History bounds: the oldest steps go first past either.
const MAX_HISTORY_STEPS: usize = 256;
const MAX_HISTORY_BYTES: usize = 64 << 20;
/// `web.read` rows and `web.preview` nodes in one reply.
const MAX_ROWS: usize = 2000;
/// Text of one read row.
const MAX_ROW_TEXT: usize = 512;
/// Export bounds: files, bytes of one asset, bytes of the whole tree.
const MAX_EXPORT_FILES: usize = 4096;
const MAX_ASSET_BYTES: u64 = 256 << 20;
const MAX_EXPORT_BYTES: u64 = 1 << 30;
/// Site sidecar and manifest files read back.
const MAX_RECORD_BYTES: u64 = 1 << 20;
const MAX_TARGETS: usize = 64;
const MAX_SEGMENTS: usize = 32;
pub const MANIFEST_FILE: &str = "studio-export-manifest.json";
pub const RECEIPT_FILE: &str = "studio-publish-receipt.json";
const SIDECAR_SUFFIX: &str = ".studio-site.json";
const SECRET_KEYS: [&str; 6] = ["password", "token", "private_key", "secret", "passphrase", "credentials"];
/// [STU-WEB-115], closed.
pub const TRANSPORTS: [&str; 6] = ["local_or_network", "ftp", "ftps", "sftp", "webdav", "version_control"];
/// [STU-WEB-117] text-mode extensions; the binary list follows.
/// The id prefix the export's port gives a page link to no file (a `link_broken` warning, module docs).
const MISSING_PREFIX: &str = "missing:";
/// The site: its walk, graph, report, document-set export, move, lock and jobs (lane SMB-WEB-SITE-1).
#[path = "web_site.rs"]
mod web_site;
pub use web_site::LOCK_FILE;
const TEXT_EXTENSIONS: [&str; 29] = [
    "as", "ascx", "asmx", "asp", "aspx", "cfm", "cfml", "cgi", "cs", "css", "dwt", "htm", "html", "inc", "js", "lbi", "mxml", "php", "php3", "php4", "php5", "pl", "shtm", "shtml", "text", "txt", "vb", "xhtm", "xhtml",
];
const BINARY_EXTENSIONS: [&str; 39] = [
    "aif", "aiff", "aifc", "bin", "bmp", "dcr", "dir", "dmg", "doc", "dxr", "exe", "fla", "gif", "jpg", "jpeg", "mno", "mov", "mpeg", "mpg", "pdf", "pic", "pict", "png", "psd", "qt", "ra", "ram", "readme", "rm", "rtf",
    "sea", "sit", "snd", "swf", "tif", "tiff", "tpl", "wav", "zip",
];

/// The web row of `command_id`.
pub fn row(command_id: &str) -> Option<&'static CommandEntry> {
    if command_id.len() > MAX_TEXT {
        return None;
    }
    action::RUN_COMMANDS.iter().find(|c| c.run_method == "web_command" && c.command_id == command_id)
}

/// A typed refusal: a stable code, a bounded target and, for publish and verify, what happened before it.
pub(super) struct No(&'static str, String, Option<Value>);
type R<T> = Result<T, No>;

fn no<T>(code: &'static str, target: &str) -> R<T> {
    Err(No(code, bounded(target), None))
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

fn web_no(e: &WebError) -> No {
    No(e.code(), e.span().map_or_else(String::new, |s| format!("{}..{}", s.start, s.end)), None)
}

/// A site record ([STU-WEB-113] subset: no testing target, server model, preprocessor, framework, linter, browser
/// baseline or version-control binding).
#[derive(Clone, Debug, PartialEq)]
struct Site {
    site_id: String,
    display_name: String,
    local_root: String,
    default_images_folder: Option<String>,
    link_base: &'static str,
    cache_enabled: bool,
    targets: Vec<Target>,
}

/// A remote target ([STU-WEB-114] subset: no passive mode, IPv6, encryption, proxy, timeouts or check-in/out).
#[derive(Clone, Debug, PartialEq)]
struct Target {
    target_id: String,
    display_name: String,
    transport: &'static str,
    host: Option<String>,
    port: Option<u16>,
    root_path: String,
    credentials_ref: Option<String>,
}

/// The export recipe ([STU-WEB-120]).
#[derive(Clone, Debug, PartialEq)]
struct Recipe {
    output_root: String,
    file: String,
    minify: bool,
    link_base: &'static str,
    /// `open` (the open document) or `site` (every document of the site).
    documents: &'static str,
}

enum Change {
    /// The edits that made the step (against the source before it) and their inverse (against the source after).
    Source { forward: Vec<TextEdit>, inverse: Vec<TextEdit> },
    Site { before: Option<Site>, after: Option<Site> },
    Recipe { before: Option<Recipe>, after: Option<Recipe> },
}

struct Step {
    label: &'static str,
    change: Change,
    ctx: CommandContext,
    revision: u64,
}

impl Step {
    fn bytes(&self) -> usize {
        match &self.change {
            Change::Source { forward, inverse } => forward.iter().chain(inverse).map(|e| e.replacement.len()).fold(0usize, usize::saturating_add),
            Change::Site { .. } | Change::Recipe { .. } => 1024,
        }
    }

    fn attribution(&self) -> Value {
        json!({
            "label": self.label, "actor": actor_json(self.ctx.actor()), "client_session": self.ctx.client_session().as_str(),
            "command_id": self.ctx.command().map(CommandId::as_str), "base_revision": self.ctx.base_revision(),
            "revision": self.revision, "correlation": self.ctx.correlation().value(), "causation": self.ctx.causation().map(CausationId::value),
        })
    }
}

/// One web document, its site, recipe and history (module docs).
#[derive(Default)]
pub struct WebHost {
    doc: Option<WebDoc>,
    path: Option<PathBuf>,
    saved_sha256: Option<String>,
    site: Option<Site>,
    recipe: Option<Recipe>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    trimmed: u64,
    published_root: Option<String>,
    /// Site-wide work is admitted as jobs (module `web_site`).
    jobs: web_site::Jobs,
}

impl WebHost {
    /// The source revision (0 before an open).
    pub fn revision(&self) -> u64 {
        self.doc.as_ref().map_or(0, WebDoc::revision)
    }

    /// The correlation of the newest step of the undo (`undo`) or redo history: `history.undo` and `history.redo`
    /// refuse while a web step is newer than the run's newest step.
    pub fn newest_correlation(&self, undo: bool) -> Option<u64> {
        let stack = if undo { &self.undo } else { &self.redo };
        stack.last().map(|s| s.ctx.correlation().value())
    }

    /// The source text of the open document.
    pub fn source(&self) -> Option<&str> {
        self.doc.as_ref().map(WebDoc::source)
    }

    /// The host's state as JSON: document, history with attribution, site, recipe.
    pub fn state_json(&self) -> Value {
        json!({
            "open": self.doc.is_some(),
            "revision": self.revision(),
            "sha256": self.doc.as_ref().map(WebDoc::sha256_hex),
            "bytes": self.doc.as_ref().map(|d| d.source().len()),
            "nodes": self.doc.as_ref().map(WebDoc::len),
            "loss": self.doc.as_ref().map(|d| d.loss().entries().iter().map(|e| e.kind.code()).collect::<Vec<_>>()),
            "path": self.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "dirty": self.doc.as_ref().map(|d| self.saved_sha256.as_deref() != Some(d.sha256_hex())),
            "undo": self.undo.iter().map(Step::attribution).collect::<Vec<_>>(),
            "redo": self.redo.iter().map(|s| s.label).collect::<Vec<_>>(),
            "history_trimmed": self.trimmed,
            "site": self.site.as_ref().map(site_json),
            "recipe": self.recipe.as_ref().map(recipe_json),
            "published_root": self.published_root,
        })
    }

    fn doc(&self) -> R<&WebDoc> {
        self.doc.as_ref().ok_or(No("no_web_document", String::new(), None))
    }

    fn execute(&mut self, row: &'static CommandEntry, params: &Value, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        cancel.check().map_err(|_| No("canceled", String::new(), None))?;
        let Value::Object(map) = params else {
            return no("parameter_invalid", "parameters");
        };
        // A secret is refused by name before the parameter check, so the field is named and the value never is.
        if let Some(key) = map.keys().find(|k| SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str())) {
            return no("credential_in_site_record", key);
        }
        if let Some(key) = map.keys().find(|k| !row.parameters.iter().any(|p| p.key == k.as_str())) {
            return no("parameter_unknown", key);
        }
        let p = P(map);
        let label = row.display_name;
        match row.command_id {
            "web.open" => self.open(&p, cancel),
            "web.read" => self.read(&p),
            "web.insert_element" => {
                let doc = self.doc()?;
                let parent = key(doc, p.text("parent")?)?;
                let index = index(doc, &parent, &p)?;
                let tag = p.text("tag")?;
                let attrs = p.attrs()?;
                let pairs: Vec<(&str, &str)> = attrs.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
                let patch = insert_element(doc, &parent, index, tag, &pairs, p.opt_text("text")?, cancel).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.insert_fragment" => {
                let doc = self.doc()?;
                let parent = key(doc, p.text("parent")?)?;
                let index = index(doc, &parent, &p)?;
                let patch = insert_fragment(doc, &parent, index, p.text("fragment")?, cancel).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.set_text" => {
                let doc = self.doc()?;
                let patch = set_text(doc, &key(doc, p.text("key")?)?, p.text("text")?).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.set_attribute" => {
                let doc = self.doc()?;
                let patch = set_attribute(doc, &key(doc, p.text("key")?)?, p.text("name")?, p.text("value")?).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.remove_attribute" => {
                let doc = self.doc()?;
                let patch = remove_attribute(doc, &key(doc, p.text("key")?)?, p.text("name")?).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.remove_node" => {
                let doc = self.doc()?;
                let patch = remove_node(doc, &key(doc, p.text("key")?)?).map_err(|e| web_no(&e))?;
                self.edit(label, patch, ctx, cancel)
            }
            "web.undo" => self.step_back(true, cancel),
            "web.redo" => self.step_back(false, cancel),
            "web.save" => self.save(&p),
            "web.site" => {
                let site = site_from(self.site.as_ref(), &p)?;
                self.record(label, Change::Site { before: self.site.clone(), after: Some(site) }, ctx)
            }
            "web.remote_target" => {
                let Some(current) = self.site.as_ref() else {
                    return no("no_site", "web.site first");
                };
                let site = target_into(current, &p)?;
                self.record(label, Change::Site { before: self.site.clone(), after: Some(site) }, ctx)
            }
            "web.export_recipe" => {
                let recipe = recipe_from(self.recipe.as_ref(), &p)?;
                self.record(label, Change::Recipe { before: self.recipe.clone(), after: Some(recipe) }, ctx)
            }
            "web.export" => self.admitted(Action::Export, ctx, cancel, |host| host.export(ctx, cancel)),
            "web.publish" => self.admitted(Action::Export, ctx, cancel, |host| host.publish(&p, ctx)),
            "web.site_report" => self.admitted(Action::Export, ctx, cancel, |host| host.site_report(ctx, cancel)),
            "web.move_file" => self.admitted(Action::Edit, ctx, cancel, |host| host.move_file(&p, ctx, cancel)),
            "web.verify" => self.verify(&p, cancel),
            "web.preview" => {
                let doc = self.doc()?;
                let width = p.number("viewport_width_px")?;
                let tree = preview_tree(doc, width, cancel).map_err(|e| web_no(&e))?;
                Ok(json!({
                    "source_revision": tree.source_revision, "viewport_width_px": tree.viewport_width_px,
                    "nodes": tree.nodes.iter().take(MAX_ROWS).map(|n| json!({
                        "key": key_text(doc, n.id), "depth": n.depth, "tag": n.tag, "role": n.role, "name": n.name.as_deref().map(row_text),
                        "heading_level": n.heading_level, "rendered": n.rendered, "hidden": n.hidden.map(|h| format!("{h:?}")), "a11y_hidden": n.a11y_hidden,
                    })).collect::<Vec<_>>(),
                    "nodes_total": tree.nodes.len(),
                    "tab_order": tree.tab_order.iter().take(MAX_ROWS).map(|&id| key_text(doc, id)).collect::<Vec<_>>(),
                    "active_rules": tree.active_rules.iter().take(MAX_ROWS).map(|r| json!({"selector": row_text(&r.selector), "specificity": r.specificity, "declarations": r.declarations.len(), "media": r.media.len(), "matched_count": r.matched_count})).collect::<Vec<_>>(),
                    "unsupported_rules": tree.unsupported.len(),
                    "unnamed": tree.unnamed.iter().take(MAX_ROWS).map(|&id| key_text(doc, id)).collect::<Vec<_>>(),
                }))
            }
            "web.media" => {
                let doc = self.doc()?;
                let blocks = document_media_blocks(doc, cancel).map_err(|e| web_no(&e))?;
                Ok(json!({"blocks": blocks.iter().take(MAX_ROWS).map(|b| json!({
                    "query": row_text(doc.source().get(b.query_span.clone()).unwrap_or("")), "depth": b.depth,
                    "breakpoints": b.breakpoints.as_ref().ok().map(|list| list.iter().map(|k| json!({"min_width_px": k.min_width_px, "max_width_px": k.max_width_px})).collect::<Vec<_>>()),
                    "error": b.breakpoints.as_ref().err().map(WebError::code),
                })).collect::<Vec<_>>()}))
            }
            "web.assets" => {
                let doc = self.doc()?;
                let port = self.port()?;
                let refs = collect_assets(doc, &port, cancel).map_err(|e| web_no(&e))?;
                Ok(json!({"assets": refs.iter().take(MAX_ROWS).map(|r| json!({
                    "key": key_text(doc, r.node), "source": source_token(&r.source), "reference": row_text(&r.reference), "outcome": outcome_json(&r.outcome), "data_depth": r.data_depth,
                })).collect::<Vec<_>>(), "total": refs.len()}))
            }
            _ => no("unknown_command", row.command_id),
        }
    }

    fn open(&mut self, p: &P<'_>, cancel: &CancellationToken) -> R<Value> {
        if let Some(doctype) = p.opt_text("doctype")?
            && doctype != "html_5"
        {
            return no("doctype_unsupported", doctype);
        }
        let limits = Limits::default();
        let (source, path) = match (p.opt_text("path")?, p.opt_text("source")?) {
            (Some(path), None) => {
                let path = PathBuf::from(checked_path(path)?);
                let bytes = read_bounded(&path, u64::try_from(limits.max_bytes).unwrap_or(u64::MAX))?;
                (String::from_utf8(bytes).map_err(|_| No("source_not_utf8", bounded(&path.to_string_lossy()), None))?, Some(path))
            }
            (None, Some(source)) => (source.to_string(), None),
            (Some(_), Some(_)) => return no("parameter_invalid", "path and source"),
            (None, None) => return no("parameter_missing", "path"),
        };
        let doc = parse(&source, 1, &limits, cancel).map_err(|e| web_no(&e))?;
        // The site record saved beside the file comes back with it; an invalid one refuses the open.
        let (site, recipe) = match &path {
            Some(path) => read_sidecar(&sidecar(path))?,
            None => (None, None),
        };
        self.saved_sha256 = path.as_ref().map(|_| doc.sha256_hex().to_string());
        self.doc = Some(doc);
        self.path = path;
        self.site = site;
        self.recipe = recipe;
        self.undo.clear();
        self.redo.clear();
        self.published_root = None;
        Ok(json!({"web": self.state_json()}))
    }

    fn read(&self, p: &P<'_>) -> R<Value> {
        let doc = self.doc()?;
        let rows = doc.view();
        let (from, depth) = match p.opt_text("key")? {
            Some(k) => {
                let id = doc.resolve(&key(doc, k)?).ok_or(No("no_such_node", bounded(k), None))?;
                let at = rows.iter().position(|r| r.id == id).ok_or(No("no_such_node", bounded(k), None))?;
                (at, Some(rows[at].depth))
            }
            None => (0, None),
        };
        let subtree = rows.iter().skip(from).enumerate().take_while(|(i, r)| *i == 0 || depth.is_none_or(|d| r.depth > d)).map(|(_, r)| r);
        let mut listed = Vec::new();
        let mut total = 0usize;
        for r in subtree {
            total = total.saturating_add(1);
            if listed.len() < MAX_ROWS {
                listed.push(json!({
                    "key": key_text(doc, r.id), "depth": r.depth, "kind": kind_token(r.kind), "tag": r.tag,
                    "attrs": r.attrs.iter().map(|(n, v)| json!([n, v.as_deref().map(row_text)])).collect::<Vec<_>>(),
                    "text": r.text.as_deref().map(row_text), "span": [r.span.start, r.span.end],
                }));
            }
        }
        let mut out = json!({"web": self.state_json(), "rows": listed, "rows_total": total});
        if p.flag("include_source")? {
            out["source"] = json!(doc.source());
        }
        Ok(out)
    }

    /// Applies one source patch as one history step.
    fn edit(&mut self, label: &'static str, patch: SourcePatch, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        if patch.is_noop() {
            return no("nothing_to_change", label);
        }
        let doc = self.doc()?;
        let (next, forward, inverse) = patched(doc, &patch, cancel)?;
        let revision = next.revision();
        self.doc = Some(next);
        self.push(Step { label, change: Change::Source { forward, inverse }, ctx: ctx.clone(), revision });
        Ok(json!({"web": self.state_json()}))
    }

    fn record(&mut self, label: &'static str, change: Change, ctx: &CommandContext) -> R<Value> {
        let same = match &change {
            Change::Site { before, after } => before == after,
            Change::Recipe { before, after } => before == after,
            Change::Source { .. } => false,
        };
        if same {
            return no("nothing_to_change", label);
        }
        self.swap(&change);
        let revision = self.revision();
        self.push(Step { label, change, ctx: ctx.clone(), revision });
        Ok(json!({"web": self.state_json()}))
    }

    /// Sets the records a site or recipe change names to their `after` values.
    fn swap(&mut self, change: &Change) {
        match change {
            Change::Site { after, .. } => self.site.clone_from(after),
            Change::Recipe { after, .. } => self.recipe.clone_from(after),
            Change::Source { .. } => {}
        }
    }

    fn push(&mut self, step: Step) {
        self.redo.clear();
        self.undo.push(step);
        let mut total = self.undo.iter().map(Step::bytes).fold(0usize, usize::saturating_add);
        while self.undo.len() > MAX_HISTORY_STEPS || (total > MAX_HISTORY_BYTES && self.undo.len() > 1) {
            let dropped = self.undo.remove(0);
            total = total.saturating_sub(dropped.bytes());
            self.trimmed = self.trimmed.saturating_add(1);
        }
    }

    /// Undo (`back`) or redo: the newest step of one stack, applied in its direction and moved to the other.
    fn step_back(&mut self, back: bool, cancel: &CancellationToken) -> R<Value> {
        let stack = if back { &self.undo } else { &self.redo };
        let Some(step) = stack.last() else {
            return no(if back { "nothing_to_undo" } else { "nothing_to_redo" }, "");
        };
        let next = match &step.change {
            Change::Source { forward, inverse } => {
                let doc = self.doc()?;
                let edits = if back { inverse.clone() } else { forward.clone() };
                Some(patched(doc, &SourcePatch::new(doc, edits), cancel)?.0)
            }
            Change::Site { .. } | Change::Recipe { .. } => None,
        };
        let Some(step) = (if back { self.undo.pop() } else { self.redo.pop() }) else {
            return no("history_changed", "");
        };
        // A step never changes shape: undo applies its inverse or restores `before`, redo its forward edits or
        // `after`, so it moves between the stacks as it is and keeps its own attribution.
        match (&step.change, next) {
            (Change::Source { .. }, Some(next)) => self.doc = Some(next),
            (Change::Site { before, after }, _) => self.site = if back { before.clone() } else { after.clone() },
            (Change::Recipe { before, after }, _) => self.recipe = if back { before.clone() } else { after.clone() },
            (Change::Source { .. }, None) => {}
        }
        let label = step.label;
        if back {
            self.redo.push(step);
        } else {
            self.undo.push(step);
        }
        Ok(json!({"step": label, "web": self.state_json()}))
    }

    fn save(&mut self, p: &P<'_>) -> R<Value> {
        let doc = self.doc()?;
        let path = match p.opt_text("path")? {
            Some(path) => PathBuf::from(checked_path(path)?),
            None => self.path.clone().ok_or(No("parameter_missing", "path".into(), None))?,
        };
        if path.exists() && !p.flag("overwrite")? {
            return no("overwrite_required", &path.to_string_lossy());
        }
        let source = doc.source().as_bytes().to_vec();
        let sha = doc.sha256_hex().to_string();
        // A record that existed is rewritten even when site and recipe are now unset (an undone site must not come
        // back on reopen); a null record reads back as none.
        let beside = sidecar(&path);
        let record = (self.site.is_some() || self.recipe.is_some() || std::fs::symlink_metadata(&beside).is_ok()).then(|| sidecar_json(self.site.as_ref(), self.recipe.as_ref()));
        if let Some(record) = &record {
            write_atomic(&beside, &serde_json::to_vec_pretty(record).map_err(|_| No("save_failed", String::new(), None))?)?;
        }
        write_atomic(&path, &source)?;
        self.path = Some(path.clone());
        self.saved_sha256 = Some(sha);
        Ok(json!({"saved": path.to_string_lossy(), "site_record": self.site.is_some() || self.recipe.is_some(), "web": self.state_json()}))
    }

    /// The reference gate's port over the site root (module docs: the document's folder is the root).
    fn port(&self) -> R<Port> {
        let doc_dir = self.path.as_ref().map(|p| p.parent().filter(|d| !d.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf));
        let root = match (&self.site, doc_dir) {
            (Some(site), Some(dir)) => {
                let root = PathBuf::from(&site.local_root);
                let same = match (std::fs::canonicalize(&root), std::fs::canonicalize(&dir)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                };
                if !same {
                    return no("document_outside_site_root", &site.local_root);
                }
                Some(root)
            }
            (Some(site), None) => Some(PathBuf::from(&site.local_root)),
            (None, dir) => dir,
        };
        let document = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        let canonical = root.as_ref().and_then(|r| std::fs::canonicalize(r).ok());
        Ok(Port { root, canonical, document, pages: false })
    }

    fn export(&self, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        if let Some(recipe) = self.recipe.as_ref().filter(|r| r.documents == "site") {
            return self.export_site(recipe, ctx, cancel);
        }
        let doc = self.doc()?;
        let recipe = self.recipe.clone().ok_or(No("no_export_recipe", String::new(), None))?;
        let out_doc = if recipe.minify { minified(doc, cancel)? } else { doc.clone() };
        // The port marks a page link to no file, so it is a warning, not a refusal (module docs).
        let port = Port { pages: true, ..self.port()? };
        let refs = collect_assets(&out_doc, &port, cancel).map_err(|e| web_no(&e))?;
        let (mut assets, mut external, mut links) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let mut broken = 0usize;
        for r in &refs {
            match &r.outcome {
                // A link to a folder (`/`, `docs/`) is navigation only: listed, never copied.
                AssetOutcome::Granted { id } if id.starts_with(LINK_PREFIX) => {
                    links.insert(bounded(id.get(LINK_PREFIX.len()..).unwrap_or("")));
                }
                AssetOutcome::Granted { id } if id.starts_with(MISSING_PREFIX) => broken = broken.saturating_add(1),
                // The document itself (`<a href="index.html">`) is written from the exported source.
                AssetOutcome::Granted { id } if port.document.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(id)) => {}
                AssetOutcome::Granted { id } => {
                    assets.insert(id.clone());
                }
                AssetOutcome::Unavailable => return no("asset_missing", &r.reference),
                AssetOutcome::Denied(DenyReason::ExternalScheme | DenyReason::ProtocolRelative) => {
                    external.insert(bounded(&r.reference));
                }
                AssetOutcome::Denied(reason) => return no("asset_denied", &format!("{}: {}", reason.code(), r.reference)),
                AssetOutcome::Inline | AssetOutcome::SameDocument => {}
            }
        }
        output_names(assets.iter().chain([&recipe.file]))?;
        if assets.len() >= MAX_EXPORT_FILES {
            return no("export_too_many_files", &assets.len().to_string());
        }
        // Every byte is read and bounded before anything is written.
        let root = port.root.clone().unwrap_or_else(|| PathBuf::from("."));
        let mut files: Vec<(String, Vec<u8>, &'static str)> = vec![(recipe.file.clone(), out_doc.source().as_bytes().to_vec(), "document")];
        let mut total = u64::try_from(out_doc.source().len()).unwrap_or(u64::MAX);
        for id in &assets {
            let bytes = read_bounded(&join(&root, id), MAX_ASSET_BYTES)?;
            total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if total > MAX_EXPORT_BYTES {
                return no("export_too_large", id);
            }
            files.push((id.clone(), bytes, "asset"));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let out = PathBuf::from(checked_path(&recipe.output_root)?);
        if out.is_file() {
            return no("output_not_a_folder", &recipe.output_root);
        }
        if std::fs::read_dir(&out).is_ok_and(|mut d| d.next().is_some()) {
            return no("output_not_empty", &recipe.output_root);
        }
        // The lines are the referring document's, not the minified output's: minifying removes no reference, so the
        // marked references of both come in the same order.
        let link_broken: Vec<Value> = if broken == 0 {
            Vec::new()
        } else {
            let source_refs;
            let list = if recipe.minify {
                source_refs = collect_assets(doc, &port, cancel).map_err(|e| web_no(&e))?;
                &source_refs
            } else {
                &refs
            };
            let name = port.document.clone().unwrap_or_else(|| recipe.file.clone());
            let mut rows: Vec<(usize, String)> = list
                .iter()
                .filter(|r| matches!(&r.outcome, AssetOutcome::Granted { id } if id.starts_with(MISSING_PREFIX)))
                .map(|r| (line_at(doc.source(), r.span.start), row_text(&r.reference)))
                .collect();
            rows.sort();
            rows.dedup();
            rows.into_iter().map(|(line, reference)| json!({"document": name, "line": line, "reference": reference})).collect()
        };
        let listed: Vec<Value> = files.iter().map(|(path, bytes, role)| json!({"path": path, "role": role, "bytes": bytes.len(), "sha256": sha256_hex(bytes)})).collect();
        let mut manifest = json!({
            "schema": "hsk.studio.web.export_manifest@1", "document": recipe.file, "source_sha256": doc.sha256_hex(),
            "recipe": {"file": recipe.file, "minify": recipe.minify, "asset_rewriting": "keep", "link_base": recipe.link_base}, "files": listed, "external": external.iter().collect::<Vec<_>>(), "links": links.iter().collect::<Vec<_>>(),
        });
        if !link_broken.is_empty() {
            manifest["link_broken"] = json!(link_broken);
        }
        for (path, bytes, _) in &files {
            write_new(&join(&out, path), bytes)?;
        }
        write_new(&out.join(MANIFEST_FILE), &serde_json::to_vec_pretty(&manifest).map_err(|_| No("export_failed", String::new(), None))?)?;
        let mut result = json!({"output_root": recipe.output_root, "manifest": manifest});
        if !link_broken.is_empty() {
            result["link_broken"] = json!(link_broken);
        }
        Ok(result)
    }

    fn publish(&mut self, p: &P<'_>, ctx: &CommandContext) -> R<Value> {
        let started = std::time::Instant::now();
        let site = self.site.clone().ok_or(No("no_site", String::new(), None))?;
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
        let recipe = self.recipe.clone().ok_or(No("no_export_recipe", String::new(), None))?;
        let export_root = PathBuf::from(&recipe.output_root);
        let manifest = read_manifest(&export_root)?;
        let target_root = PathBuf::from(checked_path(&target.root_path)?);
        // Resolved through the nearest existing folder, so a target not created yet is checked too.
        let target_at = resolved(&target_root);
        let export_at = resolved(&export_root);
        if target_at.starts_with(&export_at) || export_at.starts_with(&target_at) {
            return no("publish_over_export", &target.root_path);
        }
        let doc_dir = self.path.as_ref().and_then(|p| p.parent()).map(|d| if d.as_os_str().is_empty() { Path::new(".") } else { d });
        let sources = [Some(Path::new(&site.local_root)), doc_dir];
        if sources.into_iter().flatten().any(|s| resolved(s) == target_at) {
            return no("publish_over_source", &target.root_path);
        }
        // The site-level lock, held until the receipt is kept ([STU-WEB-131]).
        let _lock = web_site::site_lock(&site.local_root, ctx)?;
        let mut done: Vec<Value> = Vec::new();
        let mut unmapped = BTreeSet::new();
        let mut failure = None;
        for file in &manifest {
            let mode = transfer_mode(&file.path);
            if mode.1 {
                unmapped.insert(extension(&file.path));
            }
            let result = read_bounded(&join(&export_root, &file.path), MAX_ASSET_BYTES).and_then(|bytes| {
                if sha256_hex(&bytes) != file.sha256 {
                    return Err(No("export_changed", bounded(&file.path), None));
                }
                write_replacing(&join(&target_root, &file.path), &bytes)
            });
            let outcome = if result.is_ok() { "transferred" } else { "failed" };
            done.push(json!({"path": file.path, "outcome": outcome, "transfer_mode": mode.0, "bytes": file.bytes, "sha256": file.sha256}));
            if let Err(e) = result {
                failure = Some(e);
                break;
            }
        }
        let receipt = json!({
            "schema": "hsk.studio.web.publish_receipt@1", "site_id": site.site_id, "target_id": target.target_id, "transport": target.transport,
            "files": done, "files_total": manifest.len(), "complete": failure.is_none(),
            "unmapped_extensions": unmapped.iter().collect::<Vec<_>>(), "algorithm_weakening": Value::Null,
            "elapsed_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        // The receipt is kept with the export whatever the outcome, so a model can resume rather than restart.
        let kept = serde_json::to_vec_pretty(&receipt).ok().is_some_and(|bytes| write_replacing(&export_root.join(RECEIPT_FILE), &bytes).is_ok());
        if let Some(No(code, at, _)) = failure {
            return Err(No(if code == "export_changed" { code } else { "publish_failed" }, at, Some(json!({"receipt": receipt}))));
        }
        self.published_root = Some(target.root_path.clone());
        Ok(json!({"receipt": receipt, "receipt_kept": kept}))
    }

    fn verify(&self, p: &P<'_>, cancel: &CancellationToken) -> R<Value> {
        let recipe = self.recipe.as_ref().ok_or(No("no_export_recipe", String::new(), None))?;
        let root = match p.opt_text("root")? {
            Some(root) => checked_path(root)?.to_string(),
            None => self.published_root.clone().ok_or(No("parameter_missing", "root".into(), None))?,
        };
        let manifest = read_manifest(Path::new(&recipe.output_root))?;
        let root = PathBuf::from(root);
        let (mut files, mut bad, mut parsed) = (Vec::new(), 0u64, 0u64);
        for file in &manifest {
            let outcome = match read_bounded(&join(&root, &file.path), MAX_ASSET_BYTES) {
                Err(_) => "missing",
                Ok(bytes) if sha256_hex(&bytes) != file.sha256 => "mismatch",
                Ok(bytes) if file.role == "document" => match std::str::from_utf8(&bytes).ok().map(|s| parse(s, 1, &Limits::default(), cancel)) {
                    Some(Ok(_)) => {
                        parsed = parsed.saturating_add(1);
                        "match"
                    }
                    _ => "parse_failed",
                },
                Ok(_) => "match",
            };
            if outcome != "match" {
                bad = bad.saturating_add(1);
            }
            files.push(json!({"path": file.path, "outcome": outcome}));
        }
        let detail = json!({"root": root.to_string_lossy(), "files": files, "mismatched": bad, "documents_parsed": parsed});
        if bad > 0 {
            return Err(No("verify_mismatch", bad.to_string(), Some(detail)));
        }
        Ok(detail)
    }
}

/// Applies `patch` to `doc` and parses the result: the new document, the patch's edits in source order and their
/// inverse (spans in the new source holding the old bytes).
fn patched(doc: &WebDoc, patch: &SourcePatch, cancel: &CancellationToken) -> R<(WebDoc, Vec<TextEdit>, Vec<TextEdit>)> {
    let applied = apply(doc, patch).map_err(|e| web_no(&e))?;
    let next = parse(&applied.source, applied.revision, doc.limits(), cancel).map_err(|e| web_no(&e))?;
    let mut forward = patch.edits.clone();
    forward.sort_by_key(|e| (e.span.start, e.span.end));
    let mut inverse = Vec::with_capacity(forward.len());
    for (edit, span) in forward.iter().zip(&applied.changed) {
        let old = doc.source().get(edit.span.clone()).ok_or(No("invalid_span", String::new(), None))?;
        inverse.push(TextEdit { span: span.clone(), replacement: old.to_string() });
    }
    Ok((next, forward, inverse))
}

/// The document with comments (other than conditional ones and those inside `pre`/`listing`, where the next text's
/// leading LF depends on them) and whitespace-only text under the document, `html` and `head` removed by span.
fn minified(doc: &WebDoc, cancel: &CancellationToken) -> R<WebDoc> {
    let src = doc.source();
    let mut drop: Vec<std::ops::Range<usize>> = Vec::new();
    for id in 0..doc.len() {
        let Some(node) = doc.node(id) else { continue };
        let parent_tag = node.parent.and_then(|p| doc.local_name(p));
        let parent_is_root = node.parent.and_then(|p| doc.node(p)).is_some_and(|n| n.kind == NodeKind::Document);
        let text = src.get(node.span.clone()).unwrap_or("");
        let remove = match node.kind {
            NodeKind::Comment => !text.starts_with("<!--[if") && !text.starts_with("<!--<![endif") && !matches!(parent_tag.as_deref(), Some("pre" | "listing")),
            NodeKind::Text => text.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)) && (parent_is_root || matches!(parent_tag.as_deref(), Some("html" | "head"))),
            _ => false,
        };
        if remove {
            drop.push(node.span.clone());
        }
    }
    if drop.is_empty() {
        return Ok(doc.clone());
    }
    drop.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(src.len());
    let mut cursor = 0usize;
    for span in &drop {
        if span.start < cursor {
            continue;
        }
        out.push_str(src.get(cursor..span.start).unwrap_or(""));
        cursor = span.end;
    }
    out.push_str(src.get(cursor..).unwrap_or(""));
    parse(&out, doc.revision(), doc.limits(), cancel).map_err(|e| web_no(&e))
}

/// The reference gate's port: a relative or root-relative path to a regular file under the site root is granted
/// with that path as its id; anything else is denied or unavailable (module docs).
struct Port {
    root: Option<PathBuf>,
    /// The root resolved through links: a granted file must resolve inside it.
    canonical: Option<PathBuf>,
    document: Option<String>,
    /// The export's port: a page link to no file is granted as `missing:` (a warning) and one naming a folder as a
    /// link; `web.assets` reports them unavailable as before.
    pages: bool,
}

/// The id prefix of a navigation link to a folder (granted as a link, never copied).
const LINK_PREFIX: &str = "link:";

impl AssetPort for Port {
    fn document_url(&self) -> Option<&str> {
        self.document.as_deref()
    }

    fn resolve(&self, reference: &str, loads: Loads) -> AssetDecision {
        let path = reference.split(['?', '#']).next().unwrap_or("");
        if loads == Loads::of(Load::Document) && (path.is_empty() || path == "." || path.ends_with('/')) {
            return AssetDecision::Granted { id: format!("{LINK_PREFIX}{reference}") };
        }
        let Some(id) = asset_id(reference) else {
            return AssetDecision::Denied;
        };
        let (Some(root), Some(canonical)) = (&self.root, &self.canonical) else {
            return AssetDecision::Unavailable;
        };
        let file = join(root, &id);
        match std::fs::symlink_metadata(&file) {
            // A junction or symlinked folder inside the root must not reach a file outside it.
            Ok(m) if m.is_file() => match std::fs::canonicalize(&file) {
                Ok(real) if real.starts_with(canonical) => AssetDecision::Granted { id },
                _ => AssetDecision::Denied,
            },
            Ok(m) if m.is_dir() && self.pages && loads == Loads::of(Load::Document) => AssetDecision::Granted { id: format!("{LINK_PREFIX}{reference}") },
            Err(_) if self.pages && loads == Loads::of(Load::Document) => AssetDecision::Granted { id: format!("{MISSING_PREFIX}{reference}") },
            _ => AssetDecision::Unavailable,
        }
    }
}

/// The site-root-relative path of a reference: query and fragment dropped, `.` and `..` resolved without leaving the
/// root, every segment a plain portable name. `None` refuses it.
fn asset_id(reference: &str) -> Option<String> {
    let path = reference.split(['?', '#']).next()?;
    if path.is_empty() || path.len() > 1024 {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        match *part {
            "." => {}
            ".." => {
                segments.pop()?;
            }
            "" if i.saturating_add(1) == parts.len() => return None,
            name if portable(name) => segments.push(name),
            _ => return None,
        }
    }
    (!segments.is_empty() && segments.len() <= MAX_SEGMENTS).then(|| segments.join("/"))
}

/// A file or folder name every target file system reads the same way.
fn portable(name: &str) -> bool {
    let reserved = ["con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9"];
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    !name.is_empty()
        && name.len() <= 255
        && !name.ends_with('.')
        && !name.starts_with('.')
        && !reserved.contains(&stem.as_str())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '@' | ',' | '(' | ')'))
}

/// `path` resolved through its nearest existing ancestor (links followed there), the rest appended, ASCII case folded:
/// two spellings of one place compare equal before the place exists.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut at = absolute.as_path();
    let base = loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            break real;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => break at.to_path_buf(),
        }
    };
    let full = rest.into_iter().rev().fold(base, |p, s| p.join(s));
    PathBuf::from(full.to_string_lossy().to_ascii_lowercase())
}

/// One case-folded set of every output name (and the manifest and receipt), checked before the first write: NTFS and
/// APFS fold case, and no name may also be a folder of another.
fn output_names<'a>(names: impl Iterator<Item = &'a String>) -> R<()> {
    let mut seen = BTreeSet::new();
    let mut folders = BTreeSet::new();
    for name in names.map(|n| n.to_ascii_lowercase()).chain([MANIFEST_FILE.to_string(), RECEIPT_FILE.to_string()]) {
        let mut at = 0usize;
        while let Some(slash) = name.get(at..).and_then(|rest| rest.find('/')) {
            at = at.saturating_add(slash);
            folders.insert(name.get(..at).unwrap_or("").to_string());
            at = at.saturating_add(1);
        }
        if !seen.insert(name.clone()) {
            return no("export_path_collision", &name);
        }
    }
    if let Some(name) = seen.intersection(&folders).next() {
        return no("export_path_collision", name);
    }
    Ok(())
}

/// The 1-based line of byte `at`.
fn line_at(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |t| t.bytes().filter(|b| *b == b'\n').count()).saturating_add(1)
}

fn join(root: &Path, id: &str) -> PathBuf {
    id.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

/// A path parameter: non-empty, bounded, no control character.
fn checked_path(path: &str) -> R<&str> {
    if path.is_empty() || path.len() > MAX_PATH || path.chars().any(char::is_control) {
        return no("path_invalid", path);
    }
    Ok(path)
}

/// Reads a regular file of at most `limit` bytes; never holds more than `limit + 1`.
fn read_bounded(path: &Path, limit: u64) -> R<Vec<u8>> {
    let at = || bounded(&path.to_string_lossy());
    let meta = std::fs::symlink_metadata(path).map_err(|_| No("file_missing", at(), None))?;
    if !meta.is_file() {
        return Err(No("not_a_file", at(), None));
    }
    if meta.len() > limit {
        return Err(No("file_too_large", at(), None));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(limit.saturating_add(1)).read_to_end(&mut bytes)).map_err(|_| No("read_failed", at(), None))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(No("file_too_large", at(), None));
    }
    Ok(bytes)
}

/// Writes through a temporary sibling and a rename, so a reader never sees half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> R<()> {
    let at = || bounded(&path.to_string_lossy());
    let name = path.file_name().ok_or(No("path_invalid", at(), None))?.to_string_lossy().into_owned();
    let temp = path.with_file_name(format!(".{name}.studio-tmp-{}", std::process::id()));
    std::fs::write(&temp, bytes).map_err(|_| No("write_failed", at(), None))?;
    std::fs::rename(&temp, path).map_err(|_| {
        let _ = std::fs::remove_file(&temp);
        No("write_failed", at(), None)
    })
}

fn write_new(path: &Path, bytes: &[u8]) -> R<()> {
    if path.exists() {
        return no("export_path_collision", &path.to_string_lossy());
    }
    write_replacing(path, bytes)
}

fn write_replacing(path: &Path, bytes: &[u8]) -> R<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|_| No("write_failed", bounded(&parent.to_string_lossy()), None))?;
    }
    write_atomic(path, bytes)
}

fn sidecar(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}{SIDECAR_SUFFIX}"))
}

struct ManifestFile {
    path: String,
    role: String,
    bytes: u64,
    sha256: String,
}

/// The export manifest, every path re-checked as a portable relative path.
fn read_manifest(root: &Path) -> R<Vec<ManifestFile>> {
    let bytes = read_bounded(&root.join(MANIFEST_FILE), MAX_RECORD_BYTES).map_err(|_| No("no_export", bounded(&root.to_string_lossy()), None))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| No("export_manifest_invalid", String::new(), None))?;
    let files = value["files"].as_array().ok_or(No("export_manifest_invalid", "files".into(), None))?;
    if files.len() > MAX_EXPORT_FILES {
        return no("export_manifest_invalid", "files");
    }
    files
        .iter()
        .map(|f| {
            let path = f["path"].as_str().filter(|p| asset_id(p).as_deref() == Some(*p)).ok_or(No("export_manifest_invalid", "path".into(), None))?;
            let sha256 = f["sha256"].as_str().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or(No("export_manifest_invalid", bounded(path), None))?;
            Ok(ManifestFile { path: path.to_string(), role: f["role"].as_str().unwrap_or("asset").to_string(), bytes: f["bytes"].as_u64().unwrap_or(0), sha256: sha256.to_string() })
        })
        .collect()
}

/// [STU-WEB-117]: the transfer mode of a path and whether its extension is unlisted (binary, reported once).
fn transfer_mode(path: &str) -> (&'static str, bool) {
    let ext = extension(path);
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        ("text", false)
    } else {
        ("binary", !BINARY_EXTENSIONS.contains(&ext.as_str()))
    }
}

fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) => ext.to_ascii_lowercase(),
        // [STU-WEB-117] lists `readme` as a name.
        None => name.to_ascii_lowercase(),
    }
}

/// Resolves a key parameter: a node path or `#id`.
fn key(doc: &WebDoc, text: &str) -> R<NodeKey> {
    if text.len() > MAX_TEXT {
        return no("key_invalid", text);
    }
    if let Some(id) = text.strip_prefix('#') {
        return doc.find_by_html_id(id).and_then(|n| doc.key_of(n)).ok_or(No("no_such_node", bounded(text), None));
    }
    if !text.starts_with('/') {
        return no("key_invalid", text);
    }
    let key = NodeKey::new(text);
    doc.resolve(&key).map(|_| key).ok_or(No("no_such_node", bounded(text), None))
}

fn key_text(doc: &WebDoc, id: NodeId) -> Option<String> {
    doc.key_of(id).map(|k| k.as_str().to_string())
}

/// The `index` parameter, or the parent's child count (append).
fn index(doc: &WebDoc, parent: &NodeKey, p: &P<'_>) -> R<usize> {
    match p.0.get("index") {
        None => {
            let id = doc.resolve(parent).ok_or(No("no_such_node", bounded(parent.as_str()), None))?;
            Ok(doc.node(id).map_or(0, |n| n.children.len()))
        }
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|n| *n <= u32::MAX as usize).ok_or(No("parameter_invalid", "index".into(), None)),
    }
}

fn row_text(text: &str) -> String {
    text.chars().take(MAX_ROW_TEXT).collect()
}

fn kind_token(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Document => "document",
        NodeKind::Element => "element",
        NodeKind::Text => "text",
        NodeKind::Comment => "comment",
        NodeKind::Doctype => "doctype",
        NodeKind::Raw => "raw",
        NodeKind::Unsupported => "unsupported",
    }
}

fn source_token(source: &AssetSource) -> String {
    match source {
        AssetSource::Attribute(name) => format!("attribute:{}", bounded(name)),
        AssetSource::InlineStyle => "inline_style".into(),
        AssetSource::StyleBody => "style_body".into(),
        AssetSource::ScriptBody => "script_body".into(),
    }
}

fn outcome_json(outcome: &AssetOutcome) -> Value {
    match outcome {
        AssetOutcome::Granted { id } => json!({"outcome": "granted", "id": id}),
        AssetOutcome::Unavailable => json!({"outcome": "unavailable"}),
        AssetOutcome::Denied(reason) => json!({"outcome": "denied", "reason": reason.code()}),
        AssetOutcome::Inline => json!({"outcome": "inline"}),
        AssetOutcome::SameDocument => json!({"outcome": "same_document"}),
    }
}

/// Typed parameter reads over the row-checked object.
struct P<'a>(&'a Map<String, Value>);

impl P<'_> {
    fn opt_text(&self, key: &str) -> R<Option<&str>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => no("parameter_invalid", key),
        }
    }

    fn text(&self, key: &str) -> R<&str> {
        self.opt_text(key)?.ok_or(No("parameter_missing", key.to_string(), None))
    }

    fn opt_flag(&self, key: &str) -> R<Option<bool>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => no("parameter_invalid", key),
        }
    }

    fn flag(&self, key: &str) -> R<bool> {
        Ok(self.opt_flag(key)?.unwrap_or(false))
    }

    fn number(&self, key: &str) -> R<f64> {
        match self.0.get(key) {
            None | Some(Value::Null) => no("parameter_missing", key),
            Some(v) => v.as_f64().filter(|n| n.is_finite()).ok_or(No("parameter_invalid", key.to_string(), None)),
        }
    }

    /// `attrs`: a JSON object of string values, in key order.
    fn attrs(&self) -> R<Vec<(String, String)>> {
        match self.0.get("attrs") {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Object(map)) => map.iter().map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())).ok_or(No("parameter_invalid", bounded(k), None))).collect(),
            Some(_) => no("parameter_invalid", "attrs"),
        }
    }
}

/// An id: `[a-z0-9._-]{1,64}`.
fn checked_id<'a>(key: &str, id: &'a str) -> R<&'a str> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')) {
        return no("parameter_invalid", key);
    }
    Ok(id)
}

fn checked_name<'a>(key: &str, name: &'a str) -> R<&'a str> {
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return no("parameter_invalid", key);
    }
    Ok(name)
}

fn link_base(key: &str, value: &str) -> R<&'static str> {
    match value {
        "document" => Ok("document"),
        "site_root" => Ok("site_root"),
        _ => no("parameter_invalid", key),
    }
}

fn site_from(current: Option<&Site>, p: &P<'_>) -> R<Site> {
    let mut site = current.cloned().unwrap_or(Site { site_id: "site".into(), display_name: String::new(), local_root: String::new(), default_images_folder: None, link_base: "document", cache_enabled: false, targets: Vec::new() });
    if let Some(id) = p.opt_text("site_id")? {
        site.site_id = checked_id("site_id", id)?.to_string();
    }
    if let Some(name) = p.opt_text("display_name")? {
        site.display_name = checked_name("display_name", name)?.to_string();
    }
    if let Some(root) = p.opt_text("local_root")? {
        site.local_root = checked_path(root)?.to_string();
    }
    if let Some(folder) = p.opt_text("default_images_folder")? {
        site.default_images_folder = Some(asset_id(folder).filter(|f| f == folder).ok_or(No("parameter_invalid", "default_images_folder".into(), None))?);
    }
    if let Some(base) = p.opt_text("site_relative_link_base")? {
        site.link_base = link_base("site_relative_link_base", base)?;
    }
    if let Some(cache) = p.opt_flag("cache_enabled")? {
        site.cache_enabled = cache;
    }
    if site.display_name.is_empty() {
        return no("parameter_missing", "display_name");
    }
    if site.local_root.is_empty() {
        return no("parameter_missing", "local_root");
    }
    Ok(site)
}

fn target_into(current: &Site, p: &P<'_>) -> R<Site> {
    let mut site = current.clone();
    let id = checked_id("target_id", p.text("target_id")?)?.to_string();
    let at = site.targets.iter().position(|t| t.target_id == id);
    if p.flag("remove")? {
        let Some(at) = at else {
            return no("no_such_target", &id);
        };
        site.targets.remove(at);
        return Ok(site);
    }
    let mut t = at.and_then(|i| site.targets.get(i).cloned()).unwrap_or(Target { target_id: id.clone(), display_name: id.clone(), transport: "local_or_network", host: None, port: None, root_path: String::new(), credentials_ref: None });
    if let Some(name) = p.opt_text("display_name")? {
        t.display_name = checked_name("display_name", name)?.to_string();
    }
    if let Some(transport) = p.opt_text("transport")? {
        t.transport = TRANSPORTS.iter().copied().find(|m| *m == transport).ok_or(No("transport_unknown", bounded(transport), None))?;
    }
    if let Some(host) = p.opt_text("host")? {
        t.host = Some(checked_name("host", host)?.to_string());
    }
    if let Some(port) = p.0.get("port").filter(|v| !v.is_null()) {
        t.port = Some(port.as_u64().and_then(|n| u16::try_from(n).ok()).filter(|n| *n > 0).ok_or(No("parameter_invalid", "port".into(), None))?);
    }
    if let Some(root) = p.opt_text("root_path")? {
        t.root_path = checked_path(root)?.to_string();
    }
    if let Some(reference) = p.opt_text("credentials_ref")? {
        let ok = !reference.is_empty() && reference.len() <= 128 && reference.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'));
        if !ok {
            return no("parameter_invalid", "credentials_ref");
        }
        t.credentials_ref = Some(reference.to_string());
    }
    if t.root_path.is_empty() {
        return no("parameter_missing", "root_path");
    }
    if t.transport == "local_or_network" {
        if t.host.is_some() || t.port.is_some() || t.credentials_ref.is_some() {
            return no("parameter_not_applicable", "host, port and credentials_ref are for remote transports");
        }
    } else {
        if t.host.is_none() {
            return no("parameter_missing", "host");
        }
        if t.credentials_ref.is_none() {
            return no("parameter_missing", "credentials_ref");
        }
    }
    match at {
        Some(i) => site.targets[i] = t,
        None if site.targets.len() >= MAX_TARGETS => return no("too_many_targets", &id),
        None => site.targets.push(t),
    }
    Ok(site)
}

fn recipe_from(current: Option<&Recipe>, p: &P<'_>) -> R<Recipe> {
    if p.flag("emit_source_maps")? {
        return no("source_maps_unsupported", "emit_source_maps");
    }
    if p.flag("compile_preprocessors")? {
        return no("preprocessors_unsupported", "compile_preprocessors");
    }
    if let Some(policy) = p.opt_text("asset_rewriting")?
        && policy != "keep"
    {
        return no("asset_rewriting_unsupported", policy);
    }
    let mut recipe = current.cloned().unwrap_or(Recipe { output_root: String::new(), file: "index.html".into(), minify: false, link_base: "document", documents: "open" });
    if let Some(root) = p.opt_text("output_root")? {
        recipe.output_root = checked_path(root)?.to_string();
    }
    if let Some(file) = p.opt_text("file")? {
        let lower = file.to_ascii_lowercase();
        if !portable(file) || !(lower.ends_with(".html") || lower.ends_with(".htm")) || file == MANIFEST_FILE || file == RECEIPT_FILE {
            return no("parameter_invalid", "file");
        }
        recipe.file = file.to_string();
    }
    if let Some(minify) = p.opt_flag("minify")? {
        recipe.minify = minify;
    }
    if let Some(base) = p.opt_text("link_base")? {
        recipe.link_base = link_base("link_base", base)?;
    }
    if let Some(set) = p.opt_text("documents")? {
        recipe.documents = match set {
            "open" => "open",
            "site" => "site",
            _ => return no("parameter_invalid", "documents"),
        };
    }
    if recipe.output_root.is_empty() {
        return no("parameter_missing", "output_root");
    }
    Ok(recipe)
}

fn site_json(site: &Site) -> Value {
    json!({
        "site_id": site.site_id, "display_name": site.display_name, "local_root": site.local_root, "default_images_folder": site.default_images_folder,
        "site_relative_link_base": site.link_base, "cache_enabled": site.cache_enabled,
        "remote_targets": site.targets.iter().map(|t| json!({
            "target_id": t.target_id, "display_name": t.display_name, "transport": t.transport, "host": t.host, "port": t.port, "root_path": t.root_path, "credentials_ref": t.credentials_ref,
        })).collect::<Vec<_>>(),
    })
}

fn recipe_json(recipe: &Recipe) -> Value {
    json!({"output_root": recipe.output_root, "file": recipe.file, "documents": recipe.documents, "minify": recipe.minify, "emit_source_maps": false, "compile_preprocessors": false, "asset_rewriting": "keep", "link_base": recipe.link_base})
}

fn sidecar_json(site: Option<&Site>, recipe: Option<&Recipe>) -> Value {
    json!({"schema": "hsk.studio.web.site_record@1", "site": site.map(site_json), "recipe": recipe.map(recipe_json)})
}

/// The site and recipe saved beside a document, validated through the same rules as the commands.
fn read_sidecar(path: &Path) -> R<(Option<Site>, Option<Recipe>)> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok((None, None));
    }
    let invalid = |at: &str| No("site_record_invalid", bounded(at), None);
    let bytes = read_bounded(path, MAX_RECORD_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("json"))?;
    if value["schema"] != "hsk.studio.web.site_record@1" {
        return Err(invalid("schema"));
    }
    let site = match &value["site"] {
        Value::Null => None,
        Value::Object(fields) => {
            let mut plain = fields.clone();
            let targets = plain.remove("remote_targets").unwrap_or(Value::Array(Vec::new()));
            let mut site = site_from(None, &P(&plain)).map_err(|No(code, at, _)| invalid(&format!("{code} {at}")))?;
            for t in targets.as_array().ok_or_else(|| invalid("remote_targets"))? {
                let Value::Object(t) = t else { return Err(invalid("remote_targets")) };
                site = target_into(&site, &P(t)).map_err(|No(code, at, _)| invalid(&format!("{code} {at}")))?;
            }
            Some(site)
        }
        _ => return Err(invalid("site")),
    };
    let recipe = match &value["recipe"] {
        Value::Null => None,
        Value::Object(fields) => Some(recipe_from(None, &P(fields)).map_err(|No(code, at, _)| invalid(&format!("{code} {at}")))?),
        _ => return Err(invalid("recipe")),
    };
    Ok((site, recipe))
}

/// Runs one web command (`Run::web_command`).
pub(super) fn run(run: &mut Run<'_>, command_id: &str, params: &Value) -> bool {
    let before = run.frames.count();
    let Some(row) = row(command_id) else {
        let r = run.own_or_push("web_command", before, Err("unknown_command"));
        target(run, &bounded(command_id), None);
        return r;
    };
    let op = row.command_id;
    run.note_unrecordable(op);
    let base = run.web.revision();
    let ctx = run.begin_at(op, Some(base), None);
    // The host is used in place: a panic inside leaves it as it was up to that point, never emptied.
    let cancel = run.cancel;
    let result = run.web.execute(row, params, &ctx, cancel);
    match result {
        Ok(detail) => run.own_or_push(op, before, Ok(detail)),
        Err(No(code, at, detail)) => {
            let r = run.own_or_push(op, before, Err(code));
            target(run, &at, detail);
            r
        }
    }
}

fn target(run: &mut Run<'_>, at: &str, detail: Option<Value>) {
    if let Some(record) = run.ops.last_mut() {
        record["target"] = json!(at);
        if let Some(detail) = detail {
            record["detail"] = detail;
        }
    }
}
