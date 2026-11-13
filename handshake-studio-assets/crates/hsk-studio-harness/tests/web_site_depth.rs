//! Lane SMB-WEB-SITE-2 ([STU-WEB-090] to [STU-WEB-093], [STU-WEB-096] to [STU-WEB-099], [STU-WEB-113], [STU-WEB-118],
//! [STU-WEB-161]): site depth after SMB-WEB-SITE-1. Every step runs through `Run::web_command`: the site report admitted
//! as the session's read class, change-link-site-wide over every spelling of a link, templates with all five region
//! kinds, expressions and update propagation with drift, library items, the snippet library with unique triggers, and
//! the optional fields of the site record, each through save and reopen.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_session::{Cost, JobLifecycle, JobPort, JobRefused, JobScope, JobTicket};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");

/// A temp folder of this test process only, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-web-depth-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn at(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, s| p.join(s))
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.at(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.at(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
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

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn with_run(f: impl FnOnce(&mut Run<'_>)) {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    f(&mut run);
}

/// An accepted command: its detail; asserts one record with one terminal frame.
fn cmd(run: &mut Run<'_>, id: &str, params: Value) -> Value {
    let before = run.ops().len();
    assert!(run.web_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
    assert_eq!(run.ops().len(), before + 1, "{id}: one record");
    let record = run.ops().last().unwrap();
    assert_eq!(record["observe"]["frames"], 1, "{id}: one terminal frame: {record}");
    record["detail"].clone()
}

/// A refused command: its typed code; asserts the host's state did not change.
fn refused(run: &mut Run<'_>, id: &str, params: Value) -> String {
    let before = run.web_state();
    assert!(!run.web_command(id, &params), "{id} {params} was accepted");
    assert_eq!(run.web_state(), before, "{id}: a refusal changes nothing");
    let record = run.ops().last().unwrap();
    assert_eq!(record["observe"]["frames"], 1, "{id}: a refusal also ends with one frame: {record}");
    record["error"].as_str().unwrap().to_string()
}

fn key_of_tag(run: &mut Run<'_>, tag: &str) -> String {
    let rows = cmd(run, "web.read", json!({}))["rows"].clone();
    rows.as_array().unwrap().iter().find(|r| r["tag"] == tag).unwrap_or_else(|| panic!("no {tag} in {rows}"))["key"].as_str().unwrap().to_string()
}

fn source(run: &mut Run<'_>) -> String {
    cmd(run, "web.read", json!({"include_source": true}))["source"].as_str().unwrap().to_string()
}

fn define(run: &mut Run<'_>, dir: &Scratch) {
    cmd(run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
}

const PAGE: &str = "<!DOCTYPE html><html><head><title>t</title></head><body>t</body></html>";

/// A job port that keeps each asked action's token; admits everything unless `refuse`.
#[derive(Default)]
struct Actions {
    actions: Mutex<Vec<&'static str>>,
    next: AtomicU64,
    refuse: bool,
}

impl JobPort for Actions {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        self.actions.lock().unwrap().push(scope.action.as_str());
        if self.refuse {
            return Err(JobRefused { retry_after_ms: 5 });
        }
        Ok(JobTicket(self.next.fetch_add(1, Ordering::SeqCst)))
    }

    fn release(&self, _ticket: JobTicket) {}
}

/// Ledger 2026-11-12T15:08:00Z and review F14: every site-wide command of the lane is admitted through the bound port
/// with its action (the report and a sync review `read`; change link, template and library updates `edit`; applying a
/// put `export`, a get `edit`), before any work: a saturated port refuses `overloaded` and nothing is written.
#[test]
fn site_work_is_admitted_with_its_action() {
    let dir = Scratch::new("read");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>t</title></head><body><a href=\"gone.html\">g</a></body></html>");
    dir.write("site/a.png", b"png");
    dir.write("site/Templates/t.dwt", b"<!DOCTYPE html><html><head><title>t</title></head><body>t</body></html>");
    dir.write("site/Library/n.lbi", b"<nav>n</nav>");
    dir.write("remote/b.png", b"remote");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.web_command("web.site", &json!({"display_name": "s", "local_root": s(&dir.at("site"))})));
    assert!(run.web_command("web.remote_target", &json!({"target_id": "local", "root_path": s(&dir.at("remote"))})));
    let saturated = Arc::new(Actions { refuse: true, ..Actions::default() });
    run.bind_web_jobs(saturated.clone(), JobLifecycle::default());
    let before = (std::fs::read(dir.at("site/index.html")).unwrap(), std::fs::read_dir(dir.at("remote")).unwrap().count());
    for (id, params) in [
        ("web.change_link", json!({"from": "gone.html", "to": "a.png"})),
        ("web.template_update", json!({"template": "Templates/t.dwt"})),
        ("web.library_item_update", json!({"item": "Library/n.lbi"})),
        ("web.synchronize", json!({"direction": "put"})),
    ] {
        assert!(!run.web_command(id, &params), "{id} ran with the port saturated");
        assert_eq!(run.ops().last().unwrap()["error"], "overloaded", "{id}");
    }
    assert_eq!((std::fs::read(dir.at("site/index.html")).unwrap(), std::fs::read_dir(dir.at("remote")).unwrap().count()), before, "nothing written");
    let port = Arc::new(Actions::default());
    run.bind_web_jobs(port.clone(), JobLifecycle::default());
    let ok = |run: &mut Run<'_>, id: &str, params: Value| -> Value {
        assert!(run.web_command(id, &params), "{id}: {}", run.ops().last().unwrap());
        run.ops().last().unwrap()["detail"].clone()
    };
    ok(&mut run, "web.site_report", json!({}));
    ok(&mut run, "web.change_link", json!({"from": "gone.html", "to": "a.png"}));
    ok(&mut run, "web.template_update", json!({"template": "Templates/t.dwt"}));
    ok(&mut run, "web.library_item_update", json!({"item": "Library/n.lbi"}));
    let put = ok(&mut run, "web.synchronize", json!({"direction": "put"}));
    ok(&mut run, "web.synchronize", json!({"direction": "put", "apply": true, "plan_sha256": put["plan_sha256"]}));
    let get = ok(&mut run, "web.synchronize", json!({"direction": "get"}));
    ok(&mut run, "web.synchronize", json!({"direction": "get", "apply": true, "plan_sha256": get["plan_sha256"]}));
    assert_eq!(*port.actions.lock().unwrap(), ["read", "edit", "edit", "edit", "read", "export", "read", "edit"]);
    assert_eq!(std::fs::read(dir.at("site/b.png")).unwrap(), b"remote");
}

/// [STU-WEB-118] change-link-site-wide (ledger 2026-11-11T11:09:18Z, 2026-11-12T15:08:00Z): every reference to `from`
/// in any spelling the site resolves (exact, another letter case, percent-encoded; `from` may be a missing page) is
/// rewritten to `to` in the site's link base, query and fragment kept, as one transaction with a receipt. Review F7
/// (lead ruling): a spelling the engine denies (`old%2Ehtml`, ambiguous_path) is never rewritten; it is listed in the
/// receipt as left_denied and the receipt says complete=false. The red input of e1b7f955 is restored.
#[test]
fn change_link_rewrites_every_spelling_site_wide() {
    let dir = Scratch::new("change-link");
    let index = "<!DOCTYPE html><html><head><title>i</title></head><body>\n<a href=\"old.html\">1</a> <a href=\"Old.html#top\">2</a> <a href=\"old%2Ehtml?q=1\">3</a> <a href=\"news.html\">4</a> <a href=\"ol%64.html\">5</a>\n</body></html>";
    dir.write("site/index.html", index.as_bytes());
    dir.write("site/pages/p.html", b"<!DOCTYPE html><html><head><title>p</title></head><body><a href=\"../old.html\">o</a></body></html>");
    dir.write("site/news.html", PAGE.as_bytes());
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.change_link", json!({"from": "a.html", "to": "a.html"})), "nothing_to_change");
        assert_eq!(refused(run, "web.change_link", json!({"from": "../x.html", "to": "news.html"})), "move_outside_site");
        assert_eq!(refused(run, "web.change_link", json!({"from": "old.html", "to": "studio-export-manifest.json"})), "link_target_reserved");
        assert_eq!(refused(run, "web.change_link", json!({"from": "old.html", "to": "nope.html"})), "link_target_missing");
        assert_eq!(refused(run, "web.change_link", json!({"from": "zzz.html", "to": "news.html"})), "nothing_to_change");
        let before = cmd(run, "web.site_report", json!({}));
        assert_eq!(before["broken_links"].as_array().unwrap().len(), 4, "{before}");
        assert_eq!(before["denied"].as_array().unwrap().len(), 1, "the engine denies %2E: {before}");
        let changed = cmd(run, "web.change_link", json!({"from": "old.html", "to": "news.html"}));
        let receipt = &changed["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["complete"].as_bool()), (Some("hsk.studio.web.change_link_receipt@1"), Some(false)), "{receipt}");
        assert_eq!(receipt["left_denied"], json!([{"document": "index.html", "line": 2, "reference": "old%2Ehtml?q=1", "reason": "ambiguous_path"}]), "{receipt}");
        let rows: Vec<(&str, usize)> = receipt["changed"].as_array().unwrap().iter().map(|c| (c["document"].as_str().unwrap(), c["changes"].as_array().unwrap().len())).collect();
        assert_eq!(rows, [("index.html", 3), ("pages/p.html", 1)], "{receipt}");
        let first = &receipt["changed"][0]["changes"][1];
        assert_eq!((first["reference_before"].as_str(), first["reference_after"].as_str()), (Some("Old.html#top"), Some("news.html#top")));
        let after = cmd(run, "web.site_report", json!({}));
        assert_eq!(after["broken_links"], json!([]), "{after}");
    });
    assert_eq!(
        dir.read("site/index.html"),
        "<!DOCTYPE html><html><head><title>i</title></head><body>\n<a href=\"news.html\">1</a> <a href=\"news.html#top\">2</a> <a href=\"old%2Ehtml?q=1\">3</a> <a href=\"news.html\">4</a> <a href=\"news.html\">5</a>\n</body></html>"
    );
    assert!(dir.read("site/pages/p.html").contains("<a href=\"../news.html\">o</a>"));
}

