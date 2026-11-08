//! Lane SMB-COMMAND-SURFACE-1 (feature `viewer`): the typed command rows through the real process. Every
//! test spawns the built binary and speaks JSONL. Fixture: `tests/fixtures/demo.handshake` (revision 9,
//! group layer ...0001, raster layer ...0002).
#![cfg(feature = "viewer")]

use hsk_studio_harness::cli::{self, WindowArgs};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const PROCESS_LIMIT: Duration = Duration::from_secs(120);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-drive-typed-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn package(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, FIXTURE).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(feature = "desktop")]
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
}

#[cfg(not(feature = "desktop"))]
fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}

fn spawn(args: &[&str], diag: &Path) -> Child {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
    let mut command = Command::new(binary());
    command.args(args).current_dir(diag).env("STUDIO_DIAG_DIR", diag).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().expect("spawn studio-harness")
}

fn finish(mut child: Child, input: &[u8]) -> (i32, Vec<u8>) {
    let mut stdin = child.stdin.take().expect("stdin");
    let bytes = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let mut stderr = child.stderr.take().expect("stderr");
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut out);
        out
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if start.elapsed() > PROCESS_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process did not end within {PROCESS_LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap();
    let out = reader.join().unwrap();
    let _ = errors.join();
    (status.code().expect("an exit code, not a crash"), out)
}

