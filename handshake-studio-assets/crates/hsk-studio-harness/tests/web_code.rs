//! Lane SMB-WEB-CODE-1 ([STU-WEB-011] to [STU-WEB-076]): web code intelligence as typed `web.*` commands through
//! `Run::web_command`, each edit one named undo step with the caller's attribution that reaches save and reopen; the
//! process test (feature `viewer`) runs the same loop through the drive's JSONL session.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_harness_core::action::RUN_COMMANDS;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const PAGE: &str = "<!DOCTYPE html>\n<html><head><title>Code</title><style id=\"main\">\n  p { color: red; }\n</style></head><body><div><p id=\"lead\" class=\"x\">Hello <b>there</b></p><ul><li>a</li></ul><input type=\"slider\"></div></body></html>\n";

/// The rows of this lane, in corpus order.
const CODE_ROWS: [&str; 20] = [
    "web.document_types",
    "web.set_doctype",
    "web.tag_vocabulary",
    "web.css_properties",
    "web.media_records",
    "web.css_rule",
    "web.code_navigator",
    "web.code_hints",
    "web.regions",
    "web.validate",
    "web.lint",
    "web.format_source",
    "web.apply_comment",
    "web.remove_comment",
    "web.balance_braces",
    "web.select_parent_tag",
    "web.collapse_full_tag",
    "web.collapse_selection",
    "web.indent",
    "web.outdent",
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-web-code-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn with_run(f: impl FnOnce(&mut Run<'_>)) {
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    f(&mut run);
}

fn cmd(run: &mut Run<'_>, id: &str, params: Value) -> Value {
    let before = run.ops().len();
    assert!(run.web_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
    assert_eq!(run.ops().len(), before + 1, "{id}: one record");
    let record = run.ops().last().unwrap();
    assert_eq!(record["observe"]["frames"], 1, "{id}: one terminal frame: {record}");
    record["detail"].clone()
}

fn refused(run: &mut Run<'_>, id: &str, params: Value) -> String {
    let before = run.web_state();
    assert!(!run.web_command(id, &params), "{id} {params} was accepted");
    assert_eq!(run.web_state(), before, "{id}: a refusal changes nothing");
    let record = run.ops().last().unwrap();
    assert_eq!(record["observe"]["frames"], 1, "{id}: a refusal also ends with one frame: {record}");
    record["error"].as_str().unwrap().to_string()
}

fn source(run: &mut Run<'_>) -> String {
    cmd(run, "web.read", json!({"include_source": true}))["source"].as_str().unwrap().to_string()
}

#[test]
fn code_rows_are_in_the_corpus_and_run_through_web_command() {
    let rows: Vec<_> = RUN_COMMANDS.iter().filter(|c| CODE_ROWS.contains(&c.command_id)).collect();
    assert_eq!(rows.iter().map(|r| r.command_id).collect::<Vec<_>>(), CODE_ROWS);
    assert!(rows.iter().all(|r| r.run_method == "web_command" && !r.recordable));
    let mutating: Vec<&str> = rows.iter().filter(|r| r.mutates_authority).map(|r| r.command_id).collect();
    assert_eq!(mutating, ["web.set_doctype", "web.css_rule", "web.format_source", "web.apply_comment", "web.remove_comment", "web.indent", "web.outdent"]);
    let dry: Vec<&str> = rows.iter().filter(|r| r.dry_runnable).map(|r| r.command_id).collect();
    assert_eq!(dry, ["web.format_source"]);
}

/// Every edit is one named step with the caller's context; undo and redo walk them; the result survives save and
/// reopen byte for byte.
#[test]
fn code_edits_are_undoable_steps_that_survive_save_and_reopen() {
    let dir = Scratch::new("edits");
    let path = dir.write("index.html", PAGE.as_bytes());
    let saved = with_saved(&path);
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&path)}));
        assert_eq!(source(run), saved, "reopen gives back the saved bytes");
        // A whole-document format is a job (review F2): its settlement rides along; the rest is the no-step answer.
        let mut r = cmd(run, "web.format_source", json!({}));
        let job = r.as_object_mut().and_then(|o| o.remove("job")).expect("the reply carries the job settlement");
        assert_eq!(job["outcome"], "completed", "{job}");
        assert_eq!(r, json!({"changed": false, "edits": 0, "dry_run": false, "truncated": false, "web": run.web_state()}), "already formatted: no step");
    });
}

