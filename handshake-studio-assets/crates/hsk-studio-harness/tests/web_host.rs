//! Lane SMB-WEB-HOST-1 ([STU-WEB-003], [STU-WEB-004], [STU-WEB-113] to [STU-WEB-120], [STU-WEB-127], [STU-WEB-131];
//! the board-slides-web scenario, `workflows/scenario-board-slides-web.json` steps 85 to 117): the web crates' first
//! host. Every step runs through `Run::web_command`: open, edit through source patches, undo and redo, save and reopen,
//! site and remote target, export recipe, export of a static tree, publish to a folder with a receipt, verify. The
//! process test (feature `viewer`) runs the same loop through the drive's JSONL session.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_harness_core::action::RUN_COMMANDS;
use hsk_studio_harness_core::harness::web_commands::{self, MANIFEST_FILE, RECEIPT_FILE};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
/// Mixed line endings, quoting and tag case: untouched bytes must survive every edit ([STU-WEB-004]).
const PAGE: &str = "<!DOCTYPE html>\r\n<HTML lang=en>\r\n\t<Head><title>Harbour Lights</title>\r\n\t<!-- draft notes -->\r\n\t</Head>\r\n\t<BODY id='page' class='a b'>\r\n\t\t<h1 id=\"title\">Draft</h1>\r\n\t</BODY>\r\n</HTML>\r\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-web-host-{}-{label}", std::process::id()));
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

fn text(run: &mut Run<'_>) -> String {
    let detail = cmd(run, "web.read", json!({"include_source": true}));
    detail["source"].as_str().unwrap().to_string()
}

fn key_of_tag(run: &mut Run<'_>, tag: &str) -> String {
    let rows = cmd(run, "web.read", json!({}))["rows"].clone();
    rows.as_array().unwrap().iter().find(|r| r["tag"] == tag).unwrap_or_else(|| panic!("no {tag} in {rows}"))["key"].as_str().unwrap().to_string()
}

#[test]
fn web_rows_are_in_the_corpus_and_run_through_web_command() {
    let rows: Vec<_> = RUN_COMMANDS.iter().filter(|c| c.run_method == "web_command").collect();
    // 20 rows of SMB-WEB-HOST-1 plus web.site_report and web.move_file (SMB-WEB-SITE-1) plus 11 rows of
    // SMB-WEB-SITE-2 (change link, templates, library items, snippets).
    assert_eq!(rows.len(), 33, "{:?}", rows.iter().map(|r| r.command_id).collect::<Vec<_>>());
    for r in &rows {
        assert!(r.command_id.starts_with("web."), "{}", r.command_id);
        assert_eq!(web_commands::row(r.command_id).map(|x| x.command_id), Some(r.command_id));
        assert!(!r.recordable, "{}", r.command_id);
    }
    let mutating: Vec<&str> = rows.iter().filter(|r| r.mutates_authority).map(|r| r.command_id).collect();
    assert_eq!(mutating, ["web.insert_element", "web.insert_fragment", "web.set_text", "web.set_attribute", "web.remove_attribute", "web.remove_node", "web.undo", "web.redo", "web.site", "web.remote_target", "web.export_recipe", "web.instance_param", "web.instance_repeat", "web.library_item_insert", "web.snippet", "web.snippet_insert"]);
    with_run(|run| {
        assert!(!run.web_command("web.nope", &json!({})));
        assert_eq!(run.ops().last().unwrap()["error"], "unknown_command");
        assert_eq!(refused(run, "web.read", json!({})), "no_web_document");
    });
}

