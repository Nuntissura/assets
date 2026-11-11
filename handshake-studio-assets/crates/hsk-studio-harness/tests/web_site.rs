//! Lane SMB-WEB-SITE-1 ([STU-WEB-113], [STU-WEB-118], [STU-WEB-120], [STU-WEB-121], [STU-WEB-131], [STU-WEB-163],
//! [STU-WEB-164], [STU-ASSET-009]; the board-slides-web scenario steps 105, 108 to 111 and the document set of 113): a
//! local site of several pages is one link graph. Every step runs through `Run::web_command`: the site report (broken
//! links, orphaned files, external links, missing alternative text, untitled documents), export of the whole document
//! set, update-links-on-move as one receipted transaction, the site lock and the page-link warning of the open export.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_harness_core::harness::web_commands::MANIFEST_FILE;
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
        let dir = std::env::temp_dir().join(format!("hsk-web-site-{}-{label}", std::process::id()));
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
    assert_eq!(record["command_id"], id, "{id}: the record carries the command id");
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

const INDEX: &str ="<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<title>Harbour</title>\n<link rel=\"stylesheet\" href=\"styles/site.css\">\n</head>\n<body>\n<h1 id=\"top\">Harbour Lights</h1>\n<img src=\"img/hero.png\">\n<p><a href=\"thanks.html\">Thanks</a> <a href=\"thanks.html#done\">Done</a></p>\n<p><a href=\"missing.html\">Gone</a> <a href=\"thanks.html#nowhere\">Nowhere</a></p>\n<p><a href=\"https://example.org\">Example</a></p>\n</body>\n</html>\n";
const THANKS: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<link rel=\"stylesheet\" href=\"styles/site.css\">\n</head>\n<body>\n<p id=\"done\"><a href=\"index.html#top\">Back</a></p>\n<img alt=\"Logo\" src=\"img/hero.png\">\n</body>\n</html>\n";
const CSS: &str = "body { background: url(../img/bg.png); }\n";

/// The fixture of item 1, written in `order` (the walk must not depend on it).
fn site(dir: &Scratch, reversed: bool) -> PathBuf {
    let mut files: Vec<(&str, &[u8])> = vec![
        ("site/index.html", INDEX.as_bytes()),
        ("site/thanks.html", THANKS.as_bytes()),
        ("site/styles/site.css", CSS.as_bytes()),
        ("site/img/hero.png", b"\x89PNG-hero"),
        ("site/img/bg.png", b"\x89PNG-bg"),
        ("site/orphan.png", b"\x89PNG-orphan"),
    ];
    if reversed {
        files.reverse();
    }
    for (rel, bytes) in files {
        dir.write(rel, bytes);
    }
    dir.at("site/index.html")
}

fn define(run: &mut Run<'_>, dir: &Scratch) {
    cmd(run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
}

/// Every file of a folder, root-relative, with its bytes (sorted).
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Item 1: the site report, export of the document set and update-links-on-move, end to end.
#[test]
fn a_site_reports_exports_and_moves_as_one_link_graph() {
    let dir = Scratch::new("graph");
    let index = site(&dir, false);
    let original = tree(&dir.at("site"));
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        assert_eq!(refused(run, "web.site_report", json!({})), "no_site");
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        assert_eq!(report["schema"], "hsk.studio.web.site_report@1", "{report}");
        assert_eq!(
            report["broken_links"],
            json!([
                {"code": "link_broken", "severity": "warning", "document": "index.html", "line": 11, "reference": "missing.html", "target": "missing.html", "reason": "missing", "load": ["document"]},
                {"code": "link_broken", "severity": "warning", "document": "index.html", "line": 11, "reference": "thanks.html#nowhere", "target": "thanks.html#nowhere", "reason": "anchor_missing", "load": ["document"]},
            ]),
            "{report}"
        );
        assert_eq!(report["orphaned_files"], json!([{"code": "file_orphaned", "severity": "info", "file": "orphan.png"}]), "a file used only from a style sheet is not orphaned: {report}");
        assert_eq!(report["external_links"], json!([{"document": "index.html", "line": 12, "reference": "https://example.org"}]));
        assert_eq!(report["missing_alt"], json!([{"code": "missing_alt", "severity": "warning", "document": "index.html", "line": 9}]));
        assert_eq!(report["untitled_documents"], json!([{"code": "untitled_document", "severity": "warning", "document": "thanks.html"}]));
        assert_eq!((report["documents"].as_u64(), report["stylesheets"].as_u64(), report["files"].as_u64()), (Some(2), Some(1), Some(6)), "{report}");
        assert!(!dir.at("site/.studio-site.lock").exists(), "the lock is released");

        // Export of the document set: both pages from their sources, the style sheet's own image, the missing page a warning.
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site"}));
        let export = cmd(run, "web.export", json!({}));
        let manifest = &export["manifest"];
        let files: Vec<(&str, &str)> = manifest["files"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap(), f["role"].as_str().unwrap())).collect();
        assert_eq!(files, [("img/bg.png", "asset"), ("img/hero.png", "asset"), ("index.html", "document"), ("styles/site.css", "asset"), ("thanks.html", "document")], "{manifest}");
        let documents: Vec<&str> = manifest["documents"].as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap()).collect();
        assert_eq!(documents, ["index.html", "thanks.html"]);
        assert_eq!(manifest["link_broken"], json!([{"document": "index.html", "line": 11, "reference": "missing.html"}]), "{manifest}");
        assert_eq!(dir.read("out/thanks.html"), THANKS);
        assert!(!dir.at("out/orphan.png").exists());

        // Update links on move: one transaction with a receipt; the open document is reopened with a fresh history.
        cmd(run, "web.set_attribute", json!({"key": "#top", "name": "class", "value": "x"}));
        assert_eq!(refused(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"})), "document_unsaved");
        cmd(run, "web.undo", json!({}));
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
        let receipt = &moved["receipt"];
        assert_eq!((receipt["schema"].as_str(), receipt["complete"].as_bool()), (Some("hsk.studio.web.move_receipt@1"), Some(true)), "{moved}");
        let changed: Vec<(&str, usize)> = receipt["changed"].as_array().unwrap().iter().map(|c| (c["document"].as_str().unwrap(), c["changes"].as_array().unwrap().len())).collect();
        assert_eq!(changed, [("index.html", 3), ("pages/thanks.html", 3)], "{receipt}");
        let first = &receipt["changed"][0]["changes"][0];
        assert_eq!((first["reference_before"].as_str(), first["reference_after"].as_str(), first["line"].as_u64()), (Some("thanks.html"), Some("pages/thanks.html"), Some(10)));
        let at = INDEX.find("thanks.html").unwrap();
        assert_eq!(first["before"], json!([at, at + "thanks.html".len()]));
        assert!(dir.read("site/index.html").contains("<a href=\"pages/thanks.html\">Thanks</a> <a href=\"pages/thanks.html#done\">Done</a>"));
        let thanks = dir.read("site/pages/thanks.html");
        assert!(thanks.contains("href=\"../styles/site.css\"") && thanks.contains("href=\"../index.html#top\"") && thanks.contains("src=\"../img/hero.png\""), "{thanks}");
        assert!(!dir.at("site/thanks.html").exists());
        assert_eq!(moved["web"]["undo"], json!([]), "reopened with a fresh history");
        assert_eq!(moved["web"]["dirty"], false);
        let source = cmd(run, "web.read", json!({"include_source": true}))["source"].as_str().unwrap().to_string();
        assert_eq!(source, dir.read("site/index.html"), "the open document is the file on disk");
        let after = cmd(run, "web.site_report", json!({}));
        assert_eq!(after["broken_links"].as_array().unwrap().len(), 2, "{after}");
        assert_eq!(after["untitled_documents"][0]["document"], "pages/thanks.html");

        // The reverse move restores every byte.
        cmd(run, "web.move_file", json!({"from": "pages/thanks.html", "to": "thanks.html"}));
    });
    let mut back = tree(&dir.at("site"));
    back.retain(|(p, _)| !p.starts_with("pages"));
    assert_eq!(back, original, "a reverse move restores the site");
}