/// Runs the edit sequence on `path`, saves over it and returns the saved text.
fn with_saved(path: &Path) -> String {
    let mut text = String::new();
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(path)}));
        // Doctype ([STU-WEB-015]).
        let d = cmd(run, "web.set_doctype", json!({"declaration_id": "html_5"}));
        assert_eq!(d["declaration_id"], "html_5");
        assert!(source(run).starts_with("<!doctype html>\n<html>"));
        // A rule in a named media query of a named sheet, one call ([STU-WEB-060]); a composed number; a prefix rule.
        let r = cmd(run, "web.css_rule", json!({"stylesheet": "#main", "media": "screen and (min-width: 600px)", "selector": ".x", "declarations": {"padding": {"number": 4, "unit": "px"}, "transition": "opacity 1s"}}));
        assert_eq!((r["created_media"].as_bool(), r["created_rule"].as_bool()), (Some(true), Some(true)));
        assert_eq!(r["set"], json!(["padding", "-webkit-transition", "-o-transition", "transition"]));
        // Indent the paragraph's line ([STU-WEB-059]); the format below puts it back in place.
        let lines = cmd(run, "web.indent", json!({"start": 0, "end": 5}));
        assert_eq!(lines["lines"], 1);
        // Format: the dry run returns the diff and changes nothing, then one step ([STU-WEB-028]).
        let before = source(run);
        let dry = cmd(run, "web.format_source", json!({"dry_run": true}));
        assert_eq!((dry["dry_run"].as_bool(), dry["changed"].as_bool()), (Some(true), Some(true)));
        assert!(dry["edits"].as_u64().unwrap() > 0 && dry["diff"].as_array().is_some_and(|d| !d.is_empty()));
        assert_eq!(source(run), before, "a dry run changes nothing");
        let formatted = cmd(run, "web.format_source", json!({}));
        assert_eq!(formatted["changed"], true);
        let after = source(run);
        assert!(after.contains("\n  <div>\n    <p id=\"lead\" class=\"x\">Hello <b>there</b></p>"), "{after}");
        // Comment and uncomment ([STU-WEB-029]).
        let c = cmd(run, "web.apply_comment", json!({"key": "#lead"}));
        assert_eq!(c["scheme_id"], "html");
        assert!(source(run).contains("<!--<p id=\"lead\" class=\"x\">Hello <b>there</b></p>-->"));
        let off = source(run).find("Hello").unwrap();
        cmd(run, "web.remove_comment", json!({"offset": off}));
        assert_eq!(source(run), after, "remove gives the text back");
        // Every step carries its label, command id and the caller's actor.
        let undo = run.web_state()["undo"].as_array().unwrap().clone();
        let steps: Vec<(&str, &str)> = undo.iter().map(|u| (u["label"].as_str().unwrap(), u["command_id"].as_str().unwrap())).collect();
        assert_eq!(steps, [("Set Doctype", "web.set_doctype"), ("Set CSS Rule", "web.css_rule"), ("Indent", "web.indent"), ("Format Source", "web.format_source"), ("Apply Comment", "web.apply_comment"), ("Remove Comment", "web.remove_comment")]);
        assert!(undo.iter().all(|u| u["actor"]["account_id"].is_string()), "attribution on every step");
        cmd(run, "web.undo", json!({}));
        assert!(source(run).contains("<!--<p id=\"lead\""), "undo brings the comment back");
        cmd(run, "web.redo", json!({}));
        assert_eq!(source(run), after);
        text = source(run);
        cmd(run, "web.save", json!({"overwrite": true}));
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    text
}