/// Steps 89, 92, 93, 94, 96, 102 and 106: open, insert sections, copy, attributes, a stylesheet, the viewport meta
/// and a static form; undo and redo; save and reopen byte for byte.
#[test]
fn a_web_document_opens_edits_undoes_and_survives_save_and_reopen() {
    let dir = Scratch::new("edit");
    let index = dir.write("site/index.html", PAGE.as_bytes());
    let mut saved = String::new();
    with_run(|run| {
        let open = cmd(run, "web.open", json!({"path": s(&index), "doctype": "html_5"}));
        assert_eq!((open["web"]["revision"].as_u64(), open["web"]["dirty"].as_bool()), (Some(1), Some(false)));
        cmd(run, "web.insert_element", json!({"parent": "#page", "tag": "section", "attrs": {"id": "hero"}}));
        cmd(run, "web.insert_element", json!({"parent": "#hero", "tag": "h2", "text": "One night. Six hundred stalls."}));
        cmd(run, "web.set_text", json!({"key": "#title", "text": "Harbour Lights Night Market & more"}));
        cmd(run, "web.set_attribute", json!({"key": "#page", "name": "class", "value": "a b c"}));
        let head = key_of_tag(run, "head");
        cmd(run, "web.insert_element", json!({"parent": head, "tag": "style", "text": ":root { --brand: #0a3d62; } @media (min-width: 768px) { h1 { font-size: 2rem; } }"}));
        cmd(run, "web.insert_element", json!({"parent": head, "index": 0, "tag": "meta", "attrs": {"content": "width=device-width, initial-scale=1", "name": "viewport"}}));
        let before_form = text(run);
        let detail = cmd(run, "web.insert_fragment", json!({"parent": "#page", "fragment": "<form action=\"https://example.test/signup\" method=\"post\"><input type=\"email\" name=\"email\"><button type=\"submit\">Sign up</button></form>"}));
        let after_form = text(run);
        // Untouched bytes stay: line endings, tab indents, quote styles, tag case and the comment.
        assert!(after_form.starts_with("<!DOCTYPE html>\r\n<HTML lang=en>\r\n\t<Head><meta content=\"width=device-width, initial-scale=1\" name=\"viewport\"><title>"), "{after_form}");
        for kept in ["<!-- draft notes -->", "<BODY id='page' class='a b c'>", "Harbour Lights Night Market &amp; more</h1>", "</BODY>\r\n</HTML>\r\n", "<section id=\"hero\"><h2>One night. Six hundred stalls.</h2></section>"] {
            assert!(after_form.contains(kept), "{kept} missing from {after_form}");
        }
        // One named step per edit, attributed to the caller's context.
        let undo = detail["web"]["undo"].as_array().unwrap();
        assert_eq!(undo.len(), 7);
        let last = undo.last().unwrap();
        assert_eq!((last["label"].as_str(), last["command_id"].as_str(), last["actor"]["account_id"].as_str()), (Some("Insert Markup"), Some("web.insert_fragment"), Some("account")));
        assert_eq!((last["base_revision"].as_u64(), last["revision"].as_u64()), (Some(7), Some(8)));
        assert!(last["correlation"].as_u64().is_some());
        // Undo restores the exact bytes; redo reapplies them; the step keeps its attribution.
        cmd(run, "web.undo", json!({}));
        assert_eq!(text(run), before_form);
        let redo = cmd(run, "web.redo", json!({}));
        assert_eq!(text(run), after_form);
        assert_eq!(redo["web"]["undo"].as_array().unwrap().last().unwrap()["correlation"], last["correlation"]);
        for _ in 0..7 {
            cmd(run, "web.undo", json!({}));
        }
        assert_eq!(text(run), PAGE);
        assert_eq!(refused(run, "web.undo", json!({})), "nothing_to_undo");
        for _ in 0..7 {
            cmd(run, "web.redo", json!({}));
        }
        assert_eq!(text(run), after_form);
        // A save over the opened file needs the flag.
        assert_eq!(refused(run, "web.save", json!({})), "overwrite_required");
        let saved_detail = cmd(run, "web.save", json!({"overwrite": true}));
        assert_eq!((saved_detail["web"]["dirty"].as_bool(), saved_detail["site_record"].as_bool()), (Some(false), Some(false)));
        saved = after_form;
    });
    assert_eq!(std::fs::read_to_string(&index).unwrap(), saved);
    with_run(|run| {
        let open = cmd(run, "web.open", json!({"path": s(&index)}));
        assert_eq!(text(run), saved);
        assert_eq!(open["web"]["undo"], json!([]), "the history is in-process only");
    });
}

