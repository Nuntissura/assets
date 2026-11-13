//! Web rows of the command corpus (lane SMB-WEB-HOST-1: the first host of the web crates, [STU-WEB-003],
//! [STU-WEB-113] to [STU-WEB-120], [STU-WEB-127], [STU-WEB-131]), run by `Run::web_command`, module
//! `crate::harness::web_commands`. Lane SMB-WEB-SITE-1 ([STU-WEB-118], [STU-WEB-121]): `web.site_report` (no
//! parameters; the site's link graph as typed report rows), `web.move_file` (`from`, `to`: root-relative paths of one
//! file; every referring document rewritten in one receipted transaction) and the recipe's `documents` (`open` |
//! `site`). No spec command id is verified for the two new rows; they follow this file's `web.*` naming and are
//! recorded for the command contract. Typed refusals (manual: module `web_commands::web_site`): every site-wide row
//! `no_site`, `site_locked` (naming the holder), `overloaded`, `canceled`, `stale_revision`, `job_resource_invalid`,
//! `site_too_many_files`, `site_too_deep`, `site_too_large`, `site_too_many_references`; the site export also
//! `output_inside_site`, `entry_document_missing`, `document_unreadable`, `asset_missing`, `asset_denied`;
//! `web.move_file` also `move_outside_site`, `path_invalid`, `move_target_exists`, `move_case_only`,
//! `move_source_missing`, `move_reserved_file`, `move_site_record`, `move_kind_changes`, `move_open_package`,
//! `document_unsaved`, `document_changed_on_disk`, `document_unreadable`, `reference_not_rewritable`,
//! `move_rewrite_unverified`, `move_failed` (with the receipt). Both exports also `export_manifest_too_large` (a
//! manifest over 1 MiB) and cap `link_broken` at 2000 rows (`link_broken_truncated`); `document_unreadable` names the
//! unparsed file and its reason (`detail.file`, `detail.reason`); a file needing more than the engine's 1024 edits
//! (`Limits::max_edits`) refuses a move with the engine's code. Parameter and state refusals come before job
//! admission, never `overloaded`. `web.save` of a file inside `local_root` also refuses `site_locked`.
//! Lane SMB-WEB-SITE-2 ([STU-WEB-090] to [STU-WEB-093], [STU-WEB-096] to [STU-WEB-099], [STU-WEB-113],
//! [STU-WEB-118]): `web.change_link` (change-link-site-wide), `web.template_regions`, `web.template_instance`,
//! `web.instance_param`, `web.instance_repeat`, `web.template_update`, `web.library_item_insert`,
//! `web.library_item_update`, `web.snippet`, `web.snippets`, `web.snippet_insert`, and the optional site-record
//! fields of `web.site`. No spec command id is verified for these rows either; they follow the `web.*` naming. Their
//! parameters, outcomes and refusals: the module manual of `web_commands::web_site`.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

// Every row's run method is `web_command`; which parameters are required is the module's rule (its docs).
const KEY: ParamSpec = p("key", ValueType::Reference);
const PARENT: ParamSpec = p("parent", ValueType::Reference);
const INDEX: ParamSpec = p("index", ValueType::Number).opt();
const NAME: ParamSpec = p("name", ValueType::String);
const PATH: ParamSpec = p("path", ValueType::String).opt();
const OVERWRITE: ParamSpec = p("overwrite", ValueType::Boolean).opt();
const TARGET_ID: ParamSpec = p("target_id", ValueType::String);
const LINK_BASE: ParamSpec = p("site_relative_link_base", ValueType::Enumerated).opt();
const TEMPLATE: ParamSpec = p("template", ValueType::String);
const ITEM: ParamSpec = p("item", ValueType::String);
const TRIGGER: ParamSpec = p("trigger", ValueType::String).opt();