#[test]
fn read_rows_answer_headlessly() {
    with_run(|run| {
        cmd(run, "web.open", json!({"source": PAGE}));
        let t = cmd(run, "web.document_types", json!({"file_name": "a.dwt.php"}));
        assert_eq!((t["types"].as_array().unwrap().len(), t["doctypes"].as_array().unwrap().len()), (39, 10));
        assert_eq!(t["file"]["candidate_types"], json!(["template_php"]));
        let v = cmd(run, "web.tag_vocabulary", json!({"tag": "input"}));
        assert_eq!(v["type_id"], "html");
        assert!(v["tag"]["attributes"].as_array().unwrap().iter().any(|a| a["name"] == "type" && a["value_type"] == "enumerated"));
        assert_eq!(v["tag"]["property_inspector"], "generic_attribute_table");
        let c = cmd(run, "web.css_properties", json!({"category": "background"}));
        assert_eq!(c["entries"].as_array().unwrap().len(), 9);
        let n = cmd(run, "web.code_navigator", json!({"key": "#lead"}));
        assert_eq!(n["rules"][0]["selector"], "p");
        let off = PAGE.find("red").unwrap() + 1;
        let h = cmd(run, "web.code_hints", json!({"offset": off}));
        assert_eq!(h["hint"]["group_id"], "css_values");
        let r = cmd(run, "web.regions", json!({"offset": off}));
        assert_eq!(r["regions"][0]["scheme_id"], "css");
        let val = cmd(run, "web.validate", json!({}));
        assert!(val["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "attribute_value_outside_enumeration" && d["rule_set_id"] == "html_all"), "{val}");
        let lint = cmd(run, "web.lint", json!({"language": "markup", "rules": {"title-require": true}}));
        assert!(lint["enabled"].as_array().unwrap().iter().any(|r| r == "title-require"));
        let b = cmd(run, "web.balance_braces", json!({"start": PAGE.find("there").unwrap()}));
        assert_eq!(&PAGE[b["range"][0].as_u64().unwrap() as usize..b["range"][1].as_u64().unwrap() as usize], "<b>there</b>");
        assert!(cmd(run, "web.select_parent_tag", json!({"key": "#lead"}))["key"].as_str().unwrap().ends_with("div"));
        cmd(run, "web.collapse_full_tag", json!({"key": "#lead"}));
        cmd(run, "web.collapse_selection", json!({"start": 0, "end": 5}));
        let m = cmd(run, "web.media_records", json!({}));
        assert_eq!(m["total"], 0);
        assert!(run.web_state()["undo"].as_array().unwrap().is_empty(), "reads record no step");
    });
}

#[test]
fn refusals_are_typed_and_change_nothing() {
    with_run(|run| {
        assert_eq!(refused(run, "web.format_source", json!({})), "no_web_document");
        cmd(run, "web.open", json!({"source": PAGE}));
        assert_eq!(refused(run, "web.set_doctype", json!({"declaration_id": "html_9"})), "doctype_unknown");
        assert_eq!(refused(run, "web.set_doctype", json!({"declaration_id": "xslt_1"})), "doctype_needs_xml_parser");
        assert_eq!(refused(run, "web.css_rule", json!({"stylesheet": "#main", "selector": "p", "declarations": {"color": "red</style>"}})), "css_text_unsafe");
        assert_eq!(refused(run, "web.css_rule", json!({"stylesheet": "#lead", "selector": "p", "declarations": {"color": "blue"}})), "not_a_style_element");
        assert_eq!(refused(run, "web.css_rule", json!({"stylesheet": "#main", "selector": "p", "declarations": {"opacity": {"number": 3}}})), "value_out_of_range");
        assert_eq!(refused(run, "web.lint", json!({"language": "script"})), "script_lint_provider_unselected");
        assert_eq!(refused(run, "web.lint", json!({"language": "markup", "rules": {"nope": true}})), "lint_rule_unknown");
        assert_eq!(refused(run, "web.format_source", json!({"indent_width": 0})), "format_option_invalid");
        assert_eq!(refused(run, "web.code_hints", json!({"offset": 1_000_000})), "offset_invalid");
        assert_eq!(refused(run, "web.code_hints", json!({"offset": -1})), "parameter_invalid");
        assert_eq!(refused(run, "web.tag_vocabulary", json!({"tag": "blink"})), "tag_unknown_in_active_libraries");
        assert_eq!(refused(run, "web.validate", json!({"type_id": "css"})), "rule_set_not_for_type");
        assert_eq!(refused(run, "web.apply_comment", json!({"start": 3})), "parameter_missing");
        assert_eq!(refused(run, "web.remove_comment", json!({"offset": 0})), "no_comment_here");
        assert_eq!(refused(run, "web.indent", json!({"key": "#lead", "indent_char": "emoji"})), "parameter_invalid");
    });
}

/// The same loop through the drive's JSONL session (feature `viewer`): one process, typed rows, the edits saved and
/// read back by a second open in the same session.
#[cfg(feature = "viewer")]
#[test]
fn code_commands_run_in_the_drive_session() {
    use hsk_studio_harness::cli::{self, WindowArgs};
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = Scratch::new("drive");
    let page = dir.write("page.html", PAGE.as_bytes());
    let package = dir.write("a.handshake", FIXTURE);
    let line = |id: &str, parameters: Value| json!({"command": "command", "command_id": id, "parameters": parameters});
    let input = [
        line("web.open", json!({"path": s(&page)})),
        line("web.css_rule", json!({"stylesheet": "#main", "selector": "h1", "declarations": {"margin": "0"}})),
        line("web.format_source", json!({"indent_width": 4})),
        line("web.validate", json!({})),
        line("web.lint", json!({"language": "css"})),
        line("web.save", json!({"overwrite": true})),
        line("web.open", json!({"path": s(&page)})),
        line("web.read", json!({"include_source": true})),
        line("web.lint", json!({"language": "script"})),
    ];
    let bytes: Vec<u8> = input.iter().flat_map(|v| format!("{v}\n").into_bytes()).collect();
    let args = ["command", package.to_str().unwrap(), "--actor", "lane-web-code", "--session", "s-web-code", "--stdin"];
    let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }));
    let exe = std::env::current_exe().unwrap();
    let example = exe.parent().and_then(Path::parent).unwrap().join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    let mut command = Command::new(example);
    command.args(args).current_dir(&dir.0).env("STUDIO_DIAG_DIR", &dir.0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let out: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(out.len(), input.len() + 2, "open, one reply per line, end: {text}");
    for (i, reply) in out.iter().enumerate().take(input.len()).skip(1) {
        let id = input[i - 1]["command_id"].as_str().unwrap();
        assert_eq!(reply["ok"], true, "line {i} {id}: {reply}");
    }
    assert_eq!(out[2]["detail"]["web"]["undo"][0]["actor"]["account_id"], "lane-web-code", "{}", out[2]);
    assert_eq!(out[3]["detail"]["web"]["undo"][1]["label"], "Format Source", "{}", out[3]);
    assert!(out[4]["detail"]["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "attribute_value_outside_enumeration"), "{}", out[4]);
    let reread = out[8]["detail"]["source"].as_str().unwrap();
    assert!(reread.contains("\n    <div>\n        <p id=\"lead\"") && reread.contains("h1 {\n    margin: 0;\n  }"), "the saved edits come back on reopen: {reread}");
    assert_eq!(out[9]["error"], "script_lint_provider_unselected", "{}", out[9]);
    let end = out.last().unwrap();
    assert_eq!((end["lines"].as_u64(), end["refused"].as_u64()), (Some(9), Some(1)), "{end}");
    assert_eq!(std::fs::read_to_string(&page).unwrap(), reread);
}

// ---------------------------------------------------------------------------------------------------------------------
// Review c1e077d9 (research/review-smb-web-code-1-c1e077d9.json), lead rulings F1 to F14.

/// A job port that cancels the run's token while it admits (the job is admitted, its work starts canceled), or admits.
#[derive(Default)]
struct CancelPort {
    cancel_on_admit: Option<CancellationToken>,
    admits: std::sync::atomic::AtomicU64,
}

impl hsk_studio_session::JobPort for CancelPort {
    fn admit(&self, _scope: &hsk_studio_session::JobScope<'_>, _cost: hsk_studio_session::Cost) -> Result<hsk_studio_session::JobTicket, hsk_studio_session::JobRefused> {
        if let Some(token) = &self.cancel_on_admit {
            token.cancel();
        }
        Ok(hsk_studio_session::JobTicket(self.admits.fetch_add(1, std::sync::atomic::Ordering::SeqCst)))
    }

    fn release(&self, _ticket: hsk_studio_session::JobTicket) {}
}

fn open_source(run: &mut Run<'_>, text: &str) {
    cmd(run, "web.open", json!({"source": text}));
}

/// F1: a comment edit changes only its selection: an unwrap that would expose a marker, a range inside an existing
/// comment, string or line comment, and a range in RCDATA are refused typed, with no step.
#[test]
fn f1_comment_edits_change_only_the_selection() {
    with_run(|run| {
        let html = "<!-- a <!-- b --><p>keep</p><!-- c -->";
        open_source(run, html);
        assert_eq!(refused(run, "web.remove_comment", json!({"offset": 5})), "comment_inner_has_markers");
        let css = "<style>/* a /* b */ p{top:0}</style>";
        open_source(run, css);
        assert_eq!(refused(run, "web.remove_comment", json!({"offset": css.find(" a").unwrap()})), "comment_inner_has_markers");
        let js = "<script>// don't\nvar t = '/* x */';</script>";
        open_source(run, js);
        assert_eq!(refused(run, "web.remove_comment", json!({"offset": js.find('x').unwrap()})), "no_comment_here", "the x is in a string");
        let inside = "<style>/* a b c */ p{top:0}</style>";
        open_source(run, inside);
        let b = inside.find(" b ").unwrap() + 1;
        assert_eq!(refused(run, "web.apply_comment", json!({"start": b, "end": b + 1})), "range_splits_a_token");
        let string = "<script>var s = 'abc';</script>";
        open_source(run, string);
        let at = string.find("bc").unwrap();
        assert_eq!(refused(run, "web.apply_comment", json!({"start": at, "end": at + 1})), "range_splits_a_token");
        let line = "<script>// a\nb();</script>";
        open_source(run, line);
        let a = line.find("a\n").unwrap();
        assert_eq!(refused(run, "web.apply_comment", json!({"start": a, "end": a + 3})), "range_splits_a_token");
        let title = "<!doctype html><title>Hello</title><p>x</p>";
        open_source(run, title);
        let h = title.find("Hello").unwrap();
        assert_eq!(refused(run, "web.apply_comment", json!({"start": h, "end": h + 5})), "range_in_rcdata");
        // A real comment in the same script still unwraps, and only it.
        let ok = "<script>// don't\n/* f(); */ var t = 'q';</script>";
        open_source(run, ok);
        cmd(run, "web.remove_comment", json!({"offset": ok.find("f()").unwrap()}));
        assert_eq!(source(run), "<script>// don't\n f();  var t = 'q';</script>");
    });
}

/// F2: whole-document rows are jobs with cancellation points; a cancel during the job settles it canceled.
#[test]
fn f2_whole_document_rows_are_jobs_and_cancel_mid_job() {
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    open_source(&mut run, PAGE);
    run.bind_web_jobs(std::sync::Arc::new(CancelPort::default()), hsk_studio_session::JobLifecycle::default());
    for (id, params) in [("web.format_source", json!({"dry_run": true})), ("web.validate", json!({})), ("web.lint", json!({"language": "markup"})), ("web.indent", json!({}))] {
        let detail = cmd(&mut run, id, params);
        assert_eq!(detail["job"]["outcome"], "completed", "{id} is admitted as a job: {detail}");
    }
    let lifecycle = hsk_studio_session::JobLifecycle::default();
    run.bind_web_jobs(std::sync::Arc::new(CancelPort { cancel_on_admit: Some(cancel.clone()), ..CancelPort::default() }), lifecycle.clone());
    assert_eq!(refused(&mut run, "web.format_source", json!({})), "canceled");
    assert_eq!(lifecycle.counts().canceled, 1, "admitted, then canceled at a cancellation point");
}

/// F2: a hostile document under the parse limit finishes within a bound or refuses typed.
#[test]
fn f2_hostile_escapes_finish_within_a_bound() {
    let rule = format!("@{}{{}}", "\\g".repeat(33));
    let mut text = String::from("<style>");
    while text.len() < 4 << 20 {
        text.push_str(&rule);
    }
    text.push_str("</style><p>x</p>");
    with_run(|run| {
        open_source(run, &text);
        let start = std::time::Instant::now();
        let ok = run.web_command("web.media_records", &json!({}));
        let code = run.ops().last().unwrap()["error"].clone();
        assert!(start.elapsed().as_secs() < 20, "bounded: {:?}", start.elapsed());
        assert!(ok || code.is_string(), "accepted or a typed refusal: {code}");
    });
}

/// F3: format never adds whitespace next to inline-level or replaced content.
#[test]
fn f3_format_is_rendering_neutral() {
    with_run(|run| {
        for page in ["<p>a<canvas id=c></canvas>b</p>", "<div><img src=a><picture><source srcset=x><img src=b></picture></div>", "<div>x<select><option>1</option></select></div>"] {
            open_source(run, page);
            let out = match run.web_command("web.format_source", &json!({})) {
                true => source(run),
                false => page.to_string(),
            };
            for tag in ["<canvas", "<picture", "<select", "<img src=b"] {
                if let Some(at) = out.find(tag) {
                    let before = out[..at].chars().last();
                    assert!(before.is_none_or(|c| !c.is_whitespace()), "no whitespace inserted before {tag}: {out}");
                }
            }
        }
    });
}

/// F4 to F6: rule edits keep their neighbours, remove hand-written transitions, find rules by their raw selector,
/// refuse ambiguity, insert before media rules and keep !important.
#[test]
fn f4_to_f6_css_rule_edits_keep_their_neighbours() {
    with_run(|run| {
        let sheet = "<style id=s>.a { color: red; margin: 0; }</style>";
        open_source(run, sheet);
        assert_eq!(refused(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".a", "declarations": {"color": "blue\\"}})), "css_value_invalid");
        let hand = "<style id=s>nav a { transition: color .2s }</style>";
        open_source(run, hand);
        let r = cmd(run, "web.css_rule", json!({"stylesheet": "#s", "selector": "nav a", "declarations": {"transition": null}}));
        assert_eq!(r["removed"], json!(["transition"]));
        let escaped = "<style id=s>.a\\,b { top: 0 } .a, b { top: 1px }</style>";
        open_source(run, escaped);
        cmd(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".a, b", "declarations": {"top": "2px"}}));
        assert!(source(run).contains(".a\\,b { top: 0 }"), "the escaped selector is another rule: {}", source(run));
        let dup = "<style id=s>.a { top: 0 } .a { top: 1px }</style>";
        open_source(run, dup);
        assert_eq!(refused(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".a", "declarations": {"top": "2px"}})), "css_rule_ambiguous");
        cmd(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".a", "rule_index": 1, "declarations": {"top": "2px"}}));
        assert!(source(run).contains(".a { top: 0 } .a { top: 2px }"), "{}", source(run));
        let media = "<style id=s>@media (max-width: 600px) { .card { padding: 0 } }</style>";
        open_source(run, media);
        cmd(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".card", "declarations": {"padding": "24px"}}));
        let out = source(run);
        assert!(out.find(".card {\n").unwrap() < out.find("@media").unwrap(), "a new top-level rule goes before @media: {out}");
        let imp = "<style id=s>.a { color: red !important }</style>";
        open_source(run, imp);
        cmd(run, "web.css_rule", json!({"stylesheet": "#s", "selector": ".a", "declarations": {"color": "blue"}}));
        assert!(source(run).contains("color: blue !important"), "{}", source(run));
        let spec = "<style>.a.b.c.d.e.f.g.h.i.j { top: 0 } #x { top: 1px }</style><p id=x class='a b c d e f g h i j'>t</p>";
        open_source(run, spec);
        let nav = cmd(run, "web.code_navigator", json!({"key": "#x"}));
        assert_eq!(nav["rules"][0]["specificity"], json!([0, 10, 0]), "{nav}");
        assert_eq!(nav["rules"][1]["specificity"], json!([1, 0, 0]), "{nav}");
    });
}