/// Steps 85 and 114, plus the recipe of step 113: a site with a local and an SFTP target (by reference only), a
/// recipe; secrets refused by name; undo and redo of record steps; they survive save and reopen.
#[test]
fn site_targets_and_recipe_are_steps_and_survive_save_and_reopen() {
    let dir = Scratch::new("site");
    let index = dir.write("site/index.html", PAGE.as_bytes());
    let root = s(&dir.at("site"));
    let state = std::cell::RefCell::new(Value::Null);
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        assert_eq!(refused(run, "web.remote_target", json!({"target_id": "local", "root_path": "x"})), "no_site");
        assert_eq!(refused(run, "web.site", json!({"local_root": root})), "parameter_missing");
        cmd(run, "web.site", json!({"display_name": "harbour-lights", "local_root": root, "site_relative_link_base": "site_root"}));
        cmd(run, "web.remote_target", json!({"target_id": "local", "transport": "local_or_network", "root_path": s(&dir.at("target"))}));
        cmd(run, "web.remote_target", json!({"target_id": "live", "transport": "sftp", "host": "sftp.example.test", "port": 22, "root_path": "/var/www", "credentials_ref": "cred:harbour-sftp"}));
        // A secret is refused by its field name; the value is never recorded.
        let before = run.ops().len();
        assert_eq!(refused(run, "web.remote_target", json!({"target_id": "live2", "transport": "sftp", "host": "h", "root_path": "/", "password": "hunter2"})), "credential_in_site_record");
        assert_eq!(run.ops().last().unwrap()["target"], "password");
        assert!(!serde_json::to_string(&run.ops()[before..]).unwrap().contains("hunter2"));
        assert_eq!(refused(run, "web.remote_target", json!({"target_id": "live3", "transport": "sftp", "host": "h", "root_path": "/"})), "parameter_missing");
        assert_eq!(refused(run, "web.remote_target", json!({"target_id": "x", "transport": "gopher", "root_path": "/"})), "transport_unknown");
        assert_eq!(refused(run, "web.remote_target", json!({"target_id": "local2", "root_path": "/", "credentials_ref": "c"})), "parameter_not_applicable");
        assert_eq!(refused(run, "web.export_recipe", json!({"output_root": "o", "emit_source_maps": true})), "source_maps_unsupported");
        assert_eq!(refused(run, "web.export_recipe", json!({"output_root": "o", "asset_rewriting": "hash"})), "asset_rewriting_unsupported");
        let recipe = cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "minify": true, "link_base": "site_root"}));
        assert_eq!(recipe["web"]["recipe"]["minify"], true);
        assert_eq!(refused(run, "web.export_recipe", json!({"minify": true})), "nothing_to_change");
        let undone = cmd(run, "web.undo", json!({}));
        assert_eq!(undone["web"]["recipe"], Value::Null);
        let redone = cmd(run, "web.redo", json!({}));
        assert_eq!(redone["web"]["recipe"], recipe["web"]["recipe"]);
        let saved = cmd(run, "web.save", json!({"overwrite": true}));
        assert_eq!(saved["site_record"], true);
        *state.borrow_mut() = json!({"site": saved["web"]["site"], "recipe": saved["web"]["recipe"]});
    });
    let sidecar = std::fs::read_to_string(dir.at("site/index.html.studio-site.json")).unwrap();
    assert!(sidecar.contains("cred:harbour-sftp") && !sidecar.contains("hunter2"), "{sidecar}");
    with_run(|run| {
        let open = cmd(run, "web.open", json!({"path": s(&index)}));
        assert_eq!(json!({"site": open["web"]["site"], "recipe": open["web"]["recipe"]}), *state.borrow());
        assert_eq!(open["web"]["site"]["remote_targets"].as_array().unwrap().len(), 2);
    });
    // A tampered record refuses the open and leaves the host closed.
    std::fs::write(dir.at("site/index.html.studio-site.json"), sidecar.replace("\"sftp\"", "\"gopher\"")).unwrap();
    with_run(|run| assert_eq!(refused(run, "web.open", json!({"path": s(&index)})), "site_record_invalid"));
}