/// Ledger 2026-11-12T15:08:00Z: a move also rewrites the case-mismatched and percent-encoded spellings of the moved
/// file's old path in other documents; a spelling the engine denies is listed as left_denied with complete=false
/// (review F7; the red input of e1b7f955 is restored).
#[test]
fn a_move_rewrites_case_mismatched_and_percent_encoded_spellings() {
    let dir = Scratch::new("move-spellings");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"thanks.html\">a</a> <a href=\"Thanks.html\">b</a> <a href=\"thanks%2Ehtml#top\">c</a> <a href=\"th%61nks.html#top\">d</a></body></html>");
    dir.write("site/thanks.html", PAGE.as_bytes());
    with_run(|run| {
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        let reasons: Vec<&str> = report["broken_links"].as_array().unwrap().iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert_eq!(reasons, ["case_mismatch"], "{report}");
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
        assert_eq!(moved["receipt"]["changed"][0]["changes"].as_array().unwrap().len(), 3, "{moved}");
        assert_eq!(moved["receipt"]["complete"], false, "{moved}");
        assert_eq!(moved["receipt"]["left_denied"][0]["reference"], "thanks%2Ehtml#top", "{moved}");
        assert_eq!(cmd(run, "web.site_report", json!({}))["broken_links"], json!([]));
    });
    assert_eq!(
        dir.read("site/index.html"),
        "<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"pages/thanks.html\">a</a> <a href=\"pages/thanks.html\">b</a> <a href=\"thanks%2Ehtml#top\">c</a> <a href=\"pages/thanks.html#top\">d</a></body></html>"
    );
}

/// Review F9 (a): a case_mismatch row of the report is repaired by web.change_link from the misspelled name to the
/// file; and the rewrite honours site_relative_link_base site_root (review F14).
#[test]
fn change_link_repairs_a_case_mismatch_and_honours_site_root() {
    let dir = Scratch::new("change-case");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"About.html\">a</a></body></html>");
    dir.write("site/pages/p.html", b"<!DOCTYPE html><html><head><title>p</title></head><body><a href=\"../About.html\">a</a></body></html>");
    dir.write("site/about.html", PAGE.as_bytes());
    with_run(|run| {
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        assert_eq!(report["broken_links"].as_array().unwrap().len(), 2, "{report}");
        cmd(run, "web.site", json!({"site_relative_link_base": "site_root"}));
        cmd(run, "web.change_link", json!({"from": "About.html", "to": "about.html"}));
        assert_eq!(cmd(run, "web.site_report", json!({}))["broken_links"], json!([]));
    });
    assert!(dir.read("site/index.html").contains("<a href=\"/about.html\">a</a>"));
    assert!(dir.read("site/pages/p.html").contains("<a href=\"/about.html\">a</a>"));
}

/// Review F9 (b): a name holding parentheses is written into a CSS url() quoted (in a style sheet with double quotes,
/// in a double-quoted style attribute with single quotes), so the CSS still names the file.
#[test]
fn css_urls_are_quoted_when_a_name_needs_it() {
    let dir = Scratch::new("css-quote");
    dir.write("site/styles/site.css", b"body { background: url(../img/bg.png); }\n");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title><link rel=\"stylesheet\" href=\"styles/site.css\"></head><body style=\"background: url(img/bg.png)\">i</body></html>");
    dir.write("site/img/bg.png", b"png");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.move_file", json!({"from": "img/bg.png", "to": "img/bg(1).png"}));
        assert_eq!(cmd(run, "web.site_report", json!({}))["broken_links"], json!([]));
    });
    assert_eq!(dir.read("site/styles/site.css"), "body { background: url(\"../img/bg(1).png\"); }\n");
    assert!(dir.read("site/index.html").contains("style=\"background: url('img/bg(1).png')\""), "{}", dir.read("site/index.html"));
}

/// The template of the template tests: all five region kinds ([STU-WEB-091]), three parameters, an evaluated and a
/// pass-through expression ([STU-WEB-092]) and a link relative to the template's own folder.
const TEMPLATE: &str = "<!DOCTYPE html>
<html lang=\"en\">
<head>
<!-- TemplateBeginEditable name=\"doctitle\" --><title>Harbour</title><!-- TemplateEndEditable -->
<!-- TemplateParam name=\"showNews\" type=\"boolean\" value=\"true\" -->
<!-- TemplateParam name=\"showAside\" type=\"boolean\" value=\"false\" -->
<!-- TemplateParam name=\"brand\" type=\"text\" value=\"Harbour & Co\" -->
<link rel=\"stylesheet\" href=\"../styles/site.css\">
</head>
<body>
<h1><!-- TemplateExpr expr=\"brand\" --></h1>
<!-- TemplateBeginEditable name=\"main\" --><p id=\"lead\">Main</p><!-- TemplateEndEditable -->
<!-- TemplateBeginIf cond=\"showNews\" --><p class=\"news\">News</p><!-- TemplateEndIf -->
<!-- TemplateBeginIf cond=\"showAside\" --><aside><!-- TemplateBeginEditable name=\"aside\" -->Aside<!-- TemplateEndEditable --></aside><!-- TemplateEndIf -->
<ul><!-- TemplateBeginRepeat name=\"items\" --><li><!-- TemplateBeginEditable name=\"item\" -->Item<!-- TemplateEndEditable --></li><!-- TemplateEndRepeat --></ul>
<table><!-- TemplateBeginRepeat name=\"rows\" --><tr><td><!-- TemplateBeginEditable name=\"cell\" --><span>Cell</span><!-- TemplateEndEditable --></td></tr><!-- TemplateEndRepeat --></table>
<p><!-- TemplatePassthroughExpr expr=\"<?php echo $user; ?>\" --></p>
</body>
</html>
";

fn template_site(dir: &Scratch) {
    dir.write("site/Templates/main.dwt", TEMPLATE.as_bytes());
    dir.write("site/styles/site.css", b"body { color: black; }\n");
}

