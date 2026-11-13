//! Web code-intelligence rows of the command corpus (lane SMB-WEB-CODE-1; [STU-WEB-011] to [STU-WEB-076]), run by
//! `Run::web_command` like every `web.*` row; handlers in module `crate::harness::web_commands::web_code` (its docs are
//! the manual: parameters, results and typed refusals). `web.format_source`, `web.apply_comment` and
//! `web.remove_comment` are the spec's command ids ([STU-WEB-028], [STU-WEB-029]); the other ids follow this corpus's
//! `web.*` naming over the spec's surface names (no spec command id is verified for them). Mutating rows are one named
//! step of the web history each, with the caller's context; `web.format_source` is dry-runnable (`dry_run`).

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, Surface, entry, p};

const KEY: ParamSpec = p("key", ValueType::Reference);
const TYPE_ID: ParamSpec = p("type_id", ValueType::Enumerated).opt();
const OFFSET: ParamSpec = p("offset", ValueType::Number);
const START: ParamSpec = p("start", ValueType::Number);
const END: ParamSpec = p("end", ValueType::Number);
const INDENT_CHAR: ParamSpec = p("indent_char", ValueType::Enumerated).opt();
const INDENT_WIDTH: ParamSpec = p("indent_width", ValueType::Number).opt();

/// The format row: dry-runnable ([STU-WEB-028]).
const FORMAT: CommandEntry = CommandEntry {
    command_id: "web.format_source",
    run_method: "web_command",
    surface: Surface::Run,
    display_name: "Format Source",
    recordable: false,
    mutates_authority: true,
    dry_runnable: true,
    parameters: &[KEY.opt(), INDENT_CHAR, INDENT_WIDTH, p("line_length", ValueType::Number).opt(), p("attribute_wrap", ValueType::Enumerated).opt(), p("dry_run", ValueType::Boolean).opt()],
};

pub(super) const ROWS: &[CommandEntry] = &[
    entry("web.document_types", "web_command", "Document Types", false, false, &[TYPE_ID, p("file_name", ValueType::String).opt()]),
    entry("web.set_doctype", "web_command", "Set Doctype", false, true, &[p("declaration_id", ValueType::Enumerated)]),
    entry("web.tag_vocabulary", "web_command", "Tag Vocabulary", false, false, &[TYPE_ID, p("tag", ValueType::String).opt()]),
    entry("web.css_properties", "web_command", "CSS Properties", false, false, &[p("property", ValueType::String).opt(), p("category", ValueType::Enumerated).opt()]),
    entry("web.media_records", "web_command", "Media Query Records", false, false, &[]),
    entry("web.css_rule", "web_command", "Set CSS Rule", false, true, &[p("stylesheet", ValueType::Reference), p("media", ValueType::String).opt(), p("selector", ValueType::String), p("declarations", ValueType::Class)]),
    entry("web.code_navigator", "web_command", "Code Navigator", false, false, &[KEY]),
    entry("web.code_hints", "web_command", "Code Hints", false, false, &[OFFSET, TYPE_ID]),
    entry("web.regions", "web_command", "Language Regions", false, false, &[OFFSET.opt()]),
    entry("web.validate", "web_command", "Validate Markup", false, false, &[p("rule_set", ValueType::Enumerated).opt(), KEY.opt(), TYPE_ID]),
    entry("web.lint", "web_command", "Lint", false, false, &[p("language", ValueType::Enumerated), p("rules", ValueType::Class).opt(), KEY.opt()]),
    FORMAT,
    entry("web.apply_comment", "web_command", "Apply Comment", false, true, &[KEY.opt(), START.opt(), END.opt()]),
    entry("web.remove_comment", "web_command", "Remove Comment", false, true, &[OFFSET]),
    entry("web.balance_braces", "web_command", "Balance Braces", false, false, &[START, END.opt()]),
    entry("web.select_parent_tag", "web_command", "Select Parent Tag", false, false, &[KEY]),
    entry("web.collapse_full_tag", "web_command", "Collapse Full Tag", false, false, &[KEY]),
    entry("web.collapse_selection", "web_command", "Collapse Selection", false, false, &[START, END]),
    entry("web.indent", "web_command", "Indent", false, true, &[KEY.opt(), START.opt(), END.opt(), INDENT_CHAR, INDENT_WIDTH]),
    entry("web.outdent", "web_command", "Outdent", false, true, &[KEY.opt(), START.opt(), END.opt(), INDENT_CHAR, INDENT_WIDTH]),
];
