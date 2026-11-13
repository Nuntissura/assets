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

/// A job port that admits everything and keeps each admitted action's token.
#[derive(Default)]
struct Actions {
    actions: Mutex<Vec<&'static str>>,
    next: AtomicU64,
}

impl JobPort for Actions {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        self.actions.lock().unwrap().push(scope.action.as_str());
        Ok(JobTicket(self.next.fetch_add(1, Ordering::SeqCst)))
    }

    fn release(&self, _ticket: JobTicket) {}
}

/// Ledger 2026-11-12T15:08:00Z: the site report is a read-only verb, admitted as the session's `read` class; the
/// site-wide writers stay `edit`.
#[test]
fn the_site_report_is_admitted_as_a_read() {
    let dir = Scratch::new("read");
    dir.write("site/index.html", PAGE.as_bytes());
    dir.write("site/a.png", b"png");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    let port = Arc::new(Actions::default());
    run.bind_web_jobs(port.clone(), JobLifecycle::default());
    assert!(run.web_command("web.site", &json!({"display_name": "s", "local_root": s(&dir.at("site"))})));
    assert!(run.web_command("web.site_report", &json!({})), "{}", run.ops().last().unwrap());
    assert!(run.web_command("web.move_file", &json!({"from": "a.png", "to": "b.png"})), "{}", run.ops().last().unwrap());
    assert_eq!(*port.actions.lock().unwrap(), ["read", "edit"]);
}

/// [STU-WEB-118] change-link-site-wide (ledger 2026-11-11T11:09:18Z, 2026-11-12T15:08:00Z): every reference to `from`
/// in any spelling (exact, another letter case, percent-encoded; `from` may be a missing page) is rewritten to `to` in
/// the site's link base, query and fragment kept, as one transaction with a receipt.
#[test]
fn change_link_rewrites_every_spelling_site_wide() {
    let dir = Scratch::new("change-link");
    let index = "<!DOCTYPE html><html><head><title>i</title></head><body>\n<a href=\"old.html\">1</a> <a href=\"Old.html#top\">2</a> <a href=\"old%2Ehtml?q=1\">3</a> <a href=\"news.html\">4</a>\n</body></html>";
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
        assert_eq!(before["denied"], json!([]), "a percent-encoded spelling of a portable name resolves: {before}");
        let changed = cmd(run, "web.change_link", json!({"from": "old.html", "to": "news.html"}));
        let receipt = &changed["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["complete"].as_bool()), (Some("hsk.studio.web.change_link_receipt@1"), Some(true)), "{receipt}");
        let rows: Vec<(&str, usize)> = receipt["changed"].as_array().unwrap().iter().map(|c| (c["document"].as_str().unwrap(), c["changes"].as_array().unwrap().len())).collect();
        assert_eq!(rows, [("index.html", 3), ("pages/p.html", 1)], "{receipt}");
        let first = &receipt["changed"][0]["changes"][1];
        assert_eq!((first["reference_before"].as_str(), first["reference_after"].as_str()), (Some("Old.html#top"), Some("news.html#top")));
        let after = cmd(run, "web.site_report", json!({}));
        assert_eq!(after["broken_links"], json!([]), "{after}");
    });
    assert_eq!(
        dir.read("site/index.html"),
        "<!DOCTYPE html><html><head><title>i</title></head><body>\n<a href=\"news.html\">1</a> <a href=\"news.html#top\">2</a> <a href=\"news.html?q=1\">3</a> <a href=\"news.html\">4</a>\n</body></html>"
    );
    assert!(dir.read("site/pages/p.html").contains("<a href=\"../news.html\">o</a>"));
}