/// [STU-WEB-090] to [STU-WEB-092]: a template's regions carry the five kinds of the closed enumeration; an instance is
/// generated with its links re-based, the evaluated expression escaped and the pass-through expression verbatim; an
/// optional region is shown or hidden and repeat entries are added, moved and removed per instance, each one named
/// history step, with editable content kept; everything survives save and reopen.
#[test]
fn a_template_instance_carries_all_five_region_kinds() {
    let dir = Scratch::new("template");
    template_site(&dir);
    with_run(|run| {
        define(run, &dir);
        let regions = cmd(run, "web.template_regions", json!({"path": "Templates/main.dwt"}));
        assert_eq!(regions["region_kinds"], json!(["editable", "optional", "editable_optional", "repeating", "repeating_table"]));
        let kinds: Vec<(&str, &str)> = regions["regions"].as_array().unwrap().iter().map(|r| (r["name"].as_str().or(r["cond"].as_str()).unwrap(), r["kind"].as_str().unwrap())).collect();
        assert_eq!(
            kinds,
            [
                ("doctitle", "editable"),
                ("main", "editable"),
                ("showNews", "optional"),
                ("showAside", "editable_optional"),
                ("aside", "editable"),
                ("items", "repeating"),
                ("item", "editable"),
                ("rows", "repeating_table"),
                ("cell", "editable")
            ],
            "{regions}"
        );
        assert_eq!(regions["expressions"], json!([{"form": "evaluated", "code": "brand", "line": 11}, {"form": "pass_through", "code": "<?php echo $user; ?>", "line": 17}]));
        assert_eq!(refused(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "page.html", "params": {"nope": "x"}})), "template_param_missing");
        assert_eq!(refused(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "page.html", "params": {"showNews": "maybe"}})), "parameter_invalid");
        assert_eq!(refused(run, "web.template_instance", json!({"template": "Templates/gone.dwt", "path": "page.html"})), "template_not_found");
        let made = cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "page.html", "params": {"brand": "Bay <Ltd>"}}));
        assert_eq!(made["web"]["dirty"], true, "{made}");
        let page = source(run);
        assert!(page.starts_with("<!DOCTYPE html>\n<html lang=\"en\"><!-- InstanceBegin template=\"/Templates/main.dwt\" locked_sha256=\""), "{page}");
        for part in [
            "<link rel=\"stylesheet\" href=\"styles/site.css\">",
            "<h1>Bay &lt;Ltd&gt;</h1>",
            "<p><?php echo $user; ?></p>",
            "<!-- InstanceParam name=\"brand\" type=\"text\" value=\"Bay <Ltd>\" -->",
            "<!-- InstanceBeginEditable name=\"main\" --><p id=\"lead\">Main</p><!-- InstanceEndEditable -->",
        ] {
            assert!(page.contains(part), "{part} in {page}");
        }
        assert!(page.contains("<p class=\"news\">News</p>") && !page.contains("<aside>") && !page.contains("<!-- Template"), "{page}");
        assert!(
            page.contains("<ul><!-- InstanceBeginRepeat name=\"items\" --><!-- InstanceBeginRepeatEntry --><li><!-- InstanceBeginEditable name=\"item\" -->Item<!-- InstanceEndEditable --></li><!-- InstanceEndRepeatEntry --><!-- InstanceEndRepeat --></ul>"),
            "{page}"
        );
        assert!(page.ends_with("</body>\n<!-- InstanceEnd --></html>\n"), "{page}");
        cmd(run, "web.save", json!({}));
        assert_eq!(dir.read("site/page.html"), page);
        assert_eq!(refused(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "page.html"})), "instance_path_exists");
        let standing = cmd(run, "web.template_regions", json!({}));
        assert_eq!((standing["kind"].as_str(), standing["standing"]["status"].as_str()), (Some("instance"), Some("up_to_date")), "{standing}");

        // Editable content is kept when the instance is regenerated; showing the optional region is one history step.
        cmd(run, "web.set_text", json!({"key": "#lead", "text": "Hello"}));
        let edited = source(run);
        let shown = cmd(run, "web.instance_param", json!({"name": "showAside", "value": "true"}));
        assert_eq!(shown["web"]["undo"].as_array().unwrap().last().unwrap()["label"], "Set Instance Parameter");
        let with_aside = source(run);
        assert!(with_aside.contains("<aside><!-- InstanceBeginEditable name=\"aside\" -->Aside<!-- InstanceEndEditable --></aside>") && with_aside.contains("<p id=\"lead\">Hello</p>"), "{with_aside}");
        assert!(with_aside.contains("<!-- InstanceParam name=\"showAside\" type=\"boolean\" value=\"true\" -->"));
        cmd(run, "web.undo", json!({}));
        assert_eq!(source(run), edited, "undo restores every byte");
        cmd(run, "web.redo", json!({}));
        assert_eq!(refused(run, "web.instance_param", json!({"name": "showAside", "value": "yes"})), "parameter_invalid");
        assert_eq!(refused(run, "web.instance_param", json!({"name": "nope", "value": "true"})), "template_param_missing");

        // Repeat entries: add a table row, edit the first row's cell, move that row last, remove the only list item.
        cmd(run, "web.instance_repeat", json!({"region": "rows", "op": "add"}));
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "rows", "op": "move", "index": 1, "to": 0})), "nothing_to_change", "two equal rows");
        let span = key_of_tag(run, "span");
        cmd(run, "web.set_text", json!({"key": span, "text": "First"}));
        cmd(run, "web.instance_repeat", json!({"region": "rows", "op": "move", "index": 0, "to": 1}));
        cmd(run, "web.instance_repeat", json!({"region": "items", "op": "remove", "index": 0}));
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "items", "op": "remove", "index": 0})), "parameter_invalid");
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "main", "op": "add"})), "template_region_missing");
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "rows", "op": "spin"})), "parameter_invalid");
        let repeated = source(run);
        assert_eq!(repeated.matches("<tr><td>").count(), 2, "{repeated}");
        assert!(repeated.find("<span>Cell</span>") < repeated.find("<span>First</span>"), "the edited row moved last, its content kept: {repeated}");
        assert!(repeated.contains("<ul><!-- InstanceBeginRepeat name=\"items\" --><!-- InstanceEndRepeat --></ul>"), "{repeated}");
        cmd(run, "web.save", json!({"overwrite": true}));
        cmd(run, "web.open", json!({"path": s(&dir.at("site/page.html"))}));
        let reopened = cmd(run, "web.template_regions", json!({}));
        assert_eq!(reopened["standing"]["status"], "up_to_date", "{reopened}");
        assert_eq!(reopened["instance"]["repeats"], json!({"items": 0, "rows": 2}), "{reopened}");
        assert_eq!(reopened["instance"]["params"]["showAside"], "true");
        assert_eq!(source(run), repeated, "the instance reopens byte for byte");
    });
}

/// [STU-WEB-093]: editing a template updates every instance in one transaction per instance with a receipt naming each
/// changed byte range; an instance whose locked region was edited outside Studio is reported and left byte for byte
/// until the model chooses reapply or detach; an open instance that is rewritten is reopened with a fresh history.
#[test]
fn a_template_update_propagates_and_reports_drift() {
    let dir = Scratch::new("template-update");
    template_site(&dir);
    // `web.save` writes into an existing folder.
    std::fs::create_dir_all(dir.at("site/pages")).unwrap();
    with_run(|run| {
        define(run, &dir);
        for path in ["a.html", "b.html", "pages/c.html"] {
            cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": path}));
            cmd(run, "web.save", json!({}));
        }
        assert!(dir.read("site/pages/c.html").contains("href=\"../styles/site.css\""));
        // Outside Studio: an editable change in a.html (allowed), a locked change in b.html (drift).
        let a = dir.read("site/a.html").replace("<p id=\"lead\">Main</p>", "<p id=\"lead\">A</p>");
        dir.write("site/a.html", a.as_bytes());
        let b = dir.read("site/b.html").replace("<h1>", "<h1 class=\"x\">");
        dir.write("site/b.html", b.as_bytes());
        dir.write("site/Templates/main.dwt", TEMPLATE.replace("</body>", "<footer>Footer</footer>\n</body>").as_bytes());
        cmd(run, "web.open", json!({"path": s(&dir.at("site/a.html"))}));
        let standing = cmd(run, "web.template_regions", json!({"path": "b.html"}));
        assert_eq!((standing["standing"]["status"].as_str(), standing["standing"]["reason"].as_str()), (Some("drifted"), Some("locked_changed")), "{standing}");
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/main.dwt", "resolve": {"index.html": "detach"}})), "not_a_template_instance");
        assert_eq!(refused(run, "web.template_update", json!({"template": "a.html"})), "parameter_invalid");
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/main.dwt", "resolve": {"b.html": "merge"}})), "parameter_invalid");

        let updated = cmd(run, "web.template_update", json!({"template": "Templates/main.dwt"}));
        let receipt = &updated["receipt"];
        assert_eq!(receipt["schema"], "hsk.studio.web.template_receipt@1");
        let outcomes: Vec<(&str, &str)> = receipt["instances"].as_array().unwrap().iter().map(|i| (i["document"].as_str().unwrap(), i["outcome"].as_str().unwrap())).collect();
        assert_eq!(outcomes, [("a.html", "updated"), ("b.html", "drifted"), ("pages/c.html", "updated")], "{receipt}");
        assert_eq!((receipt["updated"].as_u64(), receipt["drifted"].as_u64(), receipt["complete"].as_bool()), (Some(2), Some(1), Some(true)));
        assert_eq!(receipt["instances"][1]["code"], "template_instance_drift");
        assert_eq!(receipt["instances"][1]["remedy"], json!(["reapply", "detach"]));
        assert_eq!(dir.read("site/b.html"), b, "a drifted instance is never silently overwritten");
        let a_now = dir.read("site/a.html");
        assert!(a_now.contains("<footer>Footer</footer>\n</body>") && a_now.contains("<p id=\"lead\">A</p>"), "{a_now}");
        // The receipt names every changed byte range: replaying its ranges on the old bytes gives the new file.
        let changes = &receipt["instances"][0]["changes"];
        assert_eq!(replay(&a, &a_now, changes), a_now, "{receipt}");
        assert!(changes.as_array().unwrap().iter().any(|c| a_now[at(&c["after"], 0)..at(&c["after"], 1)].contains("Footer")), "{receipt}");
        assert_eq!(updated["web"]["undo"], json!([]), "the open instance is reopened with a fresh history");
        assert_eq!(source(run), a_now);

        let resolved = cmd(run, "web.template_update", json!({"template": "Templates/main.dwt", "resolve": {"b.html": "reapply", "pages/c.html": "detach"}}));
        let outcomes: Vec<(&str, &str)> = resolved["receipt"]["instances"].as_array().unwrap().iter().map(|i| (i["document"].as_str().unwrap(), i["outcome"].as_str().unwrap())).collect();
        assert_eq!(outcomes, [("a.html", "up_to_date"), ("b.html", "reapplied"), ("pages/c.html", "detached")], "{resolved}");
        let b_now = dir.read("site/b.html");
        assert!(b_now.contains("<h1>Harbour &amp; Co</h1>") && b_now.contains("<footer>Footer</footer>"), "{b_now}");
        let c_now = dir.read("site/pages/c.html");
        assert!(!c_now.contains("Instance") && c_now.contains("<p id=\"lead\">Main</p>") && c_now.contains("<footer>Footer</footer>"), "{c_now}");
        let last = cmd(run, "web.template_update", json!({"template": "Templates/main.dwt"}));
        assert_eq!(last["receipt"]["instances"].as_array().unwrap().len(), 2, "a detached page is no longer an instance");
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/gone.dwt"})), "template_not_found");
    });
}