/// STU-WEB-163: a link to a page that does not exist is a warning in the open-document export too; any other missing
/// local reference still fails closed (STU-ASSET-009), and an export without broken links has no warning list.
#[test]
fn a_missing_page_is_a_warning_in_the_open_export() {
    let dir = Scratch::new("open-warning");
    let index = dir.write("site/index.html", b"<!DOCTYPE html>\n<html><head><title>t</title></head>\n<body>\n<!-- c -->\n<a href=\"gone.html\">Gone</a>\n</body></html>\n");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "minify": true}));
        let export = cmd(run, "web.export", json!({}));
        assert_eq!(export["manifest"]["link_broken"], json!([{"document": "index.html", "line": 5, "reference": "gone.html"}]), "the line of the source, not of the minified output: {export}");
        assert_eq!(export["link_broken"], export["manifest"]["link_broken"]);
        let paths: Vec<&str> = export["manifest"]["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["index.html"]);
        let (link, body) = (key_of_tag(run, "a"), key_of_tag(run, "body"));
        cmd(run, "web.remove_node", json!({"key": link}));
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out2"))}));
        let clean = cmd(run, "web.export", json!({}));
        assert!(clean["manifest"].get("link_broken").is_none() && clean.get("link_broken").is_none(), "{clean}");
        assert!(clean["manifest"].get("documents").is_none(), "the open export's manifest keeps its shape");
        cmd(run, "web.insert_fragment", json!({"parent": body, "fragment": "<img alt=\"a\" src=\"gone.png\">"}));
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out3"))}));
        assert_eq!(refused(run, "web.export", json!({})), "asset_missing");
    });
    assert!(!dir.at("out3").exists());
}

/// STU-WEB-131: site-wide commands take a site-level advisory lock and report its holder on contention, never blocking;
/// the lock is released on every exit path.
#[test]
fn the_site_lock_names_its_holder_and_never_blocks() {
    let dir = Scratch::new("lock");
    let index = site(&dir, false);
    let holder = json!({"schema": "hsk.studio.web.site_lock@1", "actor": {"account_id": "lane-b", "principal_id": "p"}, "client_session": "s-other", "command_id": "web.move_file", "nonce": "n", "extra": "x".repeat(4096)});
    dir.write("site/.studio-site.lock", serde_json::to_string(&holder).unwrap().as_bytes());
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        define(run, &dir);
        cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("target"))}));
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site"}));
        for (id, params) in [("web.site_report", json!({})), ("web.export", json!({})), ("web.move_file", json!({"from": "orphan.png", "to": "img/orphan.png"}))] {
            assert_eq!(refused(run, id, params), "site_locked", "{id}");
            let record = run.ops().last().unwrap();
            assert_eq!(record["detail"]["holder"], json!({"account_id": "lane-b", "client_session": "s-other", "command_id": "web.move_file"}), "{id}: only three fields, never the whole file: {record}");
            assert!(record["target"].as_str().unwrap().contains("s-other"), "{record}");
        }
        assert!(!dir.at("out").exists() && dir.at("site/orphan.png").exists(), "nothing changed");
        std::fs::remove_file(dir.at("site/.studio-site.lock")).unwrap();
        cmd(run, "web.export", json!({}));
        // The publish holds the lock too.
        dir.write("site/.studio-site.lock", b"{}");
        assert_eq!(refused(run, "web.publish", json!({})), "site_locked");
        std::fs::remove_file(dir.at("site/.studio-site.lock")).unwrap();
        cmd(run, "web.publish", json!({}));
        // A refusal inside a site command releases the lock.
        assert_eq!(refused(run, "web.move_file", json!({"from": "nope.png", "to": "x.png"})), "move_source_missing");
        assert!(!dir.at("site/.studio-site.lock").exists());
    });
}