/// Ledger 2026-11-12T15:08:00Z: a move also rewrites the case-mismatched and percent-encoded spellings of the moved
/// file's old path in other documents.
#[test]
fn a_move_rewrites_case_mismatched_and_percent_encoded_spellings() {
    let dir = Scratch::new("move-spellings");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"thanks.html\">a</a> <a href=\"Thanks.html\">b</a> <a href=\"thanks%2Ehtml#top\">c</a></body></html>");
    dir.write("site/thanks.html", PAGE.as_bytes());
    with_run(|run| {
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        let reasons: Vec<&str> = report["broken_links"].as_array().unwrap().iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert_eq!(reasons, ["case_mismatch"], "{report}");
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
        assert_eq!(moved["receipt"]["changed"][0]["changes"].as_array().unwrap().len(), 3, "{moved}");
        assert_eq!(cmd(run, "web.site_report", json!({}))["broken_links"], json!([]));
    });
    assert_eq!(
        dir.read("site/index.html"),
        "<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"pages/thanks.html\">a</a> <a href=\"pages/thanks.html\">b</a> <a href=\"pages/thanks.html#top\">c</a></body></html>"
    );
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
<table><!-- TemplateBeginRepeat name=\"rows\" --><tr><td><!-- TemplateBeginEditable name=\"cell\" -->Cell<!-- TemplateEndEditable --></td></tr><!-- TemplateEndRepeat --></table>
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

        // Repeat entries: add a table row, move it first, remove the only list item.
        cmd(run, "web.instance_repeat", json!({"region": "rows", "op": "add"}));
        cmd(run, "web.instance_repeat", json!({"region": "rows", "op": "move", "index": 1, "to": 0}));
        cmd(run, "web.instance_repeat", json!({"region": "items", "op": "remove", "index": 0}));
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "items", "op": "remove", "index": 0})), "parameter_invalid");
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "main", "op": "add"})), "template_region_missing");
        assert_eq!(refused(run, "web.instance_repeat", json!({"region": "rows", "op": "spin"})), "parameter_invalid");
        let repeated = source(run);
        assert_eq!(repeated.matches("<tr><td>").count(), 2, "{repeated}");
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

/// [STU-WEB-097] to [STU-WEB-099], [STU-WEB-161]: snippets are records of the site with verbatim insert texts; a trigger
/// claimed twice refuses `snippet_trigger_duplicate` naming both snippets and the trigger; insertion of a block, a
/// wrap and a trigger expansion are named history steps; the library survives save and reopen, and an unnamed
/// snippet read back is reported, not dropped.
#[test]
fn snippets_are_site_records_with_unique_triggers() {
    let dir = Scratch::new("snippets");
    let page = dir.write("site/s.html", b"<!DOCTYPE html><html><head><title>s</title></head><body>\nword ul\n</body></html>");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&page)}));
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "x"})), "no_site");
        define(run, &dir);
        let bullets = json!({"folder_path": "Markup/Lists", "snippet_name": "Bullets", "snippet_kind": "block", "inserts_before_selection": "<ul>\n  <li>", "inserts_after_selection": "</li>\n</ul>\n", "trigger": "ul"});
        cmd(run, "web.snippet", bullets);
        let bold = json!({"folder_path": "Markup", "snippet_name": "Bold", "snippet_kind": "wrap", "inserts_before_selection": "<b>", "inserts_after_selection": "</b>", "trigger": "ul"});
        assert_eq!(refused(run, "web.snippet", bold), "snippet_trigger_duplicate");
        let record = run.ops().last().unwrap().clone();
        assert_eq!(record["detail"], json!({"trigger": "ul", "snippets": ["Markup/Lists/Bullets", "Markup/Bold"], "severity": "error"}), "{record}");
        let added = cmd(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Bold", "snippet_kind": "wrap", "inserts_before_selection": "<b>", "inserts_after_selection": "</b>", "trigger": "b"}));
        assert_eq!(added["web"]["undo"].as_array().unwrap().last().unwrap()["label"], "Snippet");
        cmd(run, "web.undo", json!({}));
        assert_eq!(cmd(run, "web.snippets", json!({}))["snippets_total"], 1);
        cmd(run, "web.redo", json!({}));
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Big", "inserts_before_selection": "x".repeat(16 * 1024 + 1)})), "snippet_text_too_large");
        assert_eq!(refused(run, "web.snippet", json!({"folder_path": "Markup", "snippet_name": "Bad", "snippet_kind": "inline"})), "parameter_invalid");
        let library = cmd(run, "web.snippets", json!({}));
        assert_eq!(library["groups"], json!([{"top_level_group": "Markup", "count": 2, "block": 1, "wrap": 1}]), "{library}");
        assert_eq!(library["triggers"], json!({"b": "Markup/Bold", "ul": "Markup/Lists/Bullets"}));

        // Wrap "word"; expand "ul" before the cursor; both verbatim.
        let text = source(run);
        let word = text.find("word").unwrap();
        cmd(run, "web.snippet_insert", json!({"folder_path": "Markup", "snippet_name": "Bold", "offset": word, "end": word + 4}));
        let text = source(run);
        let after_ul = text.find(" ul").unwrap() + 3;
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "ul", "offset": after_ul - 1})), "snippet_trigger_not_at_offset");
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "ul", "offset": text.len() + 1})), "parameter_invalid");
        assert_eq!(refused(run, "web.snippet_insert", json!({"trigger": "zz", "offset": after_ul})), "no_such_snippet");
        cmd(run, "web.snippet_insert", json!({"trigger": "ul", "offset": after_ul}));
        let expanded = source(run);
        assert_eq!(expanded, "<!DOCTYPE html><html><head><title>s</title></head><body>\n<b>word</b> <ul>\n  <li></li>\n</ul>\n\n</body></html>");
        cmd(run, "web.undo", json!({}));
        cmd(run, "web.redo", json!({}));
        cmd(run, "web.save", json!({"overwrite": true}));
    });
    // An unnamed snippet in the saved record is reported on reopen, never dropped.
    let record = dir.at("site/s.html.studio-site.json");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    saved["site"]["snippets"].as_array_mut().unwrap().push(json!({"snippet_name": "", "folder_path": "Scripts", "snippet_kind": "block", "preview_mode": "code", "inserts_before_selection": "x", "inserts_after_selection": "", "description": "", "trigger": null}));
    std::fs::write(&record, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&page)}));
        let library = cmd(run, "web.snippets", json!({}));
        assert_eq!(library["snippets_total"], 3, "{library}");
        assert_eq!(library["diagnostics"], json!([{"code": "snippet_unnamed", "severity": "warning", "folder_path": "Scripts"}]));
        assert_eq!(library["snippets"][2]["inserts_before_selection"], Value::Null, "the listing carries no insert text");
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
    });
}