const SITE_PAGE: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<!-- build notes -->\n<link rel=\"stylesheet\" href=\"styles.css\">\n<style>@media (min-width: 768px) { .narrow { display: none; } } @media (min-width: 1200px) { h1 { color: red; } }</style>\n<title>Harbour</title>\n</head>\n<body>\n<h1 id=\"top\">Harbour Lights</h1>\n<p class=\"narrow\">Swipe for stalls</p>\n<img alt=\"Harbour at night\" src=\"hero-768.png\" srcset=\"hero-768.png 768w, hero-1280.png 1280w\">\n<img alt=\"Map\" src=\"img/map.webp\">\n<a href=\"https://example.test/tickets\">Tickets</a> <a href=\"#top\">Top</a>\n<a href=\"index.html\">Home</a> <a href=\"/\">Root</a> <a href=\"docs/\">Docs</a>\n</body>\n</html>\n";

fn site_dir(dir: &Scratch) -> PathBuf {
    dir.write("site/styles.css", b"body { margin: 0; }\n");
    dir.write("site/hero-768.png", b"\x89PNG-768");
    dir.write("site/hero-1280.png", b"\x89PNG-1280");
    dir.write("site/img/map.webp", b"RIFF-map");
    dir.write("site/index.html", SITE_PAGE.as_bytes())
}