/// A failed write restores every file already written from the in-memory originals, and says so.
#[test]
fn a_failed_move_restores_every_file_byte_for_byte() {
    let dir = Scratch::new("rollback");
    dir.write("site/a.html", b"<!DOCTYPE html><html><head><title>a</title></head><body><a href=\"c.html\">c</a></body></html>");
    dir.write("site/b.html", b"<!DOCTYPE html><html><head><title>b</title></head><body><a href=\"c.html\">c</a></body></html>");
    dir.write("site/c.html", b"<!DOCTYPE html><html><head><title>c</title></head><body><a href=\"a.html\">a</a></body></html>");
    let before = tree(&dir.at("site"));
    // The atomic writer's temporary sibling of b.html cannot be created: a folder holds its name.
    let blocker = dir.at(&format!("site/.b.html.studio-tmp-{}", std::process::id()));
    std::fs::create_dir_all(&blocker).unwrap();
    with_run(|run| {
        define(run, &dir);
        assert_eq!(refused(run, "web.move_file", json!({"from": "c.html", "to": "d/c.html"})), "move_failed");
        let detail = run.ops().last().unwrap()["detail"].clone();
        assert_eq!(detail["receipt"]["complete"], false, "{detail}");
        assert_eq!(detail["receipt"]["restored"], json!(["a.html"]), "{detail}");
        assert_eq!(detail["receipt"]["failed"], "b.html", "{detail}");
    });
    std::fs::remove_dir(&blocker).unwrap();
    assert_eq!(tree(&dir.at("site")), before, "every file is back, no folder was left");
    assert!(!dir.at("site/d").exists());
}

/// Refusals of web.move_file: typed, nothing written, nothing moved.
#[test]
fn move_refusals_are_typed_and_change_nothing() {
    let dir = Scratch::new("move-refusals");
    site(&dir, false);
    dir.write("site/index.html.studio-site.json", b"{}");
    dir.write("site/img/pic.png", b"x");
    dir.write("site/srcset.html", b"<!DOCTYPE html><html><head><title>s</title></head><body><img alt=\"a\" srcset=\"img/pic.png 1x, img/bg&#46;png 2x\"></body></html>");
    let before = tree(&dir.at("site"));
    with_run(|run| {
        assert_eq!(refused(run, "web.move_file", json!({"from": "orphan.png", "to": "x.png"})), "no_site");
        define(run, &dir);
        for (from, to, code) in [
            ("orphan.png", "../orphan.png", "move_outside_site"),
            ("orphan.png", "a/../../orphan.png", "move_outside_site"),
            ("orphan.png", "or phan.png", "path_invalid"),
            ("orphan.png", "img\\orphan.png", "path_invalid"),
            ("orphan.png", "img/hero.png", "move_target_exists"),
            ("orphan.png", "IMG/Hero.PNG", "move_target_exists"),
            ("thanks.html", "Thanks.html", "move_case_only"),
            ("orphan.png", "orphan.png", "nothing_to_change"),
            ("missing.png", "x.png", "move_source_missing"),
            ("img", "pictures", "move_source_missing"),
            ("index.html.studio-site.json", "x.json", "move_reserved_file"),
            ("orphan.png", MANIFEST_FILE, "move_reserved_file"),
            ("orphan.png", ".studio-site.lock", "path_invalid"),
            ("index.html", "home.html", "move_site_record"),
            // Review control: a reference whose decoded text moved its offsets inside a list cannot be rewritten by span.
            ("img/bg.png", "img/back.png", "reference_not_rewritable"),
        ] {
            assert_eq!(refused(run, "web.move_file", json!({"from": from, "to": to})), code, "{from} -> {to}");
        }
        let absolute = s(&dir.at("elsewhere/orphan.png"));
        for to in [absolute.as_str(), "/x.png", "//host/share/x.png", "orphan.png:s", "con.png", "NUL", "img/nul.png"] {
            assert_eq!(refused(run, "web.move_file", json!({"from": "orphan.png", "to": to})), "path_invalid", "{to}");
        }
        assert_eq!(refused(run, "web.move_file", json!({"from": "orphan.png"})), "parameter_missing");
    });
    assert_eq!(tree(&dir.at("site")), before, "nothing moved or written");
    assert!(!dir.at("elsewhere").exists());
}