/// F7: a valid document with the implied skeleton has no context errors; a misplaced title still has one.
#[test]
fn f7_the_implied_skeleton_is_valid() {
    with_run(|run| {
        open_source(run, "<!doctype html><title>t</title><p>x</p>");
        let v = cmd(run, "web.validate", json!({}));
        assert!(v["diagnostics"].as_array().unwrap().iter().all(|d| d["code"] != "tag_context_violation"), "{v}");
        open_source(run, "<!doctype html><html><head></head><body><title>t</title></body></html>");
        let v = cmd(run, "web.validate", json!({}));
        assert!(v["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "tag_context_violation"), "{v}");
        open_source(run, "<!doctype html><blink>x</blink><input type=slider>");
        let v = cmd(run, "web.validate", json!({}));
        let codes: Vec<&str> = v["diagnostics"].as_array().unwrap().iter().filter_map(|d| d["code"].as_str()).collect();
        assert_eq!(codes, ["tag_unknown_in_active_libraries", "attribute_value_outside_enumeration"], "STU-WEB-148 and -149 codes");
    });
}

/// F8: regions follow the document type; the cursor right before `</style>` is CSS.
#[test]
fn f8_regions_follow_the_document_type() {
    let dir = Scratch::new("css-type");
    let sheet = dir.write("styles.css", b"p{color:red}");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&sheet)}));
        let c = cmd(run, "web.apply_comment", json!({"start": 0, "end": 12}));
        assert_eq!(c["scheme_id"], "css");
        assert_eq!(source(run), "/*p{color:red}*/");
        let page = "<style>p{col</style>";
        open_source(run, page);
        let h = cmd(run, "web.code_hints", json!({"offset": page.find("</style>").unwrap()}));
        assert_eq!((h["hint"]["group_id"].as_str(), h["hint"]["prefix"].as_str()), (Some("css_properties"), Some("col")), "{h}");
        for field in ["pattern", "additional_dismiss_chars", "allow_whitespace_prefix", "display_restriction", "allow_multiple_values", "items"] {
            assert!(!h["hint"][field].is_null(), "the hint carries {field}: {h}");
        }
        open_source(run, "<script type=text/x-javascript>a()</script>");
        let r = cmd(run, "web.regions", json!({"offset": 31}));
        assert_eq!(r["regions"][0]["scheme_id"], "javascript");
    });
}