fn define_site(run: &mut Run<'_>, dir: &Scratch) {
    cmd(run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
    cmd(run, "web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("target"))}));
    cmd(run, "web.remote_target", json!({"target_id": "live", "transport": "sftp", "host": "sftp.example.test", "root_path": "/var/www", "credentials_ref": "cred:harbour"}));
    cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out")), "minify": true}));
}

/// Steps 97, 99, 101, 113, 116 and 117: breakpoints, structure at three widths, asset references, an export recipe
/// writing a static tree, publish to a local folder with a receipt, and verification of the published folder.
#[test]
fn export_writes_a_static_tree_and_publish_and_verify_close_the_loop() {
    let dir = Scratch::new("export");
    let index = site_dir(&dir);
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        let media = cmd(run, "web.media", json!({}));
        let mins: Vec<Value> = media["blocks"].as_array().unwrap().iter().map(|b| b["breakpoints"][0]["min_width_px"].clone()).collect();
        assert_eq!(mins, [json!(768.0), json!(1200.0)], "{media}");
        let narrow = |run: &mut Run<'_>, width: f64| {
            let tree = cmd(run, "web.preview", json!({"viewport_width_px": width}));
            tree["nodes"].as_array().unwrap().iter().find(|n| n["tag"] == "p").map(|n| n["rendered"].clone())
        };
        assert_eq!(narrow(run, 360.0), Some(json!(true)));
        assert_eq!(narrow(run, 768.0), Some(json!(false)));
        assert_eq!(narrow(run, 1280.0), Some(json!(false)));
        assert_eq!(refused(run, "web.preview", json!({"viewport_width_px": -1.0})), "invalid_viewport");
        // Without a site the document's folder is the root.
        let assets = cmd(run, "web.assets", json!({}));
        let granted: Vec<&str> = assets["assets"].as_array().unwrap().iter().filter_map(|a| a["outcome"]["id"].as_str()).collect();
        assert!(granted.contains(&"hero-1280.png") && granted.contains(&"img/map.webp") && granted.contains(&"styles.css"), "{assets}");
        assert_eq!(refused(run, "web.export", json!({})), "no_export_recipe");
        define_site(run, &dir);
        let export = cmd(run, "web.export", json!({}));
        let paths: Vec<&str> = export["manifest"]["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["hero-1280.png", "hero-768.png", "img/map.webp", "index.html", "styles.css"]);
        assert_eq!(export["manifest"]["external"], json!(["https://example.test/tickets"]));
        // Review F1: a link to the document itself and folder links are ordinary pages, listed and not refused.
        assert_eq!(export["manifest"]["links"], json!(["/", "docs/"]));
        let out = std::fs::read_to_string(dir.at("out/index.html")).unwrap();
        assert!(!out.contains("<!--") && out.starts_with("<!DOCTYPE html><html lang=\"en\"><head><link"), "minified by span: {out}");
        assert!(out.contains("<p class=\"narrow\">Swipe for stalls</p>\n<img"), "body whitespace is kept: {out}");
        assert_eq!(std::fs::read(dir.at("out/img/map.webp")).unwrap(), b"RIFF-map");
        // The export is a pure function of the bytes and the recipe: a second tree is identical.
        assert_eq!(refused(run, "web.export", json!({})), "output_not_empty");
        let manifest = std::fs::read(dir.at(&format!("out/{MANIFEST_FILE}"))).unwrap();
        cmd(run, "web.export_recipe", json!({"output_root": s(&dir.at("out2"))}));
        cmd(run, "web.export", json!({}));
        assert_eq!(std::fs::read(dir.at(&format!("out2/{MANIFEST_FILE}"))).unwrap(), manifest, "no output path, time or order in the manifest");
        assert_eq!(std::fs::read(dir.at("out2/index.html")).unwrap(), out.as_bytes());
        cmd(run, "web.undo", json!({}));
        // Publish: local only, re-hashed, a receipt with transfer modes and no credential.
        assert_eq!(refused(run, "web.publish", json!({"target_id": "live"})), "transport_unsupported");
        let published = cmd(run, "web.publish", json!({}));
        let receipt = &published["receipt"];
        assert_eq!((receipt["complete"].as_bool(), receipt["transport"].as_str(), receipt["site_id"].as_str()), (Some(true), Some("local_or_network"), Some("site")));
        let modes: Vec<(&str, &str, &str)> = receipt["files"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap(), f["outcome"].as_str().unwrap(), f["transfer_mode"].as_str().unwrap())).collect();
        assert_eq!(modes, [("hero-1280.png", "transferred", "binary"), ("hero-768.png", "transferred", "binary"), ("img/map.webp", "transferred", "binary"), ("index.html", "transferred", "text"), ("styles.css", "transferred", "text")]);
        assert_eq!(receipt["unmapped_extensions"], json!(["webp"]));
        assert!(receipt["elapsed_ms"].as_u64().is_some());
        assert!(!receipt.to_string().contains("cred:"), "{receipt}");
        assert!(dir.at(&format!("out/{RECEIPT_FILE}")).is_file());
        assert_eq!(std::fs::read(dir.at("target/index.html")).unwrap(), out.as_bytes());
        cmd(run, "web.remote_target", json!({"target_id": "src", "root_path": s(&dir.at("site"))}));
        assert_eq!(refused(run, "web.publish", json!({"target_id": "src"})), "publish_over_source");
        cmd(run, "web.remote_target", json!({"target_id": "inner", "root_path": s(&dir.at("out/live"))}));
        assert_eq!(refused(run, "web.publish", json!({"target_id": "inner"})), "publish_over_export");
        assert!(!dir.at("out/live").exists());
        let verified = cmd(run, "web.verify", json!({}));
        assert_eq!((verified["mismatched"].as_u64(), verified["documents_parsed"].as_u64()), (Some(0), Some(1)));
        std::fs::write(dir.at("target/styles.css"), b"tampered").unwrap();
        assert_eq!(refused(run, "web.verify", json!({})), "verify_mismatch");
        let files = run.ops().last().unwrap()["detail"]["files"].clone();
        assert!(files.as_array().unwrap().iter().any(|f| f["path"] == "styles.css" && f["outcome"] == "mismatch"), "{files}");
        // A changed export is not published.
        std::fs::write(dir.at("out/styles.css"), b"changed").unwrap();
        assert_eq!(refused(run, "web.publish", json!({})), "parameter_missing", "three local targets: name one");
        assert_eq!(refused(run, "web.publish", json!({"target_id": "local"})), "export_changed");
    });
}