/// A reference spelled with character references is rewritten through its whole decoded value (the span is the
/// whole value, STU-WEB-004 keeps every other byte).
#[test]
fn references_with_character_references_are_rewritten_by_their_whole_value() {
    let dir = Scratch::new("entities");
    dir.write("site/thanks.html", b"<!DOCTYPE html><html><head><title>t</title></head><body>t</body></html>");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title><style>p { background: url(thanks.html); }</style></head><body><a href='thanks&#46;html?a=1&amp;b=2'>x</a> <a href=thanks.html>y</a></body></html>");
    with_run(|run| {
        define(run, &dir);
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
        assert_eq!(moved["receipt"]["changed"][0]["changes"].as_array().unwrap().len(), 3, "{moved}");
    });
    assert_eq!(
        dir.read("site/index.html"),
        "<!DOCTYPE html><html><head><title>i</title><style>p { background: url(pages/thanks.html); }</style></head><body><a href='pages/thanks.html?a=1&amp;b=2'>x</a> <a href=pages/thanks.html>y</a></body></html>"
    );
}

/// `site_relative_link_base: site_root` writes moved references root-relative.
#[test]
fn moved_links_honour_the_site_link_base() {
    let dir = Scratch::new("link-base");
    dir.write("site/thanks.html", b"<!DOCTYPE html><html><head><title>t</title></head><body><a href=\"index.html\">i</a></body></html>");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"thanks.html\">t</a></body></html>");
    with_run(|run| {
        cmd(run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site")), "site_relative_link_base": "site_root"}));
        cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
    });
    assert!(dir.read("site/index.html").contains("<a href=\"/pages/thanks.html\">"));
    assert!(dir.read("site/pages/thanks.html").contains("<a href=\"/index.html\">"));
}

/// STU-WEB-131 deterministic: the report and the export are pure functions of the files, whatever order they were
/// created in.
#[test]
fn site_outputs_do_not_depend_on_creation_order() {
    let mut reports = Vec::new();
    let mut manifests = Vec::new();
    for (label, reversed) in [("order-a", false), ("order-b", true)] {
        let dir = Scratch::new(label);
        site(&dir, reversed);
        with_run(|run| {
            define(run, &dir);
            reports.push(cmd(run, "web.site_report", json!({})));
            cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site", "minify": true}));
            cmd(run, "web.export", json!({}));
        });
        manifests.push(std::fs::read(dir.at(&format!("out/{MANIFEST_FILE}"))).unwrap());
    }
    assert_eq!(reports[0], reports[1]);
    assert_eq!(manifests[0], manifests[1]);
}

/// Hostile trees: bounded, typed, nothing written (V4-01, V4-02).
#[test]
fn hostile_trees_are_bounded_and_typed() {
    let dir = Scratch::new("hostile");
    for i in 0..4097 {
        dir.write(&format!("many/f{i:04}.txt"), b"x");
    }
    let big = vec![b'a'; (8 << 20) + 1];
    dir.write("big/index.html", b"<!DOCTYPE html><html><head><title>t</title></head><body><a href=\"huge.html\">h</a></body></html>");
    dir.write("big/huge.html", &big);
    dir.write("big/.hidden.png", b"x");
    with_run(|run| {
        cmd(run, "web.site", json!({"display_name": "many", "local_root": s(&dir.at("many"))}));
        assert_eq!(refused(run, "web.site_report", json!({})), "site_too_many_files");
        cmd(run, "web.site", json!({"local_root": s(&dir.at("big"))}));
        let report = cmd(run, "web.site_report", json!({}));
        assert_eq!(report["unparsed"], json!([{"file": "huge.html", "code": "file_too_large"}]), "{report}");
        assert_eq!(report["graph_complete"], false);
        assert_eq!(report["skipped"], json!([{"path": ".hidden.png", "reason": "name_not_portable"}]), "{report}");
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site"}));
        assert_eq!(refused(run, "web.export", json!({})), "document_unreadable");
        let record = run.ops().last().unwrap();
        assert_eq!((record["target"].as_str(), &record["detail"]), (Some("huge.html: file_too_large"), &json!({"file": "huge.html", "reason": "file_too_large"})), "Q4: the file and its reason: {record}");
        dir.write("big/a.png", b"x");
        assert_eq!(refused(run, "web.move_file", json!({"from": "a.png", "to": "b.png"})), "document_unreadable");
        assert_eq!(run.ops().last().unwrap()["detail"], json!({"file": "huge.html", "reason": "file_too_large"}));
        assert_eq!(refused(run, "web.export_recipe", json!({"documents": "all"})), "parameter_invalid");
    });
    assert!(!dir.at("out").exists());
}

/// A junction or symlinked folder inside the root is never walked or followed.
#[test]
fn a_linked_folder_inside_the_site_is_skipped() {
    let dir = Scratch::new("link");
    dir.write("outside/secret.png", b"secret");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>t</title></head><body><img alt=\"a\" src=\"media/secret.png\"></body></html>");
    link_dir(&dir.at("outside"), &dir.at("site/media"));
    with_run(|run| {
        define(run, &dir);
        let report = cmd(run, "web.site_report", json!({}));
        assert_eq!(report["skipped"], json!([{"path": "media", "reason": "link"}]), "{report}");
        assert_eq!(report["denied"], json!([{"document": "index.html", "line": 1, "reference": "media/secret.png", "reason": "not_granted"}]), "{report}");
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site"}));
        assert_eq!(refused(run, "web.export", json!({})), "asset_denied");
    });
    assert!(!dir.at("out").exists());
}