pub(super) const ROWS: &[CommandEntry] = &[
    entry("web.open", "web_command", "Open Web Document", false, false, &[PATH, p("source", ValueType::String).opt(), p("doctype", ValueType::Enumerated).opt()]),
    entry("web.read", "web_command", "Read Web Document", false, false, &[KEY.opt(), p("include_source", ValueType::Boolean).opt()]),
    entry("web.insert_element", "web_command", "Insert Element", false, true, &[PARENT, INDEX, p("tag", ValueType::String), p("attrs", ValueType::Class).opt(), p("text", ValueType::String).opt()]),
    entry("web.insert_fragment", "web_command", "Insert Markup", false, true, &[PARENT, INDEX, p("fragment", ValueType::String)]),
    entry("web.set_text", "web_command", "Set Text", false, true, &[KEY, p("text", ValueType::String)]),
    entry("web.set_attribute", "web_command", "Set Attribute", false, true, &[KEY, NAME, p("value", ValueType::String)]),
    entry("web.remove_attribute", "web_command", "Remove Attribute", false, true, &[KEY, NAME]),
    entry("web.remove_node", "web_command", "Remove Node", false, true, &[KEY]),
    entry("web.undo", "web_command", "Undo Web Edit", false, true, &[]),
    entry("web.redo", "web_command", "Redo Web Edit", false, true, &[]),
    entry("web.save", "web_command", "Save Web Document", false, false, &[PATH, OVERWRITE]),
    entry(
        "web.site",
        "web_command",
        "Site Definition",
        false,
        true,
        &[
            p("site_id", ValueType::String).opt(),
            p("display_name", ValueType::String).opt(),
            p("local_root", ValueType::String).opt(),
            p("default_images_folder", ValueType::String).opt(),
            LINK_BASE,
            p("cache_enabled", ValueType::Boolean).opt(),
            p("testing_target", ValueType::Class).opt(),
            p("server_model_id", ValueType::Enumerated).opt(),
            p("preprocessor_config", ValueType::Class).opt(),
            p("framework_pin", ValueType::Class).opt(),
            p("linter_overrides", ValueType::Class).opt(),
            p("browser_baseline", ValueType::List).opt(),
            p("version_control_binding", ValueType::Class).opt(),
        ],
    ),
    entry(
        "web.remote_target",
        "web_command",
        "Remote Target",
        false,
        true,
        &[TARGET_ID, p("display_name", ValueType::String).opt(), p("transport", ValueType::Enumerated).opt(), p("host", ValueType::String).opt(), p("port", ValueType::Number).opt(), p("root_path", ValueType::String).opt(), p("credentials_ref", ValueType::Reference).opt(), p("remove", ValueType::Boolean).opt()],
    ),
    entry(
        "web.export_recipe",
        "web_command",
        "Export Recipe",
        false,
        true,
        &[p("output_root", ValueType::String).opt(), p("file", ValueType::String).opt(), p("minify", ValueType::Boolean).opt(), p("emit_source_maps", ValueType::Boolean).opt(), p("compile_preprocessors", ValueType::Boolean).opt(), p("asset_rewriting", ValueType::Enumerated).opt(), p("link_base", ValueType::Enumerated).opt(), p("documents", ValueType::Enumerated).opt()],
    ),
    entry("web.export", "web_command", "Export Site", false, false, &[]),
    entry("web.publish", "web_command", "Publish Site", false, false, &[TARGET_ID.opt()]),
    entry("web.verify", "web_command", "Verify Published Site", false, false, &[p("root", ValueType::String).opt()]),
    entry("web.preview", "web_command", "Preview Structure", false, false, &[p("viewport_width_px", ValueType::Number)]),
    entry("web.media", "web_command", "Media Queries", false, false, &[]),
    entry("web.assets", "web_command", "Asset References", false, false, &[]),
    entry("web.site_report", "web_command", "Site Report", false, false, &[]),
    entry("web.move_file", "web_command", "Move File", false, false, &[p("from", ValueType::String), p("to", ValueType::String)]),
    // Lane SMB-WEB-SITE-2 (module docs of `web_commands::web_site`).
    entry("web.change_link", "web_command", "Change Link Sitewide", false, false, &[p("from", ValueType::String), p("to", ValueType::String)]),
    entry("web.template_regions", "web_command", "Template Regions", false, false, &[PATH]),
    entry("web.template_instance", "web_command", "New From Template", false, false, &[TEMPLATE, p("path", ValueType::String), p("params", ValueType::Class).opt()]),
    entry("web.instance_param", "web_command", "Set Instance Parameter", false, true, &[NAME, p("value", ValueType::String)]),
    entry("web.instance_repeat", "web_command", "Edit Repeating Region", false, true, &[p("region", ValueType::String), p("op", ValueType::Enumerated), INDEX, p("to", ValueType::Number).opt()]),
    entry("web.template_update", "web_command", "Update Template Instances", false, false, &[TEMPLATE, p("resolve", ValueType::Class).opt()]),
    entry("web.library_item_insert", "web_command", "Insert Library Item", false, true, &[ITEM, PARENT, INDEX]),
    entry("web.library_item_update", "web_command", "Update Library Item", false, false, &[ITEM]),
    entry(
        "web.snippet",
        "web_command",
        "Snippet",
        false,
        true,
        &[
            p("folder_path", ValueType::String),
            p("snippet_name", ValueType::String),
            p("description", ValueType::String).opt(),
            p("snippet_kind", ValueType::Enumerated).opt(),
            p("preview_mode", ValueType::Enumerated).opt(),
            p("inserts_before_selection", ValueType::String).opt(),
            p("inserts_after_selection", ValueType::String).opt(),
            TRIGGER,
            p("remove", ValueType::Boolean).opt(),
        ],
    ),
    entry("web.snippets", "web_command", "Snippets", false, false, &[]),
    entry(
        "web.snippet_insert",
        "web_command",
        "Insert Snippet",
        false,
        true,
        &[p("folder_path", ValueType::String).opt(), p("snippet_name", ValueType::String).opt(), TRIGGER, p("offset", ValueType::Number), p("end", ValueType::Number).opt()],
    ),
];