/// Sets a file's modification time `secs_ago` seconds before now.
fn age(path: &Path, secs_ago: u64) {
    let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
}

/// [STU-WEB-119]: put, get and two-way synchronise compare by size and modification time, list the actions for review
/// and apply each as its own transaction with a per-file receipt; the files a chosen page loads are an explicit
/// option, listed by default; a second synchronise finds everything the same.
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
        assert_eq!(refused(run, "web.synchronize", json!({"direction": "put", "dependents": "maybe"})), "parameter_invalid");
        let review = cmd(run, "web.synchronize", json!({"direction": "both"}));
        assert_eq!(review["schema"], "hsk.studio.web.sync_plan@1");
        let actions: Vec<(&str, &str)> = review["actions"].as_array().unwrap().iter().map(|a| (a["path"].as_str().unwrap(), a["action"].as_str().unwrap())).collect();
        assert_eq!(actions, [("img/a.png", "put"), ("index.html", "put"), ("notes.txt", "get"), ("other.html", "get")], "{review}");
        assert!(!dir.at("remote/img/a.png").exists() && !dir.at("site/notes.txt").exists(), "a review writes nothing");
        let put = cmd(run, "web.synchronize", json!({"direction": "put"}));
        let actions: Vec<(&str, &str)> = put["actions"].as_array().unwrap().iter().map(|a| (a["path"].as_str().unwrap(), a["action"].as_str().unwrap())).collect();
        assert_eq!(actions, [("img/a.png", "put"), ("index.html", "put"), ("notes.txt", "none"), ("other.html", "remote_newer")], "{put}");

        // One page chosen: what it loads is listed (prompt), added (include) or left out (exclude); page links never.
        let chosen = cmd(run, "web.synchronize", json!({"direction": "put", "files": ["index.html"]}));
        assert_eq!((chosen["dependents"].clone(), chosen["actions"].as_array().unwrap().len()), (json!(["img/a.png"]), 1), "{chosen}");
        let with_deps = cmd(run, "web.synchronize", json!({"direction": "put", "files": ["index.html"], "dependents": "include"}));
        assert_eq!(with_deps["actions"].as_array().unwrap().len(), 2, "{with_deps}");

        let applied = cmd(run, "web.synchronize", json!({"direction": "both", "apply": true}));
        let receipt = &applied["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["transferred"].as_u64(), receipt["complete"].as_bool()), (Some("hsk.studio.web.sync_receipt@1"), Some(4), Some(true)), "{receipt}");
        assert!(receipt["files"].as_array().unwrap().iter().all(|f| f["outcome"] == "transferred" && f["time_kept"] == true), "{receipt}");
        let again = cmd(run, "web.synchronize", json!({"direction": "both"}));
        assert_eq!(again["counts"], json!({"same": 4}), "{again}");
        cmd(run, "web.remote_target", json!({"target_id": "inner", "root_path": s(&dir.at("site/inner"))}));
        assert_eq!(refused(run, "web.synchronize", json!({"target_id": "inner", "direction": "put"})), "target_inside_site");
    });
    assert_eq!(dir.read("remote/index.html"), dir.read("site/index.html"));
    assert_eq!(dir.read("site/notes.txt"), "remote only");
    assert_eq!(dir.read("site/other.html"), "newer remote copy");
    assert_eq!(std::fs::read(dir.at("remote/img/a.png")).unwrap(), b"png");
}