/// Lead ruling 2026-11-08: web.move_file never moves the opened package, nor anything onto or into it.
#[test]
fn a_move_never_touches_the_opened_package() {
    let dir = Scratch::new("package");
    site(&dir, false);
    let package = dir.write("site/doc.handshake", FIXTURE);
    let before = tree(&dir.at("site"));
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&package), "{}", run.ops().last().unwrap());
    define(&mut run, &dir);
    for (from, to) in [("doc.handshake", "old/doc.handshake"), ("orphan.png", "doc.handshake"), ("orphan.png", "doc.handshake/x.png")] {
        assert_eq!(refused(&mut run, "web.move_file", json!({"from": from, "to": to})), "move_open_package", "{from} -> {to}");
    }
    drop(run);
    assert_eq!(tree(&dir.at("site")), before);
}

/// A job port that counts every admit and release and keeps each admitted scope's resource; refuses every admit
/// when `refuse` is set (R60 probe j3: a saturated Job Runtime).
#[derive(Default)]
struct JobsPort {
    refuse: bool,
    admits: AtomicU64,
    releases: AtomicU64,
    resources: Mutex<Vec<String>>,
    actions: Mutex<Vec<&'static str>>,
    /// Canceled while the port admits: the job is admitted, and its work starts canceled.
    cancel_on_admit: Option<CancellationToken>,
}

impl JobPort for JobsPort {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        self.resources.lock().unwrap().push(scope.resource.as_str().to_string());
        self.actions.lock().unwrap().push(scope.action.as_str());
        if let Some(token) = &self.cancel_on_admit {
            token.cancel();
        }
        let n = self.admits.fetch_add(1, Ordering::SeqCst);
        if self.refuse { Err(JobRefused { retry_after_ms: 7 }) } else { Ok(JobTicket(n)) }
    }

    fn release(&self, _ticket: JobTicket) {
        self.releases.fetch_add(1, Ordering::SeqCst);
    }
}

impl JobsPort {
    fn asked(&self) -> u64 {
        self.admits.load(Ordering::SeqCst)
    }
}

/// With no package open the run's frames are unaddressed (no frame): an accepted command, record checked only.
fn bare_ok(run: &mut Run<'_>, id: &str, params: Value) {
    assert!(run.web_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
}

/// With no package open: a refused command's code; the host's state did not change.
fn bare_no(run: &mut Run<'_>, id: &str, params: Value) -> String {
    let before = run.web_state();
    assert!(!run.web_command(id, &params), "{id} {params} was accepted");
    assert_eq!(run.web_state(), before, "{id}: a refusal changes nothing");
    run.ops().last().unwrap()["error"].as_str().unwrap().to_string()
}

/// Lead ruling 2026-11-08 (R60 probe j3; resource-less admission is not accepted): with NO package open, web.export,
/// web.publish, the site export, web.site_report and web.move_file still ask the bound job port, once each, before
/// any file: a saturated port refuses `overloaded` and nothing is written; the resource is a valid id, the same for
/// every command on one site; an admitting port is asked and released once per command.
#[test]
fn with_no_package_open_web_jobs_still_ask_the_bound_port() {
    let dir = Scratch::new("no-package-jobs");
    let index = site(&dir, false);
    let before = tree(&dir.at("site"));
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.snapshot().is_none(), "no package is open");
    let saturated = Arc::new(JobsPort { refuse: true, ..JobsPort::default() });
    run.bind_web_jobs(saturated.clone(), JobLifecycle::default());
    bare_ok(&mut run, "web.open", json!({"path": s(&index)}));
    bare_ok(&mut run, "web.export_recipe", json!({"output_root": s(&dir.at("out"))}));
    // The probe's shape: no site defined, the open export only.
    assert_eq!(bare_no(&mut run, "web.export", json!({})), "overloaded");
    assert_eq!(saturated.asked(), 1, "web.export asks the port once");
    bare_ok(&mut run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
    bare_ok(&mut run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("target"))}));
    let mut asked = 1;
    for (id, params, recipe) in [
        ("web.export", json!({}), None),
        ("web.publish", json!({}), None),
        ("web.export", json!({}), Some("site")),
        ("web.site_report", json!({}), None),
        ("web.move_file", json!({"from": "orphan.png", "to": "img/orphan.png"}), None),
    ] {
        if let Some(set) = recipe {
            bare_ok(&mut run, "web.export_recipe", json!({"documents": set}));
        }
        assert_eq!(bare_no(&mut run, id, params), "overloaded", "{id}");
        asked += 1;
        assert_eq!(saturated.asked(), asked, "{id}: the port is asked exactly once");
        assert_eq!(run.ops().last().unwrap()["target"], "retry_after_ms=7");
    }
    assert!(!dir.at("out").exists() && !dir.at("target").exists(), "nothing written");
    assert_eq!(tree(&dir.at("site")), before, "the site is untouched (no lock file left)");
    let seen = saturated.resources.lock().unwrap().clone();
    assert!(seen.iter().all(|r| hsk_studio_accord::DomainId::parse(r).is_ok()), "{seen:?}");
    assert!(seen.iter().skip(1).all(|r| *r == seen[1]), "one resource per site: {seen:?}");
    // An admitting port: one admit and one release per command, and the work runs.
    let open = Arc::new(JobsPort::default());
    run.bind_web_jobs(open.clone(), JobLifecycle::default());
    bare_ok(&mut run, "web.export", json!({}));
    bare_ok(&mut run, "web.site_report", json!({}));
    assert_eq!((open.asked(), open.releases.load(Ordering::SeqCst)), (2, 2));
    assert!(dir.at("out/index.html").is_file());
}