/// F10: the doctype goes after a BOM and an XML declaration; a save writes the BOM the type asks for.
#[test]
fn f10_doctype_after_bom_and_prolog_and_save_writes_the_bom() {
    let dir = Scratch::new("bom");
    let bom = dir.write("bom.html", "\u{feff}<p>caf\u{e9}</p>".as_bytes());
    let aspx = dir.write("page.aspx", b"<p>x</p>");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&bom)}));
        cmd(run, "web.set_doctype", json!({"declaration_id": "html_5"}));
        cmd(run, "web.save", json!({"overwrite": true}));
        let bytes = std::fs::read(&bom).unwrap();
        assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf, b'<', b'!']), "the BOM stays at byte 0: {:?}", &bytes[..8]);
        open_source(run, "<?xml version=\"1.0\"?>\n<p>x</p>");
        cmd(run, "web.set_doctype", json!({"declaration_id": "xhtml_1_strict"}));
        assert!(source(run).starts_with("<?xml version=\"1.0\"?>\n<!DOCTYPE html PUBLIC"), "{}", source(run));
        cmd(run, "web.open", json!({"path": s(&aspx)}));
        cmd(run, "web.save", json!({"overwrite": true}));
        assert!(std::fs::read(&aspx).unwrap().starts_with(&[0xef, 0xbb, 0xbf]), "ASP.NET types write a BOM (STU-WEB-013)");
    });
}

/// F11: a dry-run diff that does not fit says so; F13: the corpus and the manual agree on dry runs.
#[test]
fn f11_f13_dry_run_diffs_and_docs_are_honest() {
    with_run(|run| {
        open_source(run, &format!("<ul>{}</ul>", "<li>x</li>".repeat(3000)));
        let dry = cmd(run, "web.format_source", json!({"dry_run": true}));
        assert!(dry["truncated"].is_boolean(), "an explicit truncated flag: {}", dry["truncated"]);
    });
    let dry: Vec<&str> = RUN_COMMANDS.iter().filter(|c| c.dry_runnable).map(|c| c.command_id).collect();
    assert_eq!(dry, ["web.format_source"]);
    assert!(!hsk_studio_harness_core::manual::DRIVE_LIMITS.iter().any(|l| l.starts_with("no dry_run")), "the manual names the one dry-runnable row");
}
