//! Web rows of the command corpus (lane SMB-WEB-HOST-1: the first host of the web crates, [STU-WEB-003],
//! [STU-WEB-113] to [STU-WEB-120], [STU-WEB-127], [STU-WEB-131]), run by `Run::web_command`, module
//! `crate::harness::web_commands`.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

// Every row's run method is `web_command`; which parameters are required is the module's rule (its docs).
const KEY: ParamSpec = p("key", ValueType::Reference);
const PARENT: ParamSpec = p("parent", ValueType::Reference);
const INDEX: ParamSpec = p("index", ValueType::Number);
const NAME: ParamSpec = p("name", ValueType::String);
const PATH: ParamSpec = p("path", ValueType::String);
const OVERWRITE: ParamSpec = p("overwrite", ValueType::Boolean);
const TARGET_ID: ParamSpec = p("target_id", ValueType::String);
const LINK_BASE: ParamSpec = p("site_relative_link_base", ValueType::Enumerated);

pub(super) const ROWS: &[CommandEntry] = &[
    entry("web.open", "web_command", "Open Web Document", false, false, &[PATH, p("source", ValueType::String), p("doctype", ValueType::Enumerated)]),
    entry("web.read", "web_command", "Read Web Document", false, false, &[KEY, p("include_source", ValueType::Boolean)]),
    entry("web.insert_element", "web_command", "Insert Element", false, true, &[PARENT, INDEX, p("tag", ValueType::String), p("attrs", ValueType::Class), p("text", ValueType::String)]),
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
        &[p("site_id", ValueType::String), p("display_name", ValueType::String), p("local_root", ValueType::String), p("default_images_folder", ValueType::String), LINK_BASE, p("cache_enabled", ValueType::Boolean)],
    ),
    entry(
        "web.remote_target",
        "web_command",
        "Remote Target",
        false,
        true,
        &[TARGET_ID, p("display_name", ValueType::String), p("transport", ValueType::Enumerated), p("host", ValueType::String), p("port", ValueType::Number), p("root_path", ValueType::String), p("credentials_ref", ValueType::Reference), p("remove", ValueType::Boolean)],
    ),
    entry(
        "web.export_recipe",
        "web_command",
        "Export Recipe",
        false,
        true,
        &[p("output_root", ValueType::String), p("file", ValueType::String), p("minify", ValueType::Boolean), p("emit_source_maps", ValueType::Boolean), p("compile_preprocessors", ValueType::Boolean), p("asset_rewriting", ValueType::Enumerated), p("link_base", ValueType::Enumerated)],
    ),
    entry("web.export", "web_command", "Export Site", false, false, &[]),
    entry("web.publish", "web_command", "Publish Site", false, false, &[TARGET_ID]),
    entry("web.verify", "web_command", "Verify Published Site", false, false, &[p("root", ValueType::String)]),
    entry("web.preview", "web_command", "Preview Structure", false, false, &[p("viewport_width_px", ValueType::Number)]),
    entry("web.media", "web_command", "Media Queries", false, false, &[]),
    entry("web.assets", "web_command", "Asset References", false, false, &[]),
];