fn at(range: &Value, i: usize) -> usize {
    usize::try_from(range[i].as_u64().unwrap()).unwrap()
}

/// `old` with each receipt row's `before` range replaced by the `after` range of `new`.
fn replay(old: &str, new: &str, changes: &Value) -> String {
    let mut out = String::new();
    let mut from = 0;
    for c in changes.as_array().unwrap() {
        out.push_str(&old[from..at(&c["before"], 0)]);
        out.push_str(&new[at(&c["after"], 0)..at(&c["after"], 1)]);
        from = at(&c["before"], 1);
    }
    out.push_str(&old[from..]);
    out
}

/// V4-08: markers are comments the parser found; marker text in a script or attribute is not a marker; spelling and
/// spacing variants; a malformed or unsupported template refuses typed.
#[test]
fn template_markers_are_tokenized_never_substring_matched() {
    let dir = Scratch::new("template-hostile");
    dir.write("site/Templates/decoy.dwt", b"<!DOCTYPE html><html><head><title>d</title><script>var s = \"<!-- TemplateBeginEditable name='x' -->\";</script></head><body title=\"<!-- TemplateBeginEditable name='y' -->\"><!-- templatebegineditable name=\"z\" --><!--TemplateBeginEditable name='w'-->w<!--   TemplateEndEditable   --></body></html>");
    dir.write("site/Templates/open.dwt", b"<!DOCTYPE html><html><head><title>o</title></head><body><!-- TemplateBeginEditable name=\"a\" -->a</body></html>");
    dir.write("site/Templates/expr.dwt", b"<!DOCTYPE html><html><head><title>e</title></head><body><!-- TemplateExpr expr=\"a + b\" --></body></html>");
    dir.write("site/Templates/param.dwt", b"<!DOCTYPE html><html><head><title>p</title></head><body><!-- TemplateBeginIf cond=\"gone\" -->x<!-- TemplateEndIf --></body></html>");
    dir.write("site/Templates/dup.dwt", b"<!DOCTYPE html><html><head><title>p</title></head><body><!-- TemplateBeginEditable name=\"a\" -->1<!-- TemplateEndEditable --><!-- TemplateBeginEditable name=\"a\" -->2<!-- TemplateEndEditable --></body></html>");
    dir.write("site/Templates/attrs.dwt", b"<!DOCTYPE html><html><head><title>p</title></head><body><!-- TemplateBeginEditable name=\"a\" name=\"b\" -->1<!-- TemplateEndEditable --></body></html>");
    with_run(|run| {
        define(run, &dir);
        let decoy = cmd(run, "web.template_regions", json!({"path": "Templates/decoy.dwt"}));
        let names: Vec<&str> = decoy["regions"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["w"], "only the comment spelled exactly is a marker: {decoy}");
        for (path, code) in [
            ("Templates/open.dwt", "template_markup_invalid"),
            ("Templates/expr.dwt", "template_expression_unsupported"),
            ("Templates/param.dwt", "template_param_missing"),
            ("Templates/dup.dwt", "template_region_duplicate"),
            ("Templates/attrs.dwt", "template_markup_invalid"),
        ] {
            assert_eq!(refused(run, "web.template_regions", json!({"path": path})), code, "{path}");
        }
        assert_eq!(run.ops().last().unwrap()["target"], "Templates/attrs.dwt:1");
        assert_eq!(refused(run, "web.template_regions", json!({"path": "../x.dwt"})), "move_outside_site");
    });
}

/// [STU-WEB-096]: a library item is a fragment in its own `.lbi` document; inserting it is one history step with its
/// links re-based; editing the item updates every reference, re-based per document, in one transaction with a
/// receipt; a template and a library item stay different mechanisms; a referenced item is not orphaned.
#[test]
fn library_items_insert_and_update_every_reference() {
    let dir = Scratch::new("library");
    dir.write("site/Library/nav.lbi", b"<nav><a href=\"../index.html\">Home</a> <img alt=\"logo\" src=\"../img/logo.png\"></nav>");
    let index = dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body>\n<p>x</p>\n</body></html>");
    dir.write("site/pages/a.html", b"<!DOCTYPE html><html><head><title>a</title></head><body><!-- #BeginLibraryItem \"/Library/nav.lbi\" --><nav>old</nav><!-- #EndLibraryItem --></body></html>");
    dir.write("site/img/logo.png", b"png");
    dir.write("site/Library/spare.lbi", b"<p>spare</p>");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        define(run, &dir);
        let body = key_of_tag(run, "body");
        assert_eq!(refused(run, "web.library_item_insert", json!({"item": "Library/gone.lbi", "parent": body})), "library_item_not_found");
        assert_eq!(refused(run, "web.library_item_insert", json!({"item": "Templates/x.dwt", "parent": body})), "parameter_invalid");
        let original = source(run);
        let inserted = cmd(run, "web.library_item_insert", json!({"item": "Library/nav.lbi", "parent": body, "index": 0}));
        assert_eq!(inserted["web"]["undo"].as_array().unwrap().last().unwrap()["label"], "Insert Library Item");
        let with_item = source(run);
        assert_eq!(
            with_item,
            "<!DOCTYPE html><html><head><title>i</title></head><body><!-- #BeginLibraryItem \"/Library/nav.lbi\" --><nav><a href=\"index.html\">Home</a> <img alt=\"logo\" src=\"img/logo.png\"></nav><!-- #EndLibraryItem -->\n<p>x</p>\n</body></html>"
        );
        cmd(run, "web.undo", json!({}));
        assert_eq!(source(run), original);
        cmd(run, "web.redo", json!({}));
        cmd(run, "web.save", json!({"overwrite": true}));
        let report = cmd(run, "web.site_report", json!({}));
        let orphans: Vec<&str> = report["orphaned_files"].as_array().unwrap().iter().map(|o| o["file"].as_str().unwrap()).collect();
        assert_eq!(orphans, ["Library/spare.lbi"], "{report}");

        dir.write("site/Library/nav.lbi", b"<nav><a href=\"../index.html\">Home</a> <a href=\"../pages/a.html\">A</a></nav>");
        let updated = cmd(run, "web.library_item_update", json!({"item": "Library/nav.lbi"}));
        let receipt = &updated["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["complete"].as_bool()), (Some("hsk.studio.web.library_receipt@1"), Some(true)), "{receipt}");
        let docs: Vec<&str> = receipt["changed"].as_array().unwrap().iter().map(|c| c["document"].as_str().unwrap()).collect();
        assert_eq!(docs, ["index.html", "pages/a.html"]);
        assert_eq!(updated["web"]["undo"], json!([]), "the open document is reopened from disk");
        let again = cmd(run, "web.library_item_update", json!({"item": "Library/nav.lbi"}));
        assert_eq!(again["receipt"]["changed"], json!([]), "nothing left to change");
    });
    assert!(dir.read("site/index.html").contains("<!-- #BeginLibraryItem \"/Library/nav.lbi\" --><nav><a href=\"index.html\">Home</a> <a href=\"pages/a.html\">A</a></nav><!-- #EndLibraryItem -->"));
    assert!(dir.read("site/pages/a.html").contains("<!-- #BeginLibraryItem \"/Library/nav.lbi\" --><nav><a href=\"../index.html\">Home</a> <a href=\"a.html\">A</a></nav><!-- #EndLibraryItem -->"));
    // An unbalanced marker in any document refuses the update before anything is written.
    dir.write("site/broken.html", b"<!DOCTYPE html><html><head><title>b</title></head><body><!-- #BeginLibraryItem \"/Library/nav.lbi\" -->x</body></html>");
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.library_item_update", json!({"item": "Library/nav.lbi"})), "library_markup_invalid");
        assert_eq!(run.ops().last().unwrap()["target"], "broken.html:1");
    });
}