/// Hostile input: bounded work, typed refusals, nothing changed and nothing written (V4-01, V4-02, V4-08).
#[test]
fn hostile_input_is_refused_typed_and_changes_nothing() {
    let dir = Scratch::new("hostile");
    let big = dir.write("big.html", &vec![b'a'; (8 << 20) + 1]);
    let latin1 = dir.write("latin1.html", b"<p>caf\xe9</p>");
    let deep = dir.write("deep.html", format!("{}{}", "<div>".repeat(300), "</div>".repeat(300)).as_bytes());
    with_run(|run| {
        assert_eq!(refused(run, "web.open", json!({"path": s(&dir.at("missing.html"))})), "file_missing");
        assert_eq!(refused(run, "web.open", json!({"path": s(&big)})), "file_too_large");
        assert_eq!(refused(run, "web.open", json!({"path": s(&latin1)})), "source_not_utf8");
        assert_ne!(refused(run, "web.open", json!({"path": s(&deep)})), "", "a typed limit");
        assert_eq!(refused(run, "web.open", json!({"path": "a\u{0}b"})), "path_invalid");
        assert_eq!(refused(run, "web.open", json!({"source": "<p>x</p>", "doctype": "html_4"})), "doctype_unsupported");
        assert_eq!(refused(run, "web.open", json!({"source": "<p>x</p>", "path": "a.html"})), "parameter_invalid");
        assert_eq!(refused(run, "web.open", json!({"source": "<p>x</p>", "colour": 1})), "parameter_unknown");
        let unbalanced = refused(run, "web.open", json!({"source": "<div><p>x</div>"}));
        assert!(!unbalanced.is_empty(), "misnesting is a typed refusal");
        cmd(run, "web.open", json!({"source": "<!DOCTYPE html><html><head><script>let a = 1;</script></head><body id=\"b\"><p id=\"p\">Hi <em>you</em></p></body></html>"}));
        assert_eq!(refused(run, "web.set_text", json!({"key": "nonsense", "text": "x"})), "key_invalid");
        assert_eq!(refused(run, "web.set_text", json!({"key": "/9:html", "text": "x"})), "no_such_node");
        assert_eq!(refused(run, "web.set_text", json!({"key": "#p", "text": "x"})), "has_markup_children");
        assert_eq!(refused(run, "web.insert_element", json!({"parent": "#b", "index": 99999, "tag": "p"})), "index_out_of_range");
        assert_eq!(refused(run, "web.insert_element", json!({"parent": "#b", "index": -1, "tag": "p"})), "parameter_invalid");
        assert_eq!(refused(run, "web.insert_element", json!({"parent": "#b", "tag": "1bad"})), "invalid_name");
        assert_eq!(refused(run, "web.insert_element", json!({"parent": "#b", "tag": "p", "attrs": {"id": 3}})), "parameter_invalid");
        assert_eq!(refused(run, "web.insert_element", json!({"parent": "#b", "tag": "br", "text": "x"})), "void");
        assert!(!refused(run, "web.insert_fragment", json!({"parent": "#b", "fragment": "<p>open"})).is_empty());
        // Script text that would close the element early (V4-08: escape, decoy, case).
        let script = key_of_tag(run, "script");
        for hostile in ["</script><img src=x>", "a</SCRIPT >b", "<!--<script>"] {
            assert_eq!(refused(run, "web.set_text", json!({"key": format!("{script}/0:#raw"), "text": hostile})), "raw_child", "{hostile}");
            assert_eq!(refused(run, "web.set_text", json!({"key": script.clone(), "text": hostile})), "unsafe_raw_text", "{hostile}");
        }
        assert_eq!(refused(run, "web.set_attribute", json!({"key": "#p", "name": "on click", "value": "x"})), "invalid_name");
        assert_eq!(refused(run, "web.set_attribute", json!({"key": "#p", "name": "id", "value": "p"})), "nothing_to_change");
        assert_eq!(refused(run, "web.save", json!({})), "parameter_missing");
    });
    // An asset outside the root or missing fails the export closed; nothing is written.
    let img = |src: &str| format!("<img alt=\"a\" src=\"{src}\">");
    let mut cases = vec![
        ("escape", img("../secret.png"), "asset_denied"),
        ("missing", img("gone.png"), "asset_missing"),
        ("backslash", img("img\\a.png"), "asset_denied"),
        ("reserved", img("con.png"), "asset_denied"),
        // Review F3: a reserved output name collides before anything is written.
        ("manifest", format!("<a href=\"{MANIFEST_FILE}\">m</a>"), "export_path_collision"),
    ];
    if cfg!(windows) {
        // NTFS folds case: two spellings of one file are one output name.
        cases.push(("case", format!("{}{}", img("logo.png"), img("Logo.png")), "export_path_collision"));
    }
    for (label, body, code) in cases {
        let reference = body.clone();
        let index = dir.write(&format!("{label}/site/index.html"), format!("<!DOCTYPE html><html><head><title>t</title></head><body>{body}</body></html>").as_bytes());
        dir.write(&format!("{label}/secret.png"), b"secret");
        dir.write(&format!("{label}/site/con.png"), b"device name");
        dir.write(&format!("{label}/site/logo.png"), b"logo");
        dir.write(&format!("{label}/site/{MANIFEST_FILE}"), b"{}");
        let out = dir.at(&format!("{label}/out"));
        with_run(|run| {
            cmd(run, "web.open", json!({"path": s(&index)}));
            cmd(run, "web.export_recipe", json!({"output_root": s(&out)}));
            assert_eq!(refused(run, "web.export", json!({})), code, "{reference}");
        });
        assert!(!out.exists(), "{label}: nothing written");
    }
}