/// Links `link` to the folder `target`: a folder symlink, else on Windows a junction (no privilege needed). Never a
/// silent skip (review F6): a test that needs a link fails when none can be made.
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

/// Review F1: every prefix of `to` is compared case-folded. A prefix that folds to a link or junction refuses
/// `move_outside_site` and nothing leaves the root; a prefix that folds to an existing folder takes its on-disk
/// spelling, for the path and for every rewritten reference.
#[test]
fn a_move_target_is_compared_case_folded_against_links_and_folders() {
    let dir = Scratch::new("case-fold");
    site(&dir, false);
    dir.write("outside/keep.txt", b"k");
    link_dir(&dir.at("outside"), &dir.at("site/media"));
    with_run(|run| {
        define(run, &dir);
        for to in ["media/orphan.png", "MEDIA/orphan.png", "Media/sub/orphan.png"] {
            assert_eq!(refused(run, "web.move_file", json!({"from": "orphan.png", "to": to})), "move_outside_site", "{to}");
        }
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "Img/thanks.html"}));
        assert_eq!(moved["receipt"]["to"], "img/thanks.html", "the folder's on-disk spelling: {moved}");
        let report = cmd(run, "web.site_report", json!({}));
        let reasons: Vec<&str> = report["broken_links"].as_array().unwrap().iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert!(!reasons.contains(&"case_mismatch"), "{report}");
    });
    assert_eq!(tree(&dir.at("outside")), [("keep.txt".to_string(), b"k".to_vec())], "nothing left the root");
    assert!(dir.read("site/index.html").contains("<a href=\"img/thanks.html\">Thanks</a>"), "{}", dir.read("site/index.html"));
    let names: Vec<String> = std::fs::read_dir(dir.at("site")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.contains(&"img".to_string()) && !names.contains(&"Img".to_string()), "{names:?}");
    assert!(dir.at("site/img/thanks.html").is_file());
}

/// Review F2: in the moved document every relative reference is re-based by path arithmetic on its own bytes,
/// whatever it reaches (percent-encoding and query kept); where that cannot be done the move refuses
/// `reference_not_rewritable` naming the document and line.
#[test]
fn a_moved_document_rebases_every_relative_reference() {
    let dir = Scratch::new("rebase");
    dir.write("site/photos/Summer 2024.jpg", b"jpg");
    dir.write("site/gallery.html", b"<!DOCTYPE html><html><head><title>g</title></head><body><img alt=\"s\" src=\"photos/Summer%202024.jpg?v=1\">\n<a href=\"../up.html\">up</a> <a href=\"gone.html#x\">gone</a></body></html>");
    dir.write("site/deep.html", b"<!DOCTYPE html><html><head><title>d</title></head>\n<body><a href=\"../up.html\">up</a></body></html>");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.move_file", json!({"from": "gallery.html", "to": "pages/gallery.html"}));
        // The site-root spelling cannot name a place above the root.
        cmd(run, "web.site", json!({"site_relative_link_base": "site_root"}));
        assert_eq!(refused(run, "web.move_file", json!({"from": "deep.html", "to": "pages/deep.html"})), "reference_not_rewritable");
        assert_eq!(run.ops().last().unwrap()["target"], "deep.html:2");
    });
    let gallery = dir.read("site/pages/gallery.html");
    assert!(gallery.contains("src=\"../photos/Summer%202024.jpg?v=1\"") && gallery.contains("href=\"../../up.html\"") && gallery.contains("href=\"../gone.html#x\""), "{gallery}");
    assert!(dir.at("site/deep.html").is_file() && !dir.at("site/pages/deep.html").exists());
}

/// Review F3: a value rewritten through character references is written quoted, so decoded whitespace or `>` never
/// ends the value or the tag.
#[test]
fn an_unquoted_value_rewritten_whole_is_quoted() {
    let dir = Scratch::new("unquoted");
    dir.write("site/thanks.html", b"<!DOCTYPE html><html><head><title>t</title></head><body>t</body></html>");
    dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body><a href=thanks&#46;html?q=a&#32;b>x</a><a href=thanks&#46;html?x=&gt;y>z</a></body></html>");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
    });
    assert_eq!(dir.read("site/index.html"), "<!DOCTYPE html><html><head><title>i</title></head><body><a href=\"pages/thanks.html?q=a b\">x</a><a href=\"pages/thanks.html?x=>y\">z</a></body></html>");
}

/// Review F4: an export lists at most 2000 broken links and counts the rest; a manifest larger than publish and verify
/// read back (1 MiB) is refused before the first write.
#[test]
fn the_export_caps_broken_links_and_its_manifest() {
    let dir = Scratch::new("manifest-bound");
    let links: String = (0..2100).map(|i| format!("<a href=\"gone-{i}.html\">g</a>\n")).collect();
    let index = dir.write("site/index.html", format!("<!DOCTYPE html><html><head><title>i</title></head><body>\n{links}</body></html>").as_bytes());
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        define(run, &dir);
        for (out, set) in [("out-open", "open"), ("out-site", "site")] {
            cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at(out)), "documents": set}));
            let export = cmd(run, "web.export", json!({}));
            assert_eq!(export["manifest"]["link_broken"].as_array().unwrap().len(), 2000, "{set}");
            assert_eq!((export["manifest"]["link_broken_truncated"].as_u64(), export["link_broken_truncated"].as_u64()), (Some(100), Some(100)), "{set}");
        }
    });
    // About 2600 pages with long names: the files and documents rows exceed 1 MiB.
    let big = Scratch::new("manifest-big");
    big.write("site/index.html", b"<!DOCTYPE html><html><head><title>i</title></head><body>i</body></html>");
    for i in 0..2600 {
        big.write(&format!("site/p{}{i:05}.html", "a".repeat(70)), b"<!DOCTYPE html><title>t</title>");
    }
    with_run(|run| {
        define(run, &big);
        cmd(run, "web.export_recipe", json!({"output_root": s(&big.at("out")), "documents": "site"}));
        assert_eq!(refused(run, "web.export", json!({})), "export_manifest_too_large");
    });
    assert!(!big.at("out").exists(), "refused before the first write");
}