/// [STU-WEB-097] to [STU-WEB-099], [STU-WEB-161]; review F4, lead ruling option A: the snippet library is ONE site-level
/// resource, `<local_root>/studio-snippets.json`, written through at each web.snippet (atomic, under the site lock, with
/// a receipt of the snippet before and after; no web.undo step: the reverse web.snippet is the way back). Triggers are
/// unique site-wide (from any page of the site); insert texts are verbatim; a block, a wrap and a trigger expansion are
/// named history steps of the document; an unnamed snippet read back is reported, not dropped.
#[test]
fn snippets_are_one_site_level_resource() {
    let dir = Scratch::new("snippets");
    let page = dir.write("site/s.html", b"<!DOCTYPE html><html><head><title>s</title></head><body>\nword ul\n</body></html>");
    let about = dir.write("site/about.html", PAGE.as_bytes());
    let library = dir.at("site/studio-snippets.json");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&page)}));
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "x"})), "no_site");
        define(run, &dir);
        let bullets = json!({"folder_path": "Markup/Lists", "snippet_name": "Bullets", "snippet_kind": "block", "inserts_before_selection": "<ul>\n  <li>", "inserts_after_selection": "</li>\n</ul>\n", "trigger": "ul"});
        let steps = run.web_state()["undo"].as_array().unwrap().len();
        let added = cmd(run, "web.snippet", bullets);
        let receipt = &added["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["library"].as_str(), receipt["before"].clone()), (Some("hsk.studio.web.snippet_receipt@1"), Some("studio-snippets.json"), Value::Null), "{added}");
        assert_eq!(receipt["after"]["inserts_before_selection"], "<ul>\n  <li>");
        assert!(library.is_file(), "written through at the step");
        assert_eq!(added["web"]["undo"].as_array().unwrap().len(), steps, "not a web.undo step");
        assert!(run.web_state()["site"].get("snippets").is_none(), "the site record carries no snippets (no insert text in every result)");
        let bold = json!({"folder_path": "Markup", "snippet_name": "Bold", "snippet_kind": "wrap", "inserts_before_selection": "<b>", "inserts_after_selection": "</b>", "trigger": "ul"});
        assert_eq!(refused(run, "web.snippet", bold), "snippet_trigger_duplicate");
        let record = run.ops().last().unwrap().clone();
        assert_eq!(record["detail"], json!({"trigger": "ul", "snippets": ["Markup/Lists/Bullets", "Markup/Bold"], "severity": "error"}), "{record}");
        cmd(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Bold", "snippet_kind": "wrap", "inserts_before_selection": "<b>", "inserts_after_selection": "</b>", "trigger": "b"}));
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Big", "inserts_before_selection": "x".repeat(16 * 1024 + 1)})), "snippet_text_too_large");
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Bad", "snippet_kind": "inline"})), "parameter_invalid");
        let listed = cmd(run, "web.snippets", json!({}));
        assert_eq!(listed["groups"], json!([{"top_level_group": "Markup", "count": 2, "block": 1, "wrap": 1}]), "{listed}");
        assert_eq!(listed["triggers"], json!({"b": "Markup/Bold", "ul": "Markup/Lists/Bullets"}));

        // Wrap "word"; expand "ul" before the cursor; both verbatim and undoable in the document.
        let text = source(run);
        let word = text.find("word").unwrap();
        cmd(run, "web.snippet_insert", json!({"folder_path": "Markup", "snippet_name": "Bold", "offset": word, "end": word + 4}));
        let text = source(run);
        let after_ul = text.find(" ul").unwrap() + 3;
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "ul", "offset": after_ul - 1})), "snippet_trigger_not_at_offset");
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "ul", "offset": text.len() + 1})), "parameter_invalid");
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "zz", "offset": after_ul})), "no_such_snippet");
        cmd(run, "web.snippet_insert", json!({"trigger": "ul", "offset": after_ul}));
        assert_eq!(source(run), "<!DOCTYPE html><html><head><title>s</title></head><body>\n<b>word</b> <ul>\n  <li></li>\n</ul>\n\n</body></html>");
        cmd(run, "web.undo", json!({}));
        cmd(run, "web.redo", json!({}));
        cmd(run, "web.save", json!({"overwrite": true}));

        // Another page of the same site sees the same library; a duplicate trigger from there is refused too.
        cmd(run, "web.open", json!({"path": s(&about)}));
        define(run, &dir);
        assert_eq!(cmd(run, "web.snippets", json!({}))["snippets_total"], 2);
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Underline", "trigger": "ul"})), "snippet_trigger_duplicate");
        // The reverse web.snippet is the way back; its receipt names the snippet before.
        let removed = cmd(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Bold", "remove": true}));
        assert_eq!((removed["receipt"]["before"]["snippet_name"].as_str(), removed["receipt"]["after"].clone()), (Some("Bold"), Value::Null));
        // The library is reserved: never orphaned, moved, linked to or synchronised.
        let report = cmd(run, "web.site_report", json!({}));
        assert!(!report["orphaned_files"].to_string().contains("studio-snippets.json"), "{report}");
        assert_eq!(refused(run, "web.move_file", json!({"from": "studio-snippets.json", "to": "x.json"})), "move_reserved_file");
        assert_eq!(refused(run, "web.change_link", json!({"from": "a.html", "to": "studio-snippets.json"})), "link_target_reserved");
        cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("remote"))}));
        let plan = cmd(run, "web.synchronize", json!({"direction": "put"}));
        assert!(!plan["actions"].to_string().contains("studio-snippets.json"), "{plan}");
    });
    // An unnamed snippet in the library file is reported on read, never dropped.
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&library).unwrap()).unwrap();
    assert_eq!(saved["schema"], "hsk.studio.web.snippets@1");
    saved["snippets"].as_array_mut().unwrap().push(json!({"snippet_name": "", "folder_path": "Scripts", "snippet_kind": "block", "preview_mode": "code", "inserts_before_selection": "x", "inserts_after_selection": "", "description": "", "trigger": null}));
    std::fs::write(&library, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
    with_run(|run| {
        define(run, &dir);
        let listed = cmd(run, "web.snippets", json!({}));
        assert_eq!(listed["snippets_total"], 2, "{listed}");
        assert_eq!(listed["diagnostics"], json!([{"code": "snippet_unnamed", "severity": "warning", "folder_path": "Scripts"}]));
        assert_eq!(listed["snippets"][1]["inserts_before_selection"], Value::Null, "the listing carries no insert text");
    });
}

/// Lead ruling F4 conditions: a file already named studio-snippets.json that is not the library, a corrupt library or
/// an unreadable one is refused typed and never overwritten; a per-document record that still carries snippets (the
/// shape before this ruling) is refused on open with a clear code, never migrated silently.
#[test]
fn a_foreign_or_corrupt_snippet_library_is_never_overwritten() {
    let dir = Scratch::new("snippet-library");
    dir.write("site/index.html", PAGE.as_bytes());
    let add = json!({"folder_path": "Markup", "snippet_name": "B", "inserts_before_selection": "<b>"});
    for (bytes, code) in [
        (&b"my own notes"[..], "snippet_library_foreign"),
        (&b"{\"title\": \"user data\"}"[..], "snippet_library_foreign"),
        (&b"{\"schema\": \"hsk.studio.web.snippets@1\", \"snippets\": \"x\"}"[..], "snippet_library_invalid"),
    ] {
        dir.write("site/studio-snippets.json", bytes);
        with_run(|run| {
            define(run, &dir);
            assert_eq!(refused(run, "web.snippet", add.clone()), code);
            assert_eq!(refused(run, "web.snippets", json!({})), code);
        });
        assert_eq!(std::fs::read(dir.at("site/studio-snippets.json")).unwrap(), bytes, "{code}: never overwritten");
    }
    std::fs::remove_file(dir.at("site/studio-snippets.json")).unwrap();
    std::fs::create_dir_all(dir.at("site/studio-snippets.json")).unwrap();
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.snippet", add.clone()), "snippet_library_unreadable");
    });
    std::fs::remove_dir(dir.at("site/studio-snippets.json")).unwrap();
    // A record from before the ruling, with snippets in the per-document sidecar.
    let index = dir.at("site/index.html");
    let record = json!({"schema": "hsk.studio.web.site_record@1", "recipe": null, "site": {"site_id": "site", "display_name": "s", "local_root": s(&dir.at("site")), "site_relative_link_base": "document", "cache_enabled": false, "remote_targets": [], "snippets": [{"snippet_name": "B", "folder_path": "Markup", "snippet_kind": "block", "preview_mode": "code", "inserts_before_selection": "<b>", "inserts_after_selection": "", "description": "", "trigger": null}]}});
    dir.write("site/index.html.studio-site.json", serde_json::to_vec(&record).unwrap().as_slice());
    with_run(|run| {
        assert_eq!(refused(run, "web.open", json!({"path": s(&index)})), "site_record_invalid");
        assert!(run.ops().last().unwrap()["target"].as_str().unwrap().starts_with("snippets_moved_to_site_library"), "{}", run.ops().last().unwrap());
    });
}