/// The same loop through the drive's JSONL session (feature `viewer`): one process, one reply per line, the
/// result reaches save, export, publish and verify.
#[cfg(feature = "viewer")]
#[test]
fn web_commands_run_in_the_drive_session() {
    use hsk_studio_harness::cli::{self, WindowArgs};
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = Scratch::new("drive");
    let index = site_dir(&dir);
    let package = dir.write("a.handshake", FIXTURE);
    let line = |id: &str, parameters: Value| json!({"command": "command", "command_id": id, "parameters": parameters});
    let input = [
        line("web.open", json!({"path": s(&index)})),
        line("web.insert_element", json!({"parent": "#top", "tag": "small", "text": "Friday"})),
        line("web.set_attribute", json!({"key": "#top", "name": "class", "value": "headline"})),
        line("web.undo", json!({})),
        line("web.redo", json!({})),
        line("web.save", json!({"overwrite": true})),
        line("web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))})),
        line("web.remote_target", json!({"target_id": "local", "root_path": s(&dir.at("target"))})),
        line("web.export_recipe", json!({"output_root": s(&dir.at("out")), "minify": true})),
        line("web.export", json!({})),
        line("web.publish", json!({})),
        line("web.verify", json!({})),
        line("web.save", json!({"path": s(&package), "overwrite": true})),
        json!({"command": "command", "command_id": "web.set_text", "parameters": {"key": "#top", "text": "x"}, "expected_revision": 1}),
    ];
    let bytes: Vec<u8> = input.iter().flat_map(|v| format!("{v}\n").into_bytes()).collect();
    let args = ["command", package.to_str().unwrap(), "--actor", "lane-web", "--session", "s-web", "--stdin"];
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
    for (i, reply) in out.iter().enumerate().take(input.len() - 1).skip(1) {
        let id = input[i - 1]["command_id"].as_str().unwrap();
        assert_eq!(reply["ok"], true, "line {i} {id}: {reply}");
        assert_eq!(reply["attribution"]["actor"]["account_id"], "lane-web");
    }
    assert_eq!(out[1]["detail"]["web"]["revision"], 1);
    assert_eq!(out[5]["detail"]["web"]["undo"][1]["actor"]["account_id"], "lane-web", "{}", out[5]);
    assert_eq!(out[11]["detail"]["receipt"]["complete"], true, "{}", out[11]);
    assert_eq!(out[12]["detail"]["mismatched"], 0, "{}", out[12]);
    assert_eq!(out[13]["error"], "export_over_source", "{}", out[13]);
    assert_eq!(out[14]["error"], "stale_revision", "{}", out[14]);
    let end = out.last().unwrap();
    assert_eq!((end["lines"].as_u64(), end["refused"].as_u64()), (Some(14), Some(2)), "{end}");
    assert_eq!(output.status.code(), Some(1), "a refused line makes the exit 1");
    let saved = std::fs::read_to_string(&index).unwrap();
    assert!(saved.contains("<h1 id=\"top\" class=\"headline\">Harbour Lights<small>Friday</small></h1>"), "{saved}");
    assert_eq!(std::fs::read(dir.at("target/index.html")).unwrap().len(), std::fs::read(dir.at("out/index.html")).unwrap().len());
    assert_eq!(std::fs::read(&package).unwrap(), FIXTURE, "the package is never replaced by a web save");
}

/// Review F2: an undone site does not come back after save and reopen.
#[test]
fn an_undone_site_stays_undone_after_save_and_reopen() {
    let dir = Scratch::new("undone-site");
    let index = dir.write("site/index.html", PAGE.as_bytes());
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        cmd(run, "web.site", json!({"display_name": "harbour", "local_root": s(&dir.at("site"))}));
        cmd(run, "web.save", json!({"overwrite": true}));
        cmd(run, "web.undo", json!({}));
        let saved = cmd(run, "web.save", json!({"overwrite": true}));
        assert_eq!(saved["site_record"], false);
    });
    with_run(|run| {
        let open = cmd(run, "web.open", json!({"path": s(&index)}));
        assert_eq!(open["web"]["site"], Value::Null, "{open}");
    });
}