/// Review F5: parameters are checked before admission (a typed refusal, the port not asked); a canceled token reaches
/// the lifecycle (`refused_canceled`, the port not asked); the result carries the job's settlement; publish checks
/// the token per file and settles canceled with its partial receipt.
#[test]
fn job_admission_edges_are_typed() {
    let dir = Scratch::new("job-edges");
    let index = site(&dir, false);
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    let saturated = Arc::new(JobsPort { refuse: true, ..JobsPort::default() });
    run.bind_web_jobs(saturated.clone(), JobLifecycle::default());
    bare_ok(&mut run, "web.open", json!({"path": s(&index)}));
    bare_ok(&mut run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
    bare_ok(&mut run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("target"))}));
    bare_ok(&mut run, "web.export_recipe", json!({"output_root": s(&dir.at("out"))}));
    for (id, params, code) in [
        ("web.move_file", json!({"from": "orphan.png"}), "parameter_missing"),
        ("web.move_file", json!({"from": "orphan.png", "to": "or phan.png"}), "path_invalid"),
        ("web.move_file", json!({"from": "orphan.png", "to": "orphan.png"}), "nothing_to_change"),
        ("web.publish", json!({"target_id": "nope"}), "no_such_target"),
    ] {
        assert_eq!(bare_no(&mut run, id, params), code, "{id}");
    }
    assert_eq!(saturated.asked(), 0, "malformed commands never reach the port");
    // A settled job is in the result.
    let open = Arc::new(JobsPort::default());
    run.bind_web_jobs(open.clone(), JobLifecycle::default());
    bare_ok(&mut run, "web.export", json!({}));
    let job = run.ops().last().unwrap()["detail"]["job"].clone();
    assert_eq!((job["outcome"].as_str(), job["ticket"].as_u64()), (Some("completed"), Some(0)), "{job}");
    assert!(job["correlation"].is_u64() && job["bound_revision"].is_u64(), "{job}");
    // Canceled while admitted: publish copies nothing, settles canceled and keeps its partial receipt.
    let lifecycle = JobLifecycle::default();
    run.bind_web_jobs(Arc::new(JobsPort { cancel_on_admit: Some(cancel.clone()), ..JobsPort::default() }), lifecycle.clone());
    assert_eq!(bare_no(&mut run, "web.publish", json!({})), "canceled");
    let detail = run.ops().last().unwrap()["detail"].clone();
    assert_eq!((detail["receipt"]["complete"].as_bool(), detail["receipt"]["files"].as_array().map(Vec::len)), (Some(false), Some(0)), "{detail}");
    assert!(!dir.at("target/index.html").exists());
    assert_eq!(lifecycle.counts().canceled, 1);
    // Canceled before the command: the lifecycle refuses it, the port is never asked.
    let lifecycle = JobLifecycle::default();
    let port = Arc::new(JobsPort::default());
    run.bind_web_jobs(port.clone(), lifecycle.clone());
    assert_eq!(bare_no(&mut run, "web.site_report", json!({})), "canceled");
    assert_eq!((lifecycle.counts().refused_canceled, port.asked()), (1, 0));
}

/// Review F8: a style sheet whose comment holds `</style>` is scanned like any other and written back byte for byte.
#[test]
fn a_style_sheet_holding_end_style_text_blocks_nothing() {
    let dir = Scratch::new("end-style");
    site(&dir, false);
    let old = "/* IE hack: </style> and </STYLE */\np { background: url(../img/hero.png); }\n";
    dir.write("site/styles/old.css", old.as_bytes());
    dir.write("site/old.html", b"<!DOCTYPE html><html><head><title>o</title><link rel=\"stylesheet\" href=\"styles/old.css\"></head><body>o</body></html>");
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site"}));
        cmd(run, "web.export", json!({}));
        cmd(run, "web.move_file", json!({"from": "img/hero.png", "to": "img/h2.png"}));
    });
    assert_eq!(dir.read("site/styles/old.css"), old.replace("../img/hero.png", "../img/h2.png"));
    assert_eq!(dir.read("out/styles/old.css"), old);
}

/// Q2 (lead 2026-11-11): saving a document inside local_root takes the site lock, so a move's restore never
/// overwrites a concurrent save.
#[test]
fn a_save_inside_the_site_takes_the_site_lock() {
    let dir = Scratch::new("save-lock");
    let index = site(&dir, false);
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        define(run, &dir);
        cmd(run, "web.set_attribute", json!({"key": "#top", "name": "class", "value": "x"}));
        dir.write("site/.studio-site.lock", b"{}");
        assert_eq!(refused(run, "web.save", json!({"overwrite": true})), "site_locked");
        assert_eq!(dir.read("site/index.html"), INDEX, "nothing written");
        std::fs::remove_file(dir.at("site/.studio-site.lock")).unwrap();
        cmd(run, "web.save", json!({"overwrite": true}));
        assert!(!dir.at("site/.studio-site.lock").exists(), "released");
    });
    assert!(dir.read("site/index.html").contains("class=\"x\""));
}