/// [STU-WEB-113]: the site record carries every optional field, typed and refused typed, undone as one step and read
/// back after save and reopen; a secret named anywhere inside a field refuses `credential_in_site_record` naming the
/// field path only.
#[test]
fn the_site_record_carries_every_optional_field() {
    let dir = Scratch::new("site-record");
    let page = dir.write("site/index.html", PAGE.as_bytes());
    let fields = json!({
        "testing_target": {"target_id": "test", "transport": "local_or_network", "root_path": s(&dir.at("testing"))},
        "server_model_id": "php_mysql",
        "preprocessor_config": {"source_root": "scss", "output_root": "css", "output_style": "compressed", "source_maps": true},
        "framework_pin": {"framework_version": "5.3.8", "helper_library_version": "3.7.1", "positioning_helper_version": "2.11.8"},
        "linter_overrides": {"missing_alt": "error", "doctype_missing": "off"},
        "browser_baseline": ["chrome-120", "firefox-115"],
        "version_control_binding": {"repository": "https://example.org/site.git", "branch": "main", "credentials_ref": "vault:git"},
    });
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&page)}));
        define(run, &dir);
        for (params, code, target) in [
            (json!({"server_model_id": "ruby"}), "server_model_unknown", "ruby"),
            (json!({"framework_pin": {"framework_version": "9.9.9", "helper_library_version": "1.0.0"}}), "framework_version_unsupported", "9.9.9"),
            (json!({"testing_target": {"target_id": "t", "root_path": "x", "Password": "p"}}), "credential_in_site_record", "testing_target.Password"),
            (json!({"version_control_binding": {"repository": "r", "token": "t"}}), "credential_in_site_record", "version_control_binding.token"),
            (json!({"preprocessor_config": {"source_root": "../x", "output_root": "css", "output_style": "compressed"}}), "parameter_invalid", "preprocessor_config.source_root"),
            (json!({"preprocessor_config": {"source_root": "a", "output_root": "css", "output_style": "c", "extra": 1}}), "parameter_unknown", "preprocessor_config.extra"),
            (json!({"browser_baseline": ["a", "a"]}), "parameter_invalid", "browser_baseline a"),
            (json!({"linter_overrides": {"missing_alt": "fatal"}}), "parameter_invalid", "linter_overrides.missing_alt"),
        ] {
            assert_eq!(refused(run, "web.site", params.clone()), code, "{params}");
            assert_eq!(run.ops().last().unwrap()["target"], target, "{params}");
        }
        let set = cmd(run, "web.site", fields.clone());
        let site = &set["web"]["site"];
        for (key, value) in fields.as_object().unwrap() {
            if key == "testing_target" {
                assert_eq!(site[key]["target_id"], "test");
                assert_eq!(site[key]["root_path"], value["root_path"]);
            } else {
                assert_eq!(&site[key], value, "{key}");
            }
        }
        cmd(run, "web.undo", json!({}));
        assert_eq!(run.web_state()["site"]["server_model_id"], Value::Null);
        cmd(run, "web.redo", json!({}));
        cmd(run, "web.site", json!({"browser_baseline": null}));
        assert_eq!(run.web_state()["site"]["browser_baseline"], Value::Null, "null clears an optional field");
        cmd(run, "web.save", json!({"overwrite": true}));
        let saved = run.web_state()["site"].clone();
        cmd(run, "web.open", json!({"path": s(&page)}));
        assert_eq!(run.web_state()["site"], saved, "the site record reads back field for field");
        assert!(saved.get("snippets").is_none(), "snippets are not a field of the per-document record");
    });
}

/// Sets a file's modification time `secs_ago` seconds before now.
fn age(path: &Path, secs_ago: u64) {
    let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
}

fn actions_of(plan: &Value) -> Vec<(String, String)> {
    plan["actions"].as_array().unwrap().iter().map(|a| (a["path"].as_str().unwrap().to_string(), a["action"].as_str().unwrap().to_string())).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

/// [STU-WEB-119] (lead rulings Q4, Q6, F13): put and get compare by size, modification time and, for copies of one
/// size within 2 s, content; the action list is reviewed and applied only with the review's plan digest (a changed
/// plan refuses `sync_plan_changed`); each file is its own transaction with a per-file receipt and the source time kept;
/// a newer copy is never overwritten; the files a chosen page loads are an explicit option, listed by default;
/// two-way synchronisation is not offered (no sync baseline yet).
#[test]
fn synchronize_reviews_then_applies_per_file() {
    let dir = Scratch::new("sync");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><img alt=\"a\" src=\"img/a.png\"><a href=\"other.html\">o</a></body></html>");
    dir.write("site/other.html", PAGE.as_bytes());
    dir.write("site/img/a.png", b"png");
    age(&dir.write("remote/index.html", b"old"), 3600);
    dir.write("remote/notes.txt", b"remote only");
    age(&dir.at("site/other.html"), 7200);
    age(&dir.write("remote/other.html", b"newer remote copy"), 60);
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("remote"))}));
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "sideways"})), "parameter_invalid");
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "both"})), "sync_two_way_unsupported");
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "dependents": "maybe"})), "parameter_invalid");
        let put = cmd(run, "web.synchronize", json!({"direction": "put"}));
        assert_eq!(put["schema"], "hsk.studio.web.sync_plan@1");
        assert_eq!(actions_of(&put), pairs(&[("img/a.png", "put"), ("index.html", "put"), ("notes.txt", "none"), ("other.html", "remote_newer")]), "{put}");
        assert!(!dir.at("remote/img/a.png").exists(), "a review writes nothing");
        let get = cmd(run, "web.synchronize", json!({"direction": "get"}));
        assert_eq!(actions_of(&get), pairs(&[("img/a.png", "none"), ("index.html", "local_newer"), ("notes.txt", "get"), ("other.html", "get")]), "{get}");

        // One page chosen: what it loads is listed (prompt), added (include) or left out (exclude); page links never.
        let chosen = cmd(run, "web.synchronize", json!({"direction": "put", "files": ["Index.html"]}));
        assert_eq!((chosen["dependents"].clone(), chosen["actions"].as_array().unwrap().len()), (json!(["img/a.png"]), 1), "a chosen name is matched case-folded: {chosen}");
        let with_deps = cmd(run, "web.synchronize", json!({"direction": "put", "files": ["index.html"], "dependents": "include"}));
        assert_eq!(with_deps["actions"].as_array().unwrap().len(), 2, "{with_deps}");

        // Apply needs the reviewed plan; a plan that changed since is refused.
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "apply": true})), "parameter_missing");
        dir.write("site/fresh.txt", b"new since the review");
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "apply": true, "plan_sha256": put["plan_sha256"]})), "sync_plan_changed");
        std::fs::remove_file(dir.at("site/fresh.txt")).unwrap();
        let applied = cmd(run, "web.synchronize", json!({"direction": "put", "apply": true, "plan_sha256": put["plan_sha256"]}));
        let receipt = &applied["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["transferred"].as_u64(), receipt["complete"].as_bool()), (Some("hsk.studio.web.sync_receipt@1"), Some(2), Some(true)), "{receipt}");
        assert!(receipt["files"].as_array().unwrap().iter().all(|f| f["outcome"] == "transferred" && f["time_kept"] == true), "{receipt}");
        assert_eq!(dir.read("remote/other.html"), "newer remote copy", "a newer copy is never overwritten");
        let get = cmd(run, "web.synchronize", json!({"direction": "get"}));
        cmd(run, "web.synchronize", json!({"direction": "get", "apply": true, "plan_sha256": get["plan_sha256"]}));
        let again = cmd(run, "web.synchronize", json!({"direction": "put"}));
        assert_eq!(again["counts"], json!({"same": 4}), "{again}");
        cmd(run, "web.remote_target", json!({"target_id": "inner", "root_path": s(&dir.at("site/inner"))}));
        assert_eq!(refused(run, "web.synchronize", json!({"target_id": "inner", "direction": "put"})), "target_inside_site");
    });
    assert_eq!(dir.read("remote/index.html"), dir.read("site/index.html"));
    assert_eq!(dir.read("site/notes.txt"), "remote only");
    assert_eq!(dir.read("site/other.html"), "newer remote copy");
    assert_eq!(std::fs::read(dir.at("remote/img/a.png")).unwrap(), b"png");
}