/// Review F6: after a web edit the run's undo does not reach an older package step; it refuses, naming web.undo.
#[test]
fn history_undo_refuses_while_the_newest_step_is_a_web_step() {
    const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
    with_run(|run| {
        cmd(run, "web.open", json!({"source": "<!DOCTYPE html><html><head><title>t</title></head><body><p id=\"p\">a</p></body></html>"}));
        cmd(run, "web.set_text", json!({"key": "#p", "text": "b"}));
        assert!(!run.undo());
        assert_eq!((run.ops().last().unwrap()["error"].as_str(), run.ops().last().unwrap()["target"].as_str()), (Some("newest_step_is_web"), Some("web.undo")));
        assert!(run.rename(L1, "renamed", None), "{}", run.ops().last().unwrap());
        cmd(run, "web.set_text", json!({"key": "#p", "text": "c"}));
        let revision = run.snapshot().unwrap().document().revision;
        assert!(!run.undo(), "the web edit is newer than the rename");
        assert_eq!(run.snapshot().unwrap().document().revision, revision, "nothing was undone");
        cmd(run, "web.undo", json!({}));
        assert!(run.undo(), "the rename is the newest step now: {}", run.ops().last().unwrap());
    });
}

/// Review F5: a junction or symlinked folder inside the root never grants a file outside it.
#[test]
fn a_linked_folder_does_not_reach_outside_the_root() {
    let dir = Scratch::new("link-escape");
    dir.write("outside/secret.png", b"secret");
    let index = dir.write("site/index.html", b"<!DOCTYPE html><html><head><title>t</title></head><body><img alt=\"a\" src=\"media/secret.png\"></body></html>");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(dir.at("outside"), dir.at("site/media"));
    #[cfg(not(windows))]
    let linked = std::os::unix::fs::symlink(dir.at("outside"), dir.at("site/media"));
    if linked.is_err() {
        println!("NOT_PROVEN: a_linked_folder_does_not_reach_outside_the_root: this account may not create a folder symlink");
        return;
    }
    let out = dir.at("out");
    with_run(|run| {
        cmd(run, "web.open", json!({"path": s(&index)}));
        cmd(run, "web.export_recipe", json!({"output_root": s(&out)}));
        assert_eq!(refused(run, "web.export", json!({})), "asset_denied");
    });
    assert!(!out.exists());
}