/// Q3 (lead 2026-11-11): moving the recipe's entry document updates `recipe.file`, so the next site export finds it.
#[test]
fn moving_the_entry_document_updates_the_recipe() {
    let dir = Scratch::new("entry-move");
    site(&dir, false);
    with_run(|run| {
        define(run, &dir);
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "documents": "site", "file": "thanks.html"}));
        let moved = cmd(run, "web.move_file", json!({"from": "thanks.html", "to": "pages/thanks.html"}));
        assert_eq!(moved["web"]["recipe"]["file"], "pages/thanks.html", "{moved}");
        let export = cmd(run, "web.export", json!({}));
        assert_eq!(export["manifest"]["document"], "pages/thanks.html");
    });
}

#[cfg(feature = "viewer")]
mod through_the_viewer {
    use super::{FIXTURE, JobsPort as Port, Scratch, s, site};
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    /// R60 probe j3, its exact shape: a Viewer with no package open and no site, a saturated port bound after
    /// web.open and web.export_recipe: web.export asks it once, refuses `overloaded` and writes nothing.
    #[test]
    fn the_probe_j3_export_with_no_package_asks_the_viewers_port() {
        let dir = Scratch::new("viewer-no-package");
        let index = site(&dir, false);
        let mut v = Viewer::new(Prefer::Cpu);
        v.web_command("web.open", &json!({"path": s(&index)})).unwrap();
        v.web_command("web.export_recipe", &json!({"output_root": s(&dir.at("out"))})).unwrap();
        let saturated = Arc::new(Port { refuse: true, ..Port::default() });
        v.set_job_port(saturated.clone());
        assert_eq!(v.web_command("web.export", &json!({})), Err("overloaded".to_string()));
        assert_eq!(saturated.asked(), 1, "the port is asked exactly once");
        assert!(!dir.at("out").exists(), "nothing written");
    }

    /// R60 probe j3 (lead 2026-11-08): through a Viewer whose job port is saturated, web.export, web.publish, the site
    /// export, web.site_report and web.move_file ask that port once each, refuse `overloaded` and write nothing; an
    /// admitting port is asked once and released once per command.
    #[test]
    fn web_jobs_are_admitted_through_the_viewers_job_port() {
        let dir = Scratch::new("viewer-jobs");
        let index = site(&dir, false);
        let package = dir.write("doc/a.handshake", FIXTURE);
        let mut v = Viewer::new(Prefer::Cpu);
        v.open(&package).unwrap();
        let saturated = Arc::new(Port { refuse: true, ..Port::default() });
        v.set_job_port(saturated.clone());
        v.web_command("web.open", &json!({"path": s(&index)})).unwrap();
        v.web_command("web.site", &json!({"display_name": "harbour", "local_root": s(&dir.at("site"))})).unwrap();
        v.web_command("web.remote_target", &json!({"target_id": "local", "root_path": s(&dir.at("target"))})).unwrap();
        v.web_command("web.export_recipe", &json!({"output_root": s(&dir.at("out"))})).unwrap();
        let mut asked = 0;
        for (id, params, recipe) in [
            ("web.export", json!({}), None),
            ("web.publish", json!({}), None),
            ("web.export", json!({}), Some("site")),
            ("web.site_report", json!({}), None),
            ("web.move_file", json!({"from": "orphan.png", "to": "img/orphan.png"}), None),
        ] {
            if let Some(set) = recipe {
                v.web_command("web.export_recipe", &json!({"documents": set})).unwrap();
            }
            assert_eq!(v.web_command(id, &params), Err("overloaded".to_string()), "{id}");
            asked += 1;
            assert_eq!(saturated.admits.load(Ordering::SeqCst), asked, "{id}: the port is asked exactly once");
            let record = v.run().ops().last().unwrap();
            assert_eq!(record["target"], "retry_after_ms=7", "{record}");
        }
        assert!(!dir.at("out").exists() && !dir.at("target").exists() && !dir.at("site/.studio-site.lock").exists(), "nothing written");
        assert!(dir.at("site/orphan.png").exists());
        // Q1: the open export and publish admit on the open package; site work always on the site's own id.
        let package_id = v.run().snapshot().unwrap().document().document_id.clone();
        let seen = saturated.resources.lock().unwrap().clone();
        assert_eq!(&seen[..2], [package_id.clone(), package_id.clone()], "{seen:?}");
        assert!(seen[2] != package_id && seen[2..].iter().all(|r| *r == seen[2]), "site work admits on the site's id: {seen:?}");
        assert_eq!(*saturated.actions.lock().unwrap(), ["export", "export", "export", "export", "edit"]);
        // An admitting port: one admit and one release per command, and the work runs.
        let open = Arc::new(Port::default());
        v.set_job_port(open.clone());
        v.web_command("web.export", &json!({})).unwrap();
        v.web_command("web.site_report", &json!({})).unwrap();
        assert_eq!((open.admits.load(Ordering::SeqCst), open.releases.load(Ordering::SeqCst)), (2, 2));
        assert!(dir.at("out/index.html").is_file());
    }
}