/// Review F1 (lead ruling): names are compared case-folded, as the target's file system folds them; a case-only pair
/// (Index.html against index.html) refuses `sync_case_conflict` listing the pair, and neither copy changes.
#[test]
fn a_case_only_pair_refuses_and_changes_nothing() {
    let dir = Scratch::new("sync-case");
    age(&dir.write("site/Index.html", b"three days old"), 3 * 86400);
    age(&dir.write("remote/index.html", b"edited an hour ago"), 3600);
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("remote"))}));
        for direction in ["put", "get"] {
            assert_eq!(refused(run, "web.synchronize", json!({"direction": direction})), "sync_case_conflict", "{direction}");
            assert_eq!(run.ops().last().unwrap()["detail"]["pairs"], json!([{"local": ["Index.html"], "remote": ["index.html"]}]));
        }
    });
    assert_eq!(dir.read("remote/index.html"), "edited an hour ago");
    assert_eq!(dir.read("site/Index.html"), "three days old");
    let names: Vec<String> = std::fs::read_dir(dir.at("remote")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["index.html"]);
}

/// Review F13, F14: copies of one size within 2 s whose bytes differ are a conflict, never `same`; a put of the open
/// document with unsaved edits refuses `document_unsaved` (remedy web.save); a path through a link on either side is
/// `denied_link` and never applied; a copy that cannot be written refuses `sync_failed` with the per-file receipt,
/// after the other files.
#[test]
fn sync_conflicts_unsaved_documents_links_and_failures_are_typed() {
    let dir = Scratch::new("sync-edges");
    let index = dir.write("site/index.html", PAGE.as_bytes());
    dir.write("site/a.txt", b"aaaa");
    dir.write("remote/a.txt", b"bbbb");
    let now = std::time::SystemTime::now();
    for p in ["site/a.txt", "remote/a.txt"] {
        std::fs::File::options().write(true).open(dir.at(p)).unwrap().set_modified(now).unwrap();
    }
    dir.write("outside/x.png", b"x");
    dir.write("remote/media/x.png", b"remote x");
    link_dir(&dir.at("outside"), &dir.at("site/media"));
    dir.write("site/z.txt", b"z");
    // The atomic writer's temporary sibling of remote/z.txt cannot be created: a folder holds its name.
    std::fs::create_dir_all(dir.at(&format!("remote/.z.txt.studio-tmp-{}", std::process::id()))).unwrap();
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        define(run, &dir);
        cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("remote"))}));
        let put = cmd(run, "web.synchronize", json!({"direction": "put"}));
        let actions = actions_of(&put);
        assert!(actions.contains(&("a.txt".into(), "conflict".into())), "{put}");
        assert!(actions.contains(&("media/x.png".into(), "denied_link".into())), "{put}");
        let body = key_of_tag(run, "body");
        cmd(run, "web.set_text", json!({"key": body, "text": "edited"}));
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "apply": true, "plan_sha256": put["plan_sha256"]})), "document_unsaved");
        assert_eq!(run.ops().last().unwrap()["detail"]["remedy"], "web.save");
        cmd(run, "web.undo", json!({}));
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "apply": true, "plan_sha256": put["plan_sha256"]})), "sync_failed");
        let receipt = run.ops().last().unwrap()["detail"]["receipt"].clone();
        let outcomes: Vec<(&str, &str)> = receipt["files"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap(), f["outcome"].as_str().unwrap())).collect();
        assert_eq!(outcomes, [("index.html", "transferred"), ("z.txt", "failed")], "{receipt}");
        assert_eq!(receipt["complete"], false);
    });
    assert_eq!(dir.read("remote/a.txt"), "bbbb", "a conflict is never applied");
    assert_eq!(dir.read("remote/media/x.png"), "remote x");
    assert_eq!(dir.read("outside/x.png"), "x", "nothing went through the link");
}

/// Links `link` to the folder `target`: a folder symlink, else on Windows a junction.
fn link_dir(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return;
        }
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output();
        assert!(made.is_ok_and(|o| o.status.success()) && link.is_dir(), "neither a folder symlink nor a junction could be made at {}", link.display());
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Review F2, lead ruling Q5: authored instance content with no slot in the template (a renamed or removed editable
/// region, repeat or parameter; a region a parameter now hides) refuses template_update_orphans_content listing every
/// orphan, with nothing written, until the caller chooses orphans: move_to:<region> or orphans: discard.
#[test]
fn orphaned_instance_content_is_refused_until_the_caller_chooses() {
    let dir = Scratch::new("orphans");
    template_site(&dir);
    with_run(|run| {
        define(run, &dir);
        for (path, text) in [("a.html", "A"), ("b.html", "B")] {
            cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": path}));
            cmd(run, "web.set_text", json!({"key": "#lead", "text": text}));
            cmd(run, "web.save", json!({}));
        }
        // b.html is open: hiding an optional region that holds content needs a choice too.
        cmd(run, "web.instance_param", json!({"name": "showAside", "value": "true"}));
        assert_eq!(refused(run, "web.instance_param", json!({"name": "showAside", "value": "false"})), "template_update_orphans_content");
        assert_eq!(run.ops().last().unwrap()["detail"]["orphans"], json!([{"region": "aside"}]));
        assert_eq!(refused(run, "web.instance_param", json!({"name": "showAside", "value": "false", "orphans": "keep"})), "parameter_invalid");
        cmd(run, "web.instance_param", json!({"name": "showAside", "value": "false", "orphans": "discard"}));
        cmd(run, "web.save", json!({"overwrite": true}));
    });
    let (a, b) = (dir.read("site/a.html"), dir.read("site/b.html"));
    dir.write("site/Templates/main.dwt", TEMPLATE.replace("name=\"main\"", "name=\"content\"").as_bytes());
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/main.dwt"})), "template_update_orphans_content");
        let detail = run.ops().last().unwrap()["detail"].clone();
        assert_eq!(detail["documents"], json!([{"document": "a.html", "orphans": [{"region": "main"}]}, {"document": "b.html", "orphans": [{"region": "main"}]}]), "{detail}");
        assert_eq!((dir.read("site/a.html"), dir.read("site/b.html")), (a.clone(), b.clone()), "nothing written");
        cmd(run, "web.open", json!({"path": s(&dir.at("site/a.html"))}));
        assert_eq!(refused(run, "web.instance_param", json!({"name": "showNews", "value": "false"})), "template_update_orphans_content");
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/main.dwt", "orphans": "move_to:nowhere"})), "parameter_invalid");
        cmd(run, "web.template_update", json!({"template": "Templates/main.dwt", "orphans": "move_to:content"}));
    });
    for (path, text) in [("site/a.html", "A"), ("site/b.html", "B")] {
        let now = dir.read(path);
        assert!(now.contains(&format!("<!-- InstanceBeginEditable name=\"content\" --><p id=\"lead\">{text}</p><!-- InstanceEndEditable -->")), "{now}");
    }
}

/// [STU-WEB-094], [STU-WEB-159] (review F3, lead ruling): every source edit of an instance outside its editable regions
/// refuses template_edit_in_locked_region naming the template, with detach as the remedy: each web edit row,
/// web.snippet_insert and web.library_item_insert; an edit inside an editable region is accepted and the regenerating
/// commands still work.
#[test]
fn edits_in_locked_regions_are_refused() {
    let dir = Scratch::new("locked");
    template_site(&dir);
    dir.write("site/Library/nav.lbi", b"<nav>n</nav>");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "page.html"}));
        cmd(run, "web.save", json!({}));
        cmd(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "B", "inserts_before_selection": "<b>", "inserts_after_selection": "</b>"}));
        let h1 = key_of_tag(run, "h1");
        let link = key_of_tag(run, "link");
        let body = key_of_tag(run, "body");
        let head_at = source(run).find("<link").unwrap();
        for (id, params) in [
            ("web.set_text", json!({"key": h1, "text": "x"})),
            ("web.set_attribute", json!({"key": h1, "name": "class", "value": "x"})),
            ("web.remove_attribute", json!({"key": link, "name": "rel"})),
            ("web.insert_element", json!({"parent": body, "index": 0, "tag": "p"})),
            ("web.insert_fragment", json!({"parent": body, "index": 0, "fragment": "<p>x</p>"})),
            ("web.remove_node", json!({"key": h1})),
            ("web.snippet_insert", json!({"folder_path": "Markup", "snippet_name": "B", "offset": head_at})),
            ("web.library_item_insert", json!({"item": "Library/nav.lbi", "parent": body, "index": 0})),
        ] {
            assert_eq!(refused(run, id, params), "template_edit_in_locked_region", "{id}");
            let detail = run.ops().last().unwrap()["detail"].clone();
            assert_eq!((detail["template"].as_str(), detail["remedy"].clone()), (Some("Templates/main.dwt"), json!(["detach"])), "{id}: {detail}");
        }
        cmd(run, "web.set_text", json!({"key": "#lead", "text": "inside"}));
        cmd(run, "web.instance_param", json!({"name": "showNews", "value": "false"}));
        let lead = source(run).find("<p id=\"lead\">").unwrap() + "<p id=\"lead\">".len();
        cmd(run, "web.snippet_insert", json!({"folder_path": "Markup", "snippet_name": "B", "offset": lead}));
        assert!(source(run).contains("<p id=\"lead\"><b></b>inside</p>"));
    });
}