fn run(args: &[&str], diag: &Path) -> (i32, Value) {
    let (exit, out) = finish(spawn(args, diag), b"");
    let text = String::from_utf8(out).expect("utf-8 stdout");
    (exit, serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {text}")))
}

/// One live session: each call writes one line and reads its one reply, so a later line can name what an
/// earlier reply returned.
struct Live {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl Live {
    fn open(package: &Path, actor: &str, diag: &Path) -> (Self, Value) {
        let p = package.to_str().unwrap();
        let mut child = spawn(&["command", p, "--actor", actor, "--session", "s-typed", "--stdin"], diag);
        let stdin = child.stdin.take().expect("stdin");
        let stdout = std::io::BufReader::new(child.stdout.take().expect("stdout"));
        let mut live = Self { child, stdin, stdout };
        let opened = live.read();
        assert_eq!(opened["ok"], true, "{opened}");
        (live, opened)
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut self.stdout, &mut line).expect("a reply line");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line}"))
    }

    fn call(&mut self, id: &str, parameters: Value) -> Value {
        writeln!(self.stdin, "{}", json!({"command":"command","command_id":id,"parameters":parameters})).unwrap();
        self.stdin.flush().unwrap();
        self.read()
    }

    fn ok(&mut self, id: &str, parameters: Value) -> Value {
        let reply = self.call(id, parameters);
        assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
        reply
    }

    fn refused(&mut self, id: &str, parameters: Value, code: &str) -> Value {
        let reply = self.call(id, parameters);
        assert_eq!(reply["ok"], false, "{id} should refuse: {}", reply["detail"]);
        assert_eq!(reply["error"], code, "{id}: {}", reply["detail"]);
        reply
    }

    fn close(mut self) -> i32 {
        drop(self.stdin);
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status.code().expect("an exit code, not a crash");
            }
            assert!(start.elapsed() < PROCESS_LIMIT, "the session did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn result_text(reply: &Value) -> String {
    reply["detail"]["result"].as_str().unwrap_or_else(|| panic!("no text result: {reply}")).to_string()
}

fn undo_name(reply: &Value) -> &str {
    reply["detail"]["undo_name"].as_str().unwrap_or_else(|| panic!("no undo name: {reply}"))
}

#[test]
fn every_layout_edit_is_one_named_undo_step_through_the_process() {
    let dir = Scratch::new("layout");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-a", &dir.0);
    // The editor opens on the first command; its blank document has one page.
    let first = s.ok("layout.page.new_parent", json!({"name":"Body"}));
    assert_eq!(undo_name(&first), "layout.page.new_parent");
    let editor = |reply: &Value| reply["state"]["shell"]["surfaces"]["layout_editor"].clone();
    let page = editor(&first)["pages"][0]["page_id"].as_str().expect("a page").to_string();

    let drawn = s.ok("layout.draw_frame", json!({"page_id":page,"x":10,"y":10,"w":200,"h":300}));
    assert_eq!(undo_name(&drawn), "layout.draw_frame");
    assert_eq!(drawn["detail"]["result"]["undo_steps"], 2);
    let frame = editor(&drawn)["frames"][0].clone();
    let (frame_id, story) = (frame["frame_id"].as_str().unwrap().to_string(), frame["story_id"].as_str().unwrap().to_string());
    assert_eq!(drawn["attribution"]["actor"]["account_id"], "lane-a");

    let columns = s.ok("layout.set_columns", json!({"frame":frame_id,"count":2,"gutter":12}));
    assert_eq!(editor(&columns)["frames"][0]["columns"], 2);
    let text = s.ok("layout.replace_text", json!({"story":story,"start":0,"end":0,"text":"Headline\nBody copy here"}));
    assert_eq!(editor(&text)["stories"][0]["bytes"], 23);
    // Known defect candidate, pinned by known_defect_replace_text_newline_keeps_one_paragraph.
    assert_eq!(editor(&text)["stories"][0]["paragraphs"], 1);
    let style = s.ok("layout.style.new_style", json!({"scope":"paragraph","name":"Heading"}));
    let style_id = editor(&style)["styles"]["paragraph"].as_array().and_then(|a| a.iter().find(|r| r["name"] == "Heading")).expect("the new style in state")["style_id"].as_str().unwrap().to_string();
    s.ok("layout.apply_paragraph_style", json!({"story":story,"paragraph":0,"style":style_id}));
    s.ok("layout.style.edit_style", json!({"style":style_id,"patch":{"font_size_pt":24,"based_on":null}}));
    let table = s.ok("layout.table.insert_table", json!({"story":story,"paragraph":0,"rows":2,"columns":2,"header_rows":1}));
    let table_id = editor(&table)["tables"][0]["table_id"].as_str().expect("the table in state").to_string();
    s.ok("layout.table.set_cell_text", json!({"table":table_id,"row":0,"column":0,"text":"Name"}));
    let page2 = s.ok("layout.page.insert_page", json!({"after":page,"new_spread":true}));
    assert_eq!(editor(&page2)["pages"].as_array().unwrap().len(), 2);
    let found = s.ok("layout.find", json!({"pattern":"Body"}));
    assert_eq!(found["detail"]["result"]["hits"].as_array().unwrap().len(), 1, "{found}");
    let changed = s.ok("layout.change", json!({"pattern":"Body","replacement":"Main"}));
    assert_eq!(undo_name(&changed), "layout.change");
    assert!(s.ok("layout.preflight", json!({}))["detail"]["result"]["ok"].is_boolean());

    // Undo walks the same names back, and a redo restores the step.
    let undone = s.ok("layout.undo", json!({}));
    assert_eq!(editor(&undone)["pages"].as_array().unwrap().len(), 2);
    assert_eq!(undone["detail"]["result"]["redo_name"], "layout.change");
    assert_eq!(undo_name(&undone), "layout.page.insert_page");
    let redone = s.ok("layout.redo", json!({}));
    assert_eq!(undo_name(&redone), "layout.change");

    // Typed refusals change nothing.
    let before = s.ok("layout.preflight", json!({}))["state"]["shell"]["surfaces"]["layout_editor"]["history"].clone();
    s.refused("layout.draw_frame", json!({"page_id":"no-such-page","x":0,"y":0,"w":1,"h":1}), "unknown_page");
    s.refused("layout.draw_frame", json!({"page_id":page,"x":0,"y":0,"w":-1,"h":1}), "invalid_geometry");
    s.refused("layout.draw_frame", json!({"page_id":page,"x":"NaN","y":0,"w":1,"h":1}), "parameter_invalid");
    s.refused("layout.set_columns", json!({"frame":frame_id,"count":4294967296u64,"gutter":0}), "parameter_invalid");
    s.refused("layout.table.insert_table", json!({"story":story,"paragraph":0,"rows":4294967295u64,"columns":4294967295u64,"header_rows":0}), "table_too_large");
    let after = s.ok("layout.preflight", json!({}))["state"]["shell"]["surfaces"]["layout_editor"]["history"].clone();
    assert_eq!(before, after);
    s.close();
}

#[test]
fn every_panel_family_runs_through_the_process_with_the_callers_attribution() {
    let dir = Scratch::new("panel");
    let package = dir.package("a.handshake");
    let (mut s, opened) = Live::open(&package, "lane-b", &dir.0);
    // A panel action needs a selection: it refuses first, then runs on the drawn shape.
    s.refused("panel.appearance.clear_appearance", json!({}), "command_refused");
    let a = result_text(&s.ok("viewer.draw_shape", json!({"kind":"rectangle","a":[10,10],"b":[110,60]})));
    let b = result_text(&s.ok("viewer.draw_shape", json!({"kind":"ellipse","a":[200,10],"b":[260,60]})));
    s.ok("viewer.select", json!({"layer":a}));

    let filled = s.ok("panel.appearance.set_fill", json!({"rgb8":[200,10,10]}));
    assert_eq!(filled["attribution"]["actor"]["account_id"], "lane-b");
    assert!(filled["revision"].as_u64() > opened["revision"].as_u64());
    assert!(!undo_name(&filled).is_empty(), "the run names the step");
    s.ok("panel.stroke.set_stroke", json!({"stroke":{"paint":{"kind":"solid","rgb8":[0,0,0]},"weight":2,"cap":"round","dash":[4,2]}}));
    s.ok("panel.stroke.set_stroke_markers", json!({"markers":{"start":"arrow.triangle","end":"arrow.none","alignment":"arrow_align.inside_path"}}));
    s.ok("panel.stroke.swap_stroke_markers", json!({}));
    let swatch = result_text(&s.ok("panel.swatches.add_swatch", json!({"name":"Brand","rgb8":[1,2,3]})));
    assert!(swatch.contains("swatch"));
    let style = result_text(&s.ok("panel.graphic_styles.add_graphic_style", json!({"name":"Look"})));
    assert!(style.contains("graphic style"));
    s.ok("panel.align_distribute.align", json!({"layers":[a,b],"to":"selection","how":"top"}));
    s.ok("panel.align_distribute.distribute", json!({"layers":[a,b],"to":"selection","how":"spacing","axis":0,"gap":5}));
    s.ok("panel.transform.transform", json!({"layers":[a],"op":{"kind":"rotate","degrees":15},"reference":"centre"}));
    s.ok("panel.transform.transform_each", json!({"layers":[a,b],"each":{"move_horizontal":3}}));
    s.ok("panel.path_operations.edit_path", json!({"edit":{"kind":"insert_anchor","contour":0,"segment":0,"t":0.5}}));
    let offset = result_text(&s.ok("panel.path_operations.offset_path", json!({"offset":2,"join":"round"})));
    assert!(offset.contains("offset path layer"));
    s.ok("panel.path_operations.outline_stroke", json!({}));
    s.ok("viewer.select", json!({"layer":b}));
    s.ok("panel.envelope_distort.make_envelope", json!({"spec":{"kind":"warp","style":"arc","axis":"horizontal","bend":40},"fidelity":50}));
    s.ok("panel.envelope_distort.release_envelope", json!({}));
    // The export reaches its method; it never replaces a file unasked, nor the opened package. The SVG
    // frames an artboard (lane SMB-ARTBOARD-EXPORT-1) and the fixture has none: one is added first.
    let fresh = dir.0.join("out.svg");
    add_board(&mut s);
    let exported = s.call("panel.vector_export.export_svg", json!({"path":fresh.to_str().unwrap()}));
    // Known defect candidate, pinned by known_defect_export_svg_group_duplicate_identity.
    assert_duplicate_identity(&exported);
    let taken = dir.0.join("taken.svg");
    std::fs::write(&taken, b"keep").unwrap();
    s.refused("panel.vector_export.export_svg", json!({"path":taken.to_str().unwrap()}), "overwrite_required");
    assert_eq!(std::fs::read(&taken).unwrap(), b"keep");
    s.refused("panel.vector_export.export_pdf", json!({"path":package.to_str().unwrap(),"overwrite":true}), "export_over_source");
    // Bad records are typed, at any depth, and nothing runs.
    s.refused("panel.stroke.set_stroke", json!({"stroke":{"paint":{"kind":"solid","rgb8":[0,0,300]},"weight":2}}), "parameter_invalid");
    s.refused("panel.stroke.set_stroke", json!({"stroke":{"paint":{"kind":"solid","rgb8":[0,0,0]},"weight":2,"nonsense":1}}), "parameter_unknown");
    s.refused("panel.transform.transform", json!({"layers":[a],"op":{"kind":"spin"}}), "parameter_invalid");
    s.refused("panel.align_distribute.align", json!({"layers":[a,b],"to":"selection","how":"sideways"}), "parameter_invalid");
    // Undo walks back through the run's history.
    s.ok("viewer.select", json!({"layer":a}));
    let before = s.ok("panel.stroke.swap_stroke_markers", json!({}))["revision"].as_u64().unwrap();
    writeln!(s.stdin, "{}", json!({"command":"undo"})).unwrap();
    s.stdin.flush().unwrap();
    assert!(s.read()["revision"].as_u64().unwrap() >= before);
    s.close();
}

#[test]
fn proof_vector_and_design_records_run_through_the_process() {
    let dir = Scratch::new("records");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-c", &dir.0);
    let a = result_text(&s.ok("viewer.draw_shape", json!({"kind":"rectangle","a":[10,10],"b":[110,60]})));

    // Proof: one named step per command, the state says so.
    assert!(undo_name(&s.ok("proof.select", json!({"name":"working_rgb"}))).starts_with("proof_select"));
    s.ok("proof.set_proof_colours", json!({"on":false}));
    s.ok("proof.set_gamut_warning", json!({"on":true}));
    s.ok("proof.set_gamut_style", json!({"colour":[1,0,0],"opacity":50}));
    s.ok("proof.select_colour_vision", json!({"vision":"deuteranopia"}));
    s.ok("proof.custom", json!({"condition":{"name":"Press","proof_profile":{"kind":"colour_vision","vision":"protanopia"},"proof_intent":"relative_colorimetric","display_intent":"relative_colorimetric","black_point_compensation":true,"simulate_paper_color":false,"simulate_black_ink":false}}));
    s.ok("proof.save_preset", json!({"name":"Press copy"}));
    s.refused("proof.select", json!({"name":"no such condition"}), "command_refused");
    s.refused("proof.custom", json!({"condition":{"name":"x"}}), "parameter_invalid");
    s.refused("proof.select_colour_vision", json!({"vision":"mantis"}), "parameter_invalid");

    // Vector records.
    let filled = s.ok("vector.set_fill", json!({"layer":a,"paint":{"kind":"linear","from":[0,0],"to":[10,0],"stops":[{"position":0,"rgb8":[255,0,0]},{"position":1,"rgb8":[0,0,255],"opacity":0.5}]}}));
    assert!(!undo_name(&filled).is_empty());
    s.ok("vector.set_stroke", json!({"layer":a,"stroke":{"paint":{"kind":"solid","rgb8":[0,0,0]},"weight":1}}));
    // A spot record reaches Folio, whose own range check judges the alternate value.
    let spot = s.call("vector.add_folio_swatch", json!({"name":"Spot","spec":{"kind":"spot","ink":"Reflex","measured_lab":[30,10,-40],"alternate":{"model":"cmyk","components":[100,70,0,0]}}}));
    assert!(spot["ok"] == true || spot["error"] == "command_refused", "{spot}");
    let swatch = result_text(&s.ok("vector.add_folio_swatch", json!({"name":"Process","spec":{"kind":"process","colour":{"model":"rgb","components":[0.5,0.5,0.5]}}})));
    s.ok("vector.edit_folio_swatch", json!({"swatch":swatch,"spec":{"kind":"process","colour":{"model":"rgb","components":[0.1,0.2,0.3]}}}));
    let gradient = result_text(&s.ok("vector.add_swatch", json!({"name":"Ramp","paint":{"kind":"radial","centre":[0,0],"radius":5,"stops":[{"position":0,"rgb8":[0,0,0]},{"position":1,"rgb8":[9,9,9]}]}})));
    s.ok("vector.edit_swatch", json!({"swatch":gradient,"paint":{"kind":"solid","rgb8":[5,5,5]}}));
    s.ok("vector.edit_path", json!({"layer":a,"edit":{"kind":"insert_anchor","contour":0,"segment":0,"t":0.25}}));
    s.ok("vector.transform_layers", json!({"layers":[a],"op":{"kind":"move","dx":1,"dy":2}}));
    let bounds = s.ok("vector.layer_bounds", json!({"layer":a}));
    assert!(bounds["detail"]["result"]["min"].is_array(), "{bounds}");
    s.ok("vector.set_tool_options", json!({"fill":[9,9,9],"stroke_on":false,"corner_radius":4}));
    s.refused("vector.set_fill", json!({"layer":a,"paint":{"kind":"mystery"}}), "parameter_invalid");
    s.refused("vector.add_font", json!({"family":"X","path":dir.0.join("absent.otf").to_str().unwrap()}), "resource_unreadable");

    // Design: a typed record reaches its method; a bad token refuses before it.
    s.refused("design.set_auto_layout_mode", json!({"node":a,"mode":"DIAGONAL"}), "parameter_invalid");
    let tracks = s.call("design.set_grid_tracks", json!({"node":a,"rows":[{"type":"FIXED","value":10}],"columns":[{"type":"HUG"}]}));
    assert_ne!(tracks["error"], "parameter_invalid", "{tracks}");
    assert_ne!(tracks["error"], "command_not_driveable", "{tracks}");
    s.close();
}

#[test]
fn photo_records_run_through_the_process() {
    let dir = Scratch::new("photo");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-d", &dir.0);
    // The fixture has no artboard: the method's own refusal, past every decoder.
    s.refused("photo.select_quick", json!({"layer":L2,"samples":[[1,1],[2,2]],"diameter_px":4}), "command_refused");
    s.ok("viewer.clear_selection", json!({}));
    let filled = s.call("photo.flood_fill", json!({"layer":L2,"x":1,"y":1,"tolerance":10}));
    assert_ne!(filled["error"], "parameter_invalid", "{filled}");
    let warped = s.call("photo.liquify", json!({"layer":L2,"tool":{"kind":"twirl","clockwise":false},"samples":[[1,1],[2,2]],"brush":{"size":8,"density":50,"pressure":50,"rate":50}}));
    assert_ne!(warped["error"], "parameter_invalid", "{warped}");
    assert_ne!(warped["error"], "command_not_driveable", "{warped}");
    s.ok("photo.info.add_sampler", json!({"x":1,"y":1}));
    s.ok("photo.info.set_model", json!({"model":"lab"}));
    s.refused("photo.info.set_model", json!({"model":"xyz"}), "parameter_invalid");
    s.refused("photo.liquify", json!({"layer":L2,"tool":{"kind":"melt"},"samples":[[1,1]],"brush":{"size":8,"density":50,"pressure":50,"rate":50}}), "parameter_invalid");
    let develop = s.call("photo.develop.kelvin", json!({"kelvin":5600}));
    assert_ne!(develop["error"], "parameter_invalid", "{develop}");
    assert_ne!(develop["error"], "command_not_driveable", "{develop}");
    s.close();
}

/// From `help --json` alone: every typed row refuses an undeclared key and a missing required key with
/// its own code, and the process survives every row.
#[test]
fn every_typed_row_of_the_manual_declares_its_parameters_and_refuses_by_type() {
    let dir = Scratch::new("sweep");
    let package = dir.package("a.handshake");
    let (exit, help) = run(&["help", "--json"], &dir.0);
    assert_eq!(exit, 0, "{help}");
    let rows = help["drive"]["typed"].as_array().expect("drive.typed");
    assert!(rows.len() >= 100, "{} typed rows", rows.len());
    let (mut s, _) = Live::open(&package, "lane-e", &dir.0);
    for row in rows {
        let id = row["command_id"].as_str().unwrap();
        let reply = s.call(id, json!({"not_a_parameter": 1}));
        assert_eq!(reply["error"], "parameter_unknown", "{id}: {reply}");
        let required: Vec<&str> = row["parameters"].as_array().unwrap().iter().filter(|p| p["required"] == true).filter_map(|p| p["key"].as_str()).collect();
        if !required.is_empty() {
            let reply = s.call(id, json!({}));
            assert_eq!(reply["error"], "parameter_missing", "{id}: {reply}");
        }
    }
    s.close();
}

#[test]
fn the_catalog_is_one_registry_and_old_ids_still_resolve() {
    let dir = Scratch::new("catalog");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-f", &dir.0);
    let listed = s.ok("catalog.list", json!({}));
    let descriptors = listed["detail"]["result"]["descriptors"].as_array().expect("descriptors");
    assert!(descriptors.len() > 300, "{}", descriptors.len());
    for d in descriptors {
        assert!(d["author_id"].as_str().is_some_and(|a| a.starts_with("studio.")), "{d}");
    }
    let resolve = |s: &mut Live, id: &str| s.ok("catalog.resolve", json!({"id":id}))["detail"]["result"].clone();
    // Each vocabulary reaches one record: a corpus row, a viewer row by its old and canonical id, a
    // timeline spec command, a menu leaf, a tool and a panel.
    assert_eq!(resolve(&mut s, "layer.set_visible")["kind"], "command");
    let pen = resolve(&mut s, "viewer.pen_point");
    assert_eq!(pen["command_id"], "vector.pen_point");
    assert_eq!(resolve(&mut s, "vector.pen_point")["author_id"], pen["author_id"]);
    assert_eq!(resolve(&mut s, "cmd.sequence.razorateditline")["command_id"], "timeline.razor");
    assert_eq!(resolve(&mut s, "studio.document.save")["kind"], "menu_item");
    assert_eq!(resolve(&mut s, "tool.lasso")["kind"], "tool");
    assert_eq!(resolve(&mut s, "panel.layers")["kind"], "panel");
    s.refused("catalog.resolve", json!({"id":"no.such.id"}), "unknown_command");
    // The canonical id of a viewer row runs the same method as its old id.
    let drawn = s.ok("vector.draw_shape", json!({"kind":"rectangle","a":[1,1],"b":[9,9]}));
    assert!(drawn["detail"]["result"].is_string(), "{drawn}");
    s.close();
}

fn assert_duplicate_identity(reply: &Value) {
    assert_eq!(reply["error"], "command_refused", "{reply}");
    assert!(reply["detail"]["message"].as_str().is_some_and(|m| m.contains("svg_export: folio DuplicateIdentity")), "{reply}");
}

/// The SVG export frames an artboard (lane SMB-ARTBOARD-EXPORT-1), so the demo fixture, which has none,
/// gets one before an export that must reach the engine.
fn add_board(s: &mut Live) {
    s.ok("artboard.add", json!({"name":"board","width":400,"height":100,"x":0,"y":0}));
}

/// KNOWN DEFECT (engine, not this lane's decoding): on the demo fixture, after the panel sequence of
/// `every_panel_family_runs_through_the_process_with_the_callers_attribution`, Export SVG refuses with
/// `svg_export: folio DuplicateIdentity SLYR-...0001` (the fixture's group layer). This pins today's
/// behaviour; when the engine is fixed, the assertion flips and this test is to be rewritten.
#[test]
fn known_defect_export_svg_group_duplicate_identity() {
    let dir = Scratch::new("known-svg");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-g", &dir.0);
    let a = result_text(&s.ok("viewer.draw_shape", json!({"kind":"rectangle","a":[10,10],"b":[110,60]})));
    let b = result_text(&s.ok("viewer.draw_shape", json!({"kind":"ellipse","a":[200,10],"b":[260,60]})));
    s.ok("viewer.select", json!({"layer":a}));
    s.ok("panel.appearance.set_fill", json!({"rgb8":[200,10,10]}));
    s.ok("panel.align_distribute.align", json!({"layers":[a,b],"to":"selection","how":"top"}));
    s.ok("panel.path_operations.offset_path", json!({"offset":2,"join":"round"}));
    s.ok("panel.path_operations.outline_stroke", json!({}));
    let out = dir.0.join("known.svg");
    // Without an artboard the export refuses before the engine, and writes nothing.
    let boardless = s.call("panel.vector_export.export_svg", json!({"path":out.to_str().unwrap()}));
    assert_eq!(boardless["error"], "command_refused", "{boardless}");
    assert!(boardless["detail"]["message"].as_str().is_some_and(|m| m.contains("svg_export: the document has no artboard")), "{boardless}");
    assert!(!out.exists());
    add_board(&mut s);
    assert_duplicate_identity(&s.call("panel.vector_export.export_svg", json!({"path":out.to_str().unwrap()})));
    s.close();
}

/// KNOWN DEFECT candidate (engine or Studio reading, UNVERIFIED): `layout.replace_text` with a `\n` in
/// the text keeps one paragraph (`hsk_studio_layout::replace_story_text`, `sentinels::ends_paragraph`).
/// The decoder passes the text unchanged. This pins today's behaviour.
#[test]
fn known_defect_replace_text_newline_keeps_one_paragraph() {
    let dir = Scratch::new("known-newline");
    let package = dir.package("a.handshake");
    let (mut s, _) = Live::open(&package, "lane-h", &dir.0);
    let first = s.ok("layout.preflight", json!({}));
    let page = first["state"]["shell"]["surfaces"]["layout_editor"]["pages"][0]["page_id"].as_str().expect("a page").to_string();
    let drawn = s.ok("layout.draw_frame", json!({"page_id":page,"x":10,"y":10,"w":200,"h":300}));
    let story = drawn["state"]["shell"]["surfaces"]["layout_editor"]["frames"][0]["story_id"].as_str().unwrap().to_string();
    let text = s.ok("layout.replace_text", json!({"story":story,"start":0,"end":0,"text":"one\ntwo"}));
    let st = &text["state"]["shell"]["surfaces"]["layout_editor"]["stories"][0];
    assert_eq!((st["bytes"].as_u64(), st["paragraphs"].as_u64()), (Some(7), Some(1)), "{st}");
    s.close();
}