/// Review F5: optional regions nest at most 64 deep; a deeper template refuses template_nesting_too_deep naming the
/// line, never recursing without bound; a template at the bound reads.
#[test]
fn region_nesting_is_bounded() {
    let dir = Scratch::new("nesting");
    let nested = |n: usize| {
        format!(
            "<!DOCTYPE html><html><head><title>d</title></head><body>\n<!-- TemplateParam name=\"b\" type=\"boolean\" value=\"true\" -->\n{}x{}</body></html>",
            "<!-- TemplateBeginIf cond=\"b\" -->".repeat(n),
            "<!-- TemplateEndIf -->".repeat(n)
        )
    };
    dir.write("site/Templates/deep.dwt", nested(2047).as_bytes());
    dir.write("site/Templates/edge.dwt", nested(64).as_bytes());
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.template_regions", json!({"path": "Templates/deep.dwt"})), "template_nesting_too_deep");
        assert_eq!(run.ops().last().unwrap()["target"], "Templates/deep.dwt:3");
        assert_eq!(cmd(run, "web.template_regions", json!({"path": "Templates/edge.dwt"}))["regions"].as_array().unwrap().len(), 64);
    });
}

/// Review F6 (lead ruling): a template or library item named in another letter case by an instance or a document is
/// never skipped: the instance or reference is processed and the case variant is reported in the receipt, and the
/// site report counts it as referenced. Review F14: an instance whose digest is stale but whose locked text is current
/// is `baseline_refreshed`.
#[test]
fn case_variant_markers_are_processed_and_reported() {
    let dir = Scratch::new("case-markers");
    template_site(&dir);
    dir.write("site/Library/nav.lbi", b"<nav>new</nav>");
    dir.write("site/doc.html", b"<!DOCTYPE html><html><head><title>d</title></head><body><!-- #BeginLibraryItem \"/library/Nav.lbi\" --><nav>old</nav><!-- #EndLibraryItem --></body></html>");
    with_run(|run| {
        define(run, &dir);
        for path in ["old.html", "stale.html"] {
            cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": path}));
            cmd(run, "web.save", json!({}));
        }
    });
    let old = dir.read("site/old.html").replace("template=\"/Templates/main.dwt\"", "template=\"/templates/Main.dwt\"");
    dir.write("site/old.html", old.as_bytes());
    let stale = dir.read("site/stale.html");
    let at = stale.find("locked_sha256=\"").unwrap() + "locked_sha256=\"".len();
    dir.write("site/stale.html", format!("{}{}{}", &stale[..at], "0".repeat(64), &stale[at + 64..]).as_bytes());
    with_run(|run| {
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        assert!(!report["orphaned_files"].to_string().contains("nav.lbi"), "{report}");
        let updated = cmd(run, "web.template_update", json!({"template": "Templates/main.dwt"}));
        let rows: Vec<(String, String, Value)> = updated["receipt"]["instances"].as_array().unwrap().iter().map(|i| (i["document"].as_str().unwrap().to_string(), i["outcome"].as_str().unwrap().to_string(), i["case_mismatch"].clone())).collect();
        assert_eq!(rows, [("old.html".to_string(), "up_to_date".to_string(), json!("/templates/Main.dwt")), ("stale.html".to_string(), "baseline_refreshed".to_string(), Value::Null)], "{updated}");
        let item = cmd(run, "web.library_item_update", json!({"item": "Library/nav.lbi"}));
        assert_eq!(item["receipt"]["changed"][0]["case_mismatch"], json!(["/library/Nav.lbi"]), "{item}");
    });
    assert!(dir.read("site/doc.html").contains("<nav>new</nav>"));
    assert!(!dir.read("site/stale.html").contains(&"0".repeat(64)), "the digest was refreshed");
}

/// Review F12 (lead ruling): the site-wide updates stay atomic: a malformed marker in any page refuses the whole update
/// naming every malformed page, before anything is written.
#[test]
fn malformed_pages_are_all_named() {
    let dir = Scratch::new("malformed");
    template_site(&dir);
    dir.write("site/Library/nav.lbi", b"<nav>n</nav>");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": "a.html"}));
        cmd(run, "web.save", json!({}));
    });
    dir.write("site/notes.html", b"<!DOCTYPE html><html><head><title>n</title></head><body><!-- InstanceEnd of the main column --></body></html>");
    dir.write("site/other.html", b"<!DOCTYPE html><html><head><title>o</title></head><body><!-- #BeginLibraryItem /Library/nav.lbi --></body></html>");
    dir.write("site/Templates/main.dwt", TEMPLATE.replace("</body>", "<footer>f</footer>\n</body>").as_bytes());
    let a = dir.read("site/a.html");
    with_run(|run| {
        define(run, &dir);
        for (id, params, code) in [
            ("web.template_update", json!({"template": "Templates/main.dwt"}), "template_markup_invalid"),
            ("web.library_item_update", json!({"item": "Library/nav.lbi"}), "library_markup_invalid"),
        ] {
            assert_eq!(refused(run, id, params), code, "{id}");
            assert_eq!(run.ops().last().unwrap()["detail"]["documents"], json!(["notes.html:1", "other.html:1"]), "{id}");
        }
    });
    assert_eq!(dir.read("site/a.html"), a, "nothing written");
}

/// Review F14: a write that fails restores every file already written (change link, library item update) from the
/// bytes read before; a template update stops at the failing instance and the instances before it are complete; each
/// refusal carries its receipt.
#[test]
fn failed_site_writes_restore_or_stop_with_a_receipt() {
    let dir = Scratch::new("restore");
    template_site(&dir);
    let link = b"<!DOCTYPE html><html><head><title>l</title></head><body><a href=\"old.html\">o</a></body></html>";
    dir.write("site/a.html", link);
    dir.write("site/b.html", link);
    dir.write("site/news.html", PAGE.as_bytes());
    dir.write("site/Library/nav.lbi", b"<nav>new</nav>");
    let item = b"<!DOCTYPE html><html><head><title>i</title></head><body><!-- #BeginLibraryItem \"/Library/nav.lbi\" --><nav>old</nav><!-- #EndLibraryItem --></body></html>";
    dir.write("site/c.html", item);
    dir.write("site/d.html", item);
    with_run(|run| {
        define(run, &dir);
        for path in ["e.html", "f.html"] {
            cmd(run, "web.template_instance", json!({"template": "Templates/main.dwt", "path": path}));
            cmd(run, "web.save", json!({}));
        }
    });
    dir.write("site/Templates/main.dwt", TEMPLATE.replace("</body>", "<footer>f</footer>\n</body>").as_bytes());
    let pid = std::process::id();
    for name in ["b.html", "d.html", "f.html"] {
        std::fs::create_dir_all(dir.at(&format!("site/.{name}.studio-tmp-{pid}"))).unwrap();
    }
    let before: Vec<String> = ["a.html", "b.html", "c.html", "d.html", "f.html"].iter().map(|f| dir.read(&format!("site/{f}"))).collect();
    with_run(|run| {
        define(run, &dir);
        for (id, params, code) in [
            ("web.change_link", json!({"from": "old.html", "to": "news.html"}), "change_link_failed"),
            ("web.library_item_update", json!({"item": "Library/nav.lbi"}), "library_update_failed"),
        ] {
            assert_eq!(refused(run, id, params), code, "{id}");
            let receipt = run.ops().last().unwrap()["detail"]["receipt"].clone();
            assert_eq!(receipt["complete"], false, "{id}: {receipt}");
            assert_eq!(receipt["restored"].as_array().unwrap().len(), 1, "{id}: {receipt}");
        }
        assert_eq!(refused(run, "web.template_update", json!({"template": "Templates/main.dwt"})), "template_update_failed");
        let receipt = run.ops().last().unwrap()["detail"]["receipt"].clone();
        assert_eq!((receipt["written"].clone(), receipt["failed"].as_str()), (json!(["e.html"]), Some("f.html")), "{receipt}");
    });
    let after: Vec<String> = ["a.html", "b.html", "c.html", "d.html", "f.html"].iter().map(|f| dir.read(&format!("site/{f}"))).collect();
    assert_eq!(after, before, "every restored file is back and the failed instance untouched");
    assert!(dir.read("site/e.html").contains("<footer>f</footer>"), "the instance before the failure is complete");
}
